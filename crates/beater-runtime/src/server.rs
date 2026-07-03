//! The dev server: axum in front, JS worker behind a swappable channel,
//! notify-based hot reload, plus the agent surfaces — /mcp and the
//! generated crawl layer (robots.txt, sitemap.xml, llms.txt, .well-known).

use std::collections::HashMap;
use std::convert::Infallible;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Context, Result};
use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, Request, Response, StatusCode};
use axum::middleware;
use axum::routing::{Router, get, post};
use beater_agent::ToolRegistry;
use bytes::Bytes;
use futures_util::stream;
use serde_json::json;
use tokio::sync::{RwLock, mpsc, oneshot};
use tokio_stream::wrappers::UnboundedReceiverStream;

use crate::config::AppConfig;
use crate::loader;
use crate::router::{RouteKind, RouteTable, Segment};
use crate::worker::{self, RouteBody, RouteMeta, WorkerMsg};
use crate::{crawl, mcp};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_BODY_BYTES: usize = 8 * 1024 * 1024;
const CLIENT_MODULE_PREFIX: &str = "/_beater/client/";
const RSC_FLIGHT_PREFIX: &str = "/_beater/rsc/";
static NEXT_REQUEST_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone)]
struct DevState {
    routes: Arc<RwLock<RouteTable>>,
    worker_tx: Arc<RwLock<mpsc::Sender<WorkerMsg>>>,
    registry: Arc<ToolRegistry>,
    app_name: String,
    agents: Arc<Vec<String>>,
    base_url: String,
    mcp_access: mcp::AccessConfig,
    app_dir: PathBuf,
}

pub async fn serve(
    config: AppConfig,
    host: std::net::IpAddr,
    port: u16,
    base_url: String,
    registry: ToolRegistry,
    agents: Vec<String>,
    allow_unauthenticated_remote: bool,
) -> Result<()> {
    let table = RouteTable::scan(&config.app_dir)?;
    if table.is_empty() {
        tracing::warn!(
            "no routes found under {}/app/routes",
            config.app_dir.display()
        );
    }
    for route in table.iter() {
        tracing::info!("route {} -> {}", route.pattern, route.file.display());
    }
    for tool in registry.entries() {
        tracing::info!("tool  {} (mcp)", tool.name);
    }

    let mcp_access = mcp::AccessConfig::from_env();
    if !host.is_loopback() && !mcp_access.auth_required() {
        if allow_unauthenticated_remote {
            tracing::warn!(
                "MCP endpoint is bound beyond loopback without bearer auth because --allow-unauthenticated-remote was set"
            );
        } else {
            anyhow::bail!(
                "refusing to bind {host}:{port} without {}; pass --allow-unauthenticated-remote only for isolated test networks",
                mcp::DEFAULT_TOKEN_ENV
            );
        }
    }
    if mcp_access.auth_required() {
        tracing::info!("MCP bearer-token auth enabled");
    }
    if !mcp_access.trusted_origins().is_empty() {
        tracing::info!(
            "MCP trusted origins: {}",
            mcp_access.trusted_origins().join(", ")
        );
    }

    let worker = worker::spawn()?;
    let state = DevState {
        routes: Arc::new(RwLock::new(table)),
        worker_tx: Arc::new(RwLock::new(worker.tx)),
        registry: Arc::new(registry),
        app_name: config.name.clone(),
        agents: Arc::new(agents),
        base_url,
        mcp_access,
        app_dir: config.app_dir.clone(),
    };

    spawn_reloader(config.app_dir.clone(), state.clone());
    let public_base_url = state.base_url.clone();

    let app = Router::new()
        .route(
            "/mcp",
            post(handle_mcp_post)
                .get(handle_mcp_get)
                .options(handle_mcp_options),
        )
        .route("/robots.txt", get(handle_robots))
        .route("/sitemap.xml", get(handle_sitemap))
        .route("/llms.txt", get(handle_llms))
        .route("/.well-known/beater.json", get(handle_well_known))
        .fallback(handle)
        .layer(middleware::map_response(with_route_security_headers))
        .with_state(state);
    let addr = std::net::SocketAddr::from((host, port));
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("bind {addr}"))?;
    tracing::info!(
        "beater dev listening on http://{addr} (app: {}, public: {})",
        config.name,
        public_base_url
    );

    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
            tracing::info!("shutting down");
        })
        .await?;
    Ok(())
}

