//! The agent loop. Lives in Rust — not the JS isolate — so it survives hot
//! reloads and every LLM/tool step is journaled before it executes.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use crate::anthropic::Anthropic;
use crate::journal::Journal;
use crate::registry::{AgentConfig, BeatboxConfig, ToolCallContext, ToolNeedsReview, ToolRegistry};

const MAX_TOKENS: u64 = 16000;
const MAX_LOOP_STEPS: usize = 50;

struct Ctx {
    journal: Journal,
    client: Anthropic,
    registry: ToolRegistry,
    config: AgentConfig,
    run_id: String,
}

fn runtime() -> Result<tokio::runtime::Runtime> {
    Ok(tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?)
}

fn setup(
    app_dir: &Path,
    config_value: Value,
    venv: Option<&PathBuf>,
    beatbox: &BeatboxConfig,
) -> Result<(AgentConfig, ToolRegistry)> {
    if let Some(venv) = venv {
        if venv.is_dir() {
            beater_py::attach_venv(venv)?;
        } else {
            tracing::info!("no venv at {} — stdlib-only python tools", venv.display());
        }
    }
    let config: AgentConfig = serde_json::from_value(config_value)
        .context("agent.ts default export did not match defineAgent shape")?;
    let agent_dir = app_dir.join("agents").join(&config.name);
    let registry = ToolRegistry::build_with_beatbox(&agent_dir, &config.tools, beatbox)?;
    Ok((config, registry))
}

pub fn run(
    app_dir: &Path,
    agent_name: &str,
    config_value: Value,
    venv: Option<PathBuf>,
    beatbox: BeatboxConfig,
    prompt: &str,
) -> Result<()> {
    let (config, registry) = setup(app_dir, config_value, venv.as_ref(), &beatbox)?;
    anyhow::ensure!(
        config.name == agent_name,
        "agent.ts declares name {:?} but directory is {agent_name:?}",
        config.name
    );
    let client = Anthropic::from_env()?;
    let journal = Journal::open(app_dir)?;
    let run_id = uuid::Uuid::new_v4().to_string();
    journal.create_run(&run_id, agent_name, prompt)?;
    println!("run {run_id}");

    let ctx = Ctx {
        journal,
        client,
        registry,
        config,
        run_id,
    };
    let messages = vec![json!({"role": "user", "content": prompt})];
    runtime()?.block_on(agent_loop(&ctx, messages))
}

pub fn resume(
    app_dir: &Path,
    run_id: &str,
    venv: Option<PathBuf>,
    beatbox: BeatboxConfig,
    load_config: impl Fn(&str) -> Result<Value>,
) -> Result<()> {
    let journal = Journal::open(app_dir)?;
    let run = journal.run(run_id)?;
    if run.status == "completed" {
        println!("run {run_id} already completed");
        return Ok(());
    }
    let config_value = load_config(&run.agent)?;
    let (config, registry) = setup(app_dir, config_value, venv.as_ref(), &beatbox)?;
    let steps = journal.steps(run_id)?;

    let ctx = Ctx {
        journal,
        client: Anthropic::from_env()?,
        registry,
        config,
        run_id: run_id.to_string(),
    };
    runtime()?.block_on(resume_async(&ctx, run, steps))
}

