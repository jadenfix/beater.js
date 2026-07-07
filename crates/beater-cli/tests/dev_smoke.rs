use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

struct TempDirGuard {
    path: PathBuf,
}

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

struct ChildGuard {
    child: Child,
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn dev_server_serves_routes_ssr_and_mcp_without_api_key() {
    let port = free_port();
    let workspace = workspace();
    let app = workspace.join("examples/hello");
    let beater = beater_bin(&workspace);
    let child = Command::new(beater)
        .arg("dev")
        .arg(&app)
        .arg("--host")
        .arg("127.0.0.1")
        .arg("--port")
        .arg(port.to_string())
        .env_remove("ANTHROPIC_API_KEY")
        .env_remove("BEATER_BASE_URL")
        .env_remove("BEATER_MCP_TOKEN")
        .env_remove("BEATER_MCP_TRUSTED_ORIGINS")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn beater dev");
    let _child = ChildGuard { child };

    let health = wait_for_http(port, "GET", "/api/health", None);
    assert!(health.starts_with("HTTP/1.1 200"), "{health}");
    assert!(health.contains("\"ok\":true"), "{health}");
    assert!(health.contains("\"runtime\":\"beater.js\""), "{health}");

    let head_health = http_request(port, "HEAD", "/api/health", None).expect("HEAD /api/health");
    assert!(head_health.starts_with("HTTP/1.1 200"), "{head_health}");
    assert!(!head_health.contains("\"ok\":true"), "{head_health}");

    let home = http_request(port, "GET", "/", None).expect("GET /");
    assert!(home.starts_with("HTTP/1.1 200"), "{home}");
    assert!(home.contains("content-type: text/html"), "{home}");
    assert!(
        home.contains("<h1 class=\"brand-title\">beater.js</h1>"),
        "{home}"
    );
    assert!(
        home.contains("Build the web UI and the agent loop in one place."),
        "{home}"
    );
    assert!(
        home.contains("content-security-policy: default-src 'self'"),
        "{home}"
    );
    assert!(home.contains("script-src 'self'"), "{home}");
    assert!(home.contains("x-content-type-options: nosniff"), "{home}");
    assert!(
        home.contains(r#"<script type="module" src="/_beater/client/index.js"></script>"#),
        "{home}"
    );
    assert!(home.contains("data-beater-counter"), "{home}");
    assert!(home.contains("data-beater-run-events"), "{home}");
    assert!(home.contains("data-run-history"), "{home}");
    assert!(home.contains("data-beater-action-form"), "{home}");

    let client =
        http_request(port, "GET", "/_beater/client/index.js", None).expect("GET client module");
    assert!(client.starts_with("HTTP/1.1 200"), "{client}");
    assert!(
        client.contains("content-type: application/javascript"),
        "{client}"
    );
    assert!(
        client.contains("root.dataset.state = \"hydrated\""),
        "{client}"
    );
    assert!(
        client.contains("root.dataset.bundle = clientBundleMarker"),
        "{client}"
    );
    assert!(
        client.contains("/_beater/client/index.js?dep=1"),
        "{client}"
    );
    assert!(client.contains("new EventSource"), "{client}");
    assert!(client.contains("fetch(\"/_beater/agent/runs\""), "{client}");
    assert!(!client.contains(": number"), "{client}");

    let client_dep = http_request(port, "GET", "/_beater/client/index.js?dep=1", None)
        .expect("GET client dependency module");
    assert!(client_dep.starts_with("HTTP/1.1 200"), "{client_dep}");
    assert!(
        client_dep.contains("content-type: application/javascript"),
        "{client_dep}"
    );
    assert!(client_dep.contains("client-helper-bundled"), "{client_dep}");
    assert!(!client_dep.contains(": number"), "{client_dep}");

    let runs = http_request(port, "GET", "/_beater/agent/runs", None).expect("GET run history");
    assert!(runs.starts_with("HTTP/1.1 200"), "{runs}");
    assert!(runs.contains("\"runs\":["), "{runs}");

    let missing_run =
        http_request(port, "GET", "/_beater/agent/runs/not-a-run", None).expect("GET missing run");
    assert!(missing_run.starts_with("HTTP/1.1 404"), "{missing_run}");

    let missing = http_request(port, "GET", "/not-a-route", None).expect("GET /not-a-route");
    assert!(missing.starts_with("HTTP/1.1 404"), "{missing}");
    assert!(
        missing.contains("content-security-policy: default-src 'self'"),
        "{missing}"
    );
    assert!(
        missing.contains("x-content-type-options: nosniff"),
        "{missing}"
    );

    let init = http_request(
        port,
        "POST",
        "/mcp",
        Some(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#),
    )
    .expect("POST /mcp initialize");
    assert!(init.starts_with("HTTP/1.1 200"), "{init}");
    assert!(
        init.contains("\"protocolVersion\":\"2025-11-25\""),
        "{init}"
    );
    assert!(init.contains("\"prompts\":{}"), "{init}");

    let tools_call = http_request(
        port,
        "POST",
        "/mcp",
        Some(
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"get_time","arguments":{}}}"#,
        ),
    )
    .expect("POST /mcp tools/call");
    assert!(tools_call.starts_with("HTTP/1.1 200"), "{tools_call}");
    assert!(tools_call.contains("\"isError\":false"), "{tools_call}");
    assert!(tools_call.contains("\\\"unix\\\""), "{tools_call}");

    let route_action_form = http_request_with_headers(
        port,
        "POST",
        "/api/actions/contact",
        &[("content-type", "application/x-www-form-urlencoded")],
        Some("email=agent%40example.test&message=hello&confirm=true&idempotency_key=form-1"),
    )
    .expect("POST route action form");
    assert!(
        route_action_form.starts_with("HTTP/1.1 200"),
        "{route_action_form}"
    );
    assert!(route_action_form.contains("\"action\":\"hello.contact\""));
    assert!(route_action_form.contains("\"email\":\"agent@example.test\""));
    assert!(route_action_form.contains("\"idempotency_key\":\"form-1\""));

    let tools_list = http_request(
        port,
        "POST",
        "/mcp",
        Some(r#"{"jsonrpc":"2.0","id":3,"method":"tools/list","params":{}}"#),
    )
    .expect("POST /mcp tools/list");
    assert!(tools_list.starts_with("HTTP/1.1 200"), "{tools_list}");
    assert!(
        tools_list.contains("\"name\":\"hello.contact\""),
        "{tools_list}"
    );
    assert!(
        tools_list.contains("\"idempotencyRequired\":true"),
        "{tools_list}"
    );

    let resources_list = http_request(
        port,
        "POST",
        "/mcp",
        Some(r#"{"jsonrpc":"2.0","id":5,"method":"resources/list","params":{}}"#),
    )
    .expect("POST /mcp resources/list");
    assert!(
        resources_list.starts_with("HTTP/1.1 200"),
        "{resources_list}"
    );
    assert!(
        resources_list.contains("\"uri\":\"beater://routes\""),
        "{resources_list}"
    );
    assert!(
        resources_list.contains("\"mimeType\":\"text/markdown\""),
        "{resources_list}"
    );

    let resources_read = http_request(
        port,
        "POST",
        "/mcp",
        Some(
            r#"{"jsonrpc":"2.0","id":6,"method":"resources/read","params":{"uri":"beater://routes"}}"#,
        ),
    )
    .expect("POST /mcp resources/read");
    assert!(
        resources_read.starts_with("HTTP/1.1 200"),
        "{resources_read}"
    );
    assert!(
        resources_read.contains("# beater.js route table"),
        "{resources_read}"
    );
    assert!(resources_read.contains("/api/health"), "{resources_read}");
    assert!(!resources_read.contains("/api/boom"), "{resources_read}");
    assert!(
        resources_read.contains("app/routes/index.tsx"),
        "{resources_read}"
    );
    assert!(resources_read.contains("hello.contact"), "{resources_read}");

    let prompts_list = http_request(
        port,
        "POST",
        "/mcp",
        Some(r#"{"jsonrpc":"2.0","id":7,"method":"prompts/list","params":{}}"#),
    )
    .expect("POST /mcp prompts/list");
    assert!(prompts_list.starts_with("HTTP/1.1 200"), "{prompts_list}");
    assert!(
        prompts_list.contains("\"name\":\"beater.review_pr\""),
        "{prompts_list}"
    );
    assert!(
        prompts_list.contains("\"name\":\"beater.update_docs\""),
        "{prompts_list}"
    );
    assert!(
        prompts_list.contains("\"name\":\"beater.systems_design\""),
        "{prompts_list}"
    );
    assert!(
        prompts_list.contains("\"name\":\"beater.choose_stack\""),
        "{prompts_list}"
    );

    let prompts_get = http_request(
        port,
        "POST",
        "/mcp",
        Some(
            r#"{"jsonrpc":"2.0","id":8,"method":"prompts/get","params":{"name":"beater.systems_design","arguments":{"problem":"route action reliability","constraints":"bounded replay and idempotency"}}}"#,
        ),
    )
    .expect("POST /mcp prompts/get");
    assert!(prompts_get.starts_with("HTTP/1.1 200"), "{prompts_get}");
    assert!(
        prompts_get.contains("route action reliability"),
        "{prompts_get}"
    );
    assert!(
        prompts_get.contains("bounded replay and idempotency"),
        "{prompts_get}"
    );

    let route_action_tool = http_request(
        port,
        "POST",
        "/mcp",
        Some(
            r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"hello.contact","arguments":{"email":"agent@example.test","message":"hello from mcp","confirm":true}}}"#,
        ),
    )
    .expect("POST /mcp route action tools/call");
    assert!(
        route_action_tool.starts_with("HTTP/1.1 200"),
        "{route_action_tool}"
    );
    assert!(
        route_action_tool.contains("\"isError\":false"),
        "{route_action_tool}"
    );
    assert!(
        route_action_tool.contains("\\\"action\\\":\\\"hello.contact\\\""),
        "{route_action_tool}"
    );

    let openapi = http_request(port, "GET", "/openapi.json", None).expect("GET /openapi.json");
    assert!(openapi.starts_with("HTTP/1.1 200"), "{openapi}");
    assert!(openapi.contains("\"openapi\":\"3.1.0\""), "{openapi}");
    assert!(
        openapi.contains("\"operationId\":\"hello.contact\""),
        "{openapi}"
    );
    assert!(openapi.contains("\"/api/actions/contact\""), "{openapi}");
    assert!(
        openapi.contains("\"idempotencyRequired\":true"),
        "{openapi}"
    );

    let manifest =
        http_request(port, "GET", "/.well-known/beater.json", None).expect("GET manifest");
    assert!(manifest.starts_with("HTTP/1.1 200"), "{manifest}");
    assert!(manifest.contains("\"openapi\""), "{manifest}");
    assert!(
        manifest.contains(&format!("\"http://127.0.0.1:{port}/openapi.json\"")),
        "{manifest}"
    );
    assert!(
        manifest.contains("\"name\":\"hello.contact\""),
        "{manifest}"
    );
    assert!(
        manifest.contains("\"path\":\"/api/actions/contact\""),
        "{manifest}"
    );
    assert!(manifest.contains("\"capabilities\""), "{manifest}");
    assert!(manifest.contains("\"prompts\":true"), "{manifest}");
    assert!(manifest.contains("\"resources\":true"), "{manifest}");
    assert!(manifest.contains("\"beater://routes\""), "{manifest}");
    assert!(manifest.contains("\"beater://actions\""), "{manifest}");
    assert!(
        manifest.contains("\"name\":\"beater.review_pr\""),
        "{manifest}"
    );
    assert!(
        manifest.contains("\"name\":\"beater.choose_stack\""),
        "{manifest}"
    );

    let llms = http_request(port, "GET", "/llms.txt", None).expect("GET /llms.txt");
    assert!(llms.starts_with("HTTP/1.1 200"), "{llms}");
    assert!(llms.contains("## Actions"), "{llms}");
    assert!(llms.contains("hello.contact"), "{llms}");
    assert!(llms.contains("/api/actions/contact"), "{llms}");

    let mcp_get = http_request(port, "GET", "/mcp", None).expect("GET /mcp");
    assert!(mcp_get.starts_with("HTTP/1.1 405"), "{mcp_get}");
}

#[test]
fn dev_server_round_robins_js_routes_across_worker_pool() {
    let port = free_port();
    let workspace = workspace();
    let beater = beater_bin(&workspace);
    let temp = temp_dir("worker-pool");
    let app = temp.path.join("pool-app");
    let output = Command::new(&beater)
        .arg("new")
        .arg(&app)
        .output()
        .expect("run beater new");
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let config_path = app.join("beater.toml");
    let config = std::fs::read_to_string(&config_path).expect("read beater.toml");
    let config = config.replace("port = 3000\n", "port = 3000\nworkers = 2\n");
    std::fs::write(&config_path, config).expect("write pooled beater.toml");
    std::fs::write(
        app.join("app/routes/api/pool.ts"),
        r#"
globalThis.__beaterPoolCount = globalThis.__beaterPoolCount ?? 0;

export function GET() {
  globalThis.__beaterPoolCount += 1;
  return { status: 200, body: String(globalThis.__beaterPoolCount) };
}
"#,
    )
    .expect("write pool route");

    let child = Command::new(&beater)
        .arg("dev")
        .arg(&app)
        .arg("--host")
        .arg("127.0.0.1")
        .arg("--port")
        .arg(port.to_string())
        .env_remove("ANTHROPIC_API_KEY")
        .env_remove("BEATER_BASE_URL")
        .env_remove("BEATER_MCP_TOKEN")
        .env_remove("BEATER_MCP_TRUSTED_ORIGINS")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn pooled beater dev");
    let _child = ChildGuard { child };

    let ready = wait_for_http(port, "GET", "/api/health", None);
    assert!(ready.starts_with("HTTP/1.1 200"), "{ready}");

    let first = http_request(port, "GET", "/api/pool", None).expect("GET /api/pool #1");
    let second = http_request(port, "GET", "/api/pool", None).expect("GET /api/pool #2");
    let third = http_request(port, "GET", "/api/pool", None).expect("GET /api/pool #3");
    let fourth = http_request(port, "GET", "/api/pool", None).expect("GET /api/pool #4");

    for response in [&first, &second, &third, &fourth] {
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    }
    assert_eq!(http_body(&first), "1");
    assert_eq!(http_body(&second), "1");
    assert_eq!(http_body(&third), "2");
    assert_eq!(http_body(&fourth), "2");
}

#[test]
fn dev_server_hot_reloads_agent_registry_and_metadata() {
    let port = free_port();
    let workspace = workspace();
    let temp = temp_dir("agent-hot-reload");
    let app = temp.path.join("hello");
    copy_dir_all(&workspace.join("examples/hello"), &app).expect("copy hello fixture");
    let app = app.canonicalize().expect("canonicalize hello fixture");
    std::fs::remove_dir_all(app.join("agents")).expect("remove initial agents dir");
    let beater = beater_bin(&workspace);
    let child = Command::new(beater)
        .arg("dev")
        .arg(&app)
        .arg("--host")
        .arg("127.0.0.1")
        .arg("--port")
        .arg(port.to_string())
        .env_remove("ANTHROPIC_API_KEY")
        .env_remove("BEATER_BASE_URL")
        .env_remove("BEATER_MCP_TOKEN")
        .env_remove("BEATER_MCP_TRUSTED_ORIGINS")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn beater dev");
    let _child = ChildGuard { child };

    let health = wait_for_http(port, "GET", "/api/health", None);
    assert!(health.starts_with("HTTP/1.1 200"), "{health}");
    std::thread::sleep(Duration::from_millis(500));

    let tools_list_body = r#"{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}"#;
    let initial_tools =
        http_request(port, "POST", "/mcp", Some(tools_list_body)).expect("POST tools/list");
    assert!(initial_tools.starts_with("HTTP/1.1 200"), "{initial_tools}");
    assert!(
        !initial_tools.contains("hot_reload_echo"),
        "{initial_tools}"
    );
    let initial_manifest =
        http_request(port, "GET", "/.well-known/beater.json", None).expect("GET manifest");
    assert!(
        initial_manifest.contains("\"agents\":[]"),
        "{initial_manifest}"
    );
    assert!(
        !initial_manifest.contains("\"support\""),
        "{initial_manifest}"
    );
    assert!(!initial_manifest.contains("\"ops\""), "{initial_manifest}");

    std::fs::create_dir_all(app.join("agents/support/tools"))
        .expect("create support tools dir after server start");
    std::fs::write(
        app.join("agents/support/tools/hot_reload_echo.py"),
        r#"
TOOL = {
    "description": "Echo a value after agent hot reload.",
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
    .expect("write hot reload python tool");
    std::fs::write(
        app.join("agents/support/agent.ts"),
        r#"
import { defineAgent, pyTool, rustTool } from "beater:agent";

export default defineAgent({
  name: "support",
  model: "claude-opus-4-8",
  system: "Hot reload test support agent.",
  tools: [
    rustTool("get_time"),
    pyTool("hot_reload_echo", "./tools/hot_reload_echo.py", { idempotent: true }),
  ],
});
"#,
    )
    .expect("rewrite support agent");
    std::fs::create_dir_all(app.join("agents/ops")).expect("create ops agent dir");
    std::fs::write(
        app.join("agents/ops/agent.ts"),
        r#"
import { defineAgent } from "beater:agent";

export default defineAgent({
  name: "ops",
  model: "claude-opus-4-8",
  system: "Hot reload test ops agent.",
  tools: [],
});
"#,
    )
    .expect("write ops agent");

    let reloaded_tools = wait_for_http_contains(
        port,
        "POST",
        "/mcp",
        Some(tools_list_body),
        "hot_reload_echo",
    );
    assert!(reloaded_tools.contains("get_time"), "{reloaded_tools}");
    let call = wait_for_http_contains(
        port,
        "POST",
        "/mcp",
        Some(
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"hot_reload_echo","arguments":{"value":"fresh"}}}"#,
        ),
        "fresh",
    );
    assert!(call.contains("\"isError\":false"), "{call}");

    let manifest = wait_for_http_contains(port, "GET", "/.well-known/beater.json", None, "\"ops\"");
    assert!(manifest.contains("\"support\""), "{manifest}");
    let llms = wait_for_http_contains(port, "GET", "/llms.txt", None, "ops");
    assert!(llms.contains("support"), "{llms}");
}

#[test]
fn dev_server_refuses_remote_mcp_without_bearer_token() {
    let port = free_port();
    let workspace = workspace();
    let app = workspace.join("examples/hello");
    let output = Command::new(beater_bin(&workspace))
        .arg("dev")
        .arg(&app)
        .arg("--host")
        .arg("0.0.0.0")
        .arg("--port")
        .arg(port.to_string())
        .arg("--base-url")
        .arg("https://hello.example.test")
        .env_remove("ANTHROPIC_API_KEY")
        .env_remove("BEATER_BASE_URL")
        .env_remove("BEATER_MCP_TOKEN")
        .env_remove("BEATER_MCP_TRUSTED_ORIGINS")
        .output()
        .expect("run beater dev");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("refusing to bind 0.0.0.0") && stderr.contains("BEATER_MCP_TOKEN"),
        "{stderr}"
    );
}

#[test]
fn new_scaffolds_runnable_app_and_refuses_overwrite() {
    let port = free_port();
    let workspace = workspace();
    let beater = beater_bin(&workspace);
    let temp = temp_dir("new-scaffold");
    let app = temp.path.join("my-app");

    let output = Command::new(&beater)
        .arg("new")
        .arg(&app)
        .output()
        .expect("run beater new");
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let config = std::fs::read_to_string(app.join("beater.toml")).expect("read beater.toml");
    assert!(config.contains("name = \"my-app\""), "{config}");
    for relative_path in [
        "app/routes/index.tsx",
        "app/routes/index.client.ts",
        "app/routes/index.server.tsx",
        "app/routes/api/health.ts",
        "app/routes/api/boom.ts",
        "app/routes/api/actions/contact.ts",
        "agents/support/agent.ts",
        "agents/support/tools/summarize_numbers.py",
        "agents/support/tools/slow_summarize.py",
        "agents/support/tools/slow_summarize_once.py",
        "agents/support/tools/fib.wat",
    ] {
        assert!(
            app.join(relative_path).is_file(),
            "missing scaffold file {relative_path}"
        );
    }
    std::fs::create_dir_all(app.join("node_modules/zod")).expect("create fixture zod package");
    std::fs::write(
        app.join("node_modules/zod/package.json"),
        r#"{
  "name": "zod",
  "type": "module",
  "exports": {
    ".": {
      "import": "./index.js",
      "require": "./index.cjs"
    }
  }
}"#,
    )
    .expect("write fixture zod package.json");
    std::fs::write(
        app.join("node_modules/zod/index.js"),
        "export const z = { string: () => ({ parse: (value) => String(value).trim() }) };\n",
    )
    .expect("write fixture zod index");
    std::fs::create_dir_all(app.join("node_modules/buffered"))
        .expect("create fixture buffered package");
    std::fs::write(
        app.join("node_modules/buffered/package.json"),
        r#"{
  "name": "buffered",
  "type": "module",
  "exports": {
    ".": "./index.js"
  }
}"#,
    )
    .expect("write fixture buffered package.json");
    std::fs::write(
        app.join("node_modules/buffered/index.js"),
        r#"import { Buffer } from "node:buffer";

export function encode(value) {
  const buffer = Buffer.from(value, "utf8");
  return {
    text: buffer.toString("utf8"),
    hex: buffer.toString("hex"),
    base64: buffer.toString("base64"),
    bytes: Buffer.byteLength(value, "utf8"),
    isBuffer: Buffer.isBuffer(buffer),
  };
}
"#,
    )
    .expect("write fixture buffered index");
    std::fs::write(
        app.join("app/routes/api/zod.ts"),
        r#"import { z } from "zod";

const Name = z.string();

export function GET() {
  return {
    status: 200,
    headers: { "content-type": "application/json; charset=utf-8" },
    body: JSON.stringify({ value: Name.parse(" beater ") }),
  };
}
"#,
    )
    .expect("write zod route");
    std::fs::write(
        app.join("app/routes/api/buffered.ts"),
        r#"import { encode } from "buffered";

export function GET() {
  return {
    status: 200,
    headers: { "content-type": "application/json; charset=utf-8" },
    body: JSON.stringify(encode("beater")),
  };
}
"#,
    )
    .expect("write buffered route");

    let child = Command::new(&beater)
        .arg("dev")
        .arg(&app)
        .arg("--host")
        .arg("127.0.0.1")
        .arg("--port")
        .arg(port.to_string())
        .env_remove("ANTHROPIC_API_KEY")
        .env_remove("BEATER_BASE_URL")
        .env_remove("BEATER_MCP_TOKEN")
        .env_remove("BEATER_MCP_TRUSTED_ORIGINS")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn scaffolded beater dev");
    let _child = ChildGuard { child };

    let health = wait_for_http(port, "GET", "/api/health", None);
    assert!(health.starts_with("HTTP/1.1 200"), "{health}");
    assert!(health.contains("\"runtime\":\"beater.js\""), "{health}");

    let zod = http_request(port, "GET", "/api/zod", None).expect("GET /api/zod");
    assert!(zod.starts_with("HTTP/1.1 200"), "{zod}");
    assert!(zod.contains(r#""value":"beater""#), "{zod}");

    let buffered = http_request(port, "GET", "/api/buffered", None).expect("GET /api/buffered");
    assert!(buffered.starts_with("HTTP/1.1 200"), "{buffered}");
    assert!(buffered.contains(r#""text":"beater""#), "{buffered}");
    assert!(buffered.contains(r#""hex":"626561746572""#), "{buffered}");
    assert!(buffered.contains(r#""base64":"YmVhdGVy""#), "{buffered}");
    assert!(buffered.contains(r#""bytes":6"#), "{buffered}");
    assert!(buffered.contains(r#""isBuffer":true"#), "{buffered}");

    let home = http_request(port, "GET", "/", None).expect("GET scaffolded /");
    assert!(home.starts_with("HTTP/1.1 200"), "{home}");
    assert!(
        home.contains("<h1 class=\"brand-title\">beater.js</h1>"),
        "{home}"
    );
    assert!(
        home.contains(r#"<script type="module" src="/_beater/client/index.js"></script>"#),
        "{home}"
    );
    assert!(home.contains("data-beater-run-events"), "{home}");
    assert!(home.contains("data-run-history"), "{home}");
    assert!(home.contains("data-beater-action-form"), "{home}");

    let client =
        http_request(port, "GET", "/_beater/client/index.js", None).expect("GET client module");
    assert!(client.starts_with("HTTP/1.1 200"), "{client}");
    assert!(
        client.contains("content-type: application/javascript"),
        "{client}"
    );
    assert!(
        client.contains("root.dataset.state = \"hydrated\""),
        "{client}"
    );
    assert!(
        client.contains("root.dataset.bundle = clientBundleMarker"),
        "{client}"
    );
    assert!(
        client.contains("/_beater/client/index.js?dep=1"),
        "{client}"
    );
    assert!(client.contains("new EventSource"), "{client}");
    assert!(client.contains("fetch(\"/_beater/agent/runs\""), "{client}");
    assert!(!client.contains(": number"), "{client}");

    let client_dep = http_request(port, "GET", "/_beater/client/index.js?dep=1", None)
        .expect("GET scaffolded client dependency module");
    assert!(client_dep.starts_with("HTTP/1.1 200"), "{client_dep}");
    assert!(
        client_dep.contains("content-type: application/javascript"),
        "{client_dep}"
    );
    assert!(client_dep.contains("client-helper-bundled"), "{client_dep}");
    assert!(!client_dep.contains(": number"), "{client_dep}");

    let flight =
        http_request(port, "GET", "/_beater/rsc/index.flight", None).expect("GET RSC flight");
    assert!(flight.starts_with("HTTP/1.1 200"), "{flight}");
    assert!(
        flight.contains("content-type: text/x-component"),
        "{flight}"
    );
    assert!(
        flight.contains("B{\"protocol\":\"beater-flight\""),
        "{flight}"
    );
    assert!(flight.contains("H["), "{flight}");
    assert!(flight.contains("E{\"ok\":true}"), "{flight}");

    let doctor = Command::new(&beater)
        .arg("doctor")
        .arg(&app)
        .env_remove("BEATER_BASE_URL")
        .env_remove("BEATBOX_URL")
        .env_remove("BEATBOX_API_KEY")
        .output()
        .expect("run doctor on scaffolded app");
    assert!(!doctor.status.success());
    let doctor_stdout = String::from_utf8_lossy(&doctor.stdout);
    assert!(
        doctor_stdout.contains("app:     my-app"),
        "stdout:\n{doctor_stdout}"
    );
    assert!(
        doctor_stdout.contains("venv:    MISMATCH"),
        "stdout:\n{doctor_stdout}"
    );
    assert!(
        String::from_utf8_lossy(&doctor.stderr).contains("doctor found problems"),
        "stderr:\n{}",
        String::from_utf8_lossy(&doctor.stderr)
    );

    let overwrite = Command::new(&beater)
        .arg("new")
        .arg(&app)
        .output()
        .expect("run beater new over existing app");
    assert!(!overwrite.status.success());
    assert!(
        String::from_utf8_lossy(&overwrite.stderr).contains("destination is not empty"),
        "stderr:\n{}",
        String::from_utf8_lossy(&overwrite.stderr)
    );

    let file_destination = temp.path.join("already-file");
    std::fs::write(&file_destination, "not a dir").expect("write file destination");
    let file_output = Command::new(&beater)
        .arg("new")
        .arg(&file_destination)
        .output()
        .expect("run beater new over file");
    assert!(!file_output.status.success());
    assert!(
        String::from_utf8_lossy(&file_output.stderr).contains("not a directory"),
        "stderr:\n{}",
        String::from_utf8_lossy(&file_output.stderr)
    );

    let escaped = temp.path.join("line\nbreak");
    let escaped_output = Command::new(&beater)
        .arg("new")
        .arg(&escaped)
        .output()
        .expect("run beater new with escaped name");
    assert!(
        escaped_output.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&escaped_output.stderr)
    );
    let escaped_config =
        std::fs::read_to_string(escaped.join("beater.toml")).expect("read escaped beater.toml");
    assert!(escaped_config.contains("name = \"line\\nbreak\""));
}

#[test]
fn build_creates_runnable_bundle_and_refuses_unsafe_output() {
    let port = free_port();
    let workspace = workspace();
    let beater = beater_bin(&workspace);
    let temp = temp_dir("build-bundle");
    let app = temp.path.join("my-app");
    let bundle = temp.path.join("bundle");

    let scaffold = Command::new(&beater)
        .arg("new")
        .arg(&app)
        .output()
        .expect("run beater new");
    assert!(
        scaffold.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&scaffold.stdout),
        String::from_utf8_lossy(&scaffold.stderr)
    );
    std::fs::create_dir_all(app.join(".beater")).expect("create runtime state dir");
    std::fs::write(app.join(".beater/journal.db"), "runtime state").expect("write runtime state");
    std::fs::write(app.join(".env"), "BEATER_SECRET=not-for-bundles").expect("write .env");
    std::fs::write(
        app.join(".env.production.local"),
        "BEATER_SECRET=also-not-for-bundles",
    )
    .expect("write env variant");
    std::fs::write(
        app.join(".npmrc"),
        "//registry.example.test/:_authToken=secret",
    )
    .expect("write .npmrc");
    std::fs::create_dir_all(app.join(".venv/lib/python3.11/site-packages"))
        .expect("create synthetic venv site-packages");
    std::fs::write(app.join(".venv/pyvenv.cfg"), "home = /usr/bin\n")
        .expect("write synthetic venv config");
    std::fs::write(
        app.join(".venv/lib/python3.11/site-packages/beater_fixture.py"),
        "VALUE = 'bundled'\n",
    )
    .expect("write synthetic venv package");

    let build = Command::new(&beater)
        .arg("build")
        .arg(&app)
        .arg("--out")
        .arg(&bundle)
        .output()
        .expect("run beater build");
    assert!(
        build.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&build.stdout),
        String::from_utf8_lossy(&build.stderr)
    );

    let dirty_output = Command::new(&beater)
        .arg("build")
        .arg(&app)
        .arg("--out")
        .arg(&bundle)
        .output()
        .expect("run beater build over bundle");
    assert!(!dirty_output.status.success());
    assert!(
        String::from_utf8_lossy(&dirty_output.stderr).contains("build output is not empty"),
        "stderr:\n{}",
        String::from_utf8_lossy(&dirty_output.stderr)
    );

    let unsafe_output = Command::new(&beater)
        .arg("build")
        .arg(&app)
        .arg("--out")
        .arg(app.join("dist"))
        .output()
        .expect("run beater build inside app");
    assert!(!unsafe_output.status.success());
    assert!(
        String::from_utf8_lossy(&unsafe_output.stderr).contains("outside the app directory"),
        "stderr:\n{}",
        String::from_utf8_lossy(&unsafe_output.stderr)
    );

    let not_bundle = temp.path.join("not-bundle");
    std::fs::create_dir_all(&not_bundle).expect("create non-bundle dir");
    std::fs::write(not_bundle.join("keep.txt"), "do not delete").expect("write non-bundle file");
    let force_non_bundle = Command::new(&beater)
        .arg("build")
        .arg(&app)
        .arg("--out")
        .arg(&not_bundle)
        .arg("--force")
        .output()
        .expect("run beater build --force over non-bundle");
    assert!(!force_non_bundle.status.success());
    assert!(
        String::from_utf8_lossy(&force_non_bundle.stderr).contains("without a beater-build.json"),
        "stderr:\n{}",
        String::from_utf8_lossy(&force_non_bundle.stderr)
    );
    assert!(
        not_bundle.join("keep.txt").is_file(),
        "--force should not remove non-bundle directories"
    );

    #[cfg(unix)]
    {
        let link_target = temp.path.join("link-target");
        std::fs::create_dir_all(&link_target).expect("create link target");
        let output_link = temp.path.join("bundle-link");
        std::os::unix::fs::symlink(&link_target, &output_link).expect("create output symlink");
        let symlink_output = Command::new(&beater)
            .arg("build")
            .arg(&app)
            .arg("--out")
            .arg(&output_link)
            .arg("--force")
            .output()
            .expect("run beater build over output symlink");
        assert!(!symlink_output.status.success());
        assert!(
            String::from_utf8_lossy(&symlink_output.stderr).contains("must not be a symlink"),
            "stderr:\n{}",
            String::from_utf8_lossy(&symlink_output.stderr)
        );
        assert!(
            link_target.is_dir(),
            "output symlink target should not be removed"
        );
    }

    let rebuild = Command::new(&beater)
        .arg("build")
        .arg(&app)
        .arg("--out")
        .arg(&bundle)
        .arg("--force")
        .output()
        .expect("run beater build --force");
    assert!(
        rebuild.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&rebuild.stdout),
        String::from_utf8_lossy(&rebuild.stderr)
    );

    #[cfg(unix)]
    {
        let preserved = bundle.join("preserved.txt");
        std::fs::write(&preserved, "old valid bundle").expect("write preserved bundle marker");
        let outside = temp.path.join("force-outside.txt");
        std::fs::write(&outside, "outside").expect("write outside file for force failure");
        let app_link = app.join("force-outside-link");
        std::os::unix::fs::symlink(&outside, &app_link).expect("create app symlink");
        let failed_force_rebuild = Command::new(&beater)
            .arg("build")
            .arg(&app)
            .arg("--out")
            .arg(&bundle)
            .arg("--force")
            .output()
            .expect("run failed beater build --force");
        assert!(!failed_force_rebuild.status.success());
        assert!(
            String::from_utf8_lossy(&failed_force_rebuild.stderr).contains("cannot bundle symlink"),
            "stderr:\n{}",
            String::from_utf8_lossy(&failed_force_rebuild.stderr)
        );
        std::fs::remove_file(app_link).expect("remove force app symlink");
        assert!(
            preserved.is_file(),
            "failed --force rebuild should preserve existing bundle"
        );
        std::fs::remove_file(preserved).expect("remove preserved marker");
    }

    for relative_path in [
        "bin/beater",
        "app/beater.toml",
        "app/app/routes/index.tsx",
        "app/agents/support/agent.ts",
        "run.sh",
        "Dockerfile",
        "beater-build.json",
        "README.md",
        ".dockerignore",
        "app/.venv/pyvenv.cfg",
        "app/.venv/lib/python3.11/site-packages/beater_fixture.py",
    ] {
        assert!(
            bundle.join(relative_path).is_file(),
            "missing bundle file {relative_path}"
        );
    }
    assert!(
        !bundle.join("app/.beater/journal.db").exists(),
        "runtime state should not be copied into build bundle"
    );
    assert!(
        !bundle.join("app/.env").exists(),
        "local env files should not be copied into build bundle"
    );
    assert!(
        !bundle.join("app/.env.production.local").exists(),
        "local env variants should not be copied into build bundle"
    );
    assert!(
        !bundle.join("app/.npmrc").exists(),
        "local npm credentials should not be copied into build bundle"
    );
    let manifest =
        std::fs::read_to_string(bundle.join("beater-build.json")).expect("read build manifest");
    assert!(manifest.contains("\"app\": \"my-app\""), "{manifest}");
    let dockerfile = std::fs::read_to_string(bundle.join("Dockerfile")).expect("read Dockerfile");
    assert!(dockerfile.contains("FROM python:3.11-slim"), "{dockerfile}");
    assert!(dockerfile.contains("BEATER_HOST=0.0.0.0"), "{dockerfile}");
    assert!(dockerfile.contains("USER beater"), "{dockerfile}");
    let dockerignore =
        std::fs::read_to_string(bundle.join(".dockerignore")).expect("read .dockerignore");
    assert!(dockerignore.contains(".env.*"), "{dockerignore}");

    #[cfg(unix)]
    {
        let outside = temp.path.join("outside.txt");
        std::fs::write(&outside, "outside").expect("write outside file");
        let app_link = app.join("outside-link");
        let symlink_app_bundle = temp.path.join("symlink-app-bundle");
        std::os::unix::fs::symlink(&outside, &app_link).expect("create app symlink");
        let symlink_app = Command::new(&beater)
            .arg("build")
            .arg(&app)
            .arg("--out")
            .arg(&symlink_app_bundle)
            .output()
            .expect("run beater build with app symlink");
        assert!(!symlink_app.status.success());
        assert!(
            String::from_utf8_lossy(&symlink_app.stderr).contains("cannot bundle symlink"),
            "stderr:\n{}",
            String::from_utf8_lossy(&symlink_app.stderr)
        );
        std::fs::remove_file(app_link).expect("remove app symlink");
        assert!(
            !symlink_app_bundle.exists(),
            "failed builds should clean up staging output"
        );
        let retry_after_failed_build = Command::new(&beater)
            .arg("build")
            .arg(&app)
            .arg("--out")
            .arg(&symlink_app_bundle)
            .output()
            .expect("retry beater build after failed staging build");
        assert!(
            retry_after_failed_build.status.success(),
            "stdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&retry_after_failed_build.stdout),
            String::from_utf8_lossy(&retry_after_failed_build.stderr)
        );
    }

    let child = Command::new(bundle.join("run.sh"))
        .env("BEATER_HOST", "127.0.0.1")
        .env("BEATER_PORT", port.to_string())
        .env_remove("PORT")
        .env_remove("ANTHROPIC_API_KEY")
        .env_remove("BEATER_BASE_URL")
        .env_remove("BEATER_MCP_TOKEN")
        .env_remove("BEATER_MCP_TRUSTED_ORIGINS")
        .env_remove("BEATER_ALLOW_UNAUTHENTICATED_REMOTE")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn build bundle");
    let _child = ChildGuard { child };

    let health = wait_for_http(port, "GET", "/api/health", None);
    assert!(health.starts_with("HTTP/1.1 200"), "{health}");
    assert!(health.contains("\"runtime\":\"beater.js\""), "{health}");
}

#[test]
fn dev_server_requires_mcp_bearer_token_when_configured() {
    let port = free_port();
    let workspace = workspace();
    let app = workspace.join("examples/hello");
    let beater = beater_bin(&workspace);
    let child = Command::new(beater)
        .arg("dev")
        .arg(&app)
        .arg("--host")
        .arg("0.0.0.0")
        .arg("--port")
        .arg(port.to_string())
        .arg("--base-url")
        .arg("https://hello.example.test")
        .env_remove("ANTHROPIC_API_KEY")
        .env_remove("BEATER_BASE_URL")
        .env("BEATER_MCP_TOKEN", "test-secret")
        .env("BEATER_MCP_TRUSTED_ORIGINS", "https://ops.example.test")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn beater dev");
    let _child = ChildGuard { child };

    let health = wait_for_http(port, "GET", "/api/health", None);
    assert!(health.starts_with("HTTP/1.1 200"), "{health}");

    let manifest =
        http_request(port, "GET", "/.well-known/beater.json", None).expect("GET manifest");
    assert!(manifest.starts_with("HTTP/1.1 200"), "{manifest}");
    assert!(
        manifest.contains("\"endpoint\":\"https://hello.example.test/mcp\""),
        "{manifest}"
    );
    assert!(manifest.contains("\"required\":true"), "{manifest}");
    assert!(manifest.contains("\"prompts\":true"), "{manifest}");
    assert!(
        manifest.contains("\"name\":\"beater.review_pr\""),
        "{manifest}"
    );
    assert!(
        !manifest.contains("https://ops.example.test") && !manifest.contains("trustedOrigins"),
        "{manifest}"
    );

    let body = r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#;
    let missing_auth = http_request(port, "POST", "/mcp", Some(body)).expect("POST /mcp");
    assert!(missing_auth.starts_with("HTTP/1.1 401"), "{missing_auth}");
    assert!(
        missing_auth.contains("www-authenticate: Bearer"),
        "{missing_auth}"
    );

    let good_auth = http_request_with_headers(
        port,
        "POST",
        "/mcp",
        &[("authorization", "Bearer test-secret")],
        Some(body),
    )
    .expect("POST /mcp with bearer token");
    assert!(good_auth.starts_with("HTTP/1.1 200"), "{good_auth}");
    assert!(good_auth.contains("\"tools\""), "{good_auth}");

    let preflight = http_request_with_headers(
        port,
        "OPTIONS",
        "/mcp",
        &[
            ("origin", "https://ops.example.test"),
            ("access-control-request-method", "POST"),
            (
                "access-control-request-headers",
                "authorization, content-type",
            ),
        ],
        None,
    )
    .expect("OPTIONS /mcp preflight");
    assert!(preflight.starts_with("HTTP/1.1 204"), "{preflight}");
    assert!(
        preflight.contains("access-control-allow-origin: https://ops.example.test"),
        "{preflight}"
    );
    assert!(
        preflight.contains("access-control-allow-headers: authorization, content-type"),
        "{preflight}"
    );

    let trusted_origin = http_request_with_headers(
        port,
        "POST",
        "/mcp",
        &[
            ("authorization", "Bearer test-secret"),
            ("origin", "https://ops.example.test"),
        ],
        Some(body),
    )
    .expect("POST /mcp with trusted origin");
    assert!(
        trusted_origin.starts_with("HTTP/1.1 200"),
        "{trusted_origin}"
    );
    assert!(
        trusted_origin.contains("access-control-allow-origin: https://ops.example.test"),
        "{trusted_origin}"
    );

    let bad_origin = http_request_with_headers(
        port,
        "POST",
        "/mcp",
        &[
            ("authorization", "Bearer test-secret"),
            ("origin", "https://evil.example.test"),
        ],
        Some(body),
    )
    .expect("POST /mcp with bad origin");
    assert!(bad_origin.starts_with("HTTP/1.1 403"), "{bad_origin}");

    let mcp_get = http_request(port, "GET", "/mcp", None).expect("GET /mcp");
    assert!(mcp_get.starts_with("HTTP/1.1 401"), "{mcp_get}");
    let mcp_get_authed = http_request_with_headers(
        port,
        "GET",
        "/mcp",
        &[("authorization", "Bearer test-secret")],
        None,
    )
    .expect("GET /mcp with bearer token");
    assert!(
        mcp_get_authed.starts_with("HTTP/1.1 405"),
        "{mcp_get_authed}"
    );
}

#[test]
fn doctor_reports_python_v8_and_venv_diagnostics() {
    let workspace = workspace();
    let temp = temp_dir("doctor-ok");
    let app = temp.path.join("app");
    std::fs::create_dir_all(&app).expect("create doctor app");
    std::fs::write(
        app.join("beater.toml"),
        r#"
[app]
name = "doctor-ok"
port = 31234
"#,
    )
    .expect("write doctor app config");
    let output = Command::new(beater_bin(&workspace))
        .arg("doctor")
        .arg(&app)
        .env_remove("BEATER_BASE_URL")
        .env_remove("BEATBOX_URL")
        .env_remove("BEATBOX_API_KEY")
        .output()
        .expect("run beater doctor");

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("beater doctor"), "{stdout}");
    assert!(stdout.contains("python:"), "{stdout}");
    assert!(stdout.contains("venv:"), "{stdout}");
    assert!(stdout.contains("public:"), "{stdout}");
    assert!(stdout.contains("beatbox:"), "{stdout}");
    assert!(stdout.contains("mcp:"), "{stdout}");
    assert!(stdout.contains("v8:"), "{stdout}");
}

#[test]
fn doctor_exits_nonzero_when_diagnostics_fail() {
    let workspace = workspace();
    let temp = temp_dir("doctor-missing-app");
    let missing_app = temp.path.join("missing");
    let output = Command::new(beater_bin(&workspace))
        .arg("doctor")
        .arg(&missing_app)
        .output()
        .expect("run beater doctor on missing app");

    assert!(!output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("beater doctor"), "{stdout}");
    assert!(stdout.contains("app:     UNAVAILABLE"), "{stdout}");
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("doctor found problems"), "{stderr}");
}

fn workspace() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn beater_bin(workspace: &std::path::Path) -> String {
    std::env::var("CARGO_BIN_EXE_beater").unwrap_or_else(|_| {
        workspace
            .join("target/debug/beater")
            .to_string_lossy()
            .into_owned()
    })
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn temp_dir(label: &str) -> TempDirGuard {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time before unix epoch")
        .as_nanos();
    let mut path = std::env::temp_dir();
    path.push(format!("beater-{label}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&path).expect("create temp dir");
    TempDirGuard { path }
}

fn copy_dir_all(src: &std::path::Path, dst: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let file_name = entry.file_name();
        if file_name == ".beater" {
            continue;
        }
        let src_path = entry.path();
        let dst_path = dst.join(file_name);
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            copy_dir_all(&src_path, &dst_path)?;
        } else if file_type.is_file() {
            std::fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}

fn wait_for_http(port: u16, method: &str, path: &str, body: Option<&str>) -> String {
    let started = Instant::now();
    let mut last_error = None;
    while started.elapsed() < Duration::from_secs(30) {
        match http_request(port, method, path, body) {
            Ok(response) if response.starts_with("HTTP/1.1 200") => return response,
            Ok(response) => last_error = Some(response),
            Err(e) => last_error = Some(e.to_string()),
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!(
        "server did not become ready on port {port}; last response/error: {:?}",
        last_error
    );
}

fn wait_for_http_contains(
    port: u16,
    method: &str,
    path: &str,
    body: Option<&str>,
    needle: &str,
) -> String {
    let started = Instant::now();
    let mut last_error = None;
    while started.elapsed() < Duration::from_secs(30) {
        match http_request(port, method, path, body) {
            Ok(response) if response.starts_with("HTTP/1.1 200") && response.contains(needle) => {
                return response;
            }
            Ok(response) => last_error = Some(response),
            Err(e) => last_error = Some(e.to_string()),
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!(
        "server response on {method} {path} did not contain {needle:?}; last response/error: {:?}",
        last_error
    );
}

fn http_request(
    port: u16,
    method: &str,
    path: &str,
    body: Option<&str>,
) -> std::io::Result<String> {
    http_request_with_headers(port, method, path, &[], body)
}

fn http_request_with_headers(
    port: u16,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Option<&str>,
) -> std::io::Result<String> {
    let mut stream = TcpStream::connect(("127.0.0.1", port))?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    stream.set_write_timeout(Some(Duration::from_secs(10)))?;
    let body = body.unwrap_or("");
    let has_content_type = headers
        .iter()
        .any(|(name, _)| name.eq_ignore_ascii_case("content-type"));
    let content_type = if has_content_type {
        String::new()
    } else {
        "\r\ncontent-type: application/json".to_string()
    };
    let mut request = format!(
        "{method} {path} HTTP/1.1\r\nhost: 127.0.0.1:{port}{content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    for (name, value) in headers {
        request.insert_str(
            request.find("\r\n\r\n").expect("request separator"),
            &format!("\r\n{name}: {value}"),
        );
    }
    stream.write_all(request.as_bytes())?;

    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    Ok(response)
}

fn http_body(response: &str) -> &str {
    response.split("\r\n\r\n").nth(1).unwrap_or_default().trim()
}
