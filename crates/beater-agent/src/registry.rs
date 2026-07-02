//! One registry, three tool tiers: Python files (embedded CPython), Rust
//! built-ins, and sandboxed beatbox tools. Every tool declares
//! `idempotent` — the resume-safety contract (ARCHITECTURE.md §5).

use std::fmt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine as _;
use serde::Deserialize;
use serde_json::{Value, json};

pub const DEFAULT_BEATBOX_URL: &str = "http://127.0.0.1:7300";

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
    pub kind: String, // "python" | "rust" | "sandbox"
    pub name: String,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub idempotent: bool,
    #[serde(default)]
    pub lane: Option<beatbox_client::Lane>,
    #[serde(default)]
    pub source: Option<SandboxSourceDecl>,
    #[serde(default)]
    pub policy: Option<Value>,
    #[serde(default)]
    pub entrypoint: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default, alias = "inputSchema")]
    pub input_schema: Option<Value>,
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

pub enum ToolImpl {
    Python { path: PathBuf },
    RustBuiltin,
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

impl ToolRegistry {
    /// Build from an agent's tool declarations. Python tool metadata comes
    /// from each file's module-level TOOL dict.
    pub fn build(agent_dir: &Path, decls: &[ToolDecl], beatbox: &BeatboxConfig) -> Result<Self> {
        let mut tools = Vec::new();
        for decl in decls {
            match decl.kind.as_str() {
                "python" => {
                    let rel = decl
                        .path
                        .as_deref()
                        .with_context(|| format!("python tool {} has no path", decl.name))?;
                    let path = agent_dir.join(rel.trim_start_matches("./"));
                    let (description, input_schema) = beater_py::load_tool_spec(&path)
                        .with_context(|| format!("loading python tool {}", decl.name))?;
                    tools.push(ToolEntry {
                        name: decl.name.clone(),
                        description,
                        input_schema,
                        idempotent: decl.idempotent,
                        imp: ToolImpl::Python { path },
                    });
                }
                "rust" => {
                    let entry = rust_builtin(&decl.name)
                        .with_context(|| format!("unknown rust builtin tool {}", decl.name))?;
                    tools.push(entry);
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
    pub async fn execute(
        &self,
        name: &str,
        input: &Value,
        idempotency_key: Option<String>,
    ) -> Result<String> {
        let tool = self
            .get(name)
            .with_context(|| format!("no tool named {name}"))?;
        match &tool.imp {
            ToolImpl::Python { path } => {
                beater_py::call_tool(path.clone(), input.to_string()).await
            }
            ToolImpl::RustBuiltin => execute_builtin(name, input),
            ToolImpl::Sandbox(sandbox) => {
                execute_sandbox(
                    &sandbox.beatbox,
                    sandbox.lane.clone(),
                    sandbox.source.clone(),
                    sandbox.policy.clone(),
                    sandbox.entrypoint.clone(),
                    input.clone(),
                    idempotency_key,
                )
                .await
            }
        }
    }
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
    let path = agent_dir.join(path.trim_start_matches("./"));
    let agent_dir = agent_dir
        .canonicalize()
        .with_context(|| format!("canonicalizing agent dir {}", agent_dir.display()))?;
    let path = path
        .canonicalize()
        .with_context(|| format!("canonicalizing sandbox source {}", path.display()))?;
    if !path.starts_with(&agent_dir) {
        bail!(
            "sandbox source {} escapes agent directory {}",
            path.display(),
            agent_dir.display()
        );
    }
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
    use std::collections::VecDeque;
    use std::fs;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};
    use std::thread;

    use serde_json::{Value, json};

    use super::{BeatboxConfig, SandboxSourceDecl, ToolDecl, ToolImpl, ToolRegistry};

    fn python_tool_decl(name: &str, path: &str, idempotent: bool) -> ToolDecl {
        ToolDecl {
            kind: "python".to_string(),
            name: name.to_string(),
            path: Some(path.to_string()),
            idempotent,
            lane: None,
            source: None,
            policy: None,
            entrypoint: None,
            description: None,
            input_schema: None,
        }
    }

    fn sandbox_tool_decl(name: &str, path: &str, idempotent: bool) -> ToolDecl {
        ToolDecl {
            kind: "sandbox".to_string(),
            name: name.to_string(),
            path: Some(path.to_string()),
            idempotent,
            lane: None,
            source: None,
            policy: None,
            entrypoint: None,
            description: Some("Run a tiny wasm fib fixture.".to_string()),
            input_schema: Some(json!({
                "type": "object",
                "properties": {"n": {"type": "integer"}},
                "required": ["n"],
            })),
        }
    }

    #[test]
    fn hello_slow_fixture_tools_preserve_resume_contract() {
        let agent_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join("examples/hello/agents/support");
        let registry = ToolRegistry::build(
            &agent_dir,
            &[
                python_tool_decl("slow_summarize", "./tools/slow_summarize.py", true),
                python_tool_decl(
                    "slow_summarize_once",
                    "./tools/slow_summarize_once.py",
                    false,
                ),
            ],
            &BeatboxConfig::default(),
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

    #[tokio::test]
    async fn sandbox_tool_uses_jobs_when_idempotency_key_is_present() {
        let temp = TempAgent::new("sandbox-tool");
        let wat = r#"
(module
  (func $fib (param $n i64) (result i64)
    local.get $n
    i64.const 2
    i64.lt_s
    if (result i64)
      local.get $n
    else
      local.get $n
      i64.const 1
      i64.sub
      call $fib
      local.get $n
      i64.const 2
      i64.sub
      call $fib
      i64.add
    end)
  (export "run" (func $fib)))
"#;
        fs::write(temp.path.join("tools/fib.wat"), wat).unwrap();
        let server = MockBeatbox::new(vec![
            json!({"job_id": "job-1"}),
            job_record_json("job-1", execution_result_json(55)),
        ]);
        let beatbox = BeatboxConfig {
            url: server.base_url.clone(),
            api_key: Some("test-token".to_string()),
        };
        let registry = ToolRegistry::build(
            &temp.path,
            &[sandbox_tool_decl("fib_wasm", "./tools/fib.wat", true)],
            &beatbox,
        )
        .unwrap();

        let output = registry
            .execute(
                "fib_wasm",
                &json!({"n": 10}),
                Some("beater:run-1:tool:toolu_1".to_string()),
            )
            .await
            .unwrap();

        let output: Value = serde_json::from_str(&output).unwrap();
        assert_eq!(output["status"], "ok");
        assert_eq!(output["value"], 55);
        assert_eq!(output["deterministic"], true);

        let requests = server.join();
        assert_eq!(requests.len(), 2);
        let create = &requests[0];
        assert!(create.request_line.starts_with("POST /v1/jobs "));
        assert!(create.headers.contains("authorization: bearer test-token"));
        let body: Value = serde_json::from_str(&create.body).unwrap();
        assert_eq!(body["lane"], "wasm");
        assert_eq!(body["source"]["kind"], "wasm_wat");
        assert!(body["source"]["text"].as_str().unwrap().contains("$fib"));
        assert_eq!(body["input"]["n"], 10);
        assert_eq!(body["idempotency_key"], "beater:run-1:tool:toolu_1");

        let poll = &requests[1];
        assert!(poll.request_line.starts_with("GET /v1/jobs/job-1 "));
        assert!(poll.body.is_empty());
    }

    #[tokio::test]
    async fn sandbox_tool_without_idempotency_key_uses_sync_execute() {
        let temp = TempAgent::new("sandbox-sync");
        fs::write(temp.path.join("tools/fib.wat"), "(module)").unwrap();
        let server = MockBeatbox::new(vec![execution_result_json(1)]);
        let beatbox = BeatboxConfig {
            url: server.base_url.clone(),
            api_key: None,
        };
        let registry = ToolRegistry::build(
            &temp.path,
            &[sandbox_tool_decl("fib_wasm", "./tools/fib.wat", true)],
            &beatbox,
        )
        .unwrap();

        let output = registry
            .execute("fib_wasm", &json!({"n": 1}), None)
            .await
            .unwrap();
        let output: Value = serde_json::from_str(&output).unwrap();
        assert_eq!(output["value"], 1);

        let requests = server.join();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].request_line.starts_with("POST /v1/execute "));
    }

    #[test]
    fn sandbox_source_path_must_stay_inside_agent_dir() {
        let temp = TempAgent::new("sandbox-escape");
        let outside = temp.path.parent().unwrap().join("outside.wat");
        fs::write(&outside, "(module)").unwrap();
        let escaped = format!("../{}", outside.file_name().unwrap().to_string_lossy());
        let err = match ToolRegistry::build(
            &temp.path,
            &[sandbox_tool_decl("escape", &escaped, true)],
            &BeatboxConfig::default(),
        ) {
            Ok(_) => panic!("escaped sandbox source should be rejected"),
            Err(err) => err.to_string(),
        };
        assert!(err.contains("loading sandbox source for escape"), "{err}");
    }

    #[test]
    fn sandbox_policy_accepts_partial_limit_overrides() {
        let temp = TempAgent::new("sandbox-policy");
        fs::write(temp.path.join("tools/fib.wat"), "(module)").unwrap();
        let mut decl = sandbox_tool_decl("fib_wasm", "./tools/fib.wat", true);
        decl.policy = Some(json!({"limits": {"wall_ms": 1234}}));

        let registry = ToolRegistry::build(&temp.path, &[decl], &BeatboxConfig::default()).unwrap();
        let tool = registry.get("fib_wasm").unwrap();
        let ToolImpl::Sandbox(sandbox) = &tool.imp else {
            panic!("expected sandbox tool");
        };
        assert_eq!(sandbox.policy.limits.wall_ms, 1234);
        assert_eq!(
            sandbox.policy.limits.cpu_ms,
            beatbox_client::Policy::default().limits.cpu_ms
        );
    }

    #[test]
    fn sandbox_policy_rejects_unknown_fields() {
        let temp = TempAgent::new("sandbox-policy-unknown");
        fs::write(temp.path.join("tools/fib.wat"), "(module)").unwrap();
        let mut decl = sandbox_tool_decl("fib_wasm", "./tools/fib.wat", true);
        decl.policy = Some(json!({"limits": {"wall_ms": 1234, "wall_mss": 1}}));

        let err = match ToolRegistry::build(&temp.path, &[decl], &BeatboxConfig::default()) {
            Ok(_) => panic!("unknown policy field should be rejected"),
            Err(err) => err.to_string(),
        };
        assert!(err.contains("parsing sandbox policy for fib_wasm"), "{err}");
    }

    #[test]
    fn sandbox_source_rejects_path_and_source_together() {
        let temp = TempAgent::new("sandbox-source-ambiguous");
        fs::write(temp.path.join("tools/fib.wat"), "(module)").unwrap();
        let mut decl = sandbox_tool_decl("fib_wasm", "./tools/fib.wat", true);
        decl.source = Some(SandboxSourceDecl::Wat {
            text: "(module)".to_string(),
        });

        let err = match ToolRegistry::build(&temp.path, &[decl], &BeatboxConfig::default()) {
            Ok(_) => panic!("ambiguous sandbox source should be rejected"),
            Err(err) => err.to_string(),
        };
        assert!(err.contains("loading sandbox source for fib_wasm"), "{err}");
    }

    #[test]
    fn sandbox_source_rejects_module_ref_until_beatbox_supports_it() {
        let temp = TempAgent::new("sandbox-source-module-ref");
        let mut decl = sandbox_tool_decl("fib_wasm", "", true);
        decl.path = None;
        decl.source = Some(SandboxSourceDecl::ModuleRef {
            sha256: "sha256:test".to_string(),
        });

        let err = match ToolRegistry::build(&temp.path, &[decl], &BeatboxConfig::default()) {
            Ok(_) => panic!("module_ref should be rejected during M3"),
            Err(err) => err.to_string(),
        };
        assert!(err.contains("loading sandbox source for fib_wasm"), "{err}");
    }

    fn execution_result_json(value: i64) -> Value {
        json!({
            "status": "ok",
            "value": value,
            "exit_code": null,
            "stdout": "",
            "stdout_truncated": false,
            "stderr": "",
            "stderr_truncated": false,
            "error": null,
            "metrics": {
                "wall_time_ms": 1,
                "cpu_time_ms": 1,
                "fuel_used": 42,
                "peak_memory_bytes": null,
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
                "downgrades": [],
            },
            "egress": [],
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
                "idempotency_key": "beater:run-1:tool:toolu_1",
            },
            "result": result,
            "error": null,
            "created_at": "2026-07-02T00:00:00Z",
            "updated_at": "2026-07-02T00:00:00Z",
        })
    }

    struct TempAgent {
        path: PathBuf,
    }

    impl TempAgent {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "beater-registry-{name}-{}-{}",
                std::process::id(),
                chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
            ));
            fs::create_dir_all(path.join("tools")).unwrap();
            Self { path }
        }
    }