/// Watch app/ and agents/; on change, rescan routes and swap in a fresh
/// isolate. The old worker drains and exits when its channel closes.
fn spawn_reloader(app_dir: PathBuf, state: DevState) {
    let (tx, mut rx) = mpsc::channel::<()>(16);
    let watch_dir = app_dir.clone();
    std::thread::spawn(move || {
        use notify::Watcher;
        let mut watcher =
            match notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
                if let Ok(event) = res
                    && (event.kind.is_modify() || event.kind.is_create() || event.kind.is_remove())
                {
                    let _ = tx.blocking_send(());
                }
            }) {
                Ok(w) => w,
                Err(e) => {
                    tracing::error!("watcher failed to start: {e}");
                    return;
                }
            };
        for sub in ["app", "agents"] {
            let dir = watch_dir.join(sub);
            if dir.is_dir()
                && let Err(e) = watcher.watch(&dir, notify::RecursiveMode::Recursive)
            {
                tracing::error!("watch {}: {e}", dir.display());
            }
        }
        // keep the watcher alive forever; the dev process owns its lifetime
        std::thread::park();
    });

    tokio::spawn(async move {
        while rx.recv().await.is_some() {
            // debounce editor save bursts
            tokio::time::sleep(Duration::from_millis(120)).await;
            while rx.try_recv().is_ok() {}
            match (RouteTable::scan(&app_dir), worker::spawn()) {
                (Ok(table), Ok(worker)) => {
                    *state.routes.write().await = table;
                    *state.worker_tx.write().await = worker.tx;
                    tracing::info!("reloaded (fresh isolate)");
                }
                (Err(e), _) => tracing::error!("reload: route scan failed: {e:#}"),
                (_, Err(e)) => tracing::error!("reload: worker spawn failed: {e:#}"),
            }
        }
    });
}

// ---- agent access layer ----------------------------------------------------

async fn handle_mcp_post(
    State(state): State<DevState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response<Body> {
    mcp::handle_post(&state.registry, &state.mcp_access, &headers, &body).await
}

async fn handle_mcp_get(State(state): State<DevState>, headers: HeaderMap) -> Response<Body> {
    mcp::handle_get(&state.mcp_access, &headers)
}

async fn handle_mcp_options(State(state): State<DevState>, headers: HeaderMap) -> Response<Body> {
    mcp::handle_options(&state.mcp_access, &headers)
}

async fn handle_robots(State(state): State<DevState>) -> Response<Body> {
    let routes: Vec<(String, Option<RouteMeta>)> = crawlable_routes(&state)
        .await
        .into_iter()
        .map(|(pattern, _, meta)| (pattern, meta))
        .collect();
    text_response(StatusCode::OK, crawl::robots_txt(&state.base_url, &routes))
}

/// Static page routes with their `agent` metadata resolved through the isolate
/// — the shared input for sitemap.xml and llms.txt.
async fn crawlable_routes(state: &DevState) -> Vec<(String, PathBuf, Option<RouteMeta>)> {
    let targets: Vec<(String, PathBuf, Option<String>)> = {
        let table = state.routes.read().await;
        crawlable_route_targets(&table)
    };
    let mut routes = Vec::new();
    for (pattern, file, spec) in targets {
        let meta = match spec {
            Some(spec) => route_meta(state, spec).await,
            None => None,
        };
        routes.push((pattern, file, meta));
    }
    routes
}

fn crawlable_route_targets(table: &RouteTable) -> Vec<(String, PathBuf, Option<String>)> {
    table
        .iter()
        .filter(|route| route.kind == RouteKind::Page)
        .filter(|route| {
            !route
                .segments
                .iter()
                .any(|segment| matches!(segment, Segment::Param(_)))
        })
        .map(|route| {
            let spec = deno_core::ModuleSpecifier::from_file_path(&route.file)
                .ok()
                .map(|specifier| specifier.to_string());
            (route.pattern.clone(), route.file.clone(), spec)
        })
        .collect()
}

async fn handle_sitemap(State(state): State<DevState>) -> Response<Body> {
    let routes = crawlable_routes(&state).await;
    let xml = crawl::sitemap_xml(&state.base_url, &routes);
    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "application/xml")
        .body(Body::from(xml))
        .expect("static response")
}

async fn handle_llms(State(state): State<DevState>) -> Response<Body> {
    let routes: Vec<(String, Option<RouteMeta>)> = crawlable_routes(&state)
        .await
        .into_iter()
        .map(|(pattern, _, meta)| (pattern, meta))
        .collect();
    text_response(
        StatusCode::OK,
        crawl::llms_txt(
            &state.app_name,
            &state.base_url,
            &routes,
            &state.agents,
            &state.mcp_access,
        ),
    )
}

