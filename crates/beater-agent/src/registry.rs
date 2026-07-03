//! One registry for local and networked tools: Python files (embedded CPython),
//! Rust built-ins, remote MCP providers, and (later) inline TS + sandboxed wasm.
//! Every tool declares `idempotent` — the resume-safety contract
//! (ARCHITECTURE.md §5).

use std::collections::HashMap;
use std::fmt;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail, ensure};
use base64::Engine as _;
use serde::Deserialize;
use serde_json::{Value, json};

pub const DEFAULT_BEATBOX_URL: &str = "http://127.0.0.1:7300";
const DEFAULT_PYTHON_TIMEOUT_MS: u64 = 10_000;

#[derive(Clone, Eq, PartialEq)]
pub struct BeatboxConfig {
    pub url: String,
    pub api_key: Option<String>,
}

impl fmt::Debug for BeatboxConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BeatboxConfig")
            .field("url", &self.url)
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

impl Default for BeatboxConfig {
    fn default() -> Self {
        Self {
            url: DEFAULT_BEATBOX_URL.to_string(),
            api_key: None,
        }
    }
}

impl BeatboxConfig {
    fn client(&self) -> beatbox_client::Client {
        let client = beatbox_client::Client::new(&self.url);
        match &self.api_key {
            Some(api_key) => client.with_api_key(api_key),
            None => client,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct AgentConfig {
    pub name: String,
    #[serde(default = "default_model")]
    pub model: String,
    #[serde(default)]
    pub system: String,
    #[serde(default)]
    pub tools: Vec<ToolDecl>,
}

fn default_model() -> String {
    "claude-opus-4-8".to_string()
}

#[derive(Debug, Deserialize)]
pub struct ToolDecl {
    pub kind: String, // "python" | "rust" | "remote_mcp" | "browser" | "sandbox"
    pub name: String,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub idempotent: bool,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default, rename = "inputSchema", alias = "input_schema")]
    pub input_schema: Option<Value>,
    #[serde(default)]
    pub endpoint: Option<String>,
    #[serde(default)]
    pub tool: Option<String>,
    #[serde(default)]
    pub auth: Option<RemoteMcpAuthDecl>,
    #[serde(default, rename = "timeoutMs", alias = "timeout_ms")]
    pub timeout_ms: Option<u64>,
    #[serde(default)]
    pub retry: Option<RemoteMcpRetryDecl>,
    #[serde(default)]
    pub egress: Vec<String>,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub session: Option<BrowserSessionDecl>,
    #[serde(default, rename = "allowedOrigins", alias = "allowed_origins")]
    pub allowed_origins: Vec<String>,
    #[serde(default)]
    pub secrets: Value,
    #[serde(default)]
    pub lane: Option<beatbox_client::Lane>,
    #[serde(default)]
    pub source: Option<SandboxSourceDecl>,
    #[serde(default)]
    pub policy: Option<Value>,
    #[serde(default)]
    pub entrypoint: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SandboxSourceDecl {
    Path { path: String },
    Wat { text: String },
    WasmWat { text: String },
    WasmBase64 { bytes: String },
    WasmBytesBase64 { bytes: String },
    Inline { code: String },
    ModuleRef { sha256: String },
}

#[derive(Debug, Deserialize)]
pub struct RemoteMcpAuthDecl {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    env: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct RemoteMcpRetryDecl {
    #[serde(default)]
    attempts: Option<u32>,
    #[serde(default, rename = "backoffMs", alias = "backoff_ms")]
    backoff_ms: Option<u64>,
    #[serde(default, rename = "idempotencyKey", alias = "idempotency_key")]
    idempotency_key: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct BrowserSessionDecl {
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    cleanup: Option<String>,
}

pub enum ToolImpl {
    Python {
        agent_dir: PathBuf,
        path: PathBuf,
        timeout: Duration,
    },
    RustBuiltin,
    RemoteMcp {
        config: RemoteMcpTool,
    },
    Browser {
        config: BrowserTool,
    },
    Sandbox(Box<SandboxTool>),
}

pub struct SandboxTool {
    beatbox: BeatboxConfig,
    lane: beatbox_client::Lane,
    source: beatbox_client::Source,
    policy: beatbox_client::Policy,
    entrypoint: Option<String>,
}

pub struct ToolEntry {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    pub idempotent: bool,
    pub imp: ToolImpl,
}

pub struct ToolRegistry {
    tools: Vec<ToolEntry>,
}

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct ToolNeedsReview {
    message: String,
}

impl ToolNeedsReview {
    fn remote_ambiguous(tool: &str, error: anyhow::Error) -> Self {
        Self {
            message: format!(
                "remote MCP tool {tool} had an ambiguous failure after the request may have \
                 reached the provider; review the remote system before retrying: {error:#}"
            ),
        }
    }
}

#[derive(Default)]
pub struct ToolCallContext {
    pub tool_use_id: Option<String>,
    pub idempotency_key: Option<String>,
}

impl ToolRegistry {
    /// Build from an agent's tool declarations. Python tool metadata comes
    /// from each file's module-level TOOL dict.
    pub fn build(agent_dir: &Path, decls: &[ToolDecl]) -> Result<Self> {
        Self::build_with_beatbox(agent_dir, decls, &BeatboxConfig::default())
    }

    pub fn build_with_beatbox(
        agent_dir: &Path,
        decls: &[ToolDecl],
        beatbox: &BeatboxConfig,
    ) -> Result<Self> {
        let mut tools = Vec::new();
        for decl in decls {
            match decl.kind.as_str() {
                "python" => {
                    let timeout = python_timeout(decl)?;
                    let rel = decl
                        .path
                        .as_deref()
                        .with_context(|| format!("python tool {} has no path", decl.name))?;
                    let (agent_dir, path) = contained_agent_path(agent_dir, rel, "python tool")
                        .with_context(|| format!("resolving python tool {}", decl.name))?;
                    let (description, input_schema) = beater_py::load_tool_spec(&path)
                        .with_context(|| format!("loading python tool {}", decl.name))?;
                    tools.push(ToolEntry {
                        name: decl.name.clone(),
                        description,
                        input_schema,
                        idempotent: decl.idempotent,
                        imp: ToolImpl::Python {
                            agent_dir,
                            path,
                            timeout,
                        },
                    });
                }
                "rust" => {
                    let entry = rust_builtin(&decl.name)
                        .with_context(|| format!("unknown rust builtin tool {}", decl.name))?;
                    tools.push(entry);
                }
                "remote_mcp" => {
                    let description = decl.description.clone().with_context(|| {
                        format!("remote_mcp tool {} requires description", decl.name)
                    })?;
                    let input_schema = decl.input_schema.clone().with_context(|| {
                        format!("remote_mcp tool {} requires inputSchema", decl.name)
                    })?;
                    let config = RemoteMcpTool::from_decl(decl)?;
                    tools.push(ToolEntry {
                        name: decl.name.clone(),
                        description,
                        input_schema,
                        idempotent: decl.idempotent,
                        imp: ToolImpl::RemoteMcp { config },
                    });
                }
                "browser" => {
                    let description = decl.description.clone().with_context(|| {
                        format!("browser tool {} requires description", decl.name)
                    })?;
                    let input_schema = decl.input_schema.clone().with_context(|| {
                        format!("browser tool {} requires inputSchema", decl.name)
                    })?;
                    let config = BrowserTool::from_decl(decl)?;
                    tools.push(ToolEntry {
                        name: decl.name.clone(),
                        description,
                        input_schema,
                        idempotent: decl.idempotent,
                        imp: ToolImpl::Browser { config },
                    });
                }
                "sandbox" => {
                    let lane = decl.lane.clone().unwrap_or(beatbox_client::Lane::Wasm);
                    if !matches!(lane, beatbox_client::Lane::Wasm) {
                        bail!(
                            "sandbox tool {} requested lane {lane:?}; beater.js M3 enables only beatbox wasm",
                            decl.name
                        );
                    }
                    let source = sandbox_source(agent_dir, decl)
                        .with_context(|| format!("loading sandbox source for {}", decl.name))?;
                    let policy = sandbox_policy(decl.policy.as_ref())
                        .with_context(|| format!("parsing sandbox policy for {}", decl.name))?;
                    let description = decl.description.clone().unwrap_or_else(|| {
                        format!("Run {} through beatbox's sandboxed wasm lane.", decl.name)
                    });
                    let input_schema = decl
                        .input_schema
                        .clone()
                        .unwrap_or_else(|| json!({"type": "object", "properties": {}}));
                    tools.push(ToolEntry {
                        name: decl.name.clone(),
                        description,
                        input_schema,
                        idempotent: decl.idempotent,
                        imp: ToolImpl::Sandbox(Box::new(SandboxTool {
                            beatbox: beatbox.clone(),
                            lane,
                            source,
                            policy,
                            entrypoint: decl.entrypoint.clone(),
                        })),
                    });
                }
                other => bail!("unknown tool kind {other:?} for tool {}", decl.name),
            }
        }
        Ok(Self { tools })
    }

    pub fn empty() -> Self {
        Self { tools: Vec::new() }
    }

    /// Merge another registry in; first declaration wins on name collision.
    pub fn extend(&mut self, other: ToolRegistry) {
        for tool in other.tools {
            if self.get(&tool.name).is_some() {
                tracing::warn!(
                    "duplicate tool {} across agents — keeping the first",
                    tool.name
                );
            } else {
                self.tools.push(tool);
            }
        }
    }

    pub fn entries(&self) -> &[ToolEntry] {
        &self.tools
    }

    pub fn get(&self, name: &str) -> Option<&ToolEntry> {
        self.tools.iter().find(|t| t.name == name)
    }

    /// Tool definitions in Messages API shape.
    pub fn api_tools(&self) -> Value {
        Value::Array(
            self.tools
                .iter()
                .map(|t| {
                    json!({
                        "name": t.name,
                        "description": t.description,
                        "input_schema": t.input_schema,
                    })
                })
                .collect(),
        )
    }

    /// Execute a tool; returns the result serialized as a JSON string
    /// (the tool_result content).
    pub async fn execute(&self, name: &str, input: &Value) -> Result<String> {
        self.execute_with_context(name, input, &ToolCallContext::default())
            .await
    }

    pub async fn execute_with_context(
        &self,
        name: &str,
        input: &Value,
        context: &ToolCallContext,
    ) -> Result<String> {
        let tool = self
            .get(name)
            .with_context(|| format!("no tool named {name}"))?;
        match &tool.imp {
            ToolImpl::Python {
                agent_dir,
                path,
                timeout,
            } => {
                let path = canonical_contained_path(agent_dir, path, "python tool")?;
                beater_py::call_tool_with_timeout(path.clone(), input.to_string(), *timeout).await
            }
            ToolImpl::RustBuiltin => execute_builtin(name, input),
            ToolImpl::RemoteMcp { config } => config.execute(input, context).await,
            ToolImpl::Browser { config } => config.execute(input, context).await,
            ToolImpl::Sandbox(sandbox) => {
                execute_sandbox(
                    &sandbox.beatbox,
                    sandbox.lane.clone(),
                    sandbox.source.clone(),
                    sandbox.policy.clone(),
                    sandbox.entrypoint.clone(),
                    input.clone(),
                    context.idempotency_key.clone(),
                )
                .await
            }
        }
    }
}

fn python_timeout(decl: &ToolDecl) -> Result<Duration> {
    let timeout_ms = decl.timeout_ms.unwrap_or(DEFAULT_PYTHON_TIMEOUT_MS);
    ensure!(
        timeout_ms > 0,
        "python tool {} timeoutMs must be greater than 0",
        decl.name
    );
    Ok(Duration::from_millis(timeout_ms))
}

fn sandbox_source(agent_dir: &Path, decl: &ToolDecl) -> Result<beatbox_client::Source> {
    if decl.path.is_some() && decl.source.is_some() {
        bail!(
            "sandbox tool {} cannot declare both source and path",
            decl.name
        );
    }
    match decl.source.as_ref() {
        Some(SandboxSourceDecl::Path { path }) => sandbox_source_path(agent_dir, path),
        Some(SandboxSourceDecl::Wat { text }) | Some(SandboxSourceDecl::WasmWat { text }) => {
            Ok(beatbox_client::Source::WasmWat { text: text.clone() })
        }
        Some(SandboxSourceDecl::WasmBase64 { bytes })
        | Some(SandboxSourceDecl::WasmBytesBase64 { bytes }) => {
            Ok(beatbox_client::Source::WasmBytesBase64 {
                bytes: bytes.clone(),
            })
        }
        Some(SandboxSourceDecl::Inline { code }) => {
            Ok(beatbox_client::Source::Inline { code: code.clone() })
        }
        Some(SandboxSourceDecl::ModuleRef { .. }) => {
            bail!("module_ref sandbox sources are not supported by the pinned beatbox M3 API")
        }
        None => {
            let path = decl
                .path
                .as_deref()
                .with_context(|| format!("sandbox tool {} has no source or path", decl.name))?;
            sandbox_source_path(agent_dir, path)
        }
    }
}

fn sandbox_source_path(agent_dir: &Path, path: &str) -> Result<beatbox_client::Source> {
    let (_, path) = contained_agent_path(agent_dir, path, "sandbox source")?;
    let bytes = std::fs::read(&path)
        .with_context(|| format!("reading sandbox source {}", path.display()))?;
    if path.extension().and_then(|ext| ext.to_str()) == Some("wat") {
        let text = String::from_utf8(bytes)
            .with_context(|| format!("sandbox WAT source {} is not UTF-8", path.display()))?;
        Ok(beatbox_client::Source::WasmWat { text })
    } else {
        Ok(beatbox_client::Source::WasmBytesBase64 {
            bytes: base64::engine::general_purpose::STANDARD.encode(bytes),
        })
    }
}

fn contained_agent_path(agent_dir: &Path, path: &str, label: &str) -> Result<(PathBuf, PathBuf)> {
    let path = agent_dir.join(path.trim_start_matches("./"));
    let agent_dir = agent_dir
        .canonicalize()
        .with_context(|| format!("canonicalizing agent dir {}", agent_dir.display()))?;
    let path = canonical_contained_path(&agent_dir, &path, label)?;
    Ok((agent_dir, path))
}

fn canonical_contained_path(agent_dir: &Path, path: &Path, label: &str) -> Result<PathBuf> {
    let path = path
        .canonicalize()
        .with_context(|| format!("canonicalizing {label} {}", path.display()))?;
    if !path.starts_with(agent_dir) {
        bail!(
            "{label} {} escapes agent directory {}",
            path.display(),
            agent_dir.display()
        );
    }
    Ok(path)
}

fn sandbox_policy(value: Option<&Value>) -> Result<beatbox_client::Policy> {
    let Some(value) = value else {
        return Ok(beatbox_client::Policy::default());
    };
    if !value.is_object() {
        bail!("sandbox policy must be an object");
    }
    validate_policy_overlay(value)?;
    let mut merged = serde_json::to_value(beatbox_client::Policy::default())?;
    merge_json(&mut merged, value);
    Ok(serde_json::from_value(merged)?)
}

fn validate_policy_overlay(value: &Value) -> Result<()> {
    validate_object_keys(
        value,
        "policy",
        &[
            "fs",
            "net",
            "env",
            "secrets",
            "limits",
            "determinism",
            "double_jail",
        ],
    )?;
    if let Some(fs) = value.get("fs") {
        validate_object_keys(fs, "policy.fs", &["workspace", "mounts"])?;
        if let Some(mounts) = fs.get("mounts") {
            for (index, mount) in mounts.as_array().into_iter().flatten().enumerate() {
                validate_object_keys(
                    mount,
                    &format!("policy.fs.mounts[{index}]"),
                    &["host", "guest", "mode"],
                )?;
            }
        }
    }
    if let Some(net) = value.get("net") {
        validate_object_keys(net, "policy.net", &["kind", "allow_domains", "allow_ports"])?;
    }
    if let Some(secrets) = value.get("secrets") {
        for (index, secret) in secrets.as_array().into_iter().flatten().enumerate() {
            validate_object_keys(
                secret,
                &format!("policy.secrets[{index}]"),
                &["name", "value_ref", "expose"],
            )?;
        }
    }
    if let Some(limits) = value.get("limits") {
        validate_object_keys(
            limits,
            "policy.limits",
            &[
                "wall_ms",
                "cpu_ms",
                "memory_bytes",
                "output_bytes",
                "pids",
                "disk_bytes",
                "fuel",
            ],
        )?;
    }
    if let Some(determinism) = value.get("determinism") {
        validate_object_keys(
            determinism,
            "policy.determinism",
            &["kind", "seed", "epoch_ms"],
        )?;
    }
    Ok(())
}

fn validate_object_keys(value: &Value, path: &str, allowed: &[&str]) -> Result<()> {
    let Some(object) = value.as_object() else {
        return Ok(());
    };
    for key in object.keys() {
        if !allowed.contains(&key.as_str()) {
            bail!("unknown {path}.{key}");
        }
    }
    Ok(())
}

fn merge_json(base: &mut Value, overlay: &Value) {
    match (base, overlay) {
        (Value::Object(base), Value::Object(overlay)) => {
            for (key, value) in overlay {
                match base.get_mut(key) {
                    Some(base_value) => merge_json(base_value, value),
                    None => {
                        base.insert(key.clone(), value.clone());
                    }
                }
            }
        }
        (base, overlay) => *base = overlay.clone(),
    }
}

async fn execute_sandbox(
    beatbox: &BeatboxConfig,
    lane: beatbox_client::Lane,
    source: beatbox_client::Source,
    policy: beatbox_client::Policy,
    entrypoint: Option<String>,
    input: Value,
    idempotency_key: Option<String>,
) -> Result<String> {
    let request = beatbox_client::ExecuteRequest {
        lane,
        source,
        entrypoint,
        input,
        stdin: String::new(),
        policy,
        idempotency_key,
    };
    let client = beatbox.client();
    let result = if request.idempotency_key.is_some() {
        execute_sandbox_job(&client, &request).await?
    } else {
        client.execute(&request).await?
    };
    Ok(serde_json::to_string(&result)?)
}

async fn execute_sandbox_job(
    client: &beatbox_client::Client,
    request: &beatbox_client::ExecuteRequest,
) -> Result<beatbox_client::ExecutionResult> {
    let job = client.create_job(request).await?;
    let deadline = Instant::now()
        .checked_add(job_poll_timeout(request.policy.limits.wall_ms))
        .ok_or_else(|| anyhow!("sandbox job timeout overflow"))?;
    loop {
        let record = client.get_job(&job.job_id).await?;
        match record.status {
            beatbox_client::JobStatus::Queued | beatbox_client::JobStatus::Running => {
                if Instant::now() >= deadline {
                    let _ = client.cancel_job(&job.job_id).await;
                    bail!(
                        "sandbox job {} did not finish before local poll timeout",
                        job.job_id
                    );
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            beatbox_client::JobStatus::Succeeded => {
                return record.result.ok_or_else(|| {
                    anyhow!("sandbox job {} succeeded without a result", job.job_id)
                });
            }
            beatbox_client::JobStatus::Failed => {
                if let Some(error) = record.error {
                    bail!(
                        "sandbox job {} failed: {}: {}",
                        job.job_id,
                        error.code,
                        error.message
                    );
                }
                bail!("sandbox job {} failed without an error body", job.job_id);
            }
            beatbox_client::JobStatus::Canceled => {
                bail!("sandbox job {} was canceled", job.job_id);
            }
        }
    }
}

fn job_poll_timeout(wall_ms: u64) -> Duration {
    Duration::from_millis(wall_ms.saturating_add(5_000).max(5_000))
}

pub struct RemoteMcpTool {
    endpoint: reqwest::Url,
    remote_tool: String,
    auth: RemoteMcpAuth,
    timeout: Duration,
    retry: RemoteMcpRetry,
    idempotent: bool,
}

pub struct BrowserTool {
    provider: BrowserProvider,
    timeout: Duration,
    session: BrowserSessionPolicy,
    allowed_origins: Vec<String>,
}

enum BrowserProvider {
    MockCdp,
}

struct BrowserSessionPolicy {
    scope: BrowserSessionScope,
    cleanup: BrowserSessionCleanup,
}

enum BrowserSessionScope {
    Run,
}

enum BrowserSessionCleanup {
    Always,
}

enum RemoteMcpAuth {
    None,
    BearerEnv(String),
}

struct RemoteMcpRetry {
    attempts: u32,
    backoff: Duration,
    idempotency_key: Option<IdempotencyKeySource>,
}

enum IdempotencyKeySource {
    ToolUseId,
}

enum RemoteAttempt {
    Retryable(anyhow::Error),
    ProviderFailure(anyhow::Error),
    Fatal(anyhow::Error),
}

static ACTIVE_BROWSER_SESSIONS: OnceLock<Mutex<HashMap<String, usize>>> = OnceLock::new();

fn active_browser_sessions() -> &'static Mutex<HashMap<String, usize>> {
    ACTIVE_BROWSER_SESSIONS.get_or_init(|| Mutex::new(HashMap::new()))
}

impl BrowserTool {
    fn from_decl(decl: &ToolDecl) -> Result<Self> {
        let provider = match decl
            .provider
            .as_deref()
            .map(str::trim)
            .filter(|provider| !provider.is_empty())
        {
            Some("mock_cdp") => BrowserProvider::MockCdp,
            Some(other) => bail!(
                "browser tool {} unsupported provider {other:?}; supported providers: mock_cdp",
                decl.name
            ),
            None => bail!("browser tool {} requires provider", decl.name),
        };
        let session = BrowserSessionPolicy::from_decl(decl)?;
        let timeout_ms = decl.timeout_ms.unwrap_or(30_000);
        ensure!(
            timeout_ms > 0,
            "browser tool {} timeoutMs must be greater than 0",
            decl.name
        );
        let allowed_origins = decl
            .allowed_origins
            .iter()
            .map(|origin| canonical_origin(origin))
            .collect::<Result<Vec<_>>>()
            .with_context(|| format!("browser tool {} has invalid allowedOrigins", decl.name))?;
        ensure!(
            browser_secrets_empty(&decl.secrets),
            "browser tool {} provider mock_cdp does not support secrets",
            decl.name
        );
        Ok(Self {
            provider,
            timeout: Duration::from_millis(timeout_ms),
            session,
            allowed_origins,
        })
    }

    async fn execute(&self, input: &Value, context: &ToolCallContext) -> Result<String> {
        let fut = async {
            let session_id = context.tool_use_id.as_deref().unwrap_or("manual");
            let session = BrowserSessionGuard::start(session_id);
            let result = match self.provider {
                BrowserProvider::MockCdp => self.execute_mock_cdp(input, session.id()).await,
            };
            drop(session);
            result
        };
        tokio::time::timeout(self.timeout, fut)
            .await
            .with_context(|| {
                format!(
                    "browser tool timed out after {}ms",
                    self.timeout.as_millis()
                )
            })?
    }

    async fn execute_mock_cdp(&self, input: &Value, session_id: &str) -> Result<String> {
        if let Some(delay_ms) = input.get("delayMs").and_then(Value::as_u64) {
            tokio::time::sleep(Duration::from_millis(delay_ms)).await;
        }
        let url = input.get("url").and_then(Value::as_str);
        if let Some(url) = url {
            self.ensure_url_allowed(url)?;
        }
        let task = input
            .get("task")
            .and_then(Value::as_str)
            .unwrap_or("inspect page");
        Ok(json!({
            "provider": "mock_cdp",
            "session": {
                "id": session_id,
                "scope": self.session.scope.as_str(),
                "cleanup": self.session.cleanup.as_str(),
            },
            "url": url,
            "title": "Mock Browser Page",
            "text": format!("completed browser task: {task}"),
        })
        .to_string())
    }

    fn ensure_url_allowed(&self, raw_url: &str) -> Result<()> {
        let origin = origin_from_url(raw_url)?;
        ensure!(
            self.allowed_origins
                .iter()
                .any(|allowed| allowed == &origin),
            "browser navigation to {origin} is not allowed by allowedOrigins {:?}",
            self.allowed_origins
        );
        Ok(())
    }
}

impl BrowserSessionPolicy {
    fn from_decl(decl: &ToolDecl) -> Result<Self> {
        let scope = match decl
            .session
            .as_ref()
            .and_then(|session| session.scope.as_deref())
            .unwrap_or("run")
        {
            "run" => BrowserSessionScope::Run,
            other => bail!(
                "browser tool {} unsupported session.scope {other:?}; supported scopes: run",
                decl.name
            ),
        };
        let cleanup = match decl
            .session
            .as_ref()
            .and_then(|session| session.cleanup.as_deref())
            .unwrap_or("always")
        {
            "always" => BrowserSessionCleanup::Always,
            other => bail!(
                "browser tool {} unsupported session.cleanup {other:?}; supported cleanup: always",
                decl.name
            ),
        };
        Ok(Self { scope, cleanup })
    }
}

impl BrowserSessionScope {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Run => "run",
        }
    }
}

impl BrowserSessionCleanup {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Always => "always",
        }
    }
}

struct BrowserSessionGuard {
    id: String,
}

impl BrowserSessionGuard {
    fn start(id: &str) -> Self {
        active_browser_sessions()
            .lock()
            .expect("browser session set poisoned")
            .entry(id.to_string())
            .and_modify(|count| *count += 1)
            .or_insert(1);
        Self { id: id.to_string() }
    }

