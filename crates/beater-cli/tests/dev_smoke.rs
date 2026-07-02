use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

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
        .arg("0.0.0.0")
        .arg("--port")
        .arg(port.to_string())
        .env_remove("ANTHROPIC_API_KEY")
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

    let home = http_request(port, "GET", "/", None).expect("GET /");
    assert!(home.starts_with("HTTP/1.1 200"), "{home}");
    assert!(home.contains("content-type: text/html"), "{home}");
    assert!(home.contains("<h1>beater.js</h1>"), "{home}");

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
        .env_remove("ANTHROPIC_API_KEY")
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
    let app = workspace.join("examples/hello");
    let output = Command::new(beater_bin(&workspace))
        .arg("doctor")
        .arg(&app)
        .output()
        .expect("run beater doctor");

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("beater doctor"), "{stdout}");
    assert!(stdout.contains("python:"), "{stdout}");
    assert!(stdout.contains("venv:"), "{stdout}");
    assert!(stdout.contains("mcp:"), "{stdout}");
    assert!(stdout.contains("v8:"), "{stdout}");
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