async fn handle_well_known(State(state): State<DevState>) -> Response<Body> {
    let manifest = crawl::well_known(
        &state.app_name,
        &state.base_url,
        &state.agents,
        &state.mcp_access,
    );
    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "application/json")
        .body(Body::from(manifest.to_string()))
        .expect("static response")
}

async fn route_meta(state: &DevState, specifier: String) -> Option<RouteMeta> {
    let (reply_tx, reply_rx) = oneshot::channel();
    let result = tokio::time::timeout(REQUEST_TIMEOUT, async {
        send_worker_msg(
            &state.worker_tx,
            WorkerMsg::RouteMeta {
                specifier,
                reply: reply_tx,
            },
        )
        .await
        .ok()?;
        reply_rx.await.ok()
    })
    .await
    .ok()??;

    match result {
        Ok(meta) => meta,
        Err(e) => {
            tracing::warn!("route meta failed: {e}");
            None
        }
    }
}

async fn send_worker_msg(
    worker_tx: &Arc<RwLock<mpsc::Sender<WorkerMsg>>>,
    msg: WorkerMsg,
) -> std::result::Result<(), mpsc::error::SendError<WorkerMsg>> {
    let tx = worker_tx.read().await.clone();
    tx.send(msg).await
}

// ---- route dispatch ---------------------------------------------------------

async fn handle(State(state): State<DevState>, req: Request<Body>) -> Response<Body> {
    match handle_inner(state, req).await {
        Ok(resp) => resp,
        Err(e) => text_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("beater internal error: {e:#}"),
        ),
    }
}

async fn handle_inner(state: DevState, req: Request<Body>) -> Result<Response<Body>> {
    let method = req.method().as_str().to_uppercase();
    let head = method == "HEAD";
    let path = req.uri().path().to_string();
    if path.starts_with(RSC_FLIGHT_PREFIX) {
        return rsc_flight_response(&state, &method, &path, head).await;
    }
    if path.starts_with(CLIENT_MODULE_PREFIX) {
        return client_module_response(&state.app_dir, &method, &path);
    }
    let query: HashMap<String, String> = req
        .uri()
        .query()
        .map(|q| {
            deno_core::url::form_urlencoded::parse(q.as_bytes())
                .into_owned()
                .collect()
        })
        .unwrap_or_default();

    let (specifier, params, kind) = {
        let table = state.routes.read().await;
        match table.match_path(&path) {
            Some((route, params)) => {
                let spec = deno_core::ModuleSpecifier::from_file_path(&route.file)
                    .map_err(|_| anyhow::anyhow!("bad route path {}", route.file.display()))?;
                (spec.to_string(), params, route.kind)
            }
            None => {
                return Ok(text_response(
                    StatusCode::NOT_FOUND,
                    format!("no route for {path}"),
                ));
            }
        }
    };

    let page = kind == RouteKind::Page;
    if page && method != "GET" && method != "HEAD" {
        return Ok(text_response(
            StatusCode::METHOD_NOT_ALLOWED,
            "page routes are GET-only".to_string(),
        ));
    }
    let headers: HashMap<String, String> = req
        .headers()
        .iter()
        .map(|(k, v)| {
            (
                k.as_str().to_string(),
                String::from_utf8_lossy(v.as_bytes()).into_owned(),
            )
        })
        .collect();
    let body_bytes = axum::body::to_bytes(req.into_body(), MAX_BODY_BYTES).await?;
    let body = if body_bytes.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::Value::String(String::from_utf8_lossy(&body_bytes).into_owned())
    };

    let request_json = json!({
        "id": NEXT_REQUEST_ID.fetch_add(1, Ordering::Relaxed).to_string(),
        "method": method,
        "path": path,
        "params": params,
        "query": query,
        "headers": headers,
        "body": body,
    })
    .to_string();

    let (reply_tx, reply_rx) = oneshot::channel();
    let msg = WorkerMsg::Route {
        specifier,
        method,
        request_json,
        page,
        reply: reply_tx,
    };

    let result = tokio::time::timeout(REQUEST_TIMEOUT, async {
        send_worker_msg(&state.worker_tx, msg)
            .await
            .map_err(|_| anyhow::anyhow!("js worker is gone"))?;
        reply_rx
            .await
            .map_err(|_| anyhow::anyhow!("js worker dropped the request (reloading?)"))
    })
    .await
    .map_err(|_| anyhow::anyhow!("route handler timed out after {REQUEST_TIMEOUT:?}"))??;

    match result {
        Ok(route_resp) => route_response_to_http(&state, head, route_resp).await,
        // JS error: readable, source-mapped stack in the dev response
        Err(stack) => Ok(text_response(StatusCode::INTERNAL_SERVER_ERROR, stack)),
    }
}