    impl Drop for TempAgent {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    #[derive(Debug)]
    struct CapturedRequest {
        request_line: String,
        headers: String,
        body: String,
    }

    struct MockBeatbox {
        base_url: String,
        requests: Arc<Mutex<Vec<CapturedRequest>>>,
        handle: Option<thread::JoinHandle<()>>,
    }

    impl MockBeatbox {
        fn new(responses: Vec<Value>) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();
            let requests = Arc::new(Mutex::new(Vec::new()));
            let server_requests = Arc::clone(&requests);
            let mut responses: VecDeque<String> = responses
                .into_iter()
                .map(|value| value.to_string())
                .collect();
            let handle = thread::spawn(move || {
                while let Some(response) = responses.pop_front() {
                    let (mut stream, _) = listener.accept().unwrap();
                    let request = read_http_request(&mut stream);
                    server_requests.lock().unwrap().push(request);
                    let reply = format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                        response.len(),
                        response
                    );
                    stream.write_all(reply.as_bytes()).unwrap();
                }
            });
            Self {
                base_url: format!("http://{addr}"),
                requests,
                handle: Some(handle),
            }
        }

        fn join(mut self) -> Vec<CapturedRequest> {
            if let Some(handle) = self.handle.take() {
                handle.join().unwrap();
            }
            Arc::try_unwrap(self.requests)
                .unwrap()
                .into_inner()
                .unwrap()
        }
    }

    fn read_http_request(stream: &mut std::net::TcpStream) -> CapturedRequest {
        let mut bytes = Vec::new();
        let mut buf = [0_u8; 1024];
        let mut headers_end = None;
        let mut content_len = None;
        loop {
            let n = stream.read(&mut buf).unwrap();
            assert_ne!(n, 0, "client closed before sending a complete request");
            bytes.extend_from_slice(&buf[..n]);
            if headers_end.is_none() {
                headers_end = bytes.windows(4).position(|window| window == b"\r\n\r\n");
                if let Some(end) = headers_end {
                    let headers = String::from_utf8_lossy(&bytes[..end]);
                    content_len = headers.lines().find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().unwrap())
                    });
                }
            }
            if let Some(end) = headers_end
                && content_len.is_none()
            {
                return CapturedRequest {
                    request_line: String::from_utf8_lossy(&bytes[..end])
                        .lines()
                        .next()
                        .unwrap_or_default()
                        .to_string(),
                    headers: String::from_utf8(bytes[..end].to_vec())
                        .unwrap()
                        .to_ascii_lowercase(),
                    body: String::new(),
                };
            }
            if let (Some(end), Some(len)) = (headers_end, content_len) {
                let body_start = end + 4;
                if bytes.len() >= body_start + len {
                    return CapturedRequest {
                        request_line: String::from_utf8_lossy(&bytes[..end])
                            .lines()
                            .next()
                            .unwrap_or_default()
                            .to_string(),
                        headers: String::from_utf8(bytes[..end].to_vec())
                            .unwrap()
                            .to_ascii_lowercase(),
                        body: String::from_utf8(bytes[body_start..body_start + len].to_vec())
                            .unwrap(),
                    };
                }
            }
        }
    }
}
