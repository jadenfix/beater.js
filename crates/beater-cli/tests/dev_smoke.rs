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
    let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let app = workspace.join("examples/hello");
    let beater = std::env::var("CARGO_BIN_EXE_beater").unwrap_or_else(|_| {
        workspace
            .join("target/debug/beater")
            .to_string_lossy()
            .into_owned()
    });
    let child = Command::new(beater)
        .arg("dev")
        .arg(&app)
        .arg("--host")
        .arg("0.0.0.0")
        .arg("--port")
        .arg(port.to_string())
        .env_remove("ANTHROPIC_API_KEY")
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
    let mut stream = TcpStream::connect(("127.0.0.1", port))?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    stream.set_write_timeout(Some(Duration::from_secs(10)))?;
    let body = body.unwrap_or("");
    let request = format!(
        "{method} {path} HTTP/1.1\r\nhost: 127.0.0.1:{port}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(request.as_bytes())?;

    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    Ok(response)
}