    fn id(&self) -> &str {
        &self.id
    }
}

impl Drop for BrowserSessionGuard {
    fn drop(&mut self) {
        let mut sessions = active_browser_sessions()
            .lock()
            .expect("browser session set poisoned");
        sessions
            .entry(self.id.clone())
            .and_modify(|count| *count = count.saturating_sub(1));
        sessions.retain(|_, count| *count > 0);
    }
}

#[cfg(test)]
fn browser_session_active_for_tests(id: &str) -> bool {
    active_browser_sessions()
        .lock()
        .expect("browser session set poisoned")
        .get(id)
        .copied()
        .unwrap_or_default()
        > 0
}

#[cfg(test)]
fn browser_session_count_for_tests(id: &str) -> usize {
    active_browser_sessions()
        .lock()
        .expect("browser session set poisoned")
        .get(id)
        .copied()
        .unwrap_or_default()
}

impl RemoteMcpTool {
    fn from_decl(decl: &ToolDecl) -> Result<Self> {
        let endpoint = parse_remote_endpoint(
            decl.endpoint
                .as_deref()
                .with_context(|| format!("remote_mcp tool {} requires endpoint", decl.name))?,
        )?;
        ensure!(
            egress_allows(&endpoint, &decl.egress),
            "remote_mcp tool {} endpoint {} is not allowed by egress policy {:?}",
            decl.name,
            endpoint,
            decl.egress
        );
        let remote_tool = decl
            .tool
            .as_deref()
            .map(str::trim)
            .filter(|tool| !tool.is_empty())
            .with_context(|| format!("remote_mcp tool {} requires tool", decl.name))?
            .to_string();
        let timeout_ms = decl.timeout_ms.unwrap_or(10_000);
        ensure!(
            timeout_ms > 0,
            "remote_mcp tool {} timeoutMs must be greater than 0",
            decl.name
        );
        let auth = match &decl.auth {
            None => RemoteMcpAuth::None,
            Some(auth) if auth.kind == "bearer" => {
                let env = auth
                    .env
                    .as_deref()
                    .map(str::trim)
                    .filter(|env| !env.is_empty())
                    .with_context(|| {
                        format!("remote_mcp tool {} bearer auth requires env", decl.name)
                    })?
                    .to_string();
                RemoteMcpAuth::BearerEnv(env)
            }
            Some(auth) if auth.kind == "none" => RemoteMcpAuth::None,
            Some(auth) => bail!(
                "remote_mcp tool {} has unsupported auth type {:?}",
                decl.name,
                auth.kind
            ),
        };
        ensure!(
            !matches!(auth, RemoteMcpAuth::BearerEnv(_))
                || endpoint.scheme() == "https"
                || is_loopback_endpoint(&endpoint),
            "remote_mcp tool {} bearer auth requires https for non-loopback endpoints: {}",
            decl.name,
            endpoint
        );
        let retry = RemoteMcpRetry::from_decl(decl)?;
        ensure!(
            decl.idempotent || retry.attempts <= 1 || retry.idempotency_key.is_some(),
            "remote_mcp tool {} retry attempts > 1 requires idempotencyKey for non-idempotent tools",
            decl.name
        );
        Ok(Self {
            endpoint,
            remote_tool,
            auth,
            timeout: Duration::from_millis(timeout_ms),
            retry,
            idempotent: decl.idempotent,
        })
    }

