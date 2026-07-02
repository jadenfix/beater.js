//! MCP server endpoint (spec 2025-11-25, Streamable HTTP, stateless).
//!
//! Deliberately minimal and hand-rolled: a compliant server needs one
//! endpoint that accepts POST JSON-RPC, validates Origin (DNS-rebinding
//! defense), and MAY answer GET with 405 when it offers no server-initiated
//! SSE stream. No sessions (`MCP-Session-Id` is a MAY), no SSE — every
//! request gets a single JSON object back.

use axum::body::Body;
use axum::http::{HeaderMap, Response, StatusCode};
use serde_json::{Value, json};

use beater_agent::ToolRegistry;

pub const PROTOCOL_VERSION: &str = "2025-11-25";

/// Origin MUST be validated when present; local dev accepts localhost only.
pub fn origin_allowed(headers: &HeaderMap) -> bool {
    match headers.get("origin").and_then(|v| v.to_str().ok()) {
        None => true, // non-browser clients (curl, inspector CLI) send no Origin
        Some(origin) => {
            origin.starts_with("http://localhost")
                || origin.starts_with("https://localhost")
                || origin.starts_with("http://127.0.0.1")
                || origin.starts_with("https://127.0.0.1")
        }
    }
}

pub async fn handle_post(
    registry: &ToolRegistry,
    headers: &HeaderMap,
    body: &[u8],
) -> Response<Body> {
    if !origin_allowed(headers) {
        return http_response(
            StatusCode::FORBIDDEN,
            json!({"jsonrpc": "2.0", "error": {"code": -32600, "message": "origin not allowed"}}),
        );
    }
    let message: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(e) => {
            return http_response(
                StatusCode::BAD_REQUEST,
                json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": format!("parse error: {e}")}}),
            );
        }
    };

    // Notifications and client responses carry no `id` → 202 Accepted, no body.
    let Some(id) = message.get("id").filter(|id| !id.is_null()).cloned() else {
        return Response::builder()
            .status(StatusCode::ACCEPTED)
            .body(Body::empty())
            .expect("static response");
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
    http_response(StatusCode::OK, body)
}

/// GET without a server-initiated SSE stream → 405, per spec.
pub fn handle_get() -> Response<Body> {
    Response::builder()
        .status(StatusCode::METHOD_NOT_ALLOWED)
        .header("allow", "POST")
        .body(Body::from(
            "this MCP endpoint does not offer a server-initiated stream",
        ))
        .expect("static response")
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
