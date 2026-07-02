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

use beater_agent::ToolRegistry;

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
        "tools/call" => tools_call(registry, &params).await,
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

async fn tools_call(registry: &ToolRegistry, params: &Value) -> Result<Value, (i64, String)> {
    let name = params["name"]
        .as_str()
        .ok_or((-32602, "tools/call requires params.name".to_string()))?;
    let arguments = params.get("arguments").cloned().unwrap_or(json!({}));
    if registry.get(name).is_none() {
        return Err((-32602, format!("unknown tool: {name}")));
    }
    // Tool failures are results with isError, not protocol errors.
    match registry.execute(name, &arguments).await {
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
    use axum::http::header::{AUTHORIZATION, ORIGIN};
    use axum::http::{HeaderMap, HeaderValue, StatusCode};

    use beater_agent::ToolRegistry;

    use super::{AccessConfig, handle_get, handle_options, handle_post};

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
}
