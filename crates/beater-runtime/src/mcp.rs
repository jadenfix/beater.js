//! MCP server endpoint (spec 2025-11-25, Streamable HTTP, stateless).
//!
//! Deliberately minimal and hand-rolled: a compliant server needs one
//! endpoint that accepts POST JSON-RPC, validates Origin (DNS-rebinding
//! defense), optionally requires bearer-token auth for remote management,
//! and MAY answer GET with 405 when it offers no server-initiated SSE stream.
//! No sessions (`MCP-Session-Id` is a MAY), no SSE — every request gets a
//! single JSON object back.

use std::fmt;
use std::future::Future;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::path::Path;

use axum::body::Body;
use axum::http::header::{
    ACCESS_CONTROL_ALLOW_HEADERS, ACCESS_CONTROL_ALLOW_METHODS, ACCESS_CONTROL_ALLOW_ORIGIN,
    ACCESS_CONTROL_EXPOSE_HEADERS, ACCESS_CONTROL_MAX_AGE, AUTHORIZATION, ORIGIN, VARY,
    WWW_AUTHENTICATE,
};
use axum::http::{HeaderMap, HeaderValue, Response, StatusCode};
use deno_core::url::{Host, Url};
use serde_json::{Value, json};
use uuid::Uuid;

use beater_agent::{
    Journal, ToolNeedsReview, ToolRegistry, complete_journaled_tool_call, fail_journaled_tool_call,
    start_journaled_tool_call,
};

#[derive(Clone, Debug, PartialEq)]
pub struct RouteActionTool {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    pub method: String,
    pub path: String,
    pub side_effect: String,
    pub confirm: bool,
    pub dry_run: bool,
    pub idempotency_required: bool,
    pub auth: Value,
}

pub const PROTOCOL_VERSION: &str = "2025-11-25";
pub const DEFAULT_TOKEN_ENV: &str = "BEATER_MCP_TOKEN";
pub const DEFAULT_TRUSTED_ORIGINS_ENV: &str = "BEATER_MCP_TRUSTED_ORIGINS";
pub const AETHER_PAYMENT_HEADER: &str = "x-payment";
pub const AETHER_PAYMENT_HASH_HEADER: &str = "x-aether-payment-hash";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PaymentHeaders {
    pub payment: Option<String>,
    pub payment_hash: Option<String>,
}

impl PaymentHeaders {
    fn from_headers(headers: &HeaderMap) -> Self {
        Self {
            payment: header_to_string(headers, AETHER_PAYMENT_HEADER),
            payment_hash: header_to_string(headers, AETHER_PAYMENT_HASH_HEADER),
        }
    }

    pub fn insert_into(&self, headers: &mut std::collections::HashMap<String, String>) {
        if let Some(payment) = &self.payment {
            headers.insert(AETHER_PAYMENT_HEADER.to_string(), payment.clone());
        }
        if let Some(payment_hash) = &self.payment_hash {
            headers.insert(AETHER_PAYMENT_HASH_HEADER.to_string(), payment_hash.clone());
        }
    }
}

/// MCP access policy. By default the endpoint remains local-dev friendly:
/// non-browser clients may omit Origin and no token is required. Set
/// BEATER_MCP_TOKEN before binding beyond loopback.
#[derive(Clone, Default, Eq, PartialEq)]
pub struct AccessConfig {
    bearer_token: Option<String>,
    trusted_origins: Vec<String>,
}

impl fmt::Debug for AccessConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AccessConfig")
            .field(
                "bearer_token",
                &self.bearer_token.as_ref().map(|_| "<redacted>"),
            )
            .field("trusted_origins", &self.trusted_origins)
            .finish()
    }
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

    pub fn authorized(&self, headers: &HeaderMap) -> bool {
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
    const MAX_TOKEN_BYTES: usize = 4096;
    if left.len() > MAX_TOKEN_BYTES || right.len() > MAX_TOKEN_BYTES {
        return false;
    }
    let mut diff = left.len() ^ right.len();
    for index in 0..MAX_TOKEN_BYTES {
        let left_byte = left.get(index).copied().unwrap_or(0);
        let right_byte = right.get(index).copied().unwrap_or(0);
        diff |= usize::from(left_byte ^ right_byte);
    }
    diff == 0
}

