//! MCP server endpoint (spec 2025-11-25, Streamable HTTP, stateless).
//!
//! Deliberately minimal and hand-rolled: a compliant server needs one
//! endpoint that accepts POST JSON-RPC, validates Origin (DNS-rebinding
//! defense), optionally requires bearer-token auth for remote management,
//! and MAY answer GET with 405 when it offers no server-initiated SSE stream.
//! No sessions (`MCP-Session-Id` is a MAY), no SSE — every request gets a
//! single JSON object back.

use std::net::{Ipv4Addr, Ipv6Addr};

use axum::body::Body;
use axum::http::header::{
    ACCESS_CONTROL_ALLOW_HEADERS, ACCESS_CONTROL_ALLOW_METHODS, ACCESS_CONTROL_ALLOW_ORIGIN,
    ACCESS_CONTROL_EXPOSE_HEADERS, ACCESS_CONTROL_MAX_AGE, AUTHORIZATION, ORIGIN, VARY,
    WWW_AUTHENTICATE,
};
use axum::http::{HeaderMap, HeaderValue, Response, StatusCode};
use deno_core::url::{Host, Url};
use serde_json::{Value, json};

use beater_agent::{ToolCallContext, ToolRegistry};

pub const PROTOCOL_VERSION: &str = "2025-11-25";
pub const DEFAULT_TOKEN_ENV: &str = "BEATER_MCP_TOKEN";
pub const DEFAULT_TRUSTED_ORIGINS_ENV: &str = "BEATER_MCP_TRUSTED_ORIGINS";

/// MCP access policy. By default the endpoint remains local-dev friendly:
/// non-browser clients may omit Origin and no token is required. Set
/// BEATER_MCP_TOKEN before binding beyond loopback.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AccessConfig {
    bearer_token: Option<String>,
    trusted_origins: Vec<String>,
}