async fn resume_async(
    ctx: &Ctx,
    run: crate::journal::RunRow,
    steps: Vec<crate::journal::StepRow>,
) -> Result<()> {
    let run_id = ctx.run_id.as_str();
    // Rebuild conversation state from the journal. The last llm_call's request
    // body carries the exact messages[] at that point — no delta replay needed.
    let last_llm = steps.iter().rev().find(|s| s.kind == "llm_call");
    let messages = match last_llm {
        None => vec![json!({"role": "user", "content": run.input})],
        Some(step) if step.status != "completed" => {
            // Dangling LLM call: we own the request and it had no observable
            // side effect on our state — always safe to re-issue.
            println!(
                "resuming: re-issuing interrupted LLM call (attempt {})",
                step.attempt + 1
            );
            step.request["messages"]
                .as_array()
                .context("journaled llm_call request has no messages")?
                .clone()
        }
        Some(step) => {
            let response = step.result.as_ref().context("completed step has result")?;
            let content = response["content"].clone();
            let mut messages = step.request["messages"]
                .as_array()
                .context("journaled llm_call request has no messages")?
                .clone();
            messages.push(json!({"role": "assistant", "content": content}));
            let stop_reason = response["stop_reason"].as_str().unwrap_or_default();

            if stop_reason == "end_turn" {
                // The last response needed no tools; the run actually finished.
                ctx.journal.set_run_status(run_id, "completed")?;
                println!("run {run_id} was already finished — marked completed");
                return Ok(());
            }
            if stop_reason == "pause_turn" {
                // Server-side pause: assistant turn is already appended; ask
                // the model to continue from exactly the journaled state.
                messages
            } else if stop_reason != "tool_use" {
                ctx.journal.set_run_status(run_id, "failed")?;
                if stop_reason == "refusal" {
                    bail!(
                        "run {run_id} failed before resume: model refused: {}",
                        response["stop_details"]
                    );
                }
                bail!(
                    "run {run_id} failed before resume: unexpected stop_reason {stop_reason:?} — raise max_tokens or inspect the journal"
                );
            } else {
                let tool_uses: Vec<Value> = content
                    .as_array()
                    .map(|blocks| {
                        blocks
                            .iter()
                            .filter(|b| b["type"] == "tool_use")
                            .cloned()
                            .collect()
                    })
                    .unwrap_or_default();
                if tool_uses.is_empty() {
                    ctx.journal.set_run_status(run_id, "failed")?;
                    bail!(
                        "run {run_id} failed before resume: stop_reason \"tool_use\" had no tool_use blocks"
                    );
                }

                // Fill in tool results: journaled ones verbatim; dangling ones
                // re-run ONLY if the tool is declared idempotent (§5 rule 4).
                let mut tool_results = Vec::new();
                for tu in &tool_uses {
                    let (id, name) = (
                        tu["id"].as_str().unwrap_or_default(),
                        tu["name"].as_str().unwrap_or_default(),
                    );
                    let done = steps.iter().find(|s| {
                        s.kind == "tool_call"
                            && s.status == "completed"
                            && s.tool_use_id.as_deref() == Some(id)
                    });
                    let content = match done {
                        Some(s) => s
                            .result
                            .as_ref()
                            .and_then(|r| r["content"].as_str())
                            .unwrap_or_default()
                            .to_string(),
                        None => {
                            let tool = ctx
                                .registry
                                .get(name)
                                .with_context(|| format!("no tool {name}"))?;
                            if !tool.idempotent {
                                ctx.journal.set_run_status(run_id, "needs_review")?;
                                println!(
                                    "run {run_id} needs review: tool {name} ({id}) may have executed \
                                     before the crash and is not declared idempotent — not re-running"
                                );
                                return Ok(());
                            }
                            let prior_attempts = steps
                                .iter()
                                .filter(|s| s.tool_use_id.as_deref() == Some(id))
                                .map(|s| s.attempt)
                                .max()
                                .unwrap_or(0);
                            println!(
                                "resuming: re-running interrupted tool {name} (attempt {})",
                                prior_attempts + 1
                            );
                            execute_tool_step(ctx, name, id, &tu["input"], prior_attempts + 1)
                                .await?
                        }
                    };
                    tool_results.push(
                        json!({"type": "tool_result", "tool_use_id": id, "content": content}),
                    );
                }
                messages.push(json!({"role": "user", "content": tool_results}));
                messages
            }
        }
    };

    ctx.journal.set_run_status(run_id, "running")?;
    agent_loop(ctx, messages).await
}

pub fn list_runs(app_dir: &Path) -> Result<()> {
    let journal = Journal::open(app_dir)?;
    let runs = journal.list_runs()?;
    if runs.is_empty() {
        println!("no runs");
        return Ok(());
    }
    println!(
        "{:<38} {:<12} {:<13} {:>5}  input",
        "run", "agent", "status", "steps"
    );
    for (run, steps) in runs {
        let input: String = run.input.chars().take(40).collect();
        println!(
            "{:<38} {:<12} {:<13} {:>5}  {input}",
            run.id, run.agent, run.status, steps
        );
    }
    Ok(())
}