fn header_to_string(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

pub async fn handle_post(
    registry: &ToolRegistry,
    route_actions: &[RouteActionTool],
    access: &AccessConfig,
    app_dir: &Path,
    headers: &HeaderMap,
    body: &[u8],
    route_executor: impl Fn(
        RouteActionTool,
        Value,
        beater_agent::ToolCallContext,
        PaymentHeaders,
    )
        -> std::pin::Pin<Box<dyn Future<Output = anyhow::Result<String>> + Send>>,
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

    if !message.is_object() {
        return with_cors(
            http_response(
                StatusCode::BAD_REQUEST,
                json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32600, "message": "invalid request: JSON-RPC message must be an object"}}),
            ),
            access,
            headers,
        );
    }

    if !message.get("method").is_some_and(Value::is_string) {
        return with_cors(
            http_response(
                StatusCode::BAD_REQUEST,
                json!({"jsonrpc": "2.0", "id": message.get("id").cloned().unwrap_or(Value::Null), "error": {"code": -32600, "message": "invalid request: method must be a string"}}),
            ),
            access,
            headers,
        );
    }

    // Notifications and client responses carry no `id` -> 202 Accepted, no body.
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
    let payment_headers = PaymentHeaders::from_headers(headers);
    let reply = match method {
        "initialize" => Ok(json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "beater.js", "version": env!("CARGO_PKG_VERSION")},
        })),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({"tools": tools_json(registry, route_actions)})),
        "tools/call" => {
            tools_call(
                registry,
                route_actions,
                app_dir,
                &params,
                payment_headers,
                route_executor,
            )
            .await
        }
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
                "authorization, content-type, accept, mcp-protocol-version, mcp-session-id, x-payment, x-aether-payment-hash",
            )
            .header(ACCESS_CONTROL_MAX_AGE, "600")
            .body(Body::empty())
            .expect("static response"),
        access,
        headers,
    )
}

fn tools_json(registry: &ToolRegistry, route_actions: &[RouteActionTool]) -> Value {
    let mut tools: Vec<Value> = registry
        .entries()
        .iter()
        .map(|t| {
            json!({
                "name": t.name,
                "description": t.description,
                "inputSchema": t.input_schema,
            })
        })
        .collect();
    tools.extend(route_actions.iter().map(|action| {
        json!({
            "name": action.name,
            "description": action.description,
            "inputSchema": action.input_schema,
            "x-beater-action": {
                "method": action.method,
                "path": action.path,
                "sideEffect": action.side_effect,
                "confirm": action.confirm,
                "dryRun": action.dry_run,
                "idempotencyRequired": action.idempotency_required,
                "auth": action.auth,
            }
        })
    }));
    Value::Array(tools)
}

