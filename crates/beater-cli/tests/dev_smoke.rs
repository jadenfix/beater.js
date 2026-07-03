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
    assert!(!client.contains(": number"), "{client}");

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

    let mcp_get = http_request(port, "GET", "/mcp", None).expect("GET /mcp");
    assert!(mcp_get.starts_with("HTTP/1.1 405"), "{mcp_get}");
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
    assert!(!client.contains(": number"), "{client}");

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
    assert!(
        manifest.contains("\"trustedOrigins\":[\"https://ops.example.test\"]"),
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
    let mut request = format!(
        "{method} {path} HTTP/1.1\r\nhost: 127.0.0.1:{port}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
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