async fn route_response_to_http(
    state: &DevState,
    head: bool,
    route_resp: worker::RouteResponse,
) -> Result<Response<Body>> {
    let builder = route_response_builder(route_resp.status, &route_resp.headers, &route_resp.body);
    let body = match route_resp.body {
        RouteBody::Full(body) => {
            if head {
                empty_unknown_len_body()
            } else {
                Body::from(body)
            }
        }
        RouteBody::Chunks(chunks) => {
            if head {
                empty_unknown_len_body()
            } else {
                chunks_body(chunks)
            }
        }
        RouteBody::Stream { stream_id, rx } => {
            if head {
                let _ = tokio::time::timeout(
                    REQUEST_TIMEOUT,
                    send_worker_msg(&state.worker_tx, WorkerMsg::CancelStream { stream_id }),
                )
                .await;
                drop(rx);
                empty_unknown_len_body()
            } else {
                Body::from_stream(UnboundedReceiverStream::new(rx))
            }
        }
    };
    Ok(builder.body(body)?)
}

fn route_response_builder(
    status: u16,
    headers: &HashMap<String, String>,
    body: &RouteBody,
) -> axum::http::response::Builder {
    let full_body_len = match body {
        RouteBody::Full(body) => Some(body.len()),
        RouteBody::Chunks(_) | RouteBody::Stream { .. } => None,
    };
    let mut builder = Response::builder().status(status);
    for (k, v) in headers {
        if k.eq_ignore_ascii_case("content-length") {
            continue;
        }
        builder = builder.header(k, v);
    }
    if let Some(len) = full_body_len {
        builder = builder.header("content-length", len.to_string());
    }
    builder
}

async fn rsc_flight_response(
    state: &DevState,
    method: &str,
    path: &str,
    head: bool,
) -> Result<Response<Body>> {
    if method != "GET" && method != "HEAD" {
        return Ok(text_response(
            StatusCode::METHOD_NOT_ALLOWED,
            "RSC flight streams are GET-only".to_string(),
        ));
    }

    let Some(route_path) = rsc_flight_route_path(path) else {
        return Ok(text_response(
            StatusCode::NOT_FOUND,
            "RSC flight stream not found".to_string(),
        ));
    };
    let module_path = {
        let table = state.routes.read().await;
        let Some(path) = find_rsc_server_module(&state.app_dir, &table, &route_path) else {
            return Ok(text_response(
                StatusCode::NOT_FOUND,
                "RSC flight stream not found".to_string(),
            ));
        };
        path
    };
    let specifier = deno_core::ModuleSpecifier::from_file_path(&module_path)
        .map_err(|_| anyhow::anyhow!("bad RSC server module path {}", module_path.display()))?;
    let request_json = json!({
        "id": NEXT_REQUEST_ID.fetch_add(1, Ordering::Relaxed).to_string(),
        "method": method,
        "path": path,
        "route": route_path.to_string_lossy(),
        "params": {},
        "query": {},
        "headers": {},
        "body": serde_json::Value::Null,
    })
    .to_string();

    let (reply_tx, reply_rx) = oneshot::channel();
    let result = tokio::time::timeout(REQUEST_TIMEOUT, async {
        send_worker_msg(
            &state.worker_tx,
            WorkerMsg::RscFlight {
                specifier: specifier.to_string(),
                request_json,
                reply: reply_tx,
            },
        )
        .await
        .map_err(|_| anyhow::anyhow!("js worker is gone"))?;
        reply_rx
            .await
            .map_err(|_| anyhow::anyhow!("js worker dropped the RSC flight request (reloading?)"))
    })
    .await
    .map_err(|_| anyhow::anyhow!("RSC flight stream timed out after {REQUEST_TIMEOUT:?}"))??;

    match result {
        Ok(route_resp) => route_response_to_http(state, head, route_resp).await,
        Err(stack) => Ok(text_response(StatusCode::INTERNAL_SERVER_ERROR, stack)),
    }
}