impl AccessConfig {
    pub fn from_env() -> Self {
        let bearer_token = std::env::var(DEFAULT_TOKEN_ENV).ok();
        let trusted_origins = std::env::var(DEFAULT_TRUSTED_ORIGINS_ENV)
            .ok()
            .map(|value| {
                value
                    .split(',')
                    .map(str::trim)
                    .filter(|origin| !origin.is_empty())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        Self::new(bearer_token, trusted_origins)
    }

    pub fn new(bearer_token: Option<String>, trusted_origins: Vec<String>) -> Self {
        Self {
            bearer_token: bearer_token.and_then(non_empty),
            trusted_origins: trusted_origins
                .into_iter()
                .filter_map(non_empty)
                .filter_map(canonical_origin)
                .collect(),
        }
    }

    pub fn auth_required(&self) -> bool {
        self.bearer_token.is_some()
    }

    pub fn trusted_origins(&self) -> &[String] {
        &self.trusted_origins
    }

    /// Origin MUST be validated when present. Browser callers are limited to
    /// loopback origins plus explicitly trusted remote operator origins.
    pub fn origin_allowed(&self, headers: &HeaderMap) -> bool {
        let Some(origin) = headers.get(ORIGIN) else {
            return true; // non-browser clients (curl, inspector CLI) send no Origin
        };
        let Ok(origin) = origin.to_str() else {
            return false;
        };
        let Some(origin) = canonical_origin(origin) else {
            return false;
        };
        is_loopback_origin(&origin)
            || self
                .trusted_origins
                .iter()
                .any(|allowed| allowed == &origin)
    }

    fn authorized(&self, headers: &HeaderMap) -> bool {
        let Some(expected) = self.bearer_token.as_deref() else {
            return true;
        };
        let Some(value) = headers.get(AUTHORIZATION).and_then(|v| v.to_str().ok()) else {
            return false;
        };
        let Some((scheme, token)) = value.split_once(' ') else {
            return false;
        };
        scheme.eq_ignore_ascii_case("bearer")
            && constant_time_eq(token.as_bytes(), expected.as_bytes())
    }

    fn cors_origin(&self, headers: &HeaderMap) -> Option<HeaderValue> {
        let origin = headers.get(ORIGIN)?.to_str().ok()?;
        self.origin_allowed(headers)
            .then(|| HeaderValue::from_str(origin).ok())
            .flatten()
    }
}

fn non_empty(value: String) -> Option<String> {
    let value = value.trim().to_string();
    (!value.is_empty()).then_some(value)
}

fn canonical_origin(origin: impl AsRef<str>) -> Option<String> {
    let url = parse_origin(origin.as_ref())?;
    let host = match url.host()? {
        Host::Domain(host) => host.to_string(),
        Host::Ipv4(addr) => addr.to_string(),
        Host::Ipv6(addr) => format!("[{addr}]"),
    };
    Some(format!(
        "{}://{}{}",
        url.scheme(),
        host,
        url.port()
            .map(|port| format!(":{port}"))
            .unwrap_or_default()
    ))
}

fn parse_origin(origin: &str) -> Option<Url> {
    let url = Url::parse(origin.trim().trim_end_matches('/')).ok()?;
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    if !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return None;
    }
    url.host()?;
    Some(url)
}

fn is_loopback_origin(origin: &str) -> bool {
    let Some(url) = parse_origin(origin) else {
        return false;
    };
    match url.host() {
        Some(Host::Domain(host)) => host.eq_ignore_ascii_case("localhost"),
        Some(Host::Ipv4(addr)) => addr == Ipv4Addr::LOCALHOST,
        Some(Host::Ipv6(addr)) => addr == Ipv6Addr::LOCALHOST,
        None => false,
    }
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    let max_len = left.len().max(right.len());
    let mut diff = left.len() ^ right.len();
    for index in 0..max_len {
        let left_byte = left.get(index).copied().unwrap_or(0);
        let right_byte = right.get(index).copied().unwrap_or(0);
        diff |= usize::from(left_byte ^ right_byte);
    }
    diff == 0
}

pub async fn handle_post(
    registry: &ToolRegistry,
    access: &AccessConfig,
    headers: &HeaderMap,
    body: &[u8],
) -> Response<Body> {
    if !access.origin_allowed(headers) {
        return http_response(
            StatusCode::FORBIDDEN,
            json!({"jsonrpc": "2.0", "error": {"code": -32600, "message": "origin not allowed"}}),
        );
    }
    if !access.authorized(headers) {
        return with_cors(unauthorized_response(), access, headers);
    }
    let message: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(e) => {
            return with_cors(
                http_response(
                    StatusCode::BAD_REQUEST,
                    json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": format!("parse error: {e}")}}),
                ),
                access,
                headers,
            );
        }
    };

    // Notifications and client responses carry no `id` → 202 Accepted, no body.
    let Some(id) = message.get("id").filter(|id| !id.is_null()).cloned() else {
        return with_cors(
            Response::builder()
                .status(StatusCode::ACCEPTED)
                .body(Body::empty())
                .expect("static response"),
            access,
            headers,
        );
    };

    let method = message["method"].as_str().unwrap_or_default();
    let params = message.get("params").cloned().unwrap_or(Value::Null);
    let reply = match method {
        "initialize" => Ok(json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "beater.js", "version": env!("CARGO_PKG_VERSION")},
        })),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({"tools": tools_json(registry)})),
        "tools/call" => tools_call(registry, &params, &id).await,
        other => Err((-32601, format!("method not found: {other}"))),
    };

    let body = match reply {
        Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
        Err((code, message)) => {
            json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
        }
    };
    with_cors(http_response(StatusCode::OK, body), access, headers)
}

/// GET without a server-initiated SSE stream → 405, per spec.
pub fn handle_get(access: &AccessConfig, headers: &HeaderMap) -> Response<Body> {
    if !access.origin_allowed(headers) {
        return http_response(
            StatusCode::FORBIDDEN,
            json!({"jsonrpc": "2.0", "error": {"code": -32600, "message": "origin not allowed"}}),
        );
    }
    if !access.authorized(headers) {
        return with_cors(unauthorized_response(), access, headers);
    }
    with_cors(
        Response::builder()
            .status(StatusCode::METHOD_NOT_ALLOWED)
            .header("allow", "POST")
            .body(Body::from(
                "this MCP endpoint does not offer a server-initiated stream",
            ))
            .expect("static response"),
        access,
        headers,
    )
}