    async fn execute(&self, input: &Value, context: &ToolCallContext) -> Result<String> {
        let bearer = self.bearer_token()?;
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .context("build remote MCP HTTP client")?;
        let attempts = self.effective_attempts(context);
        for attempt in 1..=attempts {
            match self
                .send_once(&client, input, context, bearer.as_deref())
                .await
            {
                Ok(result) => return Ok(result),
                Err(RemoteAttempt::Retryable(error)) if attempt < attempts => {
                    tracing::warn!(
                        "remote MCP tool {} attempt {attempt}/{attempts} failed: {error:#}",
                        self.remote_tool
                    );
                    tokio::time::sleep(self.retry.backoff).await;
                }
                Err(RemoteAttempt::Retryable(error) | RemoteAttempt::ProviderFailure(error))
                    if !self.idempotent =>
                {
                    return Err(ToolNeedsReview::remote_ambiguous(&self.remote_tool, error).into());
                }
                Err(
                    RemoteAttempt::Retryable(error)
                    | RemoteAttempt::ProviderFailure(error)
                    | RemoteAttempt::Fatal(error),
                ) => {
                    return Err(error);
                }
            }
        }
        bail!("remote MCP tool {} exhausted retries", self.remote_tool)
    }

    fn bearer_token(&self) -> Result<Option<String>> {
        match &self.auth {
            RemoteMcpAuth::None => Ok(None),
            RemoteMcpAuth::BearerEnv(env) => {
                let value = std::env::var(env)
                    .with_context(|| format!("remote_mcp bearer env {env} is not set"))?;
                ensure!(
                    !value.trim().is_empty(),
                    "remote_mcp bearer env {env} is empty"
                );
                Ok(Some(value))
            }
        }
    }