async fn tools_call(
    registry: &ToolRegistry,
    route_actions: &[RouteActionTool],
    app_dir: &Path,
    params: &Value,
    payment_headers: PaymentHeaders,
    route_executor: impl Fn(
        RouteActionTool,
        Value,
        beater_agent::ToolCallContext,
        PaymentHeaders,
    )
        -> std::pin::Pin<Box<dyn Future<Output = anyhow::Result<String>> + Send>>,
) -> Result<Value, (i64, String)> {
    let name = params["name"]
        .as_str()
        .ok_or((-32602, "tools/call requires params.name".to_string()))?;
    let arguments = params.get("arguments").cloned().unwrap_or(json!({}));
    let route_action = if registry.get(name).is_none() {
        route_actions
            .iter()
            .find(|action| action.name == name)
            .cloned()
    } else {
        None
    };
    if registry.get(name).is_none() && route_action.is_none() {
        return Err((-32602, format!("unknown tool: {name}")));
    }
    // Tool failures are results with isError, not protocol errors.
    let tool_use_id = mcp_tool_use_id();
    let run_id = mcp_run_id();
    let idempotency_key = Some(mcp_tool_idempotency_key(&run_id, &tool_use_id));
    let call = {
        let journal = open_journal(app_dir)?;
        journal
            .create_run(
                &run_id,
                "mcp",
                &json!({
                    "method": "tools/call",
                    "name": name,
                    "arguments": arguments,
                    "tool_use_id": tool_use_id,
                })
                .to_string(),
            )
            .map_err(journal_error)?;
        start_journaled_tool_call(
            &journal,
            &run_id,
            name,
            &tool_use_id,
            &arguments,
            1,
            idempotency_key,
        )
        .map_err(journal_error)?
    };
    let seq = call.seq;
    let context = call.context;
    let result = if let Some(action) = route_action {
        route_executor(
            action,
            arguments.clone(),
            context.clone(),
            payment_headers.clone(),
        )
        .await
    } else {
        registry
            .execute_with_context(name, &arguments, &context)
            .await
    };
    if let Err(error) = registry.close_browser_sessions(&run_id).await {
        tracing::warn!("browser session cleanup for MCP run {run_id} failed: {error:#}");
    }
    match result {
        Ok(result) => {
            let journal = open_journal(app_dir)?;
            complete_journaled_tool_call(&journal, &run_id, seq, &result).map_err(journal_error)?;
            journal
                .set_run_status(&run_id, "completed")
                .map_err(journal_error)?;
            Ok(json!({
                "content": [{"type": "text", "text": result}],
                "isError": false,
            }))
        }
        Err(e) => {
            let status = if e.downcast_ref::<ToolNeedsReview>().is_some() {
                "needs_review"
            } else {
                "failed"
            };
            let journal = open_journal(app_dir)?;
            fail_journaled_tool_call(&journal, &run_id, seq, &format!("{e:#}"))
                .map_err(journal_error)?;
            journal
                .set_run_status(&run_id, status)
                .map_err(journal_error)?;
            Ok(json!({
                "content": [{"type": "text", "text": format!("Error: {e:#}")}],
                "isError": true,
            }))
        }
    }
}

fn mcp_tool_use_id() -> String {
    format!("beater:mcp:{}", Uuid::new_v4())
}

fn mcp_run_id() -> String {
    Uuid::new_v4().to_string()
}

fn mcp_tool_idempotency_key(run_id: &str, tool_use_id: &str) -> String {
    format!("beater:{run_id}:tool:{tool_use_id}")
}

fn open_journal(app_dir: &Path) -> Result<Journal, (i64, String)> {
    Journal::open(app_dir).map_err(journal_error)
}