/// Browser MCP clients preflight Authorization + JSON requests.
pub fn handle_options(access: &AccessConfig, headers: &HeaderMap) -> Response<Body> {
    if !access.origin_allowed(headers) {
        return http_response(
            StatusCode::FORBIDDEN,
            json!({"jsonrpc": "2.0", "error": {"code": -32600, "message": "origin not allowed"}}),
        );
    }
    with_cors(
        Response::builder()
            .status(StatusCode::NO_CONTENT)
            .header("allow", "POST, GET, OPTIONS")
            .header(ACCESS_CONTROL_ALLOW_METHODS, "POST, GET, OPTIONS")
            .header(
                ACCESS_CONTROL_ALLOW_HEADERS,
                "authorization, content-type, accept, mcp-protocol-version, mcp-session-id",
            )
            .header(ACCESS_CONTROL_MAX_AGE, "600")
            .body(Body::empty())
            .expect("static response"),
        access,
        headers,
    )
}

fn tools_json(registry: &ToolRegistry) -> Value {
    Value::Array(
        registry
            .entries()
            .iter()
            .map(|t| {
                json!({
                    "name": t.name,
                    "description": t.description,
                    "inputSchema": t.input_schema,
                })
            })
            .collect(),
    )
}

async fn tools_call(
    registry: &ToolRegistry,
    params: &Value,
    id: &Value,
) -> Result<Value, (i64, String)> {
    let name = params["name"]
        .as_str()
        .ok_or((-32602, "tools/call requires params.name".to_string()))?;
    let arguments = params.get("arguments").cloned().unwrap_or(json!({}));
    if registry.get(name).is_none() {
        return Err((-32602, format!("unknown tool: {name}")));
    }
    // Tool failures are results with isError, not protocol errors.
    let context = ToolCallContext {
        tool_use_id: json_rpc_id_to_tool_use_id(id),
        idempotency_key: None,
    };
    match registry
        .execute_with_context(name, &arguments, &context)
        .await
    {
        Ok(result) => Ok(json!({
            "content": [{"type": "text", "text": result}],
            "isError": false,
        })),
        Err(e) => Ok(json!({
            "content": [{"type": "text", "text": format!("Error: {e:#}")}],
            "isError": true,
        })),
    }
}

fn json_rpc_id_to_tool_use_id(id: &Value) -> Option<String> {
    match id {
        Value::String(id) => Some(id.clone()),
        Value::Number(id) => Some(id.to_string()),
        _ => None,
    }
}

fn http_response(status: StatusCode, body: Value) -> Response<Body> {
    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .expect("static response")
}

fn unauthorized_response() -> Response<Body> {
    Response::builder()
        .status(StatusCode::UNAUTHORIZED)
        .header("content-type", "application/json")
        .header(WWW_AUTHENTICATE, "Bearer")
        .body(Body::from(
            json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32001, "message": "unauthorized"}})
                .to_string(),
        ))
        .expect("static response")
}