    fn effective_attempts(&self, context: &ToolCallContext) -> u32 {
        if self.idempotent || self.idempotency_key(context).is_some() {
            self.retry.attempts
        } else {
            1
        }
    }

    fn idempotency_key(&self, context: &ToolCallContext) -> Option<String> {
        match self.retry.idempotency_key {
            Some(IdempotencyKeySource::ToolUseId) => context.tool_use_id.clone(),
            None => None,
        }
    }

    async fn send_once(
        &self,
        client: &reqwest::Client,
        input: &Value,
        context: &ToolCallContext,
        bearer: Option<&str>,
    ) -> std::result::Result<String, RemoteAttempt> {
        let id = context.tool_use_id.as_deref().unwrap_or("1");
        let body = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": {
                "name": self.remote_tool,
                "arguments": input,
            },
        });
        let mut request = client
            .post(self.endpoint.clone())
            .timeout(self.timeout)
            .header("accept", "application/json")
            .header("content-type", "application/json")
            .header("mcp-protocol-version", "2025-11-25")
            .json(&body);
        if let Some(bearer) = bearer {
            request = request.header("authorization", format!("Bearer {bearer}"));
        }
        if let Some(key) = self.idempotency_key(context) {
            request = request.header("idempotency-key", key);
        }

        let response = request.send().await.map_err(|error| {
            let error = anyhow!(
                "remote MCP tool {} request to {} failed: {error}",
                self.remote_tool,
                self.endpoint
            );
            RemoteAttempt::Retryable(error)
        })?;
        let status = response.status();
        let text = response.text().await.map_err(|error| {
            RemoteAttempt::Retryable(anyhow!(
                "remote MCP tool {} response body failed: {error}",
                self.remote_tool
            ))
        })?;
        if status.is_server_error() || status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Err(RemoteAttempt::Retryable(anyhow!(
                "remote MCP tool {} returned HTTP {status}: {text}",
                self.remote_tool
            )));
        }
        if !status.is_success() {
            return Err(RemoteAttempt::Fatal(anyhow!(
                "remote MCP tool {} returned HTTP {status}: {text}",
                self.remote_tool
            )));
        }
        let message: Value = serde_json::from_str(&text).map_err(|error| {
            RemoteAttempt::Fatal(anyhow!(
                "remote MCP tool {} returned invalid JSON: {error}: {text}",
                self.remote_tool
            ))
        })?;
        if message["jsonrpc"] != "2.0" {
            return Err(RemoteAttempt::Fatal(anyhow!(
                "remote MCP tool {} response has invalid jsonrpc version: {}",
                self.remote_tool,
                message["jsonrpc"]
            )));
        }
        if message["id"] != id {
            return Err(RemoteAttempt::Fatal(anyhow!(
                "remote MCP tool {} response id {} did not match request id {id:?}",
                self.remote_tool,
                message["id"]
            )));
        }
        if let Some(error) = message.get("error") {
            return Err(RemoteAttempt::ProviderFailure(anyhow!(
                "remote MCP tool {} returned JSON-RPC error: {error}",
                self.remote_tool
            )));
        }
        let result = message.get("result").ok_or_else(|| {
            RemoteAttempt::Fatal(anyhow!(
                "remote MCP tool {} response has no result",
                self.remote_tool
            ))
        })?;
        if result["isError"].as_bool().unwrap_or(false) {
            return Err(RemoteAttempt::ProviderFailure(anyhow!(
                "remote MCP tool {} returned isError: {}",
                self.remote_tool,
                mcp_content_text(result)
            )));
        }
        mcp_result_to_string(result).map_err(RemoteAttempt::Fatal)
    }
}