fn client_module_response(app_dir: &Path, method: &str, path: &str) -> Result<Response<Body>> {
    if method != "GET" && method != "HEAD" {
        return Ok(text_response(
            StatusCode::METHOD_NOT_ALLOWED,
            "client modules are GET-only".to_string(),
        ));
    }

    let Some(route_path) = client_module_route_path(path) else {
        return Ok(text_response(
            StatusCode::NOT_FOUND,
            "client module not found".to_string(),
        ));
    };
    let Some(module_path) = find_client_module(app_dir, &route_path) else {
        return Ok(text_response(
            StatusCode::NOT_FOUND,
            "client module not found".to_string(),
        ));
    };

    let code = loader::transpile_client_module(&module_path)
        .with_context(|| format!("transpile client module {}", module_path.display()))?;
    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "application/javascript; charset=utf-8")
        .header("cache-control", "no-store")
        .body(if method == "HEAD" {
            Body::empty()
        } else {
            Body::from(code)
        })
        .map_err(Into::into)
}

fn client_module_route_path(path: &str) -> Option<PathBuf> {
    route_scoped_internal_path(path, CLIENT_MODULE_PREFIX, ".js")
}

fn find_client_module(app_dir: &Path, route_path: &Path) -> Option<PathBuf> {
    let routes_dir = app_dir.join("app").join("routes");
    ["ts", "tsx", "js", "jsx", "mjs"]
        .into_iter()
        .map(|ext| {
            routes_dir
                .join(route_path)
                .with_extension(format!("client.{ext}"))
        })
        .find(|path| path.is_file())
}

fn rsc_flight_route_path(path: &str) -> Option<PathBuf> {
    route_scoped_internal_path(path, RSC_FLIGHT_PREFIX, ".flight")
}

fn find_rsc_server_module(
    app_dir: &Path,
    routes: &RouteTable,
    route_path: &Path,
) -> Option<PathBuf> {
    if !has_matching_page_route(app_dir, routes, route_path) {
        return None;
    }
    let routes_dir = app_dir.join("app").join("routes");
    ["tsx", "ts", "jsx", "js", "mjs"]
        .into_iter()
        .map(|ext| {
            routes_dir
                .join(route_path)
                .with_extension(format!("server.{ext}"))
        })
        .find(|path| path.is_file())
}

fn has_matching_page_route(app_dir: &Path, routes: &RouteTable, route_path: &Path) -> bool {
    let routes_dir = app_dir.join("app").join("routes");
    let candidates = ["tsx", "jsx"].map(|ext| routes_dir.join(route_path).with_extension(ext));
    routes
        .iter()
        .any(|route| route.kind == RouteKind::Page && candidates.contains(&route.file))
}

fn route_scoped_internal_path(path: &str, prefix: &str, suffix: &str) -> Option<PathBuf> {
    let rel = path.strip_prefix(prefix)?.strip_suffix(suffix)?;
    if rel.is_empty() {
        return None;
    }

    let mut route_path = PathBuf::new();
    for segment in rel.split('/') {
        if segment.is_empty()
            || segment == "."
            || segment == ".."
            || segment.contains('\\')
            || segment.contains(':')
        {
            return None;
        }
        route_path.push(segment);
    }
    Some(route_path)
}

#[cfg(test)]
fn route_response(route_resp: worker::RouteResponse) -> Result<Response<Body>, axum::http::Error> {
    let worker::RouteResponse {
        status,
        headers,
        body,
    } = route_resp;
    let builder = route_response_builder(status, &headers, &body);
    builder.body(route_body(body))
}

#[cfg(test)]
fn route_response_body(route_resp: worker::RouteResponse) -> Body {
    route_body(route_resp.body)
}

#[cfg(test)]
fn route_body(body: RouteBody) -> Body {
    match body {
        RouteBody::Full(body) => Body::from(body),
        RouteBody::Chunks(chunks) => chunks_body(chunks),
        RouteBody::Stream { rx, .. } => Body::from_stream(UnboundedReceiverStream::new(rx)),
    }
}

fn chunks_body(chunks: Vec<String>) -> Body {
    let chunks = chunks
        .into_iter()
        .map(|chunk| Ok::<Bytes, Infallible>(Bytes::from(chunk)));
    Body::from_stream(stream::iter(chunks))
}

async fn with_route_security_headers(response: Response<Body>) -> Response<Body> {
    secure_route_response(response)
}

fn secure_route_response(mut response: Response<Body>) -> Response<Body> {
    apply_route_security_headers(&mut response);
    response
}