fn with_cors(
    mut response: Response<Body>,
    access: &AccessConfig,
    request_headers: &HeaderMap,
) -> Response<Body> {
    let Some(origin) = access.cors_origin(request_headers) else {
        return response;
    };
    let headers = response.headers_mut();
    headers.insert(ACCESS_CONTROL_ALLOW_ORIGIN, origin);
    headers.insert(VARY, HeaderValue::from_static("origin"));
    headers.insert(
        ACCESS_CONTROL_EXPOSE_HEADERS,
        HeaderValue::from_static("www-authenticate"),
    );
    response
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::path::Path;
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::{Duration, Instant};

    use axum::http::header::{AUTHORIZATION, ORIGIN};
    use axum::http::{HeaderMap, HeaderValue, StatusCode};

    use beater_agent::{ToolDecl, ToolRegistry};
    use serde_json::{Value, json};

    use super::{
        AccessConfig, handle_get, handle_options, handle_post, json_rpc_id_to_tool_use_id,
    };

    #[test]
    fn origin_policy_accepts_loopback_and_rejects_prefix_spoofing() {
        let mut headers = HeaderMap::new();
        headers.insert(ORIGIN, "http://localhost:3000".parse().unwrap());
        assert!(AccessConfig::default().origin_allowed(&headers));

        headers.insert(ORIGIN, "http://localhost.evil.test".parse().unwrap());
        assert!(!AccessConfig::default().origin_allowed(&headers));

        headers.insert(ORIGIN, "http://127.0.0.1.evil.test".parse().unwrap());
        assert!(!AccessConfig::default().origin_allowed(&headers));

        headers.insert(ORIGIN, "http://[::1]:3000".parse().unwrap());
        assert!(AccessConfig::default().origin_allowed(&headers));
    }

    #[test]
    fn origin_policy_accepts_explicit_trusted_remote_origins() {
        let access = AccessConfig::new(None, vec!["https://ops.example.com/".to_string()]);
        let mut headers = HeaderMap::new();
        headers.insert(ORIGIN, "https://ops.example.com".parse().unwrap());
        assert!(access.origin_allowed(&headers));

        headers.insert(ORIGIN, "https://evil.example.com".parse().unwrap());
        assert!(!access.origin_allowed(&headers));
    }

    #[test]
    fn origin_policy_rejects_malformed_and_non_origin_shapes() {
        let access = AccessConfig::default();
        for origin in [
            "null",
            "file:///tmp/app.html",
            "http://localhost/path",
            "http://localhost?x=1",
            "http://user@localhost",
            "ftp://localhost",
        ] {
            let mut headers = HeaderMap::new();
            headers.insert(ORIGIN, origin.parse().unwrap());
            assert!(!access.origin_allowed(&headers), "{origin}");
        }

        let mut headers = HeaderMap::new();
        headers.insert(ORIGIN, HeaderValue::from_bytes(b"\xff").unwrap());
        assert!(!access.origin_allowed(&headers));
    }

    #[tokio::test]
    async fn bearer_token_is_required_when_configured() {
        let registry = ToolRegistry::empty();
        let access = AccessConfig::new(Some("secret".to_string()), Vec::new());
        let headers = HeaderMap::new();

        let response = handle_post(
            &registry,
            &access,
            &headers,
            br#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
        )
        .await;

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            response.headers().get("www-authenticate").unwrap(),
            "Bearer"
        );
    }

    #[tokio::test]
    async fn bearer_token_allows_mcp_requests() {
        let registry = ToolRegistry::empty();
        let access = AccessConfig::new(Some("secret".to_string()), Vec::new());
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, "Bearer secret".parse().unwrap());

        let response = handle_post(
            &registry,
            &access,
            &headers,
            br#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn bearer_token_requires_exact_match() {
        let registry = ToolRegistry::empty();
        let access = AccessConfig::new(Some("secret".to_string()), Vec::new());
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, "Bearer secret ".parse().unwrap());

        let response = handle_post(
            &registry,
            &access,
            &headers,
            br#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
        )
        .await;

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn json_rpc_id_becomes_tool_context_id() {
        assert_eq!(
            json_rpc_id_to_tool_use_id(&json!("toolu_123")),
            Some("toolu_123".to_string())
        );
        assert_eq!(
            json_rpc_id_to_tool_use_id(&json!(42)),
            Some("42".to_string())
        );
        assert_eq!(json_rpc_id_to_tool_use_id(&json!({})), None);
    }

    #[tokio::test]
    async fn tools_call_forwards_json_rpc_id_to_remote_mcp_context() {
        let remote = MockRemoteMcp::new(json!({
            "jsonrpc": "2.0",
            "id": "mcp-call-1",
            "result": {
                "content": [{"type": "text", "text": "{\"ok\":true}"}],
                "isError": false
            }
        }));
        let registry = ToolRegistry::build(
            Path::new(""),
            &[remote_decl("crm.lookup", &remote.endpoint)],
        )
        .expect("remote MCP registry should build");

        let response = handle_post(
            &registry,
            &AccessConfig::default(),
            &HeaderMap::new(),
            br#"{"jsonrpc":"2.0","id":"mcp-call-1","method":"tools/call","params":{"name":"crm.lookup","arguments":{"email":"a@example.com"}}}"#,
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);
        let requests = remote.requests();
        assert_eq!(requests.len(), 1);
        assert!(
            requests[0]
                .headers
                .to_ascii_lowercase()
                .contains("idempotency-key: mcp-call-1"),
            "{}",
            requests[0].headers
        );
        let body: Value = serde_json::from_str(&requests[0].body).unwrap();
        assert_eq!(body["id"], "mcp-call-1");
        assert_eq!(body["params"]["name"], "lookup_contact");
        assert_eq!(body["params"]["arguments"]["email"], "a@example.com");
    }

    #[test]
    fn get_requires_auth_before_reporting_no_stream() {
        let access = AccessConfig::new(Some("secret".to_string()), Vec::new());
        let headers = HeaderMap::new();
        assert_eq!(
            handle_get(&access, &headers).status(),
            StatusCode::UNAUTHORIZED
        );

        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, "Bearer secret".parse().unwrap());
        assert_eq!(
            handle_get(&access, &headers).status(),
            StatusCode::METHOD_NOT_ALLOWED
        );
    }

    #[test]
    fn options_allows_trusted_browser_preflight_without_bearer_token() {
        let access = AccessConfig::new(
            Some("secret".to_string()),
            vec!["https://ops.example.com".to_string()],
        );
        let mut headers = HeaderMap::new();
        headers.insert(ORIGIN, "https://ops.example.com".parse().unwrap());

        let response = handle_options(&access, &headers);

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert_eq!(
            response
                .headers()
                .get("access-control-allow-origin")
                .unwrap(),
            "https://ops.example.com"
        );
        assert!(
            response
                .headers()
                .get("access-control-allow-headers")
                .unwrap()
                .to_str()
                .unwrap()
                .contains("authorization")
        );
    }

    #[test]
    fn options_rejects_untrusted_browser_origins() {
        let access = AccessConfig::new(
            Some("secret".to_string()),
            vec!["https://ops.example.com".to_string()],
        );
        let mut headers = HeaderMap::new();
        headers.insert(ORIGIN, "https://evil.example.com".parse().unwrap());

        let response = handle_options(&access, &headers);

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert!(
            response
                .headers()
                .get("access-control-allow-origin")
                .is_none()
        );
    }

    fn remote_decl(name: &str, endpoint: &str) -> ToolDecl {
        let egress = endpoint
            .strip_prefix("http://")
            .expect("test endpoint should be http")
            .split('/')
            .next()
            .unwrap();
        serde_json::from_value(json!({
            "kind": "remote_mcp",
            "name": name,
            "description": "Look up a CRM contact.",
            "inputSchema": {
                "type": "object",
                "properties": {"email": {"type": "string"}},
                "required": ["email"]
            },
            "endpoint": endpoint,
            "tool": "lookup_contact",
            "timeoutMs": 1000,
            "idempotent": false,
            "retry": {"attempts": 2, "backoffMs": 1, "idempotencyKey": "tool_use_id"},
            "egress": [egress]
        }))
        .unwrap()
    }

    #[derive(Clone)]
    struct MockRequest {
        headers: String,
        body: String,
    }

    struct MockRemoteMcp {
        endpoint: String,
        requests: Arc<Mutex<Vec<MockRequest>>>,
        handle: Option<thread::JoinHandle<()>>,
    }

    impl MockRemoteMcp {
        fn new(response: Value) -> Self {
            let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
            listener.set_nonblocking(true).unwrap();
            let endpoint = format!(
                "http://127.0.0.1:{}/mcp",
                listener.local_addr().unwrap().port()
            );
            let requests = Arc::new(Mutex::new(Vec::new()));
            let thread_requests = requests.clone();
            let body = response.to_string();
            let handle = thread::spawn(move || {
                let (mut stream, _) = accept_with_deadline(&listener);
                let request = read_request(&mut stream);
                thread_requests.lock().unwrap().push(request);
                let _ = write_response(&mut stream, &body);
            });
            Self {
                endpoint,
                requests,
                handle: Some(handle),
            }
        }

        fn requests(&self) -> Vec<MockRequest> {
            self.requests.lock().unwrap().clone()
        }
    }

    impl Drop for MockRemoteMcp {
        fn drop(&mut self) {
            if let Some(handle) = self.handle.take() {
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

    fn write_response(stream: &mut TcpStream, body: &str) -> std::io::Result<()> {
        write!(
            stream,
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        )
    }
}