async fn agent_loop(ctx: &Ctx, mut messages: Vec<Value>) -> Result<()> {
    for _ in 0..MAX_LOOP_STEPS {
        let body = json!({
            "model": ctx.config.model,
            "max_tokens": MAX_TOKENS,
            "system": ctx.config.system,
            "thinking": {"type": "adaptive"},
            "tools": ctx.registry.api_tools(),
            "messages": messages,
        });

        let seq = ctx
            .journal
            .start_step(&ctx.run_id, "llm_call", &body, None, None, 1)?;
        let response = match ctx.client.create_message(&body).await {
            Ok(r) => r,
            Err(e) => {
                ctx.journal.fail_step(&ctx.run_id, seq, &format!("{e:#}"))?;
                ctx.journal.set_run_status(&ctx.run_id, "failed")?;
                return Err(e);
            }
        };
        ctx.journal.complete_step(&ctx.run_id, seq, &response)?;

        let content = response["content"].clone();
        for block in content.as_array().into_iter().flatten() {
            if block["type"] == "text" {
                println!("{}", block["text"].as_str().unwrap_or_default());
            }
        }
        messages.push(json!({"role": "assistant", "content": content}));

        match response["stop_reason"].as_str().unwrap_or_default() {
            "tool_use" => {
                let mut tool_results = Vec::new();
                for block in content.as_array().into_iter().flatten() {
                    if block["type"] != "tool_use" {
                        continue;
                    }
                    let id = block["id"].as_str().unwrap_or_default();
                    let name = block["name"].as_str().unwrap_or_default();
                    println!("→ tool {name} {}", block["input"]);
                    let result = execute_tool_step(ctx, name, id, &block["input"], 1).await;
                    match result {
                        Ok(content) => {
                            println!("← {content}");
                            tool_results.push(json!({
                                "type": "tool_result", "tool_use_id": id, "content": content,
                            }));
                        }
                        Err(e) if e.downcast_ref::<ToolNeedsReview>().is_some() => {
                            println!("← needs review: {e:#}");
                            ctx.journal.set_run_status(&ctx.run_id, "needs_review")?;
                            return Ok(());
                        }
                        Err(e) => {
                            println!("← tool error: {e:#}");
                            tool_results.push(json!({
                                "type": "tool_result", "tool_use_id": id,
                                "content": format!("Error: {e:#}"), "is_error": true,
                            }));
                        }
                    }
                }
                messages.push(json!({"role": "user", "content": tool_results}));
            }
            "end_turn" => {
                ctx.journal.set_run_status(&ctx.run_id, "completed")?;
                return Ok(());
            }
            // server-side pause: assistant turn is already appended; re-send as-is
            "pause_turn" => continue,
            "refusal" => {
                ctx.journal.set_run_status(&ctx.run_id, "failed")?;
                bail!("model refused: {}", response["stop_details"]);
            }
            other => {
                ctx.journal.set_run_status(&ctx.run_id, "failed")?;
                bail!("unexpected stop_reason {other:?} — raise max_tokens or inspect the journal");
            }
        }
    }
    ctx.journal.set_run_status(&ctx.run_id, "failed")?;
    bail!("agent exceeded {MAX_LOOP_STEPS} loop steps")
}

/// Journal-wrapped tool execution: started row committed before the tool runs.
async fn execute_tool_step(
    ctx: &Ctx,
    name: &str,
    tool_use_id: &str,
    input: &Value,
    attempt: i64,
) -> Result<String> {
    let idempotency_key = tool_idempotency_key(&ctx.run_id, tool_use_id);
    let request = match &idempotency_key {
        Some(key) => json!({
            "name": name,
            "input": input,
            "tool_use_id": tool_use_id,
            "idempotency_key": key,
        }),
        None => json!({"name": name, "input": input, "tool_use_id": tool_use_id}),
    };
    let seq = ctx.journal.start_step(
        &ctx.run_id,
        "tool_call",
        &request,
        Some(name),
        Some(tool_use_id),
        attempt,
    )?;
    let tool_context = ToolCallContext {
        tool_use_id: Some(tool_use_id.to_string()),
        idempotency_key,
    };
    match ctx
        .registry
        .execute_with_context(name, input, &tool_context)
        .await
    {
        Ok(result) => {
            ctx.journal
                .complete_step(&ctx.run_id, seq, &json!({"content": result}))?;
            Ok(result)
        }
        Err(e) => {
            ctx.journal.fail_step(&ctx.run_id, seq, &format!("{e:#}"))?;
            Err(e)
        }
    }
}

