//! One registry for local and networked tools: Python files (embedded CPython),
//! Rust built-ins, hermetic Wasmtime tools, remote MCP providers, and browser tools.
//! Every tool declares `idempotent` — the resume-safety contract
//! (ARCHITECTURE.md §5).

use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::process;
use std::sync::{
    Arc, Mutex, OnceLock,
    atomic::{AtomicBool, Ordering},
};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail, ensure};
use base64::Engine as _;
use beater_browser::{
    BrowserAction, BrowserDriver, BrowserEngine, Observation, StepOutcome, StepStatus, UrlPolicy,
};
use beater_browser_playwright::{PlaywrightConfig, PlaywrightDriver};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::Mutex as AsyncMutex;
use wasmtime::{Config, Engine, Linker, Module, Store, StoreLimits, StoreLimitsBuilder};

pub const DEFAULT_BEATBOX_URL: &str = "http://127.0.0.1:7300";
const DEFAULT_PYTHON_TIMEOUT_MS: u64 = 10_000;
const PLAYWRIGHT_NODE_ENV: &str = "BEATER_PLAYWRIGHT_NODE";
const PLAYWRIGHT_RUNNER_ENV: &str = "BEATER_PLAYWRIGHT_RUNNER";
const BROWSER_SESSION_DIR: &str = "browser-sessions";

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
    pub kind: String, // "python" | "rust" | "remote_mcp" | "remote_mcp_provider" | "browser" | "sandbox" | "wasmtime"
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

#[derive(Clone, Debug, Deserialize)]
pub struct RemoteMcpAuthDecl {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    env: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct RemoteMcpRetryDecl {
    #[serde(default)]
    attempts: Option<u32>,
    #[serde(default, rename = "backoffMs", alias = "backoff_ms")]
    backoff_ms: Option<u64>,
    #[serde(default, rename = "idempotencyKey", alias = "idempotency_key")]
    idempotency_key: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
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
    Wasmtime(Box<WasmtimeTool>),
}

pub struct SandboxTool {
    beatbox: BeatboxConfig,
    lane: beatbox_client::Lane,
    source: beatbox_client::Source,
    policy: beatbox_client::Policy,
    entrypoint: Option<String>,
}

pub struct WasmtimeTool {
    engine: Engine,
    module: Module,
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

#[derive(Clone)]
struct BrowserSessionStore {
    root: PathBuf,
}

struct BrowserProcessLease {
    marker_path: PathBuf,
    wrapper_path: PathBuf,
}

#[derive(Deserialize)]
struct BrowserSessionMarker {
    session_id: String,
    wrapper_script: PathBuf,
    #[serde(default)]
    process_title: Option<String>,
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

pub fn browser_session_dir(app_dir: &Path) -> PathBuf {
    app_dir.join(".beater").join(BROWSER_SESSION_DIR)
}

pub fn cleanup_stale_browser_sessions(app_dir: &Path, run_id: &str) -> Result<()> {
    BrowserSessionStore::new(browser_session_dir(app_dir)).cleanup_session(run_id)
}

impl BrowserSessionStore {
    fn new(root: PathBuf) -> Self {
        Self { root }
    }

    fn lease(
        &self,
        session_id: &str,
        config: &mut PlaywrightConfig,
    ) -> Result<BrowserProcessLease> {
        fs::create_dir_all(&self.root)
            .with_context(|| format!("creating browser session dir {}", self.root.display()))?;
        let suffix = unique_browser_session_suffix();
        let safe_id = safe_browser_session_component(session_id);
        let marker_path = self.root.join(format!("{safe_id}-{suffix}.json"));
        let wrapper_path = self.root.join(format!("{safe_id}-{suffix}.cjs"));
        let runner_script = config
            .runner_script
            .canonicalize()
            .unwrap_or_else(|_| config.runner_script.clone());
        let process_title = format!("beater-playwright:{session_id}");
        let wrapper = format!(
            "process.title = {};\nprocess.env.BEATER_BROWSER_SESSION_ID = {};\nrequire({});\n",
            serde_json::to_string(&process_title)?,
            serde_json::to_string(session_id)?,
            serde_json::to_string(&runner_script.to_string_lossy())?,
        );
        fs::write(&wrapper_path, wrapper).with_context(|| {
            format!(
                "writing Playwright runner wrapper {}",
                wrapper_path.display()
            )
        })?;
        let marker = json!({
            "session_id": session_id,
            "wrapper_script": wrapper_path,
            "process_title": process_title,
            "runner_script": runner_script,
            "owner_pid": process::id(),
            "created_at": now_unix_secs(),
        });
        fs::write(&marker_path, serde_json::to_vec_pretty(&marker)?)
            .with_context(|| format!("writing browser session marker {}", marker_path.display()))?;
        config.runner_script = wrapper_path.clone();
        Ok(BrowserProcessLease {
            marker_path,
            wrapper_path,
        })
    }

    fn cleanup_session(&self, session_id: &str) -> Result<()> {
        let markers = self.markers_for_session(session_id)?;
        for marker in markers {
            terminate_processes_matching(&marker.wrapper_script, marker.process_title.as_deref())
                .with_context(|| {
                format!(
                    "terminating browser runner {}",
                    marker.wrapper_script.display()
                )
            })?;
            remove_file_if_exists(&marker.wrapper_script)?;
            remove_file_if_exists(&marker.marker_path)?;
        }
        Ok(())
    }

    fn markers_for_session(&self, session_id: &str) -> Result<Vec<StoredBrowserSessionMarker>> {
        if !self.root.is_dir() {
            return Ok(Vec::new());
        }
        let mut markers = Vec::new();
        for entry in fs::read_dir(&self.root)
            .with_context(|| format!("reading browser session dir {}", self.root.display()))?
        {
            let path = entry?.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            let Ok(bytes) = fs::read(&path) else {
                continue;
            };
            let Ok(marker) = serde_json::from_slice::<BrowserSessionMarker>(&bytes) else {
                continue;
            };
            if marker.session_id == session_id {
                markers.push(StoredBrowserSessionMarker {
                    marker_path: path,
                    wrapper_script: marker.wrapper_script,
                    process_title: marker.process_title,
                });
            }
        }
        Ok(markers)
    }
}

struct StoredBrowserSessionMarker {
    marker_path: PathBuf,
    wrapper_script: PathBuf,
    process_title: Option<String>,
}

impl BrowserProcessLease {
    fn cleanup_files(&self) -> Result<()> {
        remove_file_if_exists(&self.wrapper_path)?;
        remove_file_if_exists(&self.marker_path)?;
        Ok(())
    }

    fn cleanup_after_launch_failure(&self) -> Result<()> {
        terminate_processes_matching(&self.wrapper_path, None)?;
        self.cleanup_files()
    }
}

fn remove_file_if_exists(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("removing {}", path.display())),
    }
}

fn unique_browser_session_suffix() -> String {
    format!("{}-{}", process::id(), now_unix_nanos())
}

fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn now_unix_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}

fn safe_browser_session_component(session_id: &str) -> String {
    let value: String = session_id
        .chars()
        .map(|ch| match ch {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' | '.' => ch,
            _ => '_',
        })
        .collect();
    if value.is_empty() {
        "session".to_string()
    } else {
        value
    }
}