impl RemoteMcpRetry {
    fn from_decl(decl: &ToolDecl) -> Result<Self> {
        let attempts = decl
            .retry
            .as_ref()
            .and_then(|retry| retry.attempts)
            .unwrap_or(1);
        ensure!(
            (1..=5).contains(&attempts),
            "remote_mcp tool {} retry attempts must be between 1 and 5",
            decl.name
        );
        let backoff_ms = decl
            .retry
            .as_ref()
            .and_then(|retry| retry.backoff_ms)
            .unwrap_or(250);
        let idempotency_key = decl
            .retry
            .as_ref()
            .and_then(|retry| retry.idempotency_key.as_deref())
            .map(|source| match source {
                "tool_use_id" => Ok(IdempotencyKeySource::ToolUseId),
                other => bail!(
                    "remote_mcp tool {} unsupported idempotencyKey {other:?}",
                    decl.name
                ),
            })
            .transpose()?;
        Ok(Self {
            attempts,
            backoff: Duration::from_millis(backoff_ms),
            idempotency_key,
        })
    }
}

fn parse_remote_endpoint(raw: &str) -> Result<reqwest::Url> {
    let url = reqwest::Url::parse(raw.trim())
        .with_context(|| format!("remote_mcp endpoint is not a URL: {raw:?}"))?;
    ensure!(
        matches!(url.scheme(), "http" | "https"),
        "remote_mcp endpoint must use http or https: {url}"
    );
    ensure!(
        url.host_str().is_some(),
        "remote_mcp endpoint has no host: {url}"
    );
    ensure!(
        url.username().is_empty() && url.password().is_none(),
        "remote_mcp endpoint must not include credentials: {url}"
    );
    ensure!(
        url.query().is_none() && url.fragment().is_none(),
        "remote_mcp endpoint must not include query or fragment: {url}"
    );
    Ok(url)
}

fn canonical_origin(origin: &str) -> Result<String> {
    let url = reqwest::Url::parse(origin.trim())
        .with_context(|| format!("browser allowed origin is not a URL: {origin:?}"))?;
    ensure!(
        matches!(url.scheme(), "http" | "https"),
        "browser allowed origin must use http or https: {origin}"
    );
    ensure!(
        url.host_str().is_some(),
        "browser allowed origin has no host: {origin}"
    );
    ensure!(
        url.username().is_empty()
            && url.password().is_none()
            && url.path() == "/"
            && url.query().is_none()
            && url.fragment().is_none(),
        "browser allowed origin must be an origin without credentials, path, query, or fragment: {origin}"
    );
    origin_from_url(origin)
}

fn origin_from_url(raw_url: &str) -> Result<String> {
    let url = reqwest::Url::parse(raw_url)
        .with_context(|| format!("browser URL is not valid: {raw_url:?}"))?;
    ensure!(
        matches!(url.scheme(), "http" | "https"),
        "browser URL must use http or https: {raw_url}"
    );
    let host = url
        .host_str()
        .with_context(|| format!("browser URL has no host: {raw_url}"))?;
    Ok(format!(
        "{}://{}{}",
        url.scheme(),
        host,
        url.port()
            .map(|port| format!(":{port}"))
            .unwrap_or_default()
    ))
}

fn browser_secrets_empty(secrets: &Value) -> bool {
    matches!(secrets, Value::Null)
        || secrets
            .as_object()
            .map(serde_json::Map::is_empty)
            .unwrap_or(false)
}

fn egress_allows(endpoint: &reqwest::Url, egress: &[String]) -> bool {
    let Some(host) = endpoint.host_str() else {
        return false;
    };
    let host_port = endpoint.port().map(|port| format!("{host}:{port}"));
    egress.iter().any(|allowed| {
        let allowed = allowed.trim();
        allowed == host || host_port.as_deref() == Some(allowed)
    })
}

fn is_loopback_endpoint(endpoint: &reqwest::Url) -> bool {
    let Some(host) = endpoint.host_str() else {
        return false;
    };
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<IpAddr>()
            .map(|addr| addr.is_loopback())
            .unwrap_or(false)
}

fn mcp_result_to_string(result: &Value) -> Result<String> {
    let text = mcp_content_text(result);
    if !text.is_empty() {
        return Ok(text);
    }
    Ok(result.to_string())
}