fn tool_idempotency_key(run_id: &str, tool_use_id: &str) -> Option<String> {
    (!tool_use_id.is_empty()).then(|| format!("beater:{run_id}:tool:{tool_use_id}"))
}

#[cfg(test)]
mod tests {
    use super::{resume, run, tool_idempotency_key};
    use crate::journal::Journal;
    use crate::registry::BeatboxConfig;
    use serde_json::{Value, json};
    use std::collections::VecDeque;
    use std::fs;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};
    use std::thread;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct TempApp {
        path: PathBuf,
    }

    impl TempApp {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "beater-runner-{name}-{}-{}",
                std::process::id(),
                chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
            ));
            fs::create_dir_all(path.join("agents/support/tools")).unwrap();
            fs::write(
                path.join("agents/support/tools/echo.py"),
                r#"
TOOL = {
    "description": "Echo a value.",
    "input_schema": {
        "type": "object",
        "properties": {"value": {"type": "string"}},
        "required": ["value"],
    },
}

def run(input):
    return {"echo": input["value"]}
"#,
            )
            .unwrap();
            fs::write(
                path.join("agents/support/tools/fib.wat"),
                r#"
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
"#,
            )
            .unwrap();
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempApp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    struct EnvGuard;

    impl EnvGuard {
        fn set(base_url: &str) -> Self {
            unsafe {
                std::env::set_var("ANTHROPIC_API_KEY", "test-key");
                std::env::set_var("ANTHROPIC_BASE_URL", base_url);
            }
            Self
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            unsafe {
                std::env::remove_var("ANTHROPIC_API_KEY");
                std::env::remove_var("ANTHROPIC_BASE_URL");
            }
        }
    }

    struct MockAnthropic {
        base_url: String,
        requests: Arc<Mutex<Vec<String>>>,
        handle: Option<thread::JoinHandle<()>>,
    }

    #[derive(Debug)]
    struct CapturedRequest {
        request_line: String,
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

    impl MockAnthropic {
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
                    let body = read_http_body(&mut stream);
                    server_requests.lock().unwrap().push(body);
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

        fn join(mut self) -> Vec<String> {
            if let Some(handle) = self.handle.take() {
                handle.join().unwrap();
            }
            Arc::try_unwrap(self.requests)
                .unwrap()
                .into_inner()
                .unwrap()
        }
    }

    fn read_http_body(stream: &mut std::net::TcpStream) -> String {
        read_http_request(stream).body
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
                        body: String::from_utf8(bytes[body_start..body_start + len].to_vec())
                            .unwrap(),
                    };
                }
            }
        }
    }

    fn config(idempotent: bool) -> Value {
        json!({
            "name": "support",
            "model": "mock",
            "system": "test",
            "tools": [{
                "kind": "python",
                "name": "echo",
                "path": "./tools/echo.py",
                "idempotent": idempotent,
            }],
        })
    }

    fn browser_config() -> Value {
        json!({
            "name": "support",
            "model": "mock",
            "system": "test",
            "tools": [{
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
            }],
        })
    }

    fn sandbox_config(idempotent: bool) -> Value {
        json!({
            "name": "support",
            "model": "mock",
            "system": "test",
            "tools": [{
                "kind": "sandbox",
                "name": "fib_wasm",
                "path": "./tools/fib.wat",
                "idempotent": idempotent,
                "description": "Run fib in beatbox.",
                "inputSchema": {
                    "type": "object",
                    "properties": {"n": {"type": "integer"}},
                    "required": ["n"],
                },
            }],
        })
    }

    fn seed_interrupted_tool_run(app: &TempApp) {
        seed_interrupted_tool_run_for(app, "echo", json!({"value": "ok"}));
    }

    fn seed_interrupted_tool_run_for(app: &TempApp, name: &str, input: Value) {
        let journal = Journal::open(app.path()).unwrap();
        journal
            .create_run("run-1", "support", &format!("call {name}"))
            .unwrap();
        let request = json!({
            "messages": [{"role": "user", "content": format!("call {name}")}],
        });
        let response = json!({
            "content": [{
                "type": "tool_use",
                "id": "toolu_1",
                "name": name,
                "input": input,
            }],
            "stop_reason": "tool_use",
        });
        let llm = journal
            .start_step("run-1", "llm_call", &request, None, None, 1)
            .unwrap();
        journal.complete_step("run-1", llm, &response).unwrap();
        journal
            .start_step(
                "run-1",
                "tool_call",
                &json!({"name": name, "input": input, "tool_use_id": "toolu_1"}),
                Some(name),
                Some("toolu_1"),
                1,
            )
            .unwrap();
    }

    fn seed_completed_llm_run(app: &TempApp, status: &str, response: Value) {
        let journal = Journal::open(app.path()).unwrap();
        journal
            .create_run("run-1", "support", "continue previous response")
            .unwrap();
        let llm = journal
            .start_step(
                "run-1",
                "llm_call",
                &json!({
                    "messages": [{
                        "role": "user",
                        "content": "continue previous response",
                    }],
                }),
                None,
                None,
                1,
            )
            .unwrap();
        journal.complete_step("run-1", llm, &response).unwrap();
        journal.set_run_status("run-1", status).unwrap();
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

    #[test]
    fn resume_reruns_interrupted_idempotent_tool_once_and_finishes() {
        let _env_lock = ENV_LOCK.lock().unwrap();
        let app = TempApp::new("idempotent");
        seed_interrupted_tool_run(&app);
        let server = MockAnthropic::new(vec![json!({
            "content": [{"type": "text", "text": "done"}],
            "stop_reason": "end_turn",
        })]);
        let _env = EnvGuard::set(&server.base_url);

        resume(app.path(), "run-1", None, BeatboxConfig::default(), |_| {
            Ok(config(true))
        })
        .unwrap();
        let requests = server.join();

        assert_eq!(requests.len(), 1);
        let body: Value = serde_json::from_str(&requests[0]).unwrap();
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.last().unwrap()["role"], "user");
        assert_eq!(
            messages.last().unwrap()["content"][0]["tool_use_id"],
            "toolu_1"
        );

        let journal = Journal::open(app.path()).unwrap();
        assert_eq!(journal.run("run-1").unwrap().status, "completed");
        let steps = journal.steps("run-1").unwrap();
        let tool_steps: Vec<_> = steps
            .iter()
            .filter(|step| {
                step.kind == "tool_call" && step.tool_use_id.as_deref() == Some("toolu_1")
            })
            .collect();
        assert_eq!(tool_steps.len(), 2);
        assert_eq!(tool_steps[0].status, "started");
        assert_eq!(tool_steps[0].attempt, 1);
        assert_eq!(tool_steps[1].status, "completed");
        assert_eq!(tool_steps[1].attempt, 2);
        assert_eq!(
            tool_steps[1].request["idempotency_key"],
            "beater:run-1:tool:toolu_1"
        );
    }

    #[test]
    fn run_completes_browser_tool_through_agent_loop() {
        let _env_lock = ENV_LOCK.lock().unwrap();
        let app = TempApp::new("browser-tool");
        let server = MockAnthropic::new(vec![
            json!({
                "content": [{
                    "type": "tool_use",
                    "id": "toolu_browser",
                    "name": "browser.checkout",
                    "input": {
                        "url": "https://shop.example/cart",
                        "task": "verify checkout"
                    },
                }],
                "stop_reason": "tool_use",
            }),
            json!({
                "content": [{"type": "text", "text": "checkout verified"}],
                "stop_reason": "end_turn",
            }),
        ]);
        let _env = EnvGuard::set(&server.base_url);

        run(
            app.path(),
            "support",
            browser_config(),
            None,
            BeatboxConfig::default(),
            "verify checkout",
        )
        .unwrap();
        let requests = server.join();
        assert_eq!(requests.len(), 2);

        let body: Value = serde_json::from_str(&requests[1]).unwrap();
        let messages = body["messages"].as_array().unwrap();
        let tool_result = &messages.last().unwrap()["content"][0];
        assert_eq!(tool_result["tool_use_id"], "toolu_browser");
        assert!(
            tool_result["content"]
                .as_str()
                .unwrap()
                .contains("Mock Browser Page")
        );

        let journal = Journal::open(app.path()).unwrap();
        let (run, _) = journal.list_runs().unwrap().pop().unwrap();
        assert_eq!(run.status, "completed");
        let tool_step = journal
            .steps(&run.id)
            .unwrap()
            .into_iter()
            .find(|step| step.kind == "tool_call")
            .expect("browser tool call step");
        assert_eq!(tool_step.status, "completed");
        assert_eq!(tool_step.tool_use_id.as_deref(), Some("toolu_browser"));
    }

    #[test]
    fn resume_parks_interrupted_non_idempotent_tool_for_review() {
        let _env_lock = ENV_LOCK.lock().unwrap();
        let app = TempApp::new("non-idempotent");
        seed_interrupted_tool_run(&app);
        let _env = EnvGuard::set("http://127.0.0.1:9");

        resume(app.path(), "run-1", None, BeatboxConfig::default(), |_| {
            Ok(config(false))
        })
        .unwrap();

        let journal = Journal::open(app.path()).unwrap();
        assert_eq!(journal.run("run-1").unwrap().status, "needs_review");
        let tool_steps: Vec<_> = journal
            .steps("run-1")
            .unwrap()
            .into_iter()
            .filter(|step| step.kind == "tool_call")
            .collect();
        assert_eq!(tool_steps.len(), 1);
        assert_eq!(tool_steps[0].status, "started");
        assert_eq!(tool_steps[0].attempt, 1);
    }

    #[test]
    fn resume_preserves_failed_refusal_instead_of_marking_completed() {
        let _env_lock = ENV_LOCK.lock().unwrap();
        let app = TempApp::new("refusal-stop");
        seed_completed_llm_run(
            &app,
            "failed",
            json!({
                "content": [{"type": "text", "text": "I can't help with that."}],
                "stop_reason": "refusal",
                "stop_details": {"reason": "safety"},
            }),
        );
        let _env = EnvGuard::set("http://127.0.0.1:9");

        let err = resume(app.path(), "run-1", None, BeatboxConfig::default(), |_| {
            Ok(config(true))
        })
        .unwrap_err();

        assert!(format!("{err:#}").contains("model refused"));
        let journal = Journal::open(app.path()).unwrap();
        assert_eq!(journal.run("run-1").unwrap().status, "failed");
        assert_eq!(journal.steps("run-1").unwrap().len(), 1);
    }

    #[test]
    fn resume_marks_running_max_tokens_failed_and_does_not_run_truncated_tools() {
        let _env_lock = ENV_LOCK.lock().unwrap();
        let app = TempApp::new("max-tokens-stop");
        seed_completed_llm_run(
            &app,
            "running",
            json!({
                "content": [{
                    "type": "tool_use",
                    "id": "toolu_truncated",
                    "name": "echo",
                    "input": {"value": "possibly truncated"},
                }],
                "stop_reason": "max_tokens",
            }),
        );
        let _env = EnvGuard::set("http://127.0.0.1:9");

        let err = resume(app.path(), "run-1", None, BeatboxConfig::default(), |_| {
            Ok(config(true))
        })
        .unwrap_err();

        assert!(format!("{err:#}").contains("unexpected stop_reason \"max_tokens\""));
        let journal = Journal::open(app.path()).unwrap();
        assert_eq!(journal.run("run-1").unwrap().status, "failed");
        let steps = journal.steps("run-1").unwrap();
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].kind, "llm_call");
    }

    #[test]
    fn resume_marks_completed_end_turn_finished_without_reissuing_llm() {
        let _env_lock = ENV_LOCK.lock().unwrap();
        let app = TempApp::new("end-turn-stop");
        seed_completed_llm_run(
            &app,
            "running",
            json!({
                "content": [{"type": "text", "text": "already done"}],
                "stop_reason": "end_turn",
            }),
        );
        let _env = EnvGuard::set("http://127.0.0.1:9");

        resume(app.path(), "run-1", None, BeatboxConfig::default(), |_| {
            Ok(config(true))
        })
        .unwrap();

        let journal = Journal::open(app.path()).unwrap();
        assert_eq!(journal.run("run-1").unwrap().status, "completed");
        assert_eq!(journal.steps("run-1").unwrap().len(), 1);
    }

    #[test]
    fn resume_reissues_pause_turn_instead_of_marking_completed() {
        let _env_lock = ENV_LOCK.lock().unwrap();
        let app = TempApp::new("pause-turn-stop");
        seed_completed_llm_run(
            &app,
            "running",
            json!({
                "content": [{
                    "type": "server_tool_use",
                    "id": "srvu_1",
                    "name": "web_search",
                    "input": {"query": "beater"},
                }],
                "stop_reason": "pause_turn",
            }),
        );
        let server = MockAnthropic::new(vec![json!({
            "content": [{"type": "text", "text": "continued"}],
            "stop_reason": "end_turn",
        })]);
        let _env = EnvGuard::set(&server.base_url);

        resume(app.path(), "run-1", None, BeatboxConfig::default(), |_| {
            Ok(config(true))
        })
        .unwrap();
        let requests = server.join();

        assert_eq!(requests.len(), 1);
        let body: Value = serde_json::from_str(&requests[0]).unwrap();
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[1]["role"], "assistant");
        assert_eq!(
            messages[1]["content"],
            json!([{
                "type": "server_tool_use",
                "id": "srvu_1",
                "name": "web_search",
                "input": {"query": "beater"},
            }])
        );

        let journal = Journal::open(app.path()).unwrap();
        assert_eq!(journal.run("run-1").unwrap().status, "completed");
        assert_eq!(
            journal
                .steps("run-1")
                .unwrap()
                .iter()
                .filter(|step| step.kind == "llm_call")
                .count(),
            2
        );
    }

    #[test]
    fn resume_reruns_interrupted_idempotent_sandbox_tool_through_beatbox_job() {
        let _env_lock = ENV_LOCK.lock().unwrap();
        let app = TempApp::new("sandbox-idempotent");
        seed_interrupted_tool_run_for(&app, "fib_wasm", json!({"n": 10}));
        let anthropic = MockAnthropic::new(vec![json!({
            "content": [{"type": "text", "text": "done"}],
            "stop_reason": "end_turn",
        })]);
        let beatbox = MockBeatbox::new(vec![
            json!({"job_id": "job-1"}),
            job_record_json("job-1", execution_result_json(55)),
        ]);
        let beatbox_config = BeatboxConfig {
            url: beatbox.base_url.clone(),
            api_key: None,
        };
        let _env = EnvGuard::set(&anthropic.base_url);

        resume(app.path(), "run-1", None, beatbox_config, |_| {
            Ok(sandbox_config(true))
        })
        .unwrap();

        let beatbox_requests = beatbox.join();
        assert_eq!(beatbox_requests.len(), 2);
        assert!(
            beatbox_requests[0]
                .request_line
                .starts_with("POST /v1/jobs ")
        );
        let body: Value = serde_json::from_str(&beatbox_requests[0].body).unwrap();
        assert_eq!(body["idempotency_key"], "beater:run-1:tool:toolu_1");
        assert!(
            beatbox_requests[1]
                .request_line
                .starts_with("GET /v1/jobs/job-1 ")
        );

        let _anthropic_requests = anthropic.join();
        let journal = Journal::open(app.path()).unwrap();
        assert_eq!(journal.run("run-1").unwrap().status, "completed");
        let steps = journal.steps("run-1").unwrap();
        let completed = steps
            .iter()
            .find(|step| step.kind == "tool_call" && step.status == "completed")
            .expect("completed sandbox tool step");
        let content = completed.result.as_ref().unwrap()["content"]
            .as_str()
            .unwrap();
        let result: Value = serde_json::from_str(content).unwrap();
        assert_eq!(result["value"], 55);
    }

    #[test]
    fn resume_parks_interrupted_non_idempotent_sandbox_tool_for_review() {
        let _env_lock = ENV_LOCK.lock().unwrap();
        let app = TempApp::new("sandbox-non-idempotent");
        seed_interrupted_tool_run_for(&app, "fib_wasm", json!({"n": 10}));
        let _env = EnvGuard::set("http://127.0.0.1:9");

        resume(app.path(), "run-1", None, BeatboxConfig::default(), |_| {
            Ok(sandbox_config(false))
        })
        .unwrap();

        let journal = Journal::open(app.path()).unwrap();
        assert_eq!(journal.run("run-1").unwrap().status, "needs_review");
        let tool_steps: Vec<_> = journal
            .steps("run-1")
            .unwrap()
            .into_iter()
            .filter(|step| step.kind == "tool_call")
            .collect();
        assert_eq!(tool_steps.len(), 1);
        assert_eq!(tool_steps[0].status, "started");
    }

    #[test]
    fn idempotency_keys_are_stable_per_run_tool_use() {
        assert_eq!(
            tool_idempotency_key("run-1", "toolu_1").as_deref(),
            Some("beater:run-1:tool:toolu_1")
        );
        assert_eq!(tool_idempotency_key("run-1", ""), None);
    }
}
