//! The dev server: axum in front, JS worker behind a swappable channel,
//! notify-based hot reload, plus the agent surfaces — /mcp and the
//! generated crawl layer (robots.txt, sitemap.xml, llms.txt, .well-known).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, Request, Response, StatusCode};
use axum::routing::{Router, get, post};
use beater_agent::ToolRegistry;
use serde_json::json;
use tokio::sync::{RwLock, mpsc, oneshot};

use crate::config::AppConfig;
use crate::router::{RouteKind, RouteTable, Segment};
use crate::worker::{self, RouteMeta, WorkerMsg};
use crate::{crawl, mcp};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_BODY_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone)]
struct DevState {
    routes: Arc<RwLock<RouteTable>>,
    worker_tx: Arc<RwLock<mpsc::Sender<WorkerMsg>>>,
    registry: Arc<ToolRegistry>,
    app_name: String,
    agents: Arc<Vec<String>>,
    base_url: String,
}

pub async fn serve(
    config: AppConfig,
    host: std::net::IpAddr,
    port: u16,
    registry: ToolRegistry,
    agents: Vec<String>,
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

    let worker = worker::spawn()?;
    let state = DevState {
        routes: Arc::new(RwLock::new(table)),
        worker_tx: Arc::new(RwLock::new(worker.tx)),
        registry: Arc::new(registry),
        app_name: config.name.clone(),
        agents: Arc::new(agents),
        base_url: format!("http://{host}:{port}"),
    };

    spawn_reloader(config.app_dir.clone(), state.clone());

    let app = Router::new()
        .route("/mcp", post(handle_mcp_post).get(handle_mcp_get))
        .route("/robots.txt", get(handle_robots))
        .route("/sitemap.xml", get(handle_sitemap))
        .route("/llms.txt", get(handle_llms))
        .route("/.well-known/beater.json", get(handle_well_known))
        .fallback(handle)
        .with_state(state);
    let addr = std::net::SocketAddr::from((host, port));
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("bind {addr}"))?;
    tracing::info!(
        "beater dev listening on http://{addr} (app: {})",
        config.name
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
    mcp::handle_post(&state.registry, &headers, &body).await
}

async fn handle_mcp_get() -> Response<Body> {
    mcp::handle_get()
}

async fn handle_robots(State(state): State<DevState>) -> Response<Body> {
    text_response(StatusCode::OK, crawl::robots_txt(&state.base_url))
}

/// Static (non-parameterized) routes with their `agent` metadata resolved
/// through the isolate — the shared input for sitemap.xml and llms.txt.
async fn crawlable_routes(state: &DevState) -> Vec<(String, PathBuf, Option<RouteMeta>)> {
    let targets: Vec<(String, PathBuf, Option<String>)> = {
        let table = state.routes.read().await;
        table
            .iter()
            .filter(|r| !r.segments.iter().any(|s| matches!(s, Segment::Param(_))))
            .map(|r| {
                let spec = deno_core::ModuleSpecifier::from_file_path(&r.file)
                    .ok()
                    .map(|s| s.to_string());
                (r.pattern.clone(), r.file.clone(), spec)
            })
            .collect()
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
        crawl::llms_txt(&state.app_name, &state.base_url, &routes, &state.agents),
    )
}

async fn handle_well_known(State(state): State<DevState>) -> Response<Body> {
    let manifest = crawl::well_known(&state.app_name, &state.base_url, &state.agents);
    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "application/json")
        .body(Body::from(manifest.to_string()))
        .expect("static response")
}

async fn route_meta(state: &DevState, specifier: String) -> Option<RouteMeta> {
    let (reply_tx, reply_rx) = oneshot::channel();
    state
        .worker_tx
        .read()
        .await
        .send(WorkerMsg::RouteMeta {
            specifier,
            reply: reply_tx,
        })
        .await
        .ok()?;
    match tokio::time::timeout(REQUEST_TIMEOUT, reply_rx).await {
        Ok(Ok(Ok(meta))) => meta,
        Ok(Ok(Err(e))) => {
            tracing::warn!("route meta failed: {e}");
            None
        }
        _ => None,
    }
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
    let path = req.uri().path().to_string();
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
    state
        .worker_tx
        .read()
        .await
        .send(msg)
        .await
        .map_err(|_| anyhow::anyhow!("js worker is gone"))?;

    let result = tokio::time::timeout(REQUEST_TIMEOUT, reply_rx)
        .await
        .map_err(|_| anyhow::anyhow!("route handler timed out after {REQUEST_TIMEOUT:?}"))?
        .map_err(|_| anyhow::anyhow!("js worker dropped the request (reloading?)"))?;

    match result {
        Ok(route_resp) => {
            let mut builder = Response::builder().status(route_resp.status);
            for (k, v) in &route_resp.headers {
                builder = builder.header(k, v);
            }
            Ok(builder.body(Body::from(route_resp.body))?)
        }
        // JS error: readable, source-mapped stack in the dev response
        Err(stack) => Ok(text_response(StatusCode::INTERNAL_SERVER_ERROR, stack)),
    }
}

fn text_response(status: StatusCode, body: String) -> Response<Body> {
    Response::builder()
        .status(status)
        .header("content-type", "text/plain; charset=utf-8")
        .body(Body::from(body))
        .expect("static response")
}