fn mcp_content_text(result: &Value) -> String {
    result
        .get("content")
        .and_then(Value::as_array)
        .map(|content| {
            content
                .iter()
                .filter_map(|item| item.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

fn rust_builtin(name: &str) -> Option<ToolEntry> {
    match name {
        "get_time" => Some(ToolEntry {
            name: name.to_string(),
            description: "Get the current date and time (UTC).".to_string(),
            input_schema: json!({"type": "object", "properties": {}}),
            idempotent: true, // no side effects; safe to re-run on resume
            imp: ToolImpl::RustBuiltin,
        }),
        _ => None,
    }
}

fn execute_builtin(name: &str, _input: &Value) -> Result<String> {
    match name {
        "get_time" => {
            let now = chrono::Utc::now();
            Ok(json!({"iso": now.to_rfc3339(), "unix": now.timestamp()}).to_string())
        }
        _ => bail!("no rust builtin {name}"),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    use serde_json::{Value, json};

    use super::{
        BrowserSessionGuard, ToolCallContext, ToolDecl, ToolImpl, ToolNeedsReview, ToolRegistry,
        browser_session_active_for_tests, browser_session_count_for_tests,
    };

    #[test]
    fn hello_slow_fixture_tools_preserve_resume_contract() {
        let agent_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join("examples/hello/agents/support");
        let registry = ToolRegistry::build(
            &agent_dir,
            &[
                py_decl("slow_summarize", "./tools/slow_summarize.py", true),
                py_decl(
                    "slow_summarize_once",
                    "./tools/slow_summarize_once.py",
                    false,
                ),
            ],
        )
        .expect("slow fixture tools should load");

        let slow = registry.get("slow_summarize").expect("slow_summarize");
        assert!(slow.idempotent);
        assert!(
            slow.description
                .contains("explicitly asks for slow_summarize by name")
        );

        let once = registry
            .get("slow_summarize_once")
            .expect("slow_summarize_once");
        assert!(!once.idempotent);
        assert!(
            once.description
                .contains("explicitly asks for slow_summarize_once by name")
        );
    }

    #[test]
    fn python_tool_rejects_paths_outside_agent_dir_before_loading() {
        let fixture = TempAgentDir::new("python-containment-build");
        let sentinel = fixture.path.join("outside-loaded.txt");
        let outside = fixture.write_outside_tool("outside.py", &sentinel);

        for path in [
            "../../outside.py".to_string(),
            outside.to_string_lossy().into_owned(),
        ] {
            let error = match ToolRegistry::build(
                fixture.agent_dir.as_path(),
                &[py_decl("escape", &path, true)],
            ) {
                Ok(_) => panic!("escaping python tool path should fail: {path}"),
                Err(error) => error,
            };
            assert!(
                format!("{error:#}").contains("escapes agent directory"),
                "{error:#}"
            );
            assert!(
                !sentinel.exists(),
                "escaping python tool should not execute module-level code"
            );
        }
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn python_tool_rechecks_containment_before_execute() {
        let fixture = TempAgentDir::new("python-containment-execute");
        let tool = fixture.write_agent_tool(
            "tools/echo.py",
            r#"
TOOL = {
    "description": "Echo.",
    "input_schema": {
        "type": "object",
        "properties": {"value": {"type": "string"}},
        "required": ["value"],
    },
}

def run(input):
    return {"echo": input["value"]}
"#,
        );
        let sentinel = fixture.path.join("outside-executed.txt");
        let outside = fixture.write_outside_tool("outside.py", &sentinel);
        let registry = ToolRegistry::build(
            fixture.agent_dir.as_path(),
            &[py_decl("echo", "./tools/echo.py", true)],
        )
        .expect("contained python tool should build");

        fs::remove_file(&tool).unwrap();
        std::os::unix::fs::symlink(&outside, &tool).unwrap();
        let error = registry
            .execute("echo", &json!({"value": "ok"}))
            .await
            .expect_err("python tool symlink escape should fail before execution");

        assert!(
            format!("{error:#}").contains("escapes agent directory"),
            "{error:#}"
        );
        assert!(
            !sentinel.exists(),
            "escaping python tool should not execute after symlink replacement"
        );
    }

    #[test]
    fn python_tool_timeout_ms_is_configured() {
        let agent_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join("examples/hello/agents/support");
        let mut decl = py_decl("slow_summarize", "./tools/slow_summarize.py", true);
        decl.timeout_ms = Some(1234);
        let registry =
            ToolRegistry::build(&agent_dir, &[decl]).expect("python registry should build");
        let slow = registry.get("slow_summarize").expect("slow_summarize");
        match &slow.imp {
            ToolImpl::Python { timeout, .. } => {
                assert_eq!(*timeout, Duration::from_millis(1234));
            }
            _ => panic!("slow_summarize should be a python tool"),
        }
    }

    #[test]
    fn python_tool_timeout_ms_rejects_zero() {
        let mut decl = py_decl("sleep", "./tools/missing.py", true);
        decl.timeout_ms = Some(0);
        let error = match ToolRegistry::build(PathBuf::new().as_path(), &[decl]) {
            Ok(_) => panic!("zero python timeout should fail"),
            Err(error) => error,
        };
        assert!(format!("{error:#}").contains("timeoutMs"), "{error:#}");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn remote_mcp_executes_with_bearer_and_declared_metadata() {
        let env = unique_env("BEATER_TEST_REMOTE_MCP_TOKEN");
        unsafe {
            std::env::set_var(&env, "secret-token");
        }
        let server = MockMcp::new(vec![MockResponse::json(
            "200 OK",
            json!({
                "jsonrpc": "2.0",
                "id": "1",
                "result": {
                    "content": [{"type": "text", "text": "{\"company\":\"Acme\"}"}],
                    "isError": false
                }
            }),
        )]);
        let registry = ToolRegistry::build(
            PathBuf::new().as_path(),
            &[remote_decl(&server.endpoint, Some(&env), None, true)],
        )
        .expect("remote MCP registry should build");

        let api_tools = registry.api_tools();
        assert!(
            api_tools
                .to_string()
                .contains("\"description\":\"Look up a CRM contact.\""),
            "{api_tools}"
        );
        assert!(
            api_tools.to_string().contains("\"crm.lookup\""),
            "{api_tools}"
        );

        let result = registry
            .execute("crm.lookup", &json!({"email": "a@example.com"}))
            .await
            .expect("execute remote MCP");
        assert_eq!(result, "{\"company\":\"Acme\"}");

        let requests = server.requests();
        assert_eq!(requests.len(), 1);
        let headers = requests[0].headers.to_ascii_lowercase();
        assert!(headers.contains("authorization: bearer secret-token"));
        assert!(headers.contains("mcp-protocol-version: 2025-11-25"));
        let body: Value = serde_json::from_str(&requests[0].body).unwrap();
        assert_eq!(body["method"], "tools/call");
        assert_eq!(body["params"]["name"], "lookup_contact");
        assert_eq!(body["params"]["arguments"]["email"], "a@example.com");

        unsafe {
            std::env::remove_var(&env);
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn remote_mcp_missing_bearer_env_fails_before_network() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let endpoint = format!(
            "http://127.0.0.1:{}/mcp",
            listener.local_addr().unwrap().port()
        );
        let env = unique_env("BEATER_TEST_REMOTE_MCP_MISSING");
        unsafe {
            std::env::remove_var(&env);
        }
        let registry = ToolRegistry::build(
            PathBuf::new().as_path(),
            &[remote_decl(&endpoint, Some(&env), None, true)],
        )
        .expect("remote MCP registry should build");

        let error = registry
            .execute("crm.lookup", &json!({"email": "a@example.com"}))
            .await
            .expect_err("missing secret should fail");
        assert!(format!("{error:#}").contains("is not set"), "{error:#}");

        listener.set_nonblocking(true).unwrap();
        let accept = listener.accept();
        assert!(
            matches!(accept, Err(error) if error.kind() == std::io::ErrorKind::WouldBlock),
            "missing secret should not open a network connection"
        );
    }

    #[test]
    fn remote_mcp_egress_policy_rejects_unlisted_host() {
        let error = match ToolRegistry::build(
            PathBuf::new().as_path(),
            &[remote_decl(
                "http://127.0.0.1:65530/mcp",
                None,
                Some(vec!["api.example.com"]),
                true,
            )],
        ) {
            Ok(_) => panic!("egress mismatch should fail"),
            Err(error) => error,
        };
        assert!(format!("{error:#}").contains("egress policy"), "{error:#}");
    }

    #[test]
    fn remote_mcp_bearer_auth_rejects_plaintext_non_loopback_endpoint() {
        let error = match ToolRegistry::build(
            PathBuf::new().as_path(),
            &[remote_decl(
                "http://mcp.example.test/mcp",
                Some("REMOTE_MCP_TOKEN"),
                Some(vec!["mcp.example.test"]),
                true,
            )],
        ) {
            Ok(_) => panic!("plaintext bearer endpoint should fail"),
            Err(error) => error,
        };
        assert!(format!("{error:#}").contains("requires https"), "{error:#}");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn browser_mock_cdp_executes_and_cleans_session() {
        let registry = ToolRegistry::build(PathBuf::new().as_path(), &[browser_decl()])
            .expect("browser registry should build");

        let result = registry
            .execute_with_context(
                "browser.checkout",
                &json!({"url": "https://shop.example/cart", "task": "verify checkout"}),
                &ToolCallContext {
                    tool_use_id: Some("toolu_browser".to_string()),
                    ..ToolCallContext::default()
                },
            )
            .await
            .expect("browser tool should run");

        assert!(!browser_session_active_for_tests("toolu_browser"));
        let result: Value = serde_json::from_str(&result).unwrap();
        assert_eq!(result["provider"], "mock_cdp");
        assert_eq!(result["session"]["id"], "toolu_browser");
        assert_eq!(result["session"]["scope"], "run");
        assert_eq!(result["session"]["cleanup"], "always");
        assert_eq!(result["url"], "https://shop.example/cart");
        assert!(result["text"].as_str().unwrap().contains("verify checkout"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn browser_mock_cdp_rejects_disallowed_origin_and_cleans_session() {
        let registry = ToolRegistry::build(PathBuf::new().as_path(), &[browser_decl()])
            .expect("browser registry should build");

        let error = registry
            .execute_with_context(
                "browser.checkout",
                &json!({"url": "https://evil.example/cart", "task": "verify checkout"}),
                &ToolCallContext {
                    tool_use_id: Some("toolu_browser_reject".to_string()),
                    ..ToolCallContext::default()
                },
            )
            .await
            .expect_err("browser tool should reject disallowed origin");

        assert!(!browser_session_active_for_tests("toolu_browser_reject"));
        assert!(
            format!("{error:#}").contains("not allowed by allowedOrigins"),
            "{error:#}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn browser_mock_cdp_timeout_cleans_session() {
        let mut decl = browser_decl();
        decl.timeout_ms = Some(20);
        let registry = ToolRegistry::build(PathBuf::new().as_path(), &[decl])
            .expect("browser registry should build");

        let error = registry
            .execute_with_context(
                "browser.checkout",
                &json!({
                    "url": "https://shop.example/cart",
                    "task": "verify checkout",
                    "delayMs": 200
                }),
                &ToolCallContext {
                    tool_use_id: Some("toolu_browser_timeout".to_string()),
                    ..ToolCallContext::default()
                },
            )
            .await
            .expect_err("browser tool should time out");

        assert!(!browser_session_active_for_tests("toolu_browser_timeout"));
        assert!(format!("{error:#}").contains("timed out"), "{error:#}");
    }

    #[test]
    fn browser_session_tracking_counts_duplicate_ids() {
        assert_eq!(browser_session_count_for_tests("duplicate-session"), 0);
        let first = BrowserSessionGuard::start("duplicate-session");
        let second = BrowserSessionGuard::start("duplicate-session");
        assert_eq!(browser_session_count_for_tests("duplicate-session"), 2);
        drop(first);
        assert_eq!(browser_session_count_for_tests("duplicate-session"), 1);
        drop(second);
        assert_eq!(browser_session_count_for_tests("duplicate-session"), 0);
    }

    #[test]
    fn browser_mock_cdp_rejects_secrets() {
        let mut decl = browser_decl();
        decl.secrets = json!({"profile": {"env": "BROWSER_PROFILE_ID"}});

        let error = match ToolRegistry::build(PathBuf::new().as_path(), &[decl]) {
            Ok(_) => panic!("mock_cdp should reject secrets"),
            Err(error) => error,
        };
        assert!(
            format!("{error:#}").contains("does not support secrets"),
            "{error:#}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn remote_mcp_does_not_follow_redirects_beyond_egress() {
        let target = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        target.set_nonblocking(true).unwrap();
        let target_url = format!(
            "http://127.0.0.1:{}/mcp",
            target.local_addr().unwrap().port()
        );
        let server = MockMcp::new(vec![MockResponse::redirect(&target_url)]);
        let registry = ToolRegistry::build(
            PathBuf::new().as_path(),
            &[remote_decl(&server.endpoint, None, None, true)],
        )
        .expect("remote MCP registry should build");

        let error = registry
            .execute("crm.lookup", &json!({"email": "a@example.com"}))
            .await
            .expect_err("redirect should not be followed");
        assert!(format!("{error:#}").contains("HTTP 307"), "{error:#}");
        assert_eq!(server.requests().len(), 1);
        let accept = target.accept();
        assert!(
            matches!(accept, Err(error) if error.kind() == std::io::ErrorKind::WouldBlock),
            "redirect target should not receive a request"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn remote_mcp_timeout_fails_closed() {
        let server = MockMcp::new(vec![MockResponse {
            status: "200 OK",
            body: json!({
                "jsonrpc": "2.0",
                "id": "1",
                "result": {"content": [{"type": "text", "text": "late"}], "isError": false}
            })
            .to_string(),
            delay: Duration::from_millis(500),
            headers: Vec::new(),
        }]);
        let mut decl = remote_decl(&server.endpoint, None, None, true);
        decl.timeout_ms = Some(100);
        let registry = ToolRegistry::build(PathBuf::new().as_path(), &[decl])
            .expect("remote MCP registry should build");

        let error = registry
            .execute("crm.lookup", &json!({"email": "a@example.com"}))
            .await
            .expect_err("remote MCP timeout should fail");
        assert!(
            format!("{error:#}").contains("failed") || format!("{error:#}").contains("timed out"),
            "{error:#}"
        );
        assert_eq!(server.wait_for_requests(1, Duration::from_secs(2)).len(), 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn remote_mcp_non_idempotent_ambiguous_failure_needs_review() {
        let server = MockMcp::new(vec![MockResponse::json(
            "500 Internal Server Error",
            json!({"jsonrpc": "2.0", "id": "toolu_ambiguous", "error": {"code": -32000, "message": "maybe applied"}}),
        )]);
        let registry = ToolRegistry::build(
            PathBuf::new().as_path(),
            &[remote_decl(&server.endpoint, None, None, false)],
        )
        .expect("remote MCP registry should build");

        let error = registry
            .execute_with_context(
                "crm.lookup",
                &json!({"email": "a@example.com"}),
                &ToolCallContext {
                    tool_use_id: Some("toolu_ambiguous".to_string()),
                    ..ToolCallContext::default()
                },
            )
            .await
            .expect_err("ambiguous non-idempotent failure should need review");
        assert!(
            error.downcast_ref::<ToolNeedsReview>().is_some(),
            "{error:#}"
        );
        assert_eq!(server.requests().len(), 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn remote_mcp_non_idempotent_provider_error_needs_review() {
        for body in [
            json!({
                "jsonrpc": "2.0",
                "id": "toolu_provider_error",
                "error": {"code": -32000, "message": "provider failed after applying"}
            }),
            json!({
                "jsonrpc": "2.0",
                "id": "toolu_provider_error",
                "result": {
                    "content": [{"type": "text", "text": "provider failed after applying"}],
                    "isError": true
                }
            }),
        ] {
            let server = MockMcp::new(vec![MockResponse::json("200 OK", body)]);
            let registry = ToolRegistry::build(
                PathBuf::new().as_path(),
                &[remote_decl(&server.endpoint, None, None, false)],
            )
            .expect("remote MCP registry should build");

            let error = registry
                .execute_with_context(
                    "crm.lookup",
                    &json!({"email": "a@example.com"}),
                    &ToolCallContext {
                        tool_use_id: Some("toolu_provider_error".to_string()),
                        ..ToolCallContext::default()
                    },
                )
                .await
                .expect_err("provider error on non-idempotent tool should need review");
            assert!(
                error.downcast_ref::<ToolNeedsReview>().is_some(),
                "{error:#}"
            );
            assert_eq!(server.requests().len(), 1);
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn remote_mcp_retries_server_errors_with_tool_use_id() {
        let server = MockMcp::new(vec![
            MockResponse::json(
                "500 Internal Server Error",
                json!({"jsonrpc": "2.0", "id": "toolu_123", "error": {"code": -32000, "message": "try again"}}),
            ),
            MockResponse::json(
                "200 OK",
                json!({
                    "jsonrpc": "2.0",
                    "id": "toolu_123",
                    "result": {
                        "content": [{"type": "text", "text": "{\"ok\":true}"}],
                        "isError": false
                    }
                }),
            ),
        ]);
        let mut decl = remote_decl(&server.endpoint, None, None, false);
        decl.retry = Some(
            serde_json::from_value(json!({
                "attempts": 2,
                "backoffMs": 1,
                "idempotencyKey": "tool_use_id"
            }))
            .unwrap(),
        );
        let registry = ToolRegistry::build(PathBuf::new().as_path(), &[decl])
            .expect("remote MCP registry should build");

        let result = registry
            .execute_with_context(
                "crm.lookup",
                &json!({"email": "a@example.com"}),
                &ToolCallContext {
                    tool_use_id: Some("toolu_123".to_string()),
                    ..ToolCallContext::default()
                },
            )
            .await
            .expect("retry should recover");
        assert_eq!(result, "{\"ok\":true}");

        let requests = server.requests();
        assert_eq!(requests.len(), 2);
        for request in requests {
            assert!(
                request
                    .headers
                    .to_ascii_lowercase()
                    .contains("idempotency-key: toolu_123"),
                "{}",
                request.headers
            );
            let body: Value = serde_json::from_str(&request.body).unwrap();
            assert_eq!(body["id"], "toolu_123");
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn remote_mcp_rejects_mismatched_jsonrpc_id() {
        let server = MockMcp::new(vec![MockResponse::json(
            "200 OK",
            json!({
                "jsonrpc": "2.0",
                "id": "wrong-id",
                "result": {
                    "content": [{"type": "text", "text": "{\"ok\":true}"}],
                    "isError": false
                }
            }),
        )]);
        let registry = ToolRegistry::build(
            PathBuf::new().as_path(),
            &[remote_decl(&server.endpoint, None, None, true)],
        )
        .expect("remote MCP registry should build");

        let error = registry
            .execute_with_context(
                "crm.lookup",
                &json!({"email": "a@example.com"}),
                &ToolCallContext {
                    tool_use_id: Some("expected-id".to_string()),
                    ..ToolCallContext::default()
                },
            )
            .await
            .expect_err("mismatched response id should fail");
        assert!(
            format!("{error:#}").contains("did not match request id"),
            "{error:#}"
        );
    }

    fn py_decl(name: &str, path: &str, idempotent: bool) -> ToolDecl {
        serde_json::from_value(json!({
            "kind": "python",
            "name": name,
            "path": path,
            "idempotent": idempotent
        }))
        .unwrap()
    }

    fn remote_decl(
        endpoint: &str,
        auth_env: Option<&str>,
        egress: Option<Vec<&str>>,
        idempotent: bool,
    ) -> ToolDecl {
        let url = reqwest::Url::parse(endpoint).unwrap();
        let host = url.host_str().unwrap();
        let host_port = url
            .port()
            .map(|port| format!("{host}:{port}"))
            .unwrap_or_else(|| host.to_string());
        let mut value = json!({
            "kind": "remote_mcp",
            "name": "crm.lookup",
            "description": "Look up a CRM contact.",
            "inputSchema": {
                "type": "object",
                "properties": {"email": {"type": "string"}},
                "required": ["email"]
            },
            "endpoint": endpoint,
            "tool": "lookup_contact",
            "timeoutMs": 1000,
            "idempotent": idempotent,
            "egress": egress.unwrap_or_else(|| vec![host_port.as_str()])
        });
        if let Some(env) = auth_env {
            value["auth"] = json!({"type": "bearer", "env": env});
        }
        serde_json::from_value(value).unwrap()
    }

    fn browser_decl() -> ToolDecl {
        serde_json::from_value(json!({
            "kind": "browser",
            "name": "browser.checkout",
            "provider": "mock_cdp",
            "description": "Verify checkout in a browser.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "url": {"type": "string"},
                    "task": {"type": "string"}
                },
                "required": ["url", "task"]
            },
            "session": {"scope": "run", "cleanup": "always"},
            "allowedOrigins": ["https://shop.example"],
            "timeoutMs": 1000,
            "idempotent": false
        }))
        .unwrap()
    }

    fn unique_env(prefix: &str) -> String {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        format!("{prefix}_{}_{}", std::process::id(), nanos)
    }

    struct TempAgentDir {
        path: PathBuf,
        agent_dir: PathBuf,
    }

    impl TempAgentDir {
        fn new(label: &str) -> Self {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "beater-registry-{label}-{}-{nanos}",
                std::process::id()
            ));
            let agent_dir = path.join("agents/support");
            fs::create_dir_all(agent_dir.join("tools")).unwrap();
            Self { path, agent_dir }
        }

        fn write_agent_tool(&self, relative_path: &str, contents: &str) -> PathBuf {
            let path = self.agent_dir.join(relative_path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, contents).unwrap();
            path
        }

        fn write_outside_tool(&self, relative_path: &str, sentinel: &Path) -> PathBuf {
            let path = self.path.join(relative_path);
            fs::write(
                &path,
                format!(
                    r#"
from pathlib import Path
Path({:?}).write_text("loaded")
TOOL = {{
    "description": "Evil.",
    "input_schema": {{"type": "object"}},
}}

def run(input):
    Path({:?}).write_text("executed")
    return {{"evil": True}}
"#,
                    sentinel.to_string_lossy().as_ref(),
                    sentinel.to_string_lossy().as_ref(),
                ),
            )
            .unwrap();
            path
        }
    }

    impl Drop for TempAgentDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    #[derive(Clone)]
    struct MockRequest {
        headers: String,
        body: String,
    }

    struct MockResponse {
        status: &'static str,
        body: String,
        delay: Duration,
        headers: Vec<(String, String)>,
    }

    impl MockResponse {
        fn json(status: &'static str, body: Value) -> Self {
            Self {
                status,
                body: body.to_string(),
                delay: Duration::ZERO,
                headers: Vec::new(),
            }
        }

        fn redirect(location: &str) -> Self {
            Self {
                status: "307 Temporary Redirect",
                body: String::new(),
                delay: Duration::ZERO,
                headers: vec![("location".to_string(), location.to_string())],
            }
        }
    }

    struct MockMcp {
        endpoint: String,
        addr: std::net::SocketAddr,
        requests: Arc<Mutex<Vec<MockRequest>>>,
        handle: Option<thread::JoinHandle<()>>,
    }

    impl MockMcp {
        fn new(responses: Vec<MockResponse>) -> Self {
            let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
            listener.set_nonblocking(true).unwrap();
            let addr = listener.local_addr().unwrap();
            let endpoint = format!("http://127.0.0.1:{}/mcp", addr.port());
            let requests = Arc::new(Mutex::new(Vec::new()));
            let thread_requests = requests.clone();
            let handle = thread::spawn(move || {
                for response in responses {
                    let (mut stream, _) = accept_with_deadline(&listener);
                    let request = read_request(&mut stream);
                    thread_requests.lock().unwrap().push(request);
                    if !response.delay.is_zero() {
                        thread::sleep(response.delay);
                    }
                    let _ = write_response(
                        &mut stream,
                        response.status,
                        &response.body,
                        &response.headers,
                    );
                }
            });
            Self {
                endpoint,
                addr,
                requests,
                handle: Some(handle),
            }
        }

        fn requests(&self) -> Vec<MockRequest> {
            self.requests.lock().unwrap().clone()
        }

        fn wait_for_requests(&self, count: usize, timeout: Duration) -> Vec<MockRequest> {
            let deadline = Instant::now() + timeout;
            loop {
                let requests = self.requests();
                if requests.len() >= count || Instant::now() >= deadline {
                    return requests;
                }
                thread::sleep(Duration::from_millis(5));
            }
        }
    }

    impl Drop for MockMcp {
        fn drop(&mut self) {
            if let Some(handle) = self.handle.take() {
                let _ = TcpStream::connect(self.addr).and_then(|mut stream| {
                    stream.write_all(
                        b"POST /mcp HTTP/1.1\r\nhost: localhost\r\ncontent-length: 0\r\n\r\n",
                    )
                });
                let _ = handle.join();
            }
        }
    }

    fn accept_with_deadline(listener: &TcpListener) -> (TcpStream, std::net::SocketAddr) {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match listener.accept() {
                Ok((stream, addr)) => {
                    stream.set_nonblocking(false).unwrap();
                    return (stream, addr);
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(Instant::now() < deadline, "timed out waiting for request");
                    thread::sleep(Duration::from_millis(5));
                }
                Err(error) => panic!("accept request: {error}"),
            }
        }
    }

    fn read_request(stream: &mut TcpStream) -> MockRequest {
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut bytes = Vec::new();
        let mut chunk = [0_u8; 1024];
        loop {
            let read = stream.read(&mut chunk).unwrap();
            if read == 0 {
                break;
            }
            bytes.extend_from_slice(&chunk[..read]);
            if let Some(header_end) = header_end(&bytes) {
                let header_text = String::from_utf8_lossy(&bytes[..header_end]).to_string();
                let content_length = content_length(&header_text);
                if bytes.len() >= header_end + 4 + content_length {
                    let body = String::from_utf8_lossy(
                        &bytes[(header_end + 4)..(header_end + 4 + content_length)],
                    )
                    .to_string();
                    return MockRequest {
                        headers: header_text,
                        body,
                    };
                }
            }
        }
        panic!("incomplete HTTP request")
    }

    fn header_end(bytes: &[u8]) -> Option<usize> {
        bytes.windows(4).position(|window| window == b"\r\n\r\n")
    }

    fn content_length(headers: &str) -> usize {
        headers
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse().unwrap())
            })
            .unwrap_or(0)
    }

    fn write_response(
        stream: &mut TcpStream,
        status: &str,
        body: &str,
        headers: &[(String, String)],
    ) -> std::io::Result<()> {
        write!(
            stream,
            "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n",
            body.len()
        )?;
        for (name, value) in headers {
            write!(stream, "{name}: {value}\r\n")?;
        }
        write!(stream, "\r\n{body}")
    }
}