fn apply_route_security_headers(response: &mut Response<Body>) {
    let headers = response.headers_mut();
    headers
        .entry("content-security-policy")
        .or_insert(HeaderValue::from_static(
            "default-src 'self'; base-uri 'none'; object-src 'none'; frame-ancestors 'none'; form-action 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; font-src 'self' data:; connect-src 'self'",
        ));
    headers
        .entry("x-content-type-options")
        .or_insert(HeaderValue::from_static("nosniff"));
    headers
        .entry("referrer-policy")
        .or_insert(HeaderValue::from_static("no-referrer"));
    headers
        .entry("x-frame-options")
        .or_insert(HeaderValue::from_static("DENY"));
    headers
        .entry("cross-origin-opener-policy")
        .or_insert(HeaderValue::from_static("same-origin"));
    headers
        .entry("cross-origin-resource-policy")
        .or_insert(HeaderValue::from_static("same-origin"));
    headers
        .entry("permissions-policy")
        .or_insert(HeaderValue::from_static(
            "accelerometer=(), camera=(), geolocation=(), gyroscope=(), microphone=(), payment=(), usb=()",
        ));
}

fn empty_unknown_len_body() -> Body {
    Body::from_stream(stream::empty::<Result<Bytes, Infallible>>())
}

fn text_response(status: StatusCode, body: String) -> Response<Body> {
    Response::builder()
        .status(status)
        .header("content-type", "text/plain; charset=utf-8")
        .body(Body::from(body))
        .expect("static response")
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::time::Duration;

    use axum::body::Body;
    use axum::http::{HeaderValue, Request, Response, StatusCode};
    use axum::routing::get;
    use axum::{Router, middleware};
    use tokio::sync::{RwLock, mpsc};
    use tower::ServiceExt;

    use super::{
        apply_route_security_headers, client_module_response, client_module_route_path,
        crawlable_route_targets, find_client_module, find_rsc_server_module, route_response,
        route_response_body, rsc_flight_route_path, secure_route_response, send_worker_msg,
        text_response, with_route_security_headers,
    };
    use crate::router::RouteTable;
    use crate::worker;

    #[test]
    fn route_security_headers_are_added_by_default() {
        let mut response = Response::builder()
            .status(200)
            .header("content-type", "text/html; charset=utf-8")
            .body(Body::empty())
            .unwrap_or_else(|error| panic!("test response should build: {error}"));

        apply_route_security_headers(&mut response);
        let headers = response.headers();
        assert_eq!(
            headers.get("x-content-type-options"),
            Some(&HeaderValue::from_static("nosniff"))
        );
        assert_eq!(
            headers.get("referrer-policy"),
            Some(&HeaderValue::from_static("no-referrer"))
        );
        assert_eq!(
            headers.get("x-frame-options"),
            Some(&HeaderValue::from_static("DENY"))
        );
        assert_eq!(
            headers.get("cross-origin-opener-policy"),
            Some(&HeaderValue::from_static("same-origin"))
        );
        assert_eq!(
            headers.get("cross-origin-resource-policy"),
            Some(&HeaderValue::from_static("same-origin"))
        );
        let csp = headers
            .get("content-security-policy")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();
        assert!(csp.contains("script-src 'self'"));
        assert!(csp.contains("frame-ancestors 'none'"));
        assert!(csp.contains("object-src 'none'"));
    }

    #[test]
    fn route_security_headers_do_not_override_explicit_route_headers() {
        let mut response = Response::builder()
            .status(200)
            .header("content-security-policy", "default-src 'none'")
            .body(Body::empty())
            .unwrap_or_else(|error| panic!("test response should build: {error}"));

        apply_route_security_headers(&mut response);
        assert_eq!(
            response.headers().get("content-security-policy"),
            Some(&HeaderValue::from_static("default-src 'none'"))
        );
    }

    #[test]
    fn handler_security_headers_are_added_to_error_responses() {
        let mut response = text_response(StatusCode::NOT_FOUND, "no route".to_string());
        apply_route_security_headers(&mut response);

        assert_eq!(
            response.headers().get("x-content-type-options"),
            Some(&HeaderValue::from_static("nosniff"))
        );
        assert!(
            response
                .headers()
                .get("content-security-policy")
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .contains("script-src 'self'")
        );
    }

    #[tokio::test]
    async fn worker_send_releases_reload_lock_while_channel_is_full() {
        let (tx, mut rx) = mpsc::channel(1);
        tx.try_send(worker::WorkerMsg::CancelStream { stream_id: 1 })
            .unwrap();
        let worker_tx = Arc::new(RwLock::new(tx));

        let blocked_send = tokio::spawn({
            let worker_tx = Arc::clone(&worker_tx);
            async move {
                send_worker_msg(&worker_tx, worker::WorkerMsg::CancelStream { stream_id: 2 }).await
            }
        });

        tokio::time::sleep(Duration::from_millis(10)).await;
        let (replacement_tx, _replacement_rx) = mpsc::channel(1);
        let mut guard = tokio::time::timeout(Duration::from_millis(100), worker_tx.write())
            .await
            .expect("blocked worker sends should not hold the reload write lock");
        *guard = replacement_tx;
        drop(guard);

        match rx
            .recv()
            .await
            .expect("initial message should remain queued")
        {
            worker::WorkerMsg::CancelStream { stream_id } => assert_eq!(stream_id, 1),
            msg => panic!("unexpected worker message: {msg:?}"),
        }

        tokio::time::timeout(Duration::from_millis(100), blocked_send)
            .await
            .expect("blocked send should finish after receiver capacity frees")
            .expect("send task should not panic")
            .expect("send should succeed");

        let queued = tokio::time::timeout(Duration::from_millis(100), rx.recv())
            .await
            .expect("blocked message should be queued on the original channel")
            .expect("blocked message should be sent");
        match queued {
            worker::WorkerMsg::CancelStream { stream_id } => assert_eq!(stream_id, 2),
            msg => panic!("unexpected worker message: {msg:?}"),
        }
    }

    #[test]
    fn explicit_agent_route_security_headers_preserve_existing_headers() {
        let response = Response::builder()
            .status(200)
            .header("content-type", "application/json")
            .header("access-control-allow-origin", "http://localhost:3000")
            .header("content-security-policy", "default-src 'none'")
            .body(Body::empty())
            .unwrap_or_else(|error| panic!("test response should build: {error}"));

        let response = secure_route_response(response);
        let headers = response.headers();

        assert_eq!(
            headers.get("content-type"),
            Some(&HeaderValue::from_static("application/json"))
        );
        assert_eq!(
            headers.get("access-control-allow-origin"),
            Some(&HeaderValue::from_static("http://localhost:3000"))
        );
        assert_eq!(
            headers.get("content-security-policy"),
            Some(&HeaderValue::from_static("default-src 'none'"))
        );
        assert_eq!(
            headers.get("x-content-type-options"),
            Some(&HeaderValue::from_static("nosniff"))
        );
        assert_eq!(
            headers.get("x-frame-options"),
            Some(&HeaderValue::from_static("DENY"))
        );
    }

    #[tokio::test]
    async fn route_security_layer_adds_headers_to_method_not_allowed() {
        let app = Router::new()
            .route("/robots.txt", get(|| async { "ok" }))
            .layer(middleware::map_response(with_route_security_headers));

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/robots.txt")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(
            response.headers().get("x-content-type-options"),
            Some(&HeaderValue::from_static("nosniff"))
        );
        assert_eq!(
            response.headers().get("x-frame-options"),
            Some(&HeaderValue::from_static("DENY"))
        );
    }

    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "beater-client-module-{name}-{}-{}",
                std::process::id(),
                chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
            ));
            fs::create_dir_all(&path).unwrap();
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }

        fn write(&self, rel: &str, contents: &str) {
            let path = self.path.join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, contents).unwrap();
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    #[test]
    fn client_module_route_paths_are_normalized() {
        assert_eq!(
            client_module_route_path("/_beater/client/index.js").as_deref(),
            Some(Path::new("index"))
        );
        assert_eq!(
            client_module_route_path("/_beater/client/dashboard/settings.js").as_deref(),
            Some(Path::new("dashboard/settings"))
        );
        assert!(client_module_route_path("/_beater/client/../secret.js").is_none());
        assert!(client_module_route_path("/_beater/client/index.ts").is_none());
    }

    #[test]
    fn rsc_flight_route_paths_are_normalized() {
        assert_eq!(
            rsc_flight_route_path("/_beater/rsc/index.flight").as_deref(),
            Some(Path::new("index"))
        );
        assert_eq!(
            rsc_flight_route_path("/_beater/rsc/dashboard/settings.flight").as_deref(),
            Some(Path::new("dashboard/settings"))
        );
        assert!(rsc_flight_route_path("/_beater/rsc/../secret.flight").is_none());
        assert!(rsc_flight_route_path("/_beater/rsc/index.js").is_none());
    }

    #[test]
    fn finds_adjacent_route_client_module() {
        let app = TempDir::new("find");
        app.write(
            "app/routes/index.client.ts",
            "document.body.dataset.ready = 'true';",
        );

        let found = find_client_module(app.path(), Path::new("index")).unwrap();

        assert_eq!(found, app.path().join("app/routes/index.client.ts"));
    }

    #[test]
    fn finds_adjacent_route_server_component_module() {
        let app = TempDir::new("find-rsc");
        app.write(
            "app/routes/dashboard/settings.tsx",
            "export default function SettingsPage() {}",
        );
        app.write(
            "app/routes/dashboard/settings.server.tsx",
            "export default function SettingsFlight() {}",
        );
        let table = RouteTable::scan(app.path()).unwrap();

        let found =
            find_rsc_server_module(app.path(), &table, Path::new("dashboard/settings")).unwrap();

        assert_eq!(
            found,
            app.path().join("app/routes/dashboard/settings.server.tsx")
        );
    }

    #[test]
    fn rsc_server_module_requires_matching_page_route() {
        let app = TempDir::new("find-rsc-stray");
        app.write(
            "app/routes/admin/secret.server.tsx",
            "export default function SecretFlight() {}",
        );
        let table = RouteTable::scan(app.path()).unwrap();

        assert!(find_rsc_server_module(app.path(), &table, Path::new("admin/secret")).is_none());
    }

    #[test]
    fn crawlable_routes_exclude_api_and_parameterized_routes() {
        let app = TempDir::new("crawlable");
        app.write("app/routes/index.tsx", "export default function Home() {}");
        app.write("app/routes/about.tsx", "export default function About() {}");
        app.write("app/routes/api/health.ts", "export function GET() {}");
        app.write("app/routes/api/export.js", "export function GET() {}");
        app.write(
            "app/routes/blog/[slug].tsx",
            "export default function BlogPost() {}",
        );
        let table = RouteTable::scan(app.path()).unwrap();

        let mut patterns: Vec<_> = crawlable_route_targets(&table)
            .into_iter()
            .map(|(pattern, _, _)| pattern)
            .collect();
        patterns.sort();

        assert_eq!(patterns, vec!["/", "/about"]);
    }

    #[tokio::test]
    async fn client_module_response_serves_transpiled_javascript() {
        let app = TempDir::new("serve");
        app.write(
            "app/routes/index.client.ts",
            "const count: number = 1;\ndocument.body.dataset.count = String(count);\n",
        );

        let response =
            client_module_response(app.path(), "GET", "/_beater/client/index.js").unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get("content-type").unwrap(),
            "application/javascript; charset=utf-8"
        );
        assert_eq!(response.headers().get("cache-control").unwrap(), "no-store");

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body = String::from_utf8(body.to_vec()).unwrap();
        assert!(body.contains("document.body.dataset.count"));
        assert!(!body.contains(": number"));
    }

    #[tokio::test]
    async fn route_response_body_prefers_chunks_over_body() {
        let body = route_response_body(worker::RouteResponse {
            status: 200,
            headers: HashMap::new(),
            body: worker::RouteBody::Chunks(vec!["hello ".to_string(), "stream".to_string()]),
        });

        let bytes = axum::body::to_bytes(body, usize::MAX).await.unwrap();
        assert_eq!(&bytes[..], b"hello stream");
    }

    #[tokio::test]
    async fn route_response_body_uses_body_when_no_chunks() {
        let body = route_response_body(worker::RouteResponse {
            status: 200,
            headers: HashMap::new(),
            body: worker::RouteBody::Full("plain body".to_string()),
        });

        let bytes = axum::body::to_bytes(body, usize::MAX).await.unwrap();
        assert_eq!(&bytes[..], b"plain body");
    }

    #[tokio::test]
    async fn route_response_drops_content_length_for_chunked_body() {
        let mut headers = HashMap::new();
        headers.insert("content-length".to_string(), "999".to_string());
        headers.insert(
            "content-type".to_string(),
            "text/plain; charset=utf-8".to_string(),
        );

        let response = route_response(worker::RouteResponse {
            status: 200,
            headers,
            body: worker::RouteBody::Chunks(vec!["hello ".to_string(), "stream".to_string()]),
        })
        .unwrap();

        assert!(response.headers().get("content-length").is_none());
        assert_eq!(
            response.headers().get("content-type").unwrap(),
            "text/plain; charset=utf-8"
        );

        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(&bytes[..], b"hello stream");
    }

    #[tokio::test]
    async fn route_response_derives_content_length_for_plain_body() {
        let mut headers = HashMap::new();
        headers.insert("content-length".to_string(), "999".to_string());

        let response = route_response(worker::RouteResponse {
            status: 200,
            headers,
            body: worker::RouteBody::Full("plain body".to_string()),
        })
        .unwrap();

        assert_eq!(response.headers().get("content-length").unwrap(), "10");

        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(&bytes[..], b"plain body");
    }
}