fn journal_error(error: anyhow::Error) -> (i64, String) {
    (-32000, format!("journal error: {error:#}"))
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
        HeaderValue::from_static("www-authenticate, x-payment, x-aether-payment-hash"),
    );
    response
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::future::Future;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::path::{Path, PathBuf};
    use std::pin::Pin;
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::{Duration, Instant};

    use axum::body::Body;
    use axum::http::header::{AUTHORIZATION, ORIGIN};
    use axum::http::{HeaderMap, HeaderValue, StatusCode};

    use beater_agent::{Journal, ToolDecl, ToolRegistry};
    use serde_json::{Value, json};

    use super::{
        AETHER_PAYMENT_HASH_HEADER, AETHER_PAYMENT_HEADER, AccessConfig, PaymentHeaders,
        RouteActionTool, handle_get, handle_options, handle_post, mcp_tool_use_id,
    };

    struct TempApp {
        path: PathBuf,
    }

    impl TempApp {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "beater-mcp-{name}-{}-{}",
                std::process::id(),
                uuid::Uuid::new_v4()
            ));
            fs::create_dir_all(&path).unwrap();
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

    async fn test_handle_post(
        registry: &ToolRegistry,
        access: &AccessConfig,
        app_dir: &Path,
        headers: &HeaderMap,
        body: &[u8],
    ) -> axum::http::Response<Body> {
        handle_post(
            registry,
            &[],
            access,
            app_dir,
            headers,
            body,
            |_action, _arguments, _context, _payment_headers| {
                Pin::from(Box::new(async {
                    anyhow::bail!("test did not configure route action execution")
                })
                    as Box<dyn Future<Output = anyhow::Result<String>> + Send>)
            },
        )
        .await
    }

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
        let app = TempApp::new("auth-required");
        let registry = ToolRegistry::empty();
        let access = AccessConfig::new(Some("secret".to_string()), Vec::new());
        let headers = HeaderMap::new();

        let response = test_handle_post(
            &registry,
            &access,
            app.path(),
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
        let app = TempApp::new("auth-allows");
        let registry = ToolRegistry::empty();
        let access = AccessConfig::new(Some("secret".to_string()), Vec::new());
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, "Bearer secret".parse().unwrap());

        let response = test_handle_post(
            &registry,
            &access,
            app.path(),
            &headers,
            br#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn bearer_token_requires_exact_match() {
        let app = TempApp::new("auth-exact");
        let registry = ToolRegistry::empty();
        let access = AccessConfig::new(Some("secret".to_string()), Vec::new());
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, "Bearer secret ".parse().unwrap());

        let response = test_handle_post(
            &registry,
            &access,
            app.path(),
            &headers,
            br#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
        )
        .await;

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn debug_redacts_bearer_token() {
        let access = AccessConfig::new(
            Some("super-secret-token".to_string()),
            vec!["https://ops.example.test".to_string()],
        );

        let rendered = format!("{access:?}");

        assert!(rendered.contains("<redacted>"), "{rendered}");
        assert!(!rendered.contains("super-secret-token"), "{rendered}");
        assert!(rendered.contains("https://ops.example.test"), "{rendered}");
    }

    #[tokio::test]
    async fn non_object_json_rpc_is_invalid_request() {
        let app = TempApp::new("invalid-non-object");
        let registry = ToolRegistry::empty();

        for body in [
            br#"[]"#.as_slice(),
            br#""hello""#.as_slice(),
            br#"5"#.as_slice(),
        ] {
            let response = test_handle_post(
                &registry,
                &AccessConfig::default(),
                app.path(),
                &HeaderMap::new(),
                body,
            )
            .await;

            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
            let body = response_json(response).await;
            assert_eq!(body["id"], Value::Null);
            assert_eq!(body["error"]["code"], -32600);
        }
    }

    #[tokio::test]
    async fn missing_or_non_string_method_is_invalid_request() {
        let app = TempApp::new("invalid-method");
        let registry = ToolRegistry::empty();

        for body in [
            br#"{"jsonrpc":"2.0","id":1}"#.as_slice(),
            br#"{"jsonrpc":"2.0","id":1,"method":7}"#.as_slice(),
        ] {
            let response = test_handle_post(
                &registry,
                &AccessConfig::default(),
                app.path(),
                &HeaderMap::new(),
                body,
            )
            .await;

            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
            let body = response_json(response).await;
            assert_eq!(body["id"], 1);
            assert_eq!(body["error"]["code"], -32600);
        }
    }

    #[test]
    fn mcp_tool_use_id_is_namespaced_and_unique() {
        let first = mcp_tool_use_id();
        let second = mcp_tool_use_id();

        assert!(first.starts_with("beater:mcp:"), "{first}");
        assert!(second.starts_with("beater:mcp:"), "{second}");
        assert_ne!(first, second);
        uuid::Uuid::parse_str(first.trim_start_matches("beater:mcp:"))
            .expect("mcp tool use id should contain a UUID");
    }

    #[tokio::test]
    async fn tools_call_uses_unique_namespaced_remote_mcp_context_ids() {
        let app = TempApp::new("remote-context-ids");
        let remote = MockRemoteMcp::new_many(vec![
            json!({
            "jsonrpc": "2.0",
            "id": null,
            "result": {
                "content": [{"type": "text", "text": "{\"ok\":true}"}],
                "isError": false
            }
            }),
            json!({
                "jsonrpc": "2.0",
                "id": null,
                "result": {
                    "content": [{"type": "text", "text": "{\"ok\":true}"}],
                    "isError": false
                }
            }),
        ]);
        let registry = ToolRegistry::build(
            Path::new(""),
            &[remote_decl("crm.lookup", &remote.endpoint)],
        )
        .expect("remote MCP registry should build");

        let first_response = test_handle_post(
            &registry,
            &AccessConfig::default(),
            app.path(),
            &HeaderMap::new(),
            br#"{"jsonrpc":"2.0","id":"1","method":"tools/call","params":{"name":"crm.lookup","arguments":{"email":"a@example.com"}}}"#,
        )
        .await;
        let second_response = test_handle_post(
            &registry,
            &AccessConfig::default(),
            app.path(),
            &HeaderMap::new(),
            br#"{"jsonrpc":"2.0","id":"1","method":"tools/call","params":{"name":"crm.lookup","arguments":{"email":"b@example.com"}}}"#,
        )
        .await;

        assert_eq!(first_response.status(), StatusCode::OK);
        assert_eq!(second_response.status(), StatusCode::OK);
        assert_eq!(response_json(first_response).await["id"], "1");
        assert_eq!(response_json(second_response).await["id"], "1");

        let requests = remote.requests();
        assert_eq!(requests.len(), 2);

        let first_body: Value = serde_json::from_str(&requests[0].body).unwrap();
        let second_body: Value = serde_json::from_str(&requests[1].body).unwrap();
        let first_id = first_body["id"].as_str().unwrap();
        let second_id = second_body["id"].as_str().unwrap();
        let first_key = header_value(&requests[0].headers, "idempotency-key").unwrap();
        let second_key = header_value(&requests[1].headers, "idempotency-key").unwrap();

        assert!(first_id.starts_with("beater:mcp:"), "{first_id}");
        assert!(second_id.starts_with("beater:mcp:"), "{second_id}");
        assert_ne!(first_id, "1");
        assert_ne!(second_id, "1");
        assert_ne!(first_id, second_id);
        assert!(first_key.starts_with("beater:"), "{first_key}");
        assert!(second_key.starts_with("beater:"), "{second_key}");
        assert!(first_key.ends_with(first_id), "{first_key}");
        assert!(second_key.ends_with(second_id), "{second_key}");
        assert_ne!(first_key, first_id);
        assert_ne!(second_key, second_id);
        assert_ne!(first_key, second_key);
        assert_eq!(first_body["params"]["name"], "lookup_contact");
        assert_eq!(second_body["params"]["name"], "lookup_contact");
        assert_eq!(first_body["params"]["arguments"]["email"], "a@example.com");
        assert_eq!(second_body["params"]["arguments"]["email"], "b@example.com");
    }

    #[tokio::test]
    async fn tools_call_forwards_aether_payment_headers_to_route_actions() {
        let app = TempApp::new("route-action-payment-headers");
        let registry = ToolRegistry::empty();
        let route_actions = vec![RouteActionTool {
            name: "billing.checkout".to_string(),
            description: "Checkout".to_string(),
            input_schema: json!({"type": "object"}),
            method: "POST".to_string(),
            path: "/api/checkout".to_string(),
            side_effect: "purchase".to_string(),
            confirm: false,
            dry_run: false,
            idempotency_required: false,
            auth: Value::Null,
        }];
        let seen = Arc::new(Mutex::new(PaymentHeaders::default()));
        let seen_for_executor = Arc::clone(&seen);
        let mut headers = HeaderMap::new();
        headers.insert(AETHER_PAYMENT_HEADER, "payment-payload".parse().unwrap());
        headers.insert(AETHER_PAYMENT_HASH_HEADER, "0x1234".parse().unwrap());

        let response = handle_post(
            &registry,
            &route_actions,
            &AccessConfig::default(),
            app.path(),
            &headers,
            br#"{"jsonrpc":"2.0","id":"1","method":"tools/call","params":{"name":"billing.checkout","arguments":{"confirm":true}}}"#,
            move |_action, _arguments, _context, payment_headers| {
                let seen = Arc::clone(&seen_for_executor);
                Pin::from(Box::new(async move {
                    *seen.lock().unwrap() = payment_headers;
                    Ok("{\"ok\":true}".to_string())
                }) as Box<dyn Future<Output = anyhow::Result<String>> + Send>)
            },
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response_json(response).await["result"]["isError"], false);
        let payment_headers = seen.lock().unwrap().clone();
        assert_eq!(payment_headers.payment.as_deref(), Some("payment-payload"));
        assert_eq!(payment_headers.payment_hash.as_deref(), Some("0x1234"));
    }

    #[tokio::test]
    async fn tools_call_journals_successful_tool_execution() {
        let app = TempApp::new("journal-success");
        let registry = ToolRegistry::build(Path::new(""), &[rust_decl("get_time")])
            .expect("rust builtin registry should build");

        let response = test_handle_post(
            &registry,
            &AccessConfig::default(),
            app.path(),
            &HeaderMap::new(),
            br#"{"jsonrpc":"2.0","id":"1","method":"tools/call","params":{"name":"get_time","arguments":{}}}"#,
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);
        let body = response_json(response).await;
        assert_eq!(body["id"], "1");
        assert_eq!(body["result"]["isError"], false);

        let journal = Journal::open(app.path()).unwrap();
        let runs = journal.list_runs().unwrap();
        assert_eq!(runs.len(), 1);
        let (run, step_count) = &runs[0];
        assert_eq!(run.agent, "mcp");
        assert_eq!(run.status, "completed");
        assert_eq!(*step_count, 1);
        assert!(run.input.contains("\"method\":\"tools/call\""));
        assert!(run.input.contains("\"name\":\"get_time\""));

        let steps = journal.steps(&run.id).unwrap();
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].kind, "tool_call");
        assert_eq!(steps[0].status, "completed");
        assert_eq!(steps[0].tool_name.as_deref(), Some("get_time"));
        let tool_use_id = steps[0].tool_use_id.as_deref().unwrap();
        assert!(tool_use_id.starts_with("beater:mcp:"), "{tool_use_id}");
        assert_eq!(steps[0].request["name"], "get_time");
        assert_eq!(steps[0].request["tool_use_id"], tool_use_id);
        assert_eq!(
            steps[0].request["idempotency_key"],
            format!("beater:{}:tool:{tool_use_id}", run.id)
        );
        assert!(steps[0].result.as_ref().unwrap()["content"].is_string());
    }

    #[tokio::test]
    async fn tools_call_journals_failed_tool_execution() {
        let app = TempApp::new("journal-failed");
        let remote = MockRemoteMcp::new_many(vec![json!({
            "jsonrpc": "2.0",
            "id": null,
            "result": {
                "content": [{"type": "text", "text": "denied"}],
                "isError": true
            }
        })]);
        let registry = ToolRegistry::build(
            Path::new(""),
            &[remote_decl_with_idempotent(
                "crm.lookup",
                &remote.endpoint,
                true,
            )],
        )
        .expect("remote MCP registry should build");

        let response = test_handle_post(
            &registry,
            &AccessConfig::default(),
            app.path(),
            &HeaderMap::new(),
            br#"{"jsonrpc":"2.0","id":"1","method":"tools/call","params":{"name":"crm.lookup","arguments":{"email":"blocked@example.com"}}}"#,
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);
        let body = response_json(response).await;
        assert_eq!(body["id"], "1");
        assert_eq!(body["result"]["isError"], true);
        assert!(
            body["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("returned isError"),
            "{body}"
        );

        let journal = Journal::open(app.path()).unwrap();
        let runs = journal.list_runs().unwrap();
        assert_eq!(runs.len(), 1);
        let (run, step_count) = &runs[0];
        assert_eq!(run.agent, "mcp");
        assert_eq!(run.status, "failed");
        assert_eq!(*step_count, 1);

        let steps = journal.steps(&run.id).unwrap();
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].kind, "tool_call");
        assert_eq!(steps[0].status, "failed");
        assert_eq!(steps[0].tool_name.as_deref(), Some("crm.lookup"));
        assert!(
            steps[0].result.as_ref().unwrap()["error"]
                .as_str()
                .unwrap()
                .contains("returned isError"),
            "{:?}",
            steps[0].result
        );
    }

    #[tokio::test]
    async fn tools_call_journals_needs_review_tool_execution() {
        let app = TempApp::new("journal-needs-review");
        let remote = MockRemoteMcp::new_text("{not json");
        let registry = ToolRegistry::build(
            Path::new(""),
            &[remote_decl_with_idempotent(
                "crm.lookup",
                &remote.endpoint,
                false,
            )],
        )
        .expect("remote MCP registry should build");

        let response = test_handle_post(
            &registry,
            &AccessConfig::default(),
            app.path(),
            &HeaderMap::new(),
            br#"{"jsonrpc":"2.0","id":"1","method":"tools/call","params":{"name":"crm.lookup","arguments":{"email":"ambiguous@example.com"}}}"#,
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);
        let body = response_json(response).await;
        assert_eq!(body["id"], "1");
        assert_eq!(body["result"]["isError"], true);
        assert!(
            body["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("needs review")
                || body["result"]["content"][0]["text"]
                    .as_str()
                    .unwrap()
                    .contains("review the remote system"),
            "{body}"
        );

        let journal = Journal::open(app.path()).unwrap();
        let runs = journal.list_runs().unwrap();
        assert_eq!(runs.len(), 1);
        let (run, step_count) = &runs[0];
        assert_eq!(run.agent, "mcp");
        assert_eq!(run.status, "needs_review");
        assert_eq!(*step_count, 1);

        let steps = journal.steps(&run.id).unwrap();
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].kind, "tool_call");
        assert_eq!(steps[0].status, "failed");
        assert_eq!(steps[0].tool_name.as_deref(), Some("crm.lookup"));
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
        let allow_headers = response
            .headers()
            .get("access-control-allow-headers")
            .unwrap()
            .to_str()
            .unwrap();
        assert!(allow_headers.contains("authorization"));
        assert!(allow_headers.contains(AETHER_PAYMENT_HEADER));
        assert!(allow_headers.contains(AETHER_PAYMENT_HASH_HEADER));
        let expose_headers = response
            .headers()
            .get("access-control-expose-headers")
            .unwrap()
            .to_str()
            .unwrap();
        assert!(expose_headers.contains(AETHER_PAYMENT_HEADER));
        assert!(expose_headers.contains(AETHER_PAYMENT_HASH_HEADER));
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

    fn rust_decl(name: &str) -> ToolDecl {
        serde_json::from_value(json!({
            "kind": "rust",
            "name": name,
            "idempotent": true,
        }))
        .unwrap()
    }

    fn remote_decl(name: &str, endpoint: &str) -> ToolDecl {
        remote_decl_with_idempotent(name, endpoint, false)
    }

    fn remote_decl_with_idempotent(name: &str, endpoint: &str, idempotent: bool) -> ToolDecl {
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
            "idempotent": idempotent,
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
        fn new_many(responses: Vec<Value>) -> Self {
            let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
            listener.set_nonblocking(true).unwrap();
            let endpoint = format!(
                "http://127.0.0.1:{}/mcp",
                listener.local_addr().unwrap().port()
            );
            let requests = Arc::new(Mutex::new(Vec::new()));
            let thread_requests = requests.clone();
            let handle = thread::spawn(move || {
                for response in responses {
                    let (mut stream, _) = accept_with_deadline(&listener);
                    let request = read_request(&mut stream);
                    let request_body: Value = serde_json::from_str(&request.body).unwrap();
                    let mut response = response;
                    response["id"] = request_body["id"].clone();
                    let body = response.to_string();
                    thread_requests.lock().unwrap().push(request);
                    let _ = write_response(&mut stream, &body);
                }
            });
            Self {
                endpoint,
                requests,
                handle: Some(handle),
            }
        }

        fn new_text(response: &'static str) -> Self {
            let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
            listener.set_nonblocking(true).unwrap();
            let endpoint = format!(
                "http://127.0.0.1:{}/mcp",
                listener.local_addr().unwrap().port()
            );
            let requests = Arc::new(Mutex::new(Vec::new()));
            let thread_requests = requests.clone();
            let handle = thread::spawn(move || {
                let (mut stream, _) = accept_with_deadline(&listener);
                let request = read_request(&mut stream);
                thread_requests.lock().unwrap().push(request);
                let _ = write_response(&mut stream, response);
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

    async fn response_json(response: axum::http::Response<axum::body::Body>) -> Value {
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    fn header_value(headers: &str, name: &str) -> Option<String> {
        headers.lines().find_map(|line| {
            let (header_name, header_value) = line.split_once(':')?;
            header_name
                .eq_ignore_ascii_case(name)
                .then(|| header_value.trim().to_string())
        })
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