#[cfg(unix)]
fn terminate_processes_matching(wrapper_path: &Path, process_title: Option<&str>) -> Result<()> {
    let needle = wrapper_path.to_string_lossy();
    let output = process::Command::new("ps")
        .args(["-axo", "pid=,command="])
        .output()
        .context("listing processes for browser session cleanup")?;
    let listing = String::from_utf8_lossy(&output.stdout);
    let current_pid = process::id();
    let mut pids = Vec::new();
    for line in listing.lines() {
        let trimmed = line.trim_start();
        let Some((pid, command)) = trimmed.split_once(char::is_whitespace) else {
            continue;
        };
        let Ok(pid) = pid.parse::<u32>() else {
            continue;
        };
        let matches_wrapper = command.contains(needle.as_ref());
        let matches_title = process_title
            .map(|title| command.contains(title))
            .unwrap_or(false);
        if pid != current_pid && (matches_wrapper || matches_title) {
            pids.push(pid);
        }
    }
    for pid in &pids {
        let _ = process::Command::new("kill")
            .args(["-TERM", &pid.to_string()])
            .status();
    }
    if !pids.is_empty() {
        thread::sleep(Duration::from_millis(100));
    }
    for pid in pids {
        if process_is_running(pid) {
            let _ = process::Command::new("kill")
                .args(["-KILL", &pid.to_string()])
                .status();
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn terminate_processes_matching(_wrapper_path: &Path, _process_title: Option<&str>) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn process_is_running(pid: u32) -> bool {
    process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

#[derive(Clone, Default)]
pub struct ToolCallContext {
    pub run_id: Option<String>,
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
        Self::build_inner(agent_dir, decls, beatbox, None)
    }

    pub fn build_with_beatbox_and_browser_session_dir(
        agent_dir: &Path,
        decls: &[ToolDecl],
        beatbox: &BeatboxConfig,
        browser_session_dir: Option<PathBuf>,
    ) -> Result<Self> {
        let browser_sessions =
            browser_session_dir.map(|root| Arc::new(BrowserSessionStore::new(root)));
        Self::build_inner(agent_dir, decls, beatbox, browser_sessions)
    }

    fn build_inner(
        agent_dir: &Path,
        decls: &[ToolDecl],
        beatbox: &BeatboxConfig,
        browser_sessions: Option<Arc<BrowserSessionStore>>,
    ) -> Result<Self> {
        let mut tools = Vec::new();
        for decl in decls {
            if decl.kind == "remote_mcp_provider" {
                for entry in remote_mcp_provider_entries(decl)? {
                    push_unique_tool(&mut tools, entry);
                }
                continue;
            }
            let entry = match decl.kind.as_str() {
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
                    ToolEntry {
                        name: decl.name.clone(),
                        description,
                        input_schema,
                        idempotent: decl.idempotent,
                        imp: ToolImpl::Python {
                            agent_dir,
                            path,
                            timeout,
                        },
                    }
                }
                "rust" => {
                    let mut entry = rust_builtin(&decl.name)
                        .with_context(|| format!("unknown rust builtin tool {}", decl.name))?;
                    entry.idempotent = decl.idempotent;
                    entry
                }
                "remote_mcp" => {
                    let description = decl.description.clone().with_context(|| {
                        format!("remote_mcp tool {} requires description", decl.name)
                    })?;
                    let input_schema = decl.input_schema.clone().with_context(|| {
                        format!("remote_mcp tool {} requires inputSchema", decl.name)
                    })?;
                    let config = RemoteMcpTool::from_decl(decl)?;
                    ToolEntry {
                        name: decl.name.clone(),
                        description,
                        input_schema,
                        idempotent: decl.idempotent,
                        imp: ToolImpl::RemoteMcp { config },
                    }
                }
                "browser" => {
                    let description = decl.description.clone().with_context(|| {
                        format!("browser tool {} requires description", decl.name)
                    })?;
                    let input_schema = decl.input_schema.clone().with_context(|| {
                        format!("browser tool {} requires inputSchema", decl.name)
                    })?;
                    let config = BrowserTool::from_decl(decl, browser_sessions.clone())?;
                    ToolEntry {
                        name: decl.name.clone(),
                        description,
                        input_schema,
                        idempotent: decl.idempotent,
                        imp: ToolImpl::Browser { config },
                    }
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
                    ToolEntry {
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
                    }
                }
                "wasmtime" => {
                    let source = wasmtime_source(agent_dir, decl)
                        .with_context(|| format!("loading wasmtime source for {}", decl.name))?;
                    let policy = sandbox_policy(decl.policy.as_ref())
                        .with_context(|| format!("parsing wasmtime policy for {}", decl.name))?;
                    admit_wasmtime_policy(&policy)
                        .with_context(|| format!("checking wasmtime policy for {}", decl.name))?;
                    let engine = wasmtime_engine()?;
                    let module = Module::new(&engine, &source).map_err(|error| {
                        anyhow!("compiling wasmtime module for {}: {error}", decl.name)
                    })?;
                    let imports = module_imports(&module);
                    ensure!(
                        imports.is_empty(),
                        "wasmtime tool {} imports are disabled: {}",
                        decl.name,
                        imports.join(", ")
                    );
                    let description = decl.description.clone().unwrap_or_else(|| {
                        format!("Run {} inside the local Wasmtime sandbox.", decl.name)
                    });
                    let input_schema = decl
                        .input_schema
                        .clone()
                        .unwrap_or_else(|| json!({"type": "object", "properties": {}}));
                    ToolEntry {
                        name: decl.name.clone(),
                        description,
                        input_schema,
                        idempotent: decl.idempotent,
                        imp: ToolImpl::Wasmtime(Box::new(WasmtimeTool {
                            engine,
                            module,
                            policy,
                            entrypoint: decl.entrypoint.clone(),
                        })),
                    }
                }
                other => bail!("unknown tool kind {other:?} for tool {}", decl.name),
            };
            push_unique_tool(&mut tools, entry);
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

    pub async fn close_browser_sessions(&self, run_id: &str) -> Result<()> {
        for tool in &self.tools {
            if let ToolImpl::Browser { config } = &tool.imp {
                config
                    .close_session(run_id)
                    .await
                    .with_context(|| format!("closing browser sessions for tool {}", tool.name))?;
            }
        }
        Ok(())
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
            ToolImpl::Wasmtime(wasm) => {
                execute_wasmtime(
                    wasm.engine.clone(),
                    wasm.module.clone(),
                    wasm.policy.clone(),
                    wasm.entrypoint.clone(),
                    input.clone(),
                )
                .await
            }
        }
    }
}

fn push_unique_tool(tools: &mut Vec<ToolEntry>, tool: ToolEntry) {
    if tools.iter().any(|existing| existing.name == tool.name) {
        tracing::warn!(
            "duplicate tool {} within agent — keeping the first",
            tool.name
        );
    } else {
        tools.push(tool);
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

fn wasmtime_source(agent_dir: &Path, decl: &ToolDecl) -> Result<Vec<u8>> {
    if decl.path.is_some() && decl.source.is_some() {
        bail!(
            "wasmtime tool {} cannot declare both source and path",
            decl.name
        );
    }
    match decl.source.as_ref() {
        Some(SandboxSourceDecl::Path { path }) => wasmtime_source_path(agent_dir, path),
        Some(SandboxSourceDecl::Wat { text }) | Some(SandboxSourceDecl::WasmWat { text }) => {
            Ok(text.as_bytes().to_vec())
        }
        Some(SandboxSourceDecl::WasmBase64 { bytes })
        | Some(SandboxSourceDecl::WasmBytesBase64 { bytes }) => {
            base64::engine::general_purpose::STANDARD
                .decode(bytes)
                .context("decoding wasmtime wasm base64 source")
        }
        Some(SandboxSourceDecl::Inline { code }) => Ok(code.as_bytes().to_vec()),
        Some(SandboxSourceDecl::ModuleRef { .. }) => {
            bail!("module_ref wasmtime sources are not supported yet")
        }
        None => {
            let path = decl
                .path
                .as_deref()
                .with_context(|| format!("wasmtime tool {} has no source or path", decl.name))?;
            wasmtime_source_path(agent_dir, path)
        }
    }
}

fn wasmtime_source_path(agent_dir: &Path, path: &str) -> Result<Vec<u8>> {
    let (_, path) = contained_agent_path(agent_dir, path, "wasmtime source")?;
    std::fs::read(&path).with_context(|| format!("reading wasmtime source {}", path.display()))
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
    let client = beatbox
        .client()
        .with_timeout(job_poll_timeout(request.policy.limits.wall_ms))?;
    let result = if request.idempotency_key.is_some() {
        execute_sandbox_job(&client, &request).await?
    } else {
        client.execute(&request).await?
    };
    sandbox_result_content(&result)
}

fn sandbox_result_content(result: &beatbox_client::ExecutionResult) -> Result<String> {
    let content = serde_json::to_string(result)?;
    if result.status != beatbox_client::ExecutionStatus::Ok {
        bail!("sandbox execution returned {:?}: {content}", result.status);
    }
    Ok(content)
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

fn wasmtime_engine() -> Result<Engine> {
    let mut config = Config::new();
    config.consume_fuel(true);
    config.epoch_interruption(true);
    config.cranelift_nan_canonicalization(true);
    config.relaxed_simd_deterministic(true);
    Engine::new(&config).map_err(|error| anyhow!("creating wasmtime engine: {error}"))
}

fn admit_wasmtime_policy(policy: &beatbox_client::Policy) -> Result<()> {
    ensure!(
        policy.fs.workspace.is_none(),
        "local wasmtime tools are hermetic and expose no filesystem workspace"
    );
    ensure!(
        policy.fs.mounts.is_empty(),
        "local wasmtime tools are hermetic and expose no filesystem mounts"
    );
    ensure!(
        matches!(policy.net, beatbox_client::NetPolicy::Deny),
        "local wasmtime tools expose no network"
    );
    ensure!(
        policy.env.is_empty(),
        "local wasmtime tools expose no environment variables"
    );
    ensure!(
        policy.secrets.is_empty(),
        "local wasmtime tools expose no secrets"
    );
    Ok(())
}

fn module_imports(module: &Module) -> Vec<String> {
    module
        .imports()
        .map(|import| format!("{}::{}", import.module(), import.name()))
        .collect()
}

struct WasmtimeState {
    limits: StoreLimits,
}

async fn execute_wasmtime(
    engine: Engine,
    module: Module,
    policy: beatbox_client::Policy,
    entrypoint: Option<String>,
    input: Value,
) -> Result<String> {
    tokio::task::spawn_blocking(move || {
        let started = Instant::now();
        let memory_limit = usize::try_from(policy.limits.memory_bytes).unwrap_or(usize::MAX);
        let limits = StoreLimitsBuilder::new()
            .memory_size(memory_limit)
            .instances(1)
            .memories(1)
            .tables(4)
            .build();
        let mut store = Store::new(&engine, WasmtimeState { limits });
        store.limiter(|state| &mut state.limits);

        let requested_fuel = policy.limits.fuel.unwrap_or(10_000_000);
        store
            .set_fuel(requested_fuel)
            .map_err(|error| anyhow!("configuring wasmtime fuel: {error}"))?;
        store.set_epoch_deadline(1);
        let ticker = epoch_ticker(engine.clone(), policy.limits.wall_ms);
        let result = run_wasmtime_entrypoint(&mut store, &engine, &module, entrypoint, &input);
        ticker.stop();

        let remaining_fuel = store.get_fuel().ok();
        let fuel_used = remaining_fuel.map(|remaining| requested_fuel.saturating_sub(remaining));
        let value = result?;
        Ok(json!({
            "status": "ok",
            "impl": "wasmtime",
            "value": value,
            "metrics": {
                "wall_time_ms": started.elapsed().as_millis() as u64,
                "fuel_used": fuel_used,
            },
            "isolation": {
                "filesystem": "none",
                "network": "none",
                "imports": "denied",
            }
        })
        .to_string())
    })
    .await
    .context("wasmtime worker panicked")?
}

fn run_wasmtime_entrypoint(
    store: &mut Store<WasmtimeState>,
    engine: &Engine,
    module: &Module,
    entrypoint: Option<String>,
    input: &Value,
) -> Result<Value> {
    let linker = Linker::new(engine);
    let instance = linker
        .instantiate(&mut *store, module)
        .map_err(|error| anyhow!("instantiating wasmtime module: {error}"))?;
    let entrypoint = entrypoint.as_deref().unwrap_or("run");

    if let Ok(func) = instance.get_typed_func::<i64, i64>(&mut *store, entrypoint) {
        let input = input_i64(input)?;
        let value = func
            .call(&mut *store, input)
            .map_err(|error| anyhow!("calling wasmtime entrypoint {entrypoint}: {error}"))?;
        return Ok(json!(value));
    }

    if let Ok(func) = instance.get_typed_func::<(), i64>(&mut *store, entrypoint) {
        let value = func
            .call(&mut *store, ())
            .map_err(|error| anyhow!("calling wasmtime entrypoint {entrypoint}: {error}"))?;
        return Ok(json!(value));
    }

    if let Ok(func) = instance.get_typed_func::<(), ()>(&mut *store, entrypoint) {
        func.call(&mut *store, ())
            .map_err(|error| anyhow!("calling wasmtime entrypoint {entrypoint}: {error}"))?;
        return Ok(Value::Null);
    }

    bail!(
        "missing supported wasmtime entrypoint `{entrypoint}`; expected ()->(), ()->i64, or i64->i64"
    )
}

fn input_i64(input: &Value) -> Result<i64> {
    if input.is_null() {
        return Ok(0);
    }
    if let Some(value) = input.as_i64() {
        return Ok(value);
    }
    if let Some(value) = input.get("n").and_then(Value::as_i64) {
        return Ok(value);
    }
    bail!("wasmtime i64 entrypoints require input as an integer or {{\"n\": integer}}")
}

struct EpochTicker {
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl EpochTicker {
    fn stop(mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn epoch_ticker(engine: Engine, wall_ms: u64) -> EpochTicker {
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = Arc::clone(&stop);
    let sleep_for = Duration::from_millis(wall_ms.max(1));
    let handle = thread::spawn(move || {
        let tick = Duration::from_millis(10);
        let started = Instant::now();
        while started.elapsed() < sleep_for {
            if thread_stop.load(Ordering::SeqCst) {
                return;
            }
            thread::sleep(tick);
        }
        if !thread_stop.load(Ordering::SeqCst) {
            engine.increment_epoch();
        }
    });
    EpochTicker {
        stop,
        handle: Some(handle),
    }
}

pub struct RemoteMcpTool {
    endpoint: reqwest::Url,
    remote_tool: String,
    auth: RemoteMcpAuth,
    timeout: Duration,
    retry: RemoteMcpRetry,
    idempotent: bool,
    session: Option<RemoteMcpSessionPolicy>,
    session_id: Mutex<Option<String>>,
}

pub struct BrowserTool {
    provider: BrowserProvider,
    timeout: Duration,
    session: BrowserSessionPolicy,
    allowed_origins: Vec<String>,
    sessions: Arc<AsyncMutex<HashMap<String, BrowserSessionState>>>,
    store: Option<Arc<BrowserSessionStore>>,
    secrets: BrowserSecrets,
}

enum BrowserSessionState {
    Mock {
        _guard: BrowserSessionGuard,
        calls: u64,
    },
    Playwright {
        _guard: BrowserSessionGuard,
        driver: Box<PlaywrightDriver>,
        lease: Option<BrowserProcessLease>,
        calls: u64,
    },
}

enum BrowserProvider {
    MockCdp,
    Playwright,
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

#[derive(Clone, Default)]
struct BrowserSecrets {
    sources: HashMap<String, BrowserSecretSource>,
}

#[derive(Clone)]
struct BrowserSecretSource {
    env: String,
}

#[derive(Debug)]
struct ResolvedBrowserAction {
    action: BrowserAction,
    redacted: BrowserAction,
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

struct RemoteMcpSessionPolicy {
    scope: RemoteMcpSessionScope,
    cleanup: RemoteMcpSessionCleanup,
}

enum RemoteMcpSessionScope {
    Run,
}

enum RemoteMcpSessionCleanup {
    Always,
}

enum IdempotencyKeySource {
    ToolUseId,
}

enum RemoteAttempt {
    Retryable(anyhow::Error),
    RetryableNoSideEffect(anyhow::Error),
    ProviderFailure(anyhow::Error),
    AmbiguousSuccess(anyhow::Error),
    Fatal(anyhow::Error),
}

static ACTIVE_BROWSER_SESSIONS: OnceLock<Mutex<HashMap<String, usize>>> = OnceLock::new();

fn active_browser_sessions() -> &'static Mutex<HashMap<String, usize>> {
    ACTIVE_BROWSER_SESSIONS.get_or_init(|| Mutex::new(HashMap::new()))
}

impl BrowserTool {
    fn from_decl(decl: &ToolDecl, store: Option<Arc<BrowserSessionStore>>) -> Result<Self> {
        let provider = match decl
            .provider
            .as_deref()
            .map(str::trim)
            .filter(|provider| !provider.is_empty())
        {
            Some("mock_cdp") => BrowserProvider::MockCdp,
            Some("playwright") => BrowserProvider::Playwright,
            Some(other) => bail!(
                "browser tool {} unsupported provider {other:?}; supported providers: mock_cdp, playwright",
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
        let secrets = BrowserSecrets::from_decl(&decl.name, &decl.secrets)?;
        Ok(Self {
            provider,
            timeout: Duration::from_millis(timeout_ms),
            session,
            allowed_origins,
            sessions: Arc::new(AsyncMutex::new(HashMap::new())),
            store,
            secrets,
        })
    }

    async fn execute(&self, input: &Value, context: &ToolCallContext) -> Result<String> {
        let fut = async {
            let session_id = self.session.session_id(context).to_string();
            match self.provider {
                BrowserProvider::MockCdp => {
                    self.execute_mock_cdp(input, &session_id, context).await
                }
                BrowserProvider::Playwright => {
                    self.execute_playwright(input, &session_id, context).await
                }
            }
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

    async fn execute_mock_cdp(
        &self,
        input: &Value,
        session_id: &str,
        context: &ToolCallContext,
    ) -> Result<String> {
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
        let (calls, reused, transient_guard) = if self.session.is_persistent(context) {
            let mut sessions = self.sessions.lock().await;
            let reused = sessions.contains_key(session_id);
            let state = sessions.entry(session_id.to_string()).or_insert_with(|| {
                BrowserSessionState::Mock {
                    _guard: BrowserSessionGuard::start(session_id),
                    calls: 0,
                }
            });
            let BrowserSessionState::Mock { calls, .. } = state else {
                bail!("browser session {session_id} has a mismatched provider");
            };
            *calls += 1;
            (*calls, reused, None)
        } else {
            (1, false, Some(BrowserSessionGuard::start(session_id)))
        };
        let output_session_id = transient_guard
            .as_ref()
            .map(BrowserSessionGuard::id)
            .unwrap_or(session_id);
        Ok(json!({
            "provider": "mock_cdp",
            "session": {
                "id": output_session_id,
                "scope": self.session.scope.as_str(),
                "cleanup": self.session.cleanup.as_str(),
                "calls": calls,
                "reused": reused,
            },
            "url": url,
            "title": "Mock Browser Page",
            "text": format!("completed browser task: {task}"),
        })
        .to_string())
    }

    async fn execute_playwright(
        &self,
        input: &Value,
        session_id: &str,
        context: &ToolCallContext,
    ) -> Result<String> {
        let url = input
            .get("url")
            .and_then(Value::as_str)
            .context("playwright browser tool requires string input.url")?;
        self.ensure_url_allowed(url)?;

        let action = browser_action_from_input_with_secrets(input, &self.secrets)?;
        if let Some(ResolvedBrowserAction {
            action: BrowserAction::Goto { url },
            ..
        }) = &action
        {
            self.ensure_url_allowed(url)?;
        }
        if self.session.is_persistent(context) {
            let mut sessions = self.sessions.lock().await;
            let reused = sessions.contains_key(session_id);
            if !reused {
                let (config, lease) = self.playwright_config_for_session(session_id)?;
                let driver = match PlaywrightDriver::launch(config).await {
                    Ok(driver) => driver.with_policy(UrlPolicy::allow_all()),
                    Err(error) => {
                        if let Some(lease) = &lease {
                            let _ = lease.cleanup_after_launch_failure();
                        }
                        return Err(anyhow::Error::new(error));
                    }
                };
                sessions.insert(
                    session_id.to_string(),
                    BrowserSessionState::Playwright {
                        _guard: BrowserSessionGuard::start(session_id),
                        driver: Box::new(driver),
                        lease,
                        calls: 0,
                    },
                );
            }
            let state = sessions
                .get_mut(session_id)
                .context("browser session disappeared after insert")?;
            let BrowserSessionState::Playwright { driver, calls, .. } = state else {
                bail!("browser session {session_id} has a mismatched provider");
            };
            *calls += 1;
            let calls = *calls;
            return self
                .execute_playwright_with_driver(driver, input, session_id, calls, reused, action)
                .await;
        }

        let guard = BrowserSessionGuard::start(session_id);
        let (config, lease) = self.playwright_config_for_session(session_id)?;
        let mut driver = match PlaywrightDriver::launch(config).await {
            Ok(driver) => driver.with_policy(UrlPolicy::allow_all()),
            Err(error) => {
                if let Some(lease) = &lease {
                    let _ = lease.cleanup_after_launch_failure();
                }
                return Err(anyhow::Error::new(error));
            }
        };
        let result = self
            .execute_playwright_with_driver(&mut driver, input, guard.id(), 1, false, action)
            .await;
        let close_result = driver.close().await.map_err(anyhow::Error::new);
        if let Some(lease) = &lease {
            let _ = lease.cleanup_files();
        }
        drop(guard);
        match (result, close_result) {
            (Ok(output), Ok(())) => Ok(output),
            (Ok(_), Err(error)) => Err(error).context("closing Playwright browser session"),
            (Err(error), _) => Err(error),
        }
    }

    async fn execute_playwright_with_driver(
        &self,
        driver: &mut PlaywrightDriver,
        input: &Value,
        session_id: &str,
        calls: u64,
        reused: bool,
        action: Option<ResolvedBrowserAction>,
    ) -> Result<String> {
        let url = input
            .get("url")
            .and_then(Value::as_str)
            .context("playwright browser tool requires string input.url")?;
        async {
            let mut observation = driver.goto(url).await.map_err(anyhow::Error::new)?;
            let mut outcome = None;
            if let Some(action) = action {
                let step_outcome = driver
                    .act(&action.action)
                    .await
                    .map_err(anyhow::Error::new)?;
                observation = step_outcome.observation.clone();
                outcome = Some(json!({
                    "action": action.redacted,
                    "status": browser_step_status(&step_outcome),
                    "error": step_outcome.error,
                    "grounding": step_outcome.grounding,
                }));
            }
            Ok::<_, anyhow::Error>(
                json!({
                    "provider": "playwright",
                    "engine": "chromium",
                    "session": {
                        "id": session_id,
                        "scope": self.session.scope.as_str(),
                        "cleanup": self.session.cleanup.as_str(),
                        "calls": calls,
                        "reused": reused,
                    },
                    "url": observation.url,
                    "title": observation.title,
                    "text": browser_observation_text(&observation),
                    "domHtml": observation.dom_html.as_deref().map(truncate_browser_text),
                    "console": observation.console,
                    "network": observation.network,
                    "outcome": outcome,
                })
                .to_string(),
            )
        }
        .await
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

    async fn close_session(&self, session_id: &str) -> Result<()> {
        let state = self.sessions.lock().await.remove(session_id);
        match state {
            Some(BrowserSessionState::Playwright {
                mut driver, lease, ..
            }) => {
                let close = driver
                    .close()
                    .await
                    .map_err(anyhow::Error::new)
                    .context("closing Playwright browser session");
                if let Some(lease) = &lease {
                    let _ = lease.cleanup_files();
                }
                close
            }
            Some(BrowserSessionState::Mock { .. }) | None => Ok(()),
        }
    }

    fn playwright_config_for_session(
        &self,
        session_id: &str,
    ) -> Result<(PlaywrightConfig, Option<BrowserProcessLease>)> {
        let mut config = playwright_config();
        let lease = match &self.store {
            Some(store) => Some(store.lease(session_id, &mut config)?),
            None => None,
        };
        Ok((config, lease))
    }
}

fn playwright_config() -> PlaywrightConfig {
    let mut config = PlaywrightConfig::new(BrowserEngine::Chromium);
    if let Some(node_bin) = non_empty_env(PLAYWRIGHT_NODE_ENV) {
        config = config.with_node_bin(node_bin);
    }
    if let Some(runner_script) = non_empty_env(PLAYWRIGHT_RUNNER_ENV) {
        config = config.with_runner_script(runner_script);
    }
    config
}

fn non_empty_env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

impl BrowserSecrets {
    fn from_decl(tool_name: &str, value: &Value) -> Result<Self> {
        if matches!(value, Value::Null)
            || value
                .as_object()
                .map(serde_json::Map::is_empty)
                .unwrap_or(false)
        {
            return Ok(Self::default());
        }
        let object = value
            .as_object()
            .with_context(|| format!("browser tool {tool_name} secrets must be an object"))?;
        let mut sources = HashMap::new();
        for (name, spec) in object {
            let name = name.trim();
            ensure!(
                !name.is_empty(),
                "browser tool {tool_name} secret names must not be empty"
            );
            let env = browser_secret_env(tool_name, name, spec)?;
            sources.insert(name.to_string(), BrowserSecretSource { env });
        }
        Ok(Self { sources })
    }

    fn resolve(&self, name: &str) -> Result<String> {
        let source = self
            .sources
            .get(name)
            .with_context(|| format!("browser action referenced unknown secret {name:?}"))?;
        non_empty_env(&source.env)
            .with_context(|| format!("browser secret {name:?} env {} is missing", source.env))
    }
}

fn browser_secret_env(tool_name: &str, name: &str, spec: &Value) -> Result<String> {
    let env = match spec {
        Value::String(env) => env.as_str(),
        Value::Object(object) => {
            let kind = object
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("env");
            ensure!(
                kind == "env",
                "browser tool {tool_name} secret {name:?} unsupported type {kind:?}; supported type: env"
            );
            object
                .get("env")
                .and_then(Value::as_str)
                .with_context(|| {
                    format!("browser tool {tool_name} secret {name:?} requires string env")
                })?
        }
        _ => bail!("browser tool {tool_name} secret {name:?} must be a string env or object"),
    }
    .trim();
    ensure!(
        !env.is_empty(),
        "browser tool {tool_name} secret {name:?} env must not be empty"
    );
    Ok(env.to_string())
}

fn browser_action_from_input_with_secrets(
    input: &Value,
    secrets: &BrowserSecrets,
) -> Result<Option<ResolvedBrowserAction>> {
    let Some(action_value) = input.get("action") else {
        return Ok(None);
    };
    if let Some(secret_name) = browser_type_secret_name(action_value, input)? {
        let selector = browser_type_selector(action_value, input)?.to_string();
        let text = secrets.resolve(secret_name)?;
        let redacted_text = format!("<redacted:{secret_name}>");
        return Ok(Some(ResolvedBrowserAction {
            action: BrowserAction::Type {
                selector: selector.clone(),
                text,
            },
            redacted: BrowserAction::Type {
                selector,
                text: redacted_text,
            },
        }));
    }
    let action = browser_action_from_input(input)?;
    Ok(action.map(|action| ResolvedBrowserAction {
        redacted: action.clone(),
        action,
    }))
}

fn browser_type_secret_name<'a>(
    action_value: &'a Value,
    input: &'a Value,
) -> Result<Option<&'a str>> {
    if action_value.as_str() == Some("type") {
        return Ok(input.get("textSecret").and_then(Value::as_str));
    }
    if action_value.is_object() {
        let verb = action_value
            .get("type")
            .or_else(|| action_value.get("action"))
            .and_then(Value::as_str);
        if verb == Some("type") {
            let source = action_value.get("args").unwrap_or(action_value);
            return Ok(source
                .get("textSecret")
                .or_else(|| source.get("text_secret"))
                .and_then(Value::as_str));
        }
    }
    Ok(None)
}

fn browser_type_selector<'a>(action_value: &'a Value, input: &'a Value) -> Result<&'a str> {
    if action_value.as_str() == Some("type") {
        return required_browser_str(input, "selector");
    }
    let source = action_value.get("args").unwrap_or(action_value);
    required_browser_str(source, "selector")
}

fn browser_action_from_input(input: &Value) -> Result<Option<BrowserAction>> {
    let Some(action_value) = input.get("action") else {
        return Ok(None);
    };
    if action_value.is_object()
        && let Ok(action) = serde_json::from_value::<BrowserAction>(action_value.clone())
    {
        return Ok(Some(action));
    }
    let (verb, source) = match action_value.as_str() {
        Some(verb) => (verb, input),
        None => {
            let verb = action_value
                .get("type")
                .or_else(|| action_value.get("action"))
                .and_then(Value::as_str)
                .context("browser action object requires string type or action")?;
            (verb, action_value)
        }
    };
    Ok(Some(browser_action_from_parts(verb, source)?))
}

fn browser_action_from_parts(verb: &str, source: &Value) -> Result<BrowserAction> {
    match verb {
        "goto" => Ok(BrowserAction::Goto {
            url: required_browser_str(source, "url")?.to_string(),
        }),
        "click" => Ok(BrowserAction::Click {
            selector: required_browser_str(source, "selector")?.to_string(),
        }),
        "type" => Ok(BrowserAction::Type {
            selector: required_browser_str(source, "selector")?.to_string(),
            text: required_browser_str(source, "text")?.to_string(),
        }),
        "scroll" => Ok(BrowserAction::Scroll {
            x: source.get("x").and_then(Value::as_i64).unwrap_or(0),
            y: source.get("y").and_then(Value::as_i64).unwrap_or(0),
        }),
        "select" => Ok(BrowserAction::Select {
            selector: required_browser_str(source, "selector")?.to_string(),
            value: required_browser_str(source, "value")?.to_string(),
        }),
        "wait" => Ok(BrowserAction::Wait {
            millis: source
                .get("millis")
                .or_else(|| source.get("ms"))
                .and_then(Value::as_u64)
                .context("browser wait action requires integer millis or ms")?,
        }),
        "extract" => Ok(BrowserAction::Extract {
            selector: required_browser_str(source, "selector")?.to_string(),
        }),
        other => bail!("unsupported browser action {other:?}"),
    }
}

fn required_browser_str<'a>(source: &'a Value, field: &str) -> Result<&'a str> {
    source
        .get(field)
        .and_then(Value::as_str)
        .with_context(|| format!("browser action requires string {field}"))
}

fn browser_step_status(outcome: &StepOutcome) -> &'static str {
    match outcome.status {
        StepStatus::Ok => "ok",
        StepStatus::Error => "error",
    }
}

fn browser_observation_text(observation: &Observation) -> Option<String> {
    observation.dom_html.as_deref().map(truncate_browser_text)
}

fn truncate_browser_text(text: &str) -> String {
    const MAX_BROWSER_TEXT: usize = 16 * 1024;
    if text.len() <= MAX_BROWSER_TEXT {
        return text.to_string();
    }
    let mut end = MAX_BROWSER_TEXT;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...[truncated]", &text[..end])
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

    fn session_id<'a>(&self, context: &'a ToolCallContext) -> &'a str {
        match self.scope {
            BrowserSessionScope::Run => context
                .run_id
                .as_deref()
                .or(context.tool_use_id.as_deref())
                .unwrap_or("manual"),
        }
    }

    fn is_persistent(&self, context: &ToolCallContext) -> bool {
        matches!(self.scope, BrowserSessionScope::Run) && context.run_id.is_some()
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
        let session = RemoteMcpSessionPolicy::from_decl(decl)?;
        Ok(Self {
            endpoint,
            remote_tool,
            auth,
            timeout: Duration::from_millis(timeout_ms),
            retry,
            idempotent: decl.idempotent,
            session,
            session_id: Mutex::new(None),
        })
    }

    async fn execute(&self, input: &Value, context: &ToolCallContext) -> Result<String> {
        let bearer = self.bearer_token()?;
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .context("build remote MCP HTTP client")?;
        let session_id = self
            .session_id(&client, bearer.as_deref())
            .await
            .with_context(|| format!("initializing remote MCP session for {}", self.remote_tool))?;
        let attempts = self.effective_attempts(context);
        for attempt in 1..=attempts {
            match self
                .send_once(
                    &client,
                    input,
                    context,
                    bearer.as_deref(),
                    session_id.as_deref(),
                )
                .await
            {
                Ok(result) => return Ok(result),
                Err(
                    RemoteAttempt::Retryable(error) | RemoteAttempt::RetryableNoSideEffect(error),
                ) if attempt < attempts => {
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
                Err(RemoteAttempt::AmbiguousSuccess(error)) if !self.idempotent => {
                    return Err(ToolNeedsReview::remote_ambiguous(&self.remote_tool, error).into());
                }
                Err(
                    RemoteAttempt::Retryable(error)
                    | RemoteAttempt::RetryableNoSideEffect(error)
                    | RemoteAttempt::ProviderFailure(error)
                    | RemoteAttempt::AmbiguousSuccess(error)
                    | RemoteAttempt::Fatal(error),
                ) => {
                    return Err(error);
                }
            }
        }
        bail!("remote MCP tool {} exhausted retries", self.remote_tool)
    }

    async fn session_id(
        &self,
        client: &reqwest::Client,
        bearer: Option<&str>,
    ) -> Result<Option<String>> {
        if self.session.is_none() {
            return Ok(None);
        }
        if let Some(existing) = self.session_id.lock().unwrap().clone() {
            return Ok(Some(existing));
        }
        let session_id = self.initialize_session(client, bearer).await?;
        let mut slot = self.session_id.lock().unwrap();
        let value = slot.get_or_insert(session_id);
        Ok(Some(value.clone()))
    }

    async fn initialize_session(
        &self,
        client: &reqwest::Client,
        bearer: Option<&str>,
    ) -> Result<String> {
        if let Some(session) = &self.session {
            tracing::debug!(
                remote_tool = %self.remote_tool,
                scope = session.scope.as_str(),
                cleanup = session.cleanup.as_str(),
                "initializing remote MCP session"
            );
        }
        let id = "beater:init";
        let body = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": "beater.js", "version": env!("CARGO_PKG_VERSION")},
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
        let response = request.send().await.with_context(|| {
            format!("remote MCP initialize request to {} failed", self.endpoint)
        })?;
        let status = response.status();
        let session_id = response
            .headers()
            .get("mcp-session-id")
            .and_then(|value| value.to_str().ok())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        let text = response.text().await.with_context(|| {
            format!(
                "remote MCP initialize response body failed for {}",
                self.endpoint
            )
        })?;
        ensure!(
            status.is_success(),
            "remote MCP initialize returned HTTP {status}: {text}"
        );
        let message: Value = serde_json::from_str(&text)
            .with_context(|| format!("remote MCP initialize returned invalid JSON: {text}"))?;
        ensure!(
            message["jsonrpc"] == "2.0",
            "remote MCP initialize response has invalid jsonrpc version: {}",
            message["jsonrpc"]
        );
        ensure!(
            message["id"] == id,
            "remote MCP initialize response id {} did not match request id {id:?}",
            message["id"]
        );
        ensure!(
            message.get("error").is_none(),
            "remote MCP initialize returned JSON-RPC error: {}",
            message["error"]
        );
        ensure!(
            message.get("result").is_some(),
            "remote MCP initialize response has no result"
        );
        session_id.with_context(|| {
            format!(
                "remote_mcp tool {} requested session support but initialize returned no mcp-session-id",
                self.remote_tool
            )
        })
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
        context.idempotency_key.clone().or_else(|| {
            self.retry
                .idempotency_key
                .as_ref()
                .and_then(|source| match source {
                    IdempotencyKeySource::ToolUseId => context.tool_use_id.clone(),
                })
        })
    }

    async fn send_once(
        &self,
        client: &reqwest::Client,
        input: &Value,
        context: &ToolCallContext,
        bearer: Option<&str>,
        session_id: Option<&str>,
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
        if let Some(session_id) = session_id {
            request = request.header("mcp-session-id", session_id);
        }
        if let Some(key) = self.idempotency_key(context) {
            request = request.header("idempotency-key", key);
        }

        let response = request.send().await.map_err(|error| {
            let had_no_side_effect = error.is_connect();
            let error = anyhow!(
                "remote MCP tool {} request to {} failed: {error}",
                self.remote_tool,
                self.endpoint
            );
            if had_no_side_effect {
                RemoteAttempt::RetryableNoSideEffect(error)
            } else {
                RemoteAttempt::Retryable(error)
            }
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
            RemoteAttempt::AmbiguousSuccess(anyhow!(
                "remote MCP tool {} returned invalid JSON: {error}: {text}",
                self.remote_tool
            ))
        })?;
        if message["jsonrpc"] != "2.0" {
            return Err(RemoteAttempt::AmbiguousSuccess(anyhow!(
                "remote MCP tool {} response has invalid jsonrpc version: {}",
                self.remote_tool,
                message["jsonrpc"]
            )));
        }
        if message["id"] != id {
            return Err(RemoteAttempt::AmbiguousSuccess(anyhow!(
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
            RemoteAttempt::AmbiguousSuccess(anyhow!(
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
        mcp_result_to_string(result).map_err(RemoteAttempt::AmbiguousSuccess)
    }
}

fn remote_mcp_provider_entries(decl: &ToolDecl) -> Result<Vec<ToolEntry>> {
    let prefix = decl.name.trim();
    ensure!(
        !prefix.is_empty(),
        "remote_mcp_provider requires a non-empty name prefix"
    );
    let endpoint = parse_remote_endpoint(
        decl.endpoint
            .as_deref()
            .with_context(|| format!("remote_mcp_provider {} requires endpoint", decl.name))?,
    )?;
    ensure!(
        egress_allows(&endpoint, &decl.egress),
        "remote_mcp_provider {} endpoint {} is not allowed by egress policy {:?}",
        decl.name,
        endpoint,
        decl.egress
    );
    let timeout_ms = decl.timeout_ms.unwrap_or(10_000);
    ensure!(
        timeout_ms > 0,
        "remote_mcp_provider {} timeoutMs must be greater than 0",
        decl.name
    );
    ensure!(
        !matches!(
            decl.auth.as_ref().map(|auth| auth.kind.as_str()),
            Some("bearer")
        ) || endpoint.scheme() == "https"
            || is_loopback_endpoint(&endpoint),
        "remote_mcp_provider {} bearer auth requires https for non-loopback endpoints: {}",
        decl.name,
        endpoint
    );

    let bearer = remote_mcp_provider_bearer(decl)?;
    let timeout = Duration::from_millis(timeout_ms);
    let provider_name = decl.name.clone();
    let discovered = {
        let endpoint = endpoint.clone();
        let bearer = bearer.clone();
        thread::spawn(move || {
            remote_mcp_provider_discover_blocking(
                &provider_name,
                endpoint,
                timeout,
                bearer.as_deref(),
            )
        })
        .join()
        .map_err(|_| anyhow!("remote_mcp_provider {} discovery panicked", decl.name))??
    };

    let mut entries = Vec::new();
    for tool in discovered {
        let remote_name = tool["name"]
            .as_str()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .with_context(|| {
                format!(
                    "remote_mcp_provider {} returned a tool without a string name",
                    decl.name
                )
            })?;
        let local_name = format!("{prefix}.{remote_name}");
        let description = tool["description"]
            .as_str()
            .map(str::trim)
            .filter(|description| !description.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| format!("Remote MCP tool {remote_name}."));
        let input_schema = tool
            .get("inputSchema")
            .or_else(|| tool.get("input_schema"))
            .cloned()
            .unwrap_or_else(|| json!({"type": "object", "properties": {}}));
        let remote_decl = ToolDecl {
            kind: "remote_mcp".to_string(),
            name: local_name.clone(),
            path: None,
            idempotent: decl.idempotent,
            description: Some(description.clone()),
            input_schema: Some(input_schema.clone()),
            endpoint: decl.endpoint.clone(),
            tool: Some(remote_name.to_string()),
            auth: decl.auth.clone(),
            timeout_ms: decl.timeout_ms,
            retry: decl.retry.clone(),
            egress: decl.egress.clone(),
            provider: None,
            session: decl.session.clone(),
            allowed_origins: Vec::new(),
            secrets: Value::Null,
            lane: None,
            source: None,
            policy: None,
            entrypoint: None,
        };
        let config = RemoteMcpTool::from_decl(&remote_decl)?;
        entries.push(ToolEntry {
            name: local_name,
            description,
            input_schema,
            idempotent: decl.idempotent,
            imp: ToolImpl::RemoteMcp { config },
        });
    }
    Ok(entries)
}

fn remote_mcp_provider_discover_blocking(
    provider_name: &str,
    endpoint: reqwest::Url,
    timeout: Duration,
    bearer: Option<&str>,
) -> Result<Vec<Value>> {
    let client = reqwest::blocking::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .context("build remote MCP discovery HTTP client")?;
    let session_id = remote_mcp_provider_initialize(&client, &endpoint, timeout, bearer)
        .with_context(|| format!("initializing remote_mcp_provider {provider_name}"))?;
    remote_mcp_provider_tools_list(&client, &endpoint, timeout, bearer, session_id.as_deref())
        .with_context(|| format!("listing tools for remote_mcp_provider {provider_name}"))
}

fn remote_mcp_provider_bearer(decl: &ToolDecl) -> Result<Option<String>> {
    match &decl.auth {
        None => Ok(None),
        Some(auth) if auth.kind == "bearer" => {
            let env = auth
                .env
                .as_deref()
                .map(str::trim)
                .filter(|env| !env.is_empty())
                .with_context(|| {
                    format!("remote_mcp_provider {} bearer auth requires env", decl.name)
                })?;
            let value = std::env::var(env)
                .with_context(|| format!("remote_mcp_provider bearer env {env} is not set"))?;
            ensure!(
                !value.trim().is_empty(),
                "remote_mcp_provider bearer env {env} is empty"
            );
            Ok(Some(value))
        }
        Some(auth) if auth.kind == "none" => Ok(None),
        Some(auth) => bail!(
            "remote_mcp_provider {} has unsupported auth type {:?}",
            decl.name,
            auth.kind
        ),
    }
}

fn remote_mcp_provider_initialize(
    client: &reqwest::blocking::Client,
    endpoint: &reqwest::Url,
    timeout: Duration,
    bearer: Option<&str>,
) -> Result<Option<String>> {
    let id = "beater:discover:init";
    let body = json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "clientInfo": {"name": "beater.js", "version": env!("CARGO_PKG_VERSION")},
        },
    });
    let (message, session_id) =
        remote_mcp_provider_request(client, endpoint, timeout, bearer, None, &body)?;
    ensure!(
        message.get("result").is_some(),
        "remote MCP initialize response has no result"
    );
    Ok(session_id)
}

fn remote_mcp_provider_tools_list(
    client: &reqwest::blocking::Client,
    endpoint: &reqwest::Url,
    timeout: Duration,
    bearer: Option<&str>,
    session_id: Option<&str>,
) -> Result<Vec<Value>> {
    let body = json!({
        "jsonrpc": "2.0",
        "id": "beater:discover:tools",
        "method": "tools/list",
        "params": {},
    });
    let (message, _) =
        remote_mcp_provider_request(client, endpoint, timeout, bearer, session_id, &body)?;
    let tools = message["result"]["tools"]
        .as_array()
        .with_context(|| format!("remote MCP tools/list response has no tools array: {message}"))?;
    Ok(tools.clone())
}

fn remote_mcp_provider_request(
    client: &reqwest::blocking::Client,
    endpoint: &reqwest::Url,
    timeout: Duration,
    bearer: Option<&str>,
    session_id: Option<&str>,
    body: &Value,
) -> Result<(Value, Option<String>)> {
    let id = body["id"].as_str().unwrap_or_default();
    let method = body["method"].as_str().unwrap_or("request");
    let mut request = client
        .post(endpoint.clone())
        .timeout(timeout)
        .header("accept", "application/json")
        .header("content-type", "application/json")
        .header("mcp-protocol-version", "2025-11-25")
        .json(body);
    if let Some(bearer) = bearer {
        request = request.header("authorization", format!("Bearer {bearer}"));
    }
    if let Some(session_id) = session_id {
        request = request.header("mcp-session-id", session_id);
    }
    let response = request
        .send()
        .with_context(|| format!("remote MCP discovery {method} request to {endpoint} failed"))?;
    let status = response.status();
    let session_id = response
        .headers()
        .get("mcp-session-id")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let text = response
        .text()
        .with_context(|| format!("remote MCP discovery {method} response body failed"))?;
    ensure!(
        status.is_success(),
        "remote MCP discovery {method} returned HTTP {status}: {text}"
    );
    let message: Value = serde_json::from_str(&text)
        .with_context(|| format!("remote MCP discovery {method} returned invalid JSON: {text}"))?;
    ensure!(
        message["jsonrpc"] == "2.0",
        "remote MCP discovery {method} response has invalid jsonrpc version: {}",
        message["jsonrpc"]
    );
    ensure!(
        message["id"] == id,
        "remote MCP discovery {method} response id {} did not match request id {id:?}",
        message["id"]
    );
    ensure!(
        message.get("error").is_none(),
        "remote MCP discovery {method} returned JSON-RPC error: {}",
        message["error"]
    );
    Ok((message, session_id))
}

impl RemoteMcpSessionPolicy {
    fn from_decl(decl: &ToolDecl) -> Result<Option<Self>> {
        let Some(session) = decl.session.as_ref() else {
            return Ok(None);
        };
        let scope = match session.scope.as_deref().unwrap_or("run") {
            "run" => RemoteMcpSessionScope::Run,
            other => bail!(
                "remote_mcp tool {} unsupported session.scope {other:?}; supported scopes: run",
                decl.name
            ),
        };
        let cleanup = match session.cleanup.as_deref().unwrap_or("always") {
            "always" => RemoteMcpSessionCleanup::Always,
            other => bail!(
                "remote_mcp tool {} unsupported session.cleanup {other:?}; supported cleanup: always",
                decl.name
            ),
        };
        Ok(Some(Self { scope, cleanup }))
    }
}

impl RemoteMcpSessionScope {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Run => "run",
        }
    }
}

impl RemoteMcpSessionCleanup {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Always => "always",
        }
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
        "cpp_double" => Some(ToolEntry {
            name: name.to_string(),
            description: "Double an integer through the host C++ tool bridge.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {"n": {"type": "integer"}},
                "required": ["n"],
                "additionalProperties": false,
            }),
            idempotent: true,
            imp: ToolImpl::RustBuiltin,
        }),
        _ => None,
    }
}

fn execute_builtin(name: &str, input: &Value) -> Result<String> {
    match name {
        "get_time" => {
            let now = chrono::Utc::now();
            Ok(json!({"iso": now.to_rfc3339(), "unix": now.timestamp()}).to_string())
        }
        "cpp_double" => {
            let n = input
                .get("n")
                .and_then(Value::as_i64)
                .context("cpp_double requires integer input field n")?;
            Ok(json!({"value": crate::cpp_bridge::double(n)}).to_string())
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

    use beater_browser::BrowserAction;
    use serde_json::{Value, json};

    use super::{
        BrowserSecrets, BrowserSessionGuard, BrowserSessionStore, ToolCallContext, ToolDecl,
        ToolImpl, ToolNeedsReview, ToolRegistry, browser_action_from_input,
        browser_action_from_input_with_secrets, browser_session_active_for_tests,
        browser_session_count_for_tests,
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
    fn registry_deduplicates_tool_names_within_one_agent() {
        let registry = ToolRegistry::build(
            PathBuf::new().as_path(),
            &[rust_decl("get_time", true), rust_decl("get_time", false)],
        )
        .expect("registry should keep first duplicate");

        assert_eq!(registry.entries().len(), 1);
        let tool = registry.get("get_time").unwrap();
        assert!(matches!(tool.imp, ToolImpl::RustBuiltin));
        assert!(tool.idempotent);
    }

    #[test]
    fn rust_builtin_honors_declared_idempotent_flag() {
        let registry =
            ToolRegistry::build(PathBuf::new().as_path(), &[rust_decl("get_time", false)])
                .expect("rust builtin registry should build");

        assert!(!registry.get("get_time").unwrap().idempotent);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cpp_builtin_executes_through_rust_tool_registry() {
        let registry =
            ToolRegistry::build(PathBuf::new().as_path(), &[rust_decl("cpp_double", true)])
                .expect("C++ builtin registry should build");
        let tool = registry.get("cpp_double").unwrap();
        assert!(matches!(tool.imp, ToolImpl::RustBuiltin));
        assert_eq!(tool.input_schema["properties"]["n"]["type"], "integer");

        let result = registry
            .execute("cpp_double", &json!({"n": 21}))
            .await
            .expect("C++ builtin should execute");
        assert_eq!(serde_json::from_str::<Value>(&result).unwrap()["value"], 42);
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
    async fn remote_mcp_initializes_and_reuses_provider_session() {
        let mut init = MockResponse::json(
            "200 OK",
            json!({
                "jsonrpc": "2.0",
                "id": "beater:init",
                "result": {
                    "protocolVersion": "2025-11-25",
                    "capabilities": {},
                    "serverInfo": {"name": "mock-mcp", "version": "1.0.0"}
                }
            }),
        );
        init.headers
            .push(("mcp-session-id".to_string(), "session-123".to_string()));
        let server = MockMcp::new(vec![
            init,
            MockResponse::json(
                "200 OK",
                json!({
                    "jsonrpc": "2.0",
                    "id": "toolu_first",
                    "result": {
                        "content": [{"type": "text", "text": "{\"ok\":\"first\"}"}],
                        "isError": false
                    }
                }),
            ),
            MockResponse::json(
                "200 OK",
                json!({
                    "jsonrpc": "2.0",
                    "id": "toolu_second",
                    "result": {
                        "content": [{"type": "text", "text": "{\"ok\":\"second\"}"}],
                        "isError": false
                    }
                }),
            ),
        ]);
        let mut decl = remote_decl(&server.endpoint, None, None, true);
        decl.session =
            Some(serde_json::from_value(json!({"scope": "run", "cleanup": "always"})).unwrap());
        let registry = ToolRegistry::build(PathBuf::new().as_path(), &[decl])
            .expect("remote MCP registry should build");

        let first = registry
            .execute_with_context(
                "crm.lookup",
                &json!({"email": "a@example.com"}),
                &ToolCallContext {
                    tool_use_id: Some("toolu_first".to_string()),
                    ..ToolCallContext::default()
                },
            )
            .await
            .expect("first session tool call should succeed");
        let second = registry
            .execute_with_context(
                "crm.lookup",
                &json!({"email": "b@example.com"}),
                &ToolCallContext {
                    tool_use_id: Some("toolu_second".to_string()),
                    ..ToolCallContext::default()
                },
            )
            .await
            .expect("second session tool call should succeed");

        assert_eq!(first, "{\"ok\":\"first\"}");
        assert_eq!(second, "{\"ok\":\"second\"}");
        let requests = server.requests();
        assert_eq!(requests.len(), 3);
        let init_body: Value = serde_json::from_str(&requests[0].body).unwrap();
        assert_eq!(init_body["method"], "initialize");
        assert!(
            !requests[0]
                .headers
                .to_ascii_lowercase()
                .contains("mcp-session-id")
        );
        for request in &requests[1..] {
            assert!(
                request
                    .headers
                    .to_ascii_lowercase()
                    .contains("mcp-session-id: session-123"),
                "{}",
                request.headers
            );
            let body: Value = serde_json::from_str(&request.body).unwrap();
            assert_eq!(body["method"], "tools/call");
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn remote_mcp_provider_discovers_tools_list_and_executes_imported_tool() {
        let server = MockMcp::new(vec![
            MockResponse::json(
                "200 OK",
                json!({
                    "jsonrpc": "2.0",
                    "id": "beater:discover:init",
                    "result": {
                        "protocolVersion": "2025-11-25",
                        "capabilities": {"tools": {}},
                        "serverInfo": {"name": "mock-mcp", "version": "1.0.0"}
                    }
                }),
            ),
            MockResponse::json(
                "200 OK",
                json!({
                    "jsonrpc": "2.0",
                    "id": "beater:discover:tools",
                    "result": {
                        "tools": [{
                            "name": "lookup_contact",
                            "description": "Look up a CRM contact from the provider.",
                            "inputSchema": {
                                "type": "object",
                                "properties": {"email": {"type": "string"}},
                                "required": ["email"]
                            }
                        }]
                    }
                }),
            ),
            MockResponse::json(
                "200 OK",
                json!({
                    "jsonrpc": "2.0",
                    "id": "toolu_provider",
                    "result": {
                        "content": [{"type": "text", "text": "{\"ok\":true}"}],
                        "isError": false
                    }
                }),
            ),
        ]);
        let registry = ToolRegistry::build(
            PathBuf::new().as_path(),
            &[provider_decl(&server.endpoint, "crm")],
        )
        .expect("provider registry should discover tools");

        let tool = registry
            .get("crm.lookup_contact")
            .expect("discovered provider tool should be imported");
        assert_eq!(tool.description, "Look up a CRM contact from the provider.");
        assert_eq!(tool.input_schema["properties"]["email"]["type"], "string");

        let result = registry
            .execute_with_context(
                "crm.lookup_contact",
                &json!({"email": "a@example.com"}),
                &ToolCallContext {
                    run_id: None,
                    tool_use_id: Some("toolu_provider".to_string()),
                    idempotency_key: None,
                },
            )
            .await
            .expect("imported provider tool should execute");
        assert_eq!(result, "{\"ok\":true}");

        let requests = server.wait_for_requests(3, Duration::from_secs(1));
        assert!(requests[0].body.contains(r#""method":"initialize""#));
        assert!(requests[1].body.contains(r#""method":"tools/list""#));
        assert!(
            requests[2].body.contains(r#""name":"lookup_contact""#),
            "{}",
            requests[2].body
        );
        assert!(
            requests[2].body.contains(r#""email":"a@example.com""#),
            "{}",
            requests[2].body
        );
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

    #[test]
    fn remote_mcp_session_policy_rejects_unsupported_scope() {
        let mut decl = remote_decl("http://127.0.0.1:65530/mcp", None, None, true);
        decl.session =
            Some(serde_json::from_value(json!({"scope": "global", "cleanup": "always"})).unwrap());
        let error = match ToolRegistry::build(PathBuf::new().as_path(), &[decl]) {
            Ok(_) => panic!("unsupported remote MCP session scope should fail"),
            Err(error) => error,
        };
        assert!(
            format!("{error:#}").contains("unsupported session.scope"),
            "{error:#}"
        );
    }

    #[test]
    fn browser_playwright_provider_builds_and_accepts_scoped_secrets() {
        let mut decl = browser_decl();
        decl.provider = Some("playwright".to_string());
        ToolRegistry::build(PathBuf::new().as_path(), &[decl])
            .expect("playwright browser registry should build");

        let mut decl = browser_decl();
        decl.provider = Some("playwright".to_string());
        decl.secrets = json!({"password": {"type": "env", "env": "SHOP_PASSWORD"}});
        ToolRegistry::build(PathBuf::new().as_path(), &[decl])
            .expect("playwright browser registry should accept env-scoped secrets");

        let mut decl = browser_decl();
        decl.provider = Some("playwright".to_string());
        decl.secrets = json!({"password": {"type": "literal", "value": "secret"}});
        let error = match ToolRegistry::build(PathBuf::new().as_path(), &[decl]) {
            Ok(_) => panic!("browser provider should reject unsupported secret source types"),
            Err(error) => error,
        };
        assert!(
            format!("{error:#}").contains("unsupported type"),
            "{error:#}"
        );
    }

    #[test]
    fn browser_type_action_resolves_secret_and_redacts_result_action() {
        let env = unique_env("BEATER_BROWSER_SECRET");
        unsafe {
            std::env::set_var(&env, "correct horse battery staple");
        }
        let secrets = BrowserSecrets::from_decl(
            "browser.checkout",
            &json!({"password": {"env": env.clone()}}),
        )
        .unwrap();

        let action = browser_action_from_input_with_secrets(
            &json!({
                "action": "type",
                "selector": "#password",
                "textSecret": "password"
            }),
            &secrets,
        )
        .unwrap()
        .expect("action should resolve");

        assert_eq!(
            action.action,
            BrowserAction::Type {
                selector: "#password".to_string(),
                text: "correct horse battery staple".to_string(),
            }
        );
        assert_eq!(
            action.redacted,
            BrowserAction::Type {
                selector: "#password".to_string(),
                text: "<redacted:password>".to_string(),
            }
        );
        let nested = browser_action_from_input_with_secrets(
            &json!({
                "action": {
                    "action": "type",
                    "args": {"selector": "#password", "textSecret": "password"}
                }
            }),
            &secrets,
        )
        .unwrap()
        .expect("nested action should resolve");
        assert_eq!(nested.action, action.action);
        assert_eq!(nested.redacted, action.redacted);
        unsafe {
            std::env::remove_var(env);
        }
    }

    #[test]
    fn browser_type_action_missing_secret_env_fails_before_driver() {
        let env = unique_env("BEATER_BROWSER_MISSING_SECRET");
        unsafe {
            std::env::remove_var(&env);
        }
        let secrets =
            BrowserSecrets::from_decl("browser.checkout", &json!({"password": env})).unwrap();

        let error = browser_action_from_input_with_secrets(
            &json!({
                "action": {"type": "type", "selector": "#password", "textSecret": "password"}
            }),
            &secrets,
        )
        .expect_err("missing env should fail");

        assert!(format!("{error:#}").contains("env"), "{error:#}");
    }

    #[test]
    fn browser_playwright_provider_enforces_allowed_origins_before_driver() {
        let mut decl = browser_decl();
        decl.provider = Some("playwright".to_string());
        let registry = ToolRegistry::build(PathBuf::new().as_path(), &[decl])
            .expect("playwright browser registry should build");
        let tool = registry
            .get("browser.checkout")
            .expect("browser tool should exist");
        let ToolImpl::Browser { config } = &tool.imp else {
            panic!("expected browser tool");
        };
        let error = config
            .ensure_url_allowed("https://evil.example/cart")
            .expect_err("disallowed origin should fail before driver launch");
        assert!(
            format!("{error:#}").contains("not allowed by allowedOrigins"),
            "{error:#}"
        );
    }

    #[test]
    fn browser_action_input_supports_compact_and_driver_shapes() {
        assert_eq!(
            browser_action_from_input(&json!({"action": "click", "selector": "#buy"}))
                .unwrap()
                .unwrap(),
            BrowserAction::Click {
                selector: "#buy".to_string()
            }
        );
        assert_eq!(
            browser_action_from_input(&json!({
                "action": {"action": "type", "args": {"selector": "#email", "text": "a@example.com"}}
            }))
            .unwrap()
            .unwrap(),
            BrowserAction::Type {
                selector: "#email".to_string(),
                text: "a@example.com".to_string()
            }
        );
        assert_eq!(
            browser_action_from_input(&json!({"action": {"type": "wait", "ms": 50}}))
                .unwrap()
                .unwrap(),
            BrowserAction::Wait { millis: 50 }
        );
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
    async fn browser_run_scope_uses_run_id_for_session() {
        let registry = ToolRegistry::build(PathBuf::new().as_path(), &[browser_decl()])
            .expect("browser registry should build");

        let context = ToolCallContext {
            run_id: Some("run-browser-session".to_string()),
            tool_use_id: Some("toolu_browser".to_string()),
            ..ToolCallContext::default()
        };
        let first = registry
            .execute_with_context(
                "browser.checkout",
                &json!({"url": "https://shop.example/cart", "task": "verify checkout"}),
                &context,
            )
            .await
            .expect("first browser tool call should run");
        let second = registry
            .execute_with_context(
                "browser.checkout",
                &json!({"url": "https://shop.example/cart", "task": "verify checkout again"}),
                &context,
            )
            .await
            .expect("second browser tool call should reuse the run session");

        assert!(browser_session_active_for_tests("run-browser-session"));
        let first: Value = serde_json::from_str(&first).unwrap();
        let second: Value = serde_json::from_str(&second).unwrap();
        assert_eq!(first["session"]["id"], "run-browser-session");
        assert_eq!(first["session"]["scope"], "run");
        assert_eq!(first["session"]["calls"], 1);
        assert_eq!(first["session"]["reused"], false);
        assert_eq!(second["session"]["id"], "run-browser-session");
        assert_eq!(second["session"]["calls"], 2);
        assert_eq!(second["session"]["reused"], true);

        registry
            .close_browser_sessions("run-browser-session")
            .await
            .expect("browser session cleanup should succeed");
        assert!(!browser_session_active_for_tests("run-browser-session"));
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
    #[cfg(unix)]
    fn stale_browser_session_cleanup_kills_marked_runner_and_removes_files() {
        let temp = TempAgentDir::new("browser-session-cleanup");
        let root = temp.path.join(".beater/browser-sessions");
        fs::create_dir_all(&root).unwrap();
        let wrapper_path = root.join("run-browser-cleanup.cjs");
        let marker_path = root.join("run-browser-cleanup.json");
        fs::write(&wrapper_path, "#!/bin/sh\nsleep 30\n").unwrap();
        let mut child = std::process::Command::new("sh")
            .arg(&wrapper_path)
            .spawn()
            .unwrap();
        fs::write(
            &marker_path,
            json!({
                "session_id": "run-browser-cleanup",
                "wrapper_script": wrapper_path.clone(),
                "runner_script": "/dev/null",
                "owner_pid": 1,
                "created_at": 1,
            })
            .to_string(),
        )
        .unwrap();

        BrowserSessionStore::new(root)
            .cleanup_session("run-browser-cleanup")
            .unwrap();

        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if child.try_wait().unwrap().is_some() {
                break;
            }
            thread::sleep(Duration::from_millis(25));
        }
        assert!(child.try_wait().unwrap().is_some());
        assert!(!marker_path.exists());
        assert!(!wrapper_path.exists());
    }

    #[test]
    fn browser_mock_cdp_accepts_scoped_secrets() {
        let mut decl = browser_decl();
        decl.secrets = json!({"profile": {"env": "BROWSER_PROFILE_ID"}});

        ToolRegistry::build(PathBuf::new().as_path(), &[decl])
            .expect("mock browser registry should accept env-scoped secrets");
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
    async fn remote_mcp_non_idempotent_malformed_success_needs_review() {
        for response in [
            MockResponse::text("200 OK", "{not json"),
            MockResponse::json(
                "200 OK",
                json!({
                    "jsonrpc": "1.0",
                    "id": "toolu_malformed",
                    "result": {
                        "content": [{"type": "text", "text": "{\"ok\":true}"}],
                        "isError": false
                    }
                }),
            ),
            MockResponse::json(
                "200 OK",
                json!({
                    "jsonrpc": "2.0",
                    "id": "wrong-id",
                    "result": {
                        "content": [{"type": "text", "text": "{\"ok\":true}"}],
                        "isError": false
                    }
                }),
            ),
            MockResponse::json("200 OK", json!({"jsonrpc": "2.0", "id": "toolu_malformed"})),
        ] {
            let server = MockMcp::new(vec![response]);
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
                        tool_use_id: Some("toolu_malformed".to_string()),
                        ..ToolCallContext::default()
                    },
                )
                .await
                .expect_err("malformed HTTP 200 should need review for non-idempotent tools");
            assert!(
                error.downcast_ref::<ToolNeedsReview>().is_some(),
                "{error:#}"
            );
            assert_eq!(server.requests().len(), 1);
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn remote_mcp_fatal_client_errors_do_not_need_review() {
        let server = MockMcp::new(vec![MockResponse::text("400 Bad Request", "{not json")]);
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
                    tool_use_id: Some("toolu_bad_request".to_string()),
                    ..ToolCallContext::default()
                },
            )
            .await
            .expect_err("HTTP 400 should remain fatal");
        assert!(
            error.downcast_ref::<ToolNeedsReview>().is_none(),
            "{error:#}"
        );
        assert!(format!("{error:#}").contains("HTTP 400"), "{error:#}");
        assert_eq!(server.requests().len(), 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn remote_mcp_connect_errors_do_not_need_review_for_non_idempotent_tools() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let endpoint = format!(
            "http://127.0.0.1:{}/mcp",
            listener.local_addr().unwrap().port()
        );
        drop(listener);
        let mut decl = remote_decl(&endpoint, None, None, false);
        decl.timeout_ms = Some(100);
        let registry = ToolRegistry::build(PathBuf::new().as_path(), &[decl])
            .expect("remote MCP registry should build");

        let error = registry
            .execute_with_context(
                "crm.lookup",
                &json!({"email": "a@example.com"}),
                &ToolCallContext {
                    tool_use_id: Some("toolu_connect".to_string()),
                    ..ToolCallContext::default()
                },
            )
            .await
            .expect_err("connect-refused should fail");

        assert!(
            error.downcast_ref::<ToolNeedsReview>().is_none(),
            "{error:#}"
        );
        assert!(format!("{error:#}").contains("failed"), "{error:#}");
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
    async fn remote_mcp_sends_journaled_idempotency_key_for_idempotent_tools() {
        let server = MockMcp::new(vec![MockResponse::json(
            "200 OK",
            json!({
                "jsonrpc": "2.0",
                "id": "toolu_journaled",
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

        registry
            .execute_with_context(
                "crm.lookup",
                &json!({"email": "a@example.com"}),
                &ToolCallContext {
                    run_id: None,
                    tool_use_id: Some("toolu_journaled".to_string()),
                    idempotency_key: Some("beater:run-1:tool:toolu_journaled".to_string()),
                },
            )
            .await
            .expect("idempotent remote MCP call should succeed");

        let requests = server.requests();
        assert_eq!(requests.len(), 1);
        let headers = requests[0].headers.to_ascii_lowercase();
        assert!(
            headers.contains("idempotency-key: beater:run-1:tool:toolu_journaled"),
            "{}",
            requests[0].headers
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn remote_mcp_prefers_journaled_idempotency_key_over_tool_use_id_header() {
        let server = MockMcp::new(vec![MockResponse::json(
            "200 OK",
            json!({
                "jsonrpc": "2.0",
                "id": "toolu_raw",
                "result": {
                    "content": [{"type": "text", "text": "{\"ok\":true}"}],
                    "isError": false
                }
            }),
        )]);
        let mut decl = remote_decl(&server.endpoint, None, None, true);
        decl.retry = Some(
            serde_json::from_value(json!({
                "attempts": 1,
                "idempotencyKey": "tool_use_id"
            }))
            .unwrap(),
        );
        let registry = ToolRegistry::build(PathBuf::new().as_path(), &[decl])
            .expect("remote MCP registry should build");

        registry
            .execute_with_context(
                "crm.lookup",
                &json!({"email": "a@example.com"}),
                &ToolCallContext {
                    run_id: None,
                    tool_use_id: Some("toolu_raw".to_string()),
                    idempotency_key: Some("beater:run-1:tool:toolu_raw".to_string()),
                },
            )
            .await
            .expect("idempotent remote MCP call should succeed");

        let requests = server.requests();
        assert_eq!(requests.len(), 1);
        let headers = requests[0].headers.to_ascii_lowercase();
        assert!(
            headers.contains("idempotency-key: beater:run-1:tool:toolu_raw"),
            "{}",
            requests[0].headers
        );
        assert!(!headers.contains("idempotency-key: toolu_raw"));
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

    #[tokio::test(flavor = "current_thread")]
    async fn sandbox_non_ok_execute_result_fails_closed() {
        let server = MockMcp::new(vec![MockResponse::json(
            "200 OK",
            execution_result_json("denied", Some(("policy_denied", "network blocked"))),
        )]);
        let registry = ToolRegistry::build_with_beatbox(
            PathBuf::new().as_path(),
            &[sandbox_decl(false)],
            &super::BeatboxConfig {
                url: server.endpoint.clone(),
                api_key: None,
            },
        )
        .expect("sandbox registry should build");

        let error = registry
            .execute("fib_wasm", &json!({"n": 10}))
            .await
            .expect_err("sandbox denied result should fail");
        let text = format!("{error:#}");
        assert!(text.contains("sandbox execution returned Denied"), "{text}");
        assert!(text.contains("\"status\":\"denied\""), "{text}");

        let requests = server.requests();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].headers.starts_with("POST /mcp/v1/execute "));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn sandbox_non_ok_job_result_fails_closed() {
        let server = MockMcp::new(vec![
            MockResponse::json("202 Accepted", json!({"job_id": "job-1"})),
            MockResponse::json(
                "200 OK",
                job_record_json(
                    "job-1",
                    execution_result_json("timeout", Some(("wall_timeout", "wall time exceeded"))),
                ),
            ),
        ]);
        let registry = ToolRegistry::build_with_beatbox(
            PathBuf::new().as_path(),
            &[sandbox_decl(true)],
            &super::BeatboxConfig {
                url: server.endpoint.clone(),
                api_key: None,
            },
        )
        .expect("sandbox registry should build");

        let error = registry
            .execute_with_context(
                "fib_wasm",
                &json!({"n": 10}),
                &ToolCallContext {
                    idempotency_key: Some("beater:run-1:tool:toolu_1".to_string()),
                    ..ToolCallContext::default()
                },
            )
            .await
            .expect_err("sandbox timeout job result should fail");
        let text = format!("{error:#}");
        assert!(
            text.contains("sandbox execution returned Timeout"),
            "{text}"
        );
        assert!(text.contains("\"status\":\"timeout\""), "{text}");

        let requests = server.requests();
        assert_eq!(requests.len(), 2);
        assert!(requests[0].headers.starts_with("POST /mcp/v1/jobs "));
        assert!(requests[1].headers.starts_with("GET /mcp/v1/jobs/job-1 "));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn wasmtime_tool_runs_hermetic_wasm_function() {
        let registry = ToolRegistry::build(PathBuf::new().as_path(), &[wasmtime_decl()])
            .expect("wasmtime registry should build");

        let result = registry
            .execute("double_wasm", &json!({"n": 21}))
            .await
            .expect("wasmtime tool should run");
        let result: Value = serde_json::from_str(&result).unwrap();
        assert_eq!(result["status"], "ok");
        assert_eq!(result["impl"], "wasmtime");
        assert_eq!(result["value"], 42);
        assert_eq!(result["isolation"]["filesystem"], "none");
        assert_eq!(result["isolation"]["network"], "none");
        assert_eq!(result["isolation"]["imports"], "denied");
    }

    #[test]
    fn wasmtime_tool_rejects_filesystem_imports_before_execution() {
        let mut decl = wasmtime_decl();
        decl.source = Some(
            serde_json::from_value(json!({
                "kind": "wasm_wat",
                "text": r#"
                    (module
                      (import "wasi_snapshot_preview1" "path_open"
                        (func $path_open (param i32 i32 i32 i32 i32 i64 i64 i32 i32) (result i32)))
                      (func (export "run") (result i64)
                        i64.const 1))
                "#
            }))
            .unwrap(),
        );
        let error = match ToolRegistry::build(PathBuf::new().as_path(), &[decl]) {
            Ok(_) => panic!("wasmtime tool with host imports should fail"),
            Err(error) => error,
        };
        let text = format!("{error:#}");
        assert!(text.contains("imports are disabled"), "{text}");
        assert!(text.contains("wasi_snapshot_preview1::path_open"), "{text}");
    }

    #[test]
    fn wasmtime_policy_rejects_filesystem_mounts() {
        let mut decl = wasmtime_decl();
        decl.policy = Some(json!({
            "fs": {
                "mounts": [{
                    "host": "/tmp",
                    "guest": "/host",
                    "mode": "ro"
                }]
            }
        }));
        let error = match ToolRegistry::build(PathBuf::new().as_path(), &[decl]) {
            Ok(_) => panic!("wasmtime tool with filesystem mount should fail"),
            Err(error) => error,
        };
        assert!(
            format!("{error:#}").contains("no filesystem mounts"),
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

    fn rust_decl(name: &str, idempotent: bool) -> ToolDecl {
        serde_json::from_value(json!({
            "kind": "rust",
            "name": name,
            "idempotent": idempotent,
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

    fn provider_decl(endpoint: &str, prefix: &str) -> ToolDecl {
        let url = reqwest::Url::parse(endpoint).unwrap();
        let host = url.host_str().unwrap();
        let host_port = url
            .port()
            .map(|port| format!("{host}:{port}"))
            .unwrap_or_else(|| host.to_string());
        serde_json::from_value(json!({
            "kind": "remote_mcp_provider",
            "name": prefix,
            "endpoint": endpoint,
            "timeoutMs": 1000,
            "idempotent": true,
            "egress": [host_port]
        }))
        .unwrap()
    }

    fn sandbox_decl(idempotent: bool) -> ToolDecl {
        serde_json::from_value(json!({
            "kind": "sandbox",
            "name": "fib_wasm",
            "source": {"kind": "wasm_wat", "text": "(module)"},
            "idempotent": idempotent,
            "description": "Run fib in beatbox.",
            "inputSchema": {
                "type": "object",
                "properties": {"n": {"type": "integer"}},
                "required": ["n"]
            }
        }))
        .unwrap()
    }

    fn wasmtime_decl() -> ToolDecl {
        serde_json::from_value(json!({
            "kind": "wasmtime",
            "name": "double_wasm",
            "source": {
                "kind": "wasm_wat",
                "text": r#"
                    (module
                      (func (export "run") (param i64) (result i64)
                        local.get 0
                        i64.const 2
                        i64.mul))
                "#
            },
            "description": "Double an integer in the local Wasmtime sandbox.",
            "inputSchema": {
                "type": "object",
                "properties": {"n": {"type": "integer"}},
                "required": ["n"]
            },
            "policy": {
                "limits": {
                    "wall_ms": 5000,
                    "memory_bytes": 67108864,
                    "fuel": 1000000
                }
            },
            "idempotent": true
        }))
        .unwrap()
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

    fn execution_result_json(status: &str, error: Option<(&str, &str)>) -> Value {
        json!({
            "status": status,
            "value": if status == "ok" { json!(55) } else { Value::Null },
            "exit_code": null,
            "stdout": "",
            "stdout_truncated": false,
            "stderr": "",
            "stderr_truncated": false,
            "error": error.map(|(code, message)| json!({"code": code, "message": message})),
            "metrics": {
                "wall_time_ms": 1,
                "cpu_time_ms": 1,
                "fuel_used": 42,
                "peak_memory_bytes": null
            },
            "lane": "wasm",
            "deterministic": true,
            "inputs_digest": "sha256:test",
            "engine_version": "test",
            "beatbox_version": "test",
            "effective_isolation": {
                "os": "test",
                "mechanisms": ["wasmtime", "empty-linker"],
                "landlock_abi": null,
                "downgrades": []
            },
            "egress": []
        })
    }

    fn job_record_json(job_id: &str, result: Value) -> Value {
        json!({
            "job_id": job_id,
            "status": "succeeded",
            "request": {
                "lane": "wasm",
                "source": {"kind": "wasm_wat", "text": "(module)"},
                "entrypoint": null,
                "input": {"n": 10},
                "stdin": "",
                "policy": {},
                "idempotency_key": "beater:run-1:tool:toolu_1"
            },
            "result": result,
            "error": null,
            "created_at": "2026-07-02T00:00:00Z",
            "updated_at": "2026-07-02T00:00:00Z"
        })
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

        fn text(status: &'static str, body: &str) -> Self {
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
