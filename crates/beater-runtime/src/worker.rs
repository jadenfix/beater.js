//! The JS worker: one dedicated OS thread owning a `JsRuntime` (it is !Send),
//! driven by a current-thread tokio runtime. The host talks to it over an
//! mpsc channel; the dev server can keep a small pool of these workers hot.

use std::cell::RefCell;
use std::collections::HashMap;
use std::io;
use std::rc::Rc;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::task::{Context as TaskContext, Poll, Waker};
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{Context, Result};
use bytes::Bytes;
use deno_core::error::{CoreError, CoreErrorKind, JsError};
use deno_core::{JsRuntime, OpState, PollEventLoopOptions, RuntimeOptions, extension, op2, v8};
use serde::Deserialize;
use tokio::sync::{mpsc, oneshot};

use crate::loader::BeaterModuleLoader;

const WORKER_SHUTDOWN_STREAM_ERROR: &str = "js worker shut down before stream completed";
const WORKER_EXECUTION_TIMEOUT_ERROR: &str = "js execution timed out";
const STREAM_CHUNK_QUEUE_CAPACITY: usize = 16;
#[cfg(test)]
const WORKER_EXECUTION_TIMEOUT: Duration = Duration::from_millis(50);
#[cfg(not(test))]
const WORKER_EXECUTION_TIMEOUT: Duration = Duration::from_secs(30);

type StreamItem = Result<Bytes, io::Error>;
type StreamSender = mpsc::Sender<StreamItem>;
type StreamReceiver = mpsc::Receiver<StreamItem>;

#[derive(Debug)]
pub enum WorkerMsg {
    Route {
        /// file:// specifier of the route module.
        specifier: String,
        /// Uppercase HTTP method (picks the module export).
        method: String,
        /// JSON-serialized request object passed to the handler.
        request_json: String,
        /// Page routes render their default export as React SSR.
        page: bool,
        reply: oneshot::Sender<Result<RouteResponse, String>>,
    },
    RscFlight {
        /// file:// specifier of the route-scoped server component module.
        specifier: String,
        /// JSON-serialized request object passed to the server component.
        request_json: String,
        reply: oneshot::Sender<Result<RouteResponse, String>>,
    },
    /// Read a route module's optional `export const agent = {...}` metadata.
    RouteMeta {
        specifier: String,
        reply: oneshot::Sender<Result<Option<RouteMeta>, String>>,
    },
    CancelStream {
        stream_id: u32,
    },
    #[cfg(test)]
    EvalBool {
        code: String,
        reply: oneshot::Sender<Result<bool, String>>,
    },
    #[cfg(test)]
    RegisterTestStream {
        stream_id: u32,
        reply: oneshot::Sender<StreamReceiver>,
    },
}

#[derive(Debug, Clone, Deserialize)]
pub struct RouteMeta {
    pub title: Option<String>,
    pub description: Option<String>,
    pub crawl: bool,
    #[serde(default)]
    pub actions: Vec<RouteActionMeta>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RouteActionMeta {
    pub name: String,
    pub description: Option<String>,
    #[serde(default)]
    pub method: Option<String>,
    #[serde(default, rename = "inputSchema", alias = "input_schema")]
    pub input_schema: serde_json::Value,
    #[serde(default, rename = "sideEffect", alias = "side_effect")]
    pub side_effect: Option<String>,
    #[serde(default)]
    pub confirm: bool,
    #[serde(default, rename = "dryRun", alias = "dry_run")]
    pub dry_run: bool,
    #[serde(
        default,
        rename = "idempotencyRequired",
        alias = "idempotency_required"
    )]
    pub idempotency_required: bool,
    #[serde(default)]
    pub auth: serde_json::Value,
}

#[derive(Debug)]
pub struct RouteResponse {
    pub status: u16,
    pub headers: HashMap<String, String>,
    pub body: RouteBody,
}

#[derive(Debug)]
pub enum RouteBody {
    Full(String),
    Chunks(Vec<String>),
    Stream {
        stream_id: u32,
        rx: mpsc::Receiver<Result<Bytes, io::Error>>,
    },
}

#[derive(Debug, Deserialize)]
struct JsRouteResponse {
    status: u16,
    #[serde(default)]
    headers: HashMap<String, String>,
    #[serde(default)]
    body: String,
    #[serde(default)]
    body_chunks: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct JsPageStream {
    status: u16,
    #[serde(default)]
    headers: HashMap<String, String>,
}

#[derive(Default)]
struct WorkerState {
    streams: HashMap<u32, StreamSender>,
}

#[op2(async(lazy), fast)]
async fn op_beater_sleep(ms: f64) {
    tokio::time::sleep(std::time::Duration::from_millis(ms.max(0.0) as u64)).await;
}

#[op2(async(lazy), fast)]
async fn op_beater_stream_chunk(
    state: Rc<RefCell<OpState>>,
    stream_id: u32,
    #[buffer(copy)] chunk: Vec<u8>,
) -> bool {
    let Some(tx) = state
        .borrow()
        .borrow::<WorkerState>()
        .streams
        .get(&stream_id)
        .cloned()
    else {
        return false;
    };
    if tx.send(Ok(Bytes::from(chunk))).await.is_err() {
        state
            .borrow_mut()
            .borrow_mut::<WorkerState>()
            .streams
            .remove(&stream_id);
        return false;
    }
    true
}

#[op2(fast)]
fn op_beater_stream_end(state: &mut OpState, stream_id: u32) {
    state.borrow_mut::<WorkerState>().streams.remove(&stream_id);
}

#[op2(fast)]
fn op_beater_stream_error(state: &mut OpState, stream_id: u32, #[string] error: String) {
    if let Some(tx) = state.borrow_mut::<WorkerState>().streams.remove(&stream_id) {
        enqueue_stream_error(tx, error);
    }
}

// generated struct is pub; used by worker + agent_config isolates
extension!(
    beater_ext,
    ops = [
        op_beater_sleep,
        op_beater_stream_chunk,
        op_beater_stream_end,
        op_beater_stream_error
    ],
    state = |state| {
        state.put(WorkerState::default());
    },
);

pub struct WorkerHandle {
    pub tx: mpsc::Sender<WorkerMsg>,
}

/// Spawn a fresh isolate on its own thread. Dropping the last sender shuts
/// the worker down after it drains in-flight messages.
pub fn spawn() -> Result<WorkerHandle> {
    let (tx, rx) = mpsc::channel::<WorkerMsg>(64);
    std::thread::Builder::new()
        .name("beater-js".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("worker tokio runtime");
            let local = tokio::task::LocalSet::new();
            local.block_on(&rt, worker_main(rx));
        })
        .context("spawn js worker thread")?;
    Ok(WorkerHandle { tx })
}

async fn worker_main(mut rx: mpsc::Receiver<WorkerMsg>) {
    let mut runtime = new_js_runtime();
    tracing::debug!("js worker ready (V8 {})", v8::VERSION_STRING);

    let mut next_stream_id = 1_u32;
    loop {
        if let Err(e) = poll_event_loop_once(&mut runtime) {
            fail_active_streams(&mut runtime, e);
        }
        if active_streams(&runtime) {
            tokio::select! {
                maybe_msg = rx.recv() => {
                    let Some(msg) = maybe_msg else {
                        fail_active_streams_for_shutdown(&mut runtime);
                        break;
                    };
                    if handle_worker_msg(&mut runtime, &mut next_stream_id, msg).await {
                        fail_active_streams(&mut runtime, WORKER_EXECUTION_TIMEOUT_ERROR.to_string());
                        recycle_js_runtime(&mut runtime);
                    }
                }
                _ = tokio::time::sleep(Duration::from_millis(5)) => {}
            }
        } else {
            tokio::select! {
                maybe_msg = rx.recv() => {
                    let Some(msg) = maybe_msg else { break };
                    if handle_worker_msg(&mut runtime, &mut next_stream_id, msg).await {
                        recycle_js_runtime(&mut runtime);
                    }
                }
                _ = tokio::time::sleep(Duration::from_millis(5)) => {}
            }
        }
    }
    tracing::debug!("js worker shutting down");
}

fn new_js_runtime() -> JsRuntime {
    let mut runtime = JsRuntime::new(RuntimeOptions {
        module_loader: Some(Rc::new(BeaterModuleLoader)),
        extensions: vec![beater_ext::init()],
        ..Default::default()
    });
    runtime
        .execute_script("beater:bootstrap", include_str!("bootstrap.js"))
        .expect("bootstrap.js must evaluate");
    runtime
}

fn recycle_js_runtime(runtime: &mut JsRuntime) {
    // Dropping a just-terminated Deno runtime can re-enter V8 in an invalid handle scope.
    // Timeouts are exceptional; install a fresh isolate and leave the terminated one inert.
    let terminated = std::mem::replace(runtime, new_js_runtime());
    std::mem::forget(terminated);
}

async fn handle_worker_msg(
    runtime: &mut JsRuntime,
    next_stream_id: &mut u32,
    msg: WorkerMsg,
) -> bool {
    match msg {
        WorkerMsg::Route {
            specifier,
            method,
            request_json,
            page,
            reply,
        } => {
            if page {
                let stream_id = allocate_stream_id(next_stream_id);
                dispatch_page_stream(runtime, &specifier, &request_json, stream_id, reply).await
            } else {
                let result = dispatch_api(runtime, &specifier, &method, &request_json).await;
                let recycle = is_execution_timeout(&result);
                let _ = reply.send(result);
                recycle
            }
        }
        WorkerMsg::RscFlight {
            specifier,
            request_json,
            reply,
        } => {
            let stream_id = allocate_stream_id(next_stream_id);
            dispatch_rsc_flight_stream(runtime, &specifier, &request_json, stream_id, reply).await
        }
        WorkerMsg::RouteMeta { specifier, reply } => {
            let result = route_meta(runtime, &specifier).await;
            let recycle = is_execution_timeout(&result);
            let _ = reply.send(result);
            recycle
        }
        WorkerMsg::CancelStream { stream_id } => {
            let result = cancel_page_stream(runtime, stream_id).await;
            let recycle = is_execution_timeout(&result);
            if result.is_err() {
                remove_page_stream(runtime, stream_id);
            }
            recycle
        }
        #[cfg(test)]
        WorkerMsg::EvalBool { code, reply } => {
            let result = eval_bool_script(runtime, &code);
            let recycle = is_execution_timeout(&result);
            let _ = reply.send(result);
            recycle
        }
        #[cfg(test)]
        WorkerMsg::RegisterTestStream { stream_id, reply } => {
            let (body_tx, body_rx) = stream_body_channel();
            register_page_stream(runtime, stream_id, body_tx);
            let _ = reply.send(body_rx);
            false
        }
    }
}

fn allocate_stream_id(next_stream_id: &mut u32) -> u32 {
    let stream_id = (*next_stream_id).max(1);
    *next_stream_id = stream_id.wrapping_add(1).max(1);
    stream_id
}

#[cfg(test)]
fn eval_bool_script(runtime: &mut JsRuntime, code: &str) -> Result<bool, String> {
    let deadline = ExecutionDeadline::start(runtime, "test eval");
    let global = match runtime.execute_script("beater:test-eval-bool", code.to_string()) {
        Ok(global) => global,
        Err(e) => {
            let error = if deadline.tripped() {
                deadline.timeout_error()
            } else {
                format_js_error(&e)
            };
            return deadline.finish(runtime, Err(error));
        }
    };
    let result = {
        deno_core::scope!(scope, runtime);
        let local = v8::Local::new(scope, global);
        deno_core::serde_v8::from_v8(scope, local)
            .map_err(|e| format!("eval did not return bool: {e}"))
    };
    deadline.finish(runtime, result)
}

fn active_streams(runtime: &JsRuntime) -> bool {
    !runtime
        .op_state()
        .borrow()
        .borrow::<WorkerState>()
        .streams
        .is_empty()
}

fn poll_event_loop_once(runtime: &mut JsRuntime) -> Result<(), String> {
    let mut cx = TaskContext::from_waker(Waker::noop());
    match runtime.poll_event_loop(&mut cx, PollEventLoopOptions::default()) {
        Poll::Ready(Ok(())) | Poll::Pending => Ok(()),
        Poll::Ready(Err(e)) => Err(format_core_error(e)),
    }
}

fn fail_active_streams(runtime: &mut JsRuntime, error: String) {
    let streams = {
        let op_state = runtime.op_state();
        let mut op_state = op_state.borrow_mut();
        std::mem::take(&mut op_state.borrow_mut::<WorkerState>().streams)
    };
    for (_, tx) in streams {
        enqueue_stream_error(tx, error.clone());
    }
}

fn fail_active_streams_for_shutdown(runtime: &mut JsRuntime) {
    fail_active_streams(runtime, WORKER_SHUTDOWN_STREAM_ERROR.to_string());
}

fn enqueue_stream_error(tx: StreamSender, error: String) {
    let item = Err(io::Error::other(error));
    match tx.try_send(item) {
        Ok(()) | Err(mpsc::error::TrySendError::Closed(_)) => {}
        Err(mpsc::error::TrySendError::Full(item)) => {
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                handle.spawn(async move {
                    let _ = tx.send(item).await;
                });
            }
        }
    }
}

fn stream_body_channel() -> (StreamSender, StreamReceiver) {
    mpsc::channel(STREAM_CHUNK_QUEUE_CAPACITY)
}

struct ExecutionDeadline {
    tripped: Arc<AtomicBool>,
    cancel: Option<std::sync::mpsc::Sender<()>>,
    watchdog: Option<JoinHandle<()>>,
    operation: &'static str,
}

impl ExecutionDeadline {
    fn start(runtime: &mut JsRuntime, operation: &'static str) -> Self {
        let tripped = Arc::new(AtomicBool::new(false));
        let watchdog_tripped = Arc::clone(&tripped);
        let (cancel, cancelled) = std::sync::mpsc::channel();
        let isolate = runtime.v8_isolate().thread_safe_handle();
        let watchdog = std::thread::spawn(move || {
            if cancelled
                .recv_timeout(WORKER_EXECUTION_TIMEOUT)
                .is_err_and(|err| err == std::sync::mpsc::RecvTimeoutError::Timeout)
            {
                watchdog_tripped.store(true, Ordering::Release);
                let _ = isolate.terminate_execution();
            };
        });
        Self {
            tripped,
            cancel: Some(cancel),
            watchdog: Some(watchdog),
            operation,
        }
    }

    fn tripped(&self) -> bool {
        self.tripped.load(Ordering::Acquire)
    }

    fn timeout_error(&self) -> String {
        format!(
            "{WORKER_EXECUTION_TIMEOUT_ERROR} during {} after {:?}",
            self.operation, WORKER_EXECUTION_TIMEOUT
        )
    }

    fn finish<T>(
        mut self,
        runtime: &mut JsRuntime,
        result: Result<T, String>,
    ) -> Result<T, String> {
        if let Some(cancel) = self.cancel.take() {
            let _ = cancel.send(());
        }
        if let Some(watchdog) = self.watchdog.take() {
            let _ = watchdog.join();
        }
        if self.tripped.load(Ordering::Acquire) {
            let _ = runtime.v8_isolate().cancel_terminate_execution();
            return Err(self.timeout_error());
        }
        result
    }
}

fn is_execution_timeout<T>(result: &Result<T, String>) -> bool {
    matches!(result, Err(error) if error.contains(WORKER_EXECUTION_TIMEOUT_ERROR))
}

fn format_core_error_for_deadline(deadline: &ExecutionDeadline, err: CoreError) -> String {
    if deadline.tripped() {
        deadline.timeout_error()
    } else {
        format_core_error(err)
    }
}

fn execute_script_with_deadline(
    runtime: &mut JsRuntime,
    name: &'static str,
    code: String,
    operation: &'static str,
) -> Result<(v8::Global<v8::Value>, ExecutionDeadline), String> {
    let deadline = ExecutionDeadline::start(runtime, operation);
    match runtime.execute_script(name, code) {
        Ok(value) => Ok((value, deadline)),
        Err(e) => {
            let error = if deadline.tripped() {
                deadline.timeout_error()
            } else {
                format_js_error(&e)
            };
            Err(deadline.finish::<()>(runtime, Err(error)).unwrap_err())
        }
    }
}

async fn dispatch_api(
    runtime: &mut JsRuntime,
    specifier: &str,
    method: &str,
    request_json: &str,
) -> Result<RouteResponse, String> {
    let code = format!(
        "globalThis.__beaterDispatch({}, {}, {})",
        serde_json::Value::String(specifier.to_string()),
        serde_json::Value::String(method.to_string()),
        request_json,
    );
    let (promise, deadline) =
        execute_script_with_deadline(runtime, "beater:dispatch", code, "route handler")?;
    let resolved = runtime.resolve(promise);
    let global = match runtime
        .with_event_loop_promise(resolved, PollEventLoopOptions::default())
        .await
    {
        Ok(global) => global,
        Err(e) => {
            let error = format_core_error_for_deadline(&deadline, e);
            return deadline.finish(runtime, Err(error));
        }
    };

    let response: Result<JsRouteResponse, String> = {
        deno_core::scope!(scope, runtime);
        let local = v8::Local::new(scope, global);
        deno_core::serde_v8::from_v8(scope, local)
            .map_err(|e| format!("route response did not match {{ status, headers, body }}: {e}"))
    };
    let response = match response {
        Ok(response) => response,
        Err(e) => return deadline.finish(runtime, Err(e)),
    };
    deadline.finish(
        runtime,
        Ok(RouteResponse {
            status: response.status,
            headers: response.headers,
            body: if response.body_chunks.is_empty() {
                RouteBody::Full(response.body)
            } else {
                RouteBody::Chunks(response.body_chunks)
            },
        }),
    )
}

async fn dispatch_page_stream(
    runtime: &mut JsRuntime,
    specifier: &str,
    request_json: &str,
    stream_id: u32,
    reply: oneshot::Sender<Result<RouteResponse, String>>,
) -> bool {
    let (body_tx, body_rx) = stream_body_channel();
    register_page_stream(runtime, stream_id, body_tx);

    let page_stream = match prepare_page_stream(runtime, specifier, request_json, stream_id).await {
        Ok(stream) => stream,
        Err(e) => {
            remove_page_stream(runtime, stream_id);
            let recycle = e.contains(WORKER_EXECUTION_TIMEOUT_ERROR);
            let _ = reply.send(Err(e));
            return recycle;
        }
    };

    let response = RouteResponse {
        status: page_stream.status,
        headers: page_stream.headers,
        body: RouteBody::Stream {
            stream_id,
            rx: body_rx,
        },
    };
    if reply.send(Ok(response)).is_err() {
        let _ = cancel_page_stream(runtime, stream_id).await;
    }
    false
}

async fn dispatch_rsc_flight_stream(
    runtime: &mut JsRuntime,
    specifier: &str,
    request_json: &str,
    stream_id: u32,
    reply: oneshot::Sender<Result<RouteResponse, String>>,
) -> bool {
    let (body_tx, body_rx) = stream_body_channel();
    register_page_stream(runtime, stream_id, body_tx);

    let flight_stream =
        match prepare_rsc_flight_stream(runtime, specifier, request_json, stream_id).await {
            Ok(stream) => stream,
            Err(e) => {
                remove_page_stream(runtime, stream_id);
                let recycle = e.contains(WORKER_EXECUTION_TIMEOUT_ERROR);
                let _ = reply.send(Err(e));
                return recycle;
            }
        };

    let response = RouteResponse {
        status: flight_stream.status,
        headers: flight_stream.headers,
        body: RouteBody::Stream {
            stream_id,
            rx: body_rx,
        },
    };
    if reply.send(Ok(response)).is_err() {
        let _ = cancel_page_stream(runtime, stream_id).await;
    }
    false
}

fn register_page_stream(runtime: &mut JsRuntime, stream_id: u32, tx: StreamSender) {
    runtime
        .op_state()
        .borrow_mut()
        .borrow_mut::<WorkerState>()
        .streams
        .insert(stream_id, tx);
}

fn remove_page_stream(runtime: &mut JsRuntime, stream_id: u32) {
    runtime
        .op_state()
        .borrow_mut()
        .borrow_mut::<WorkerState>()
        .streams
        .remove(&stream_id);
}

async fn prepare_page_stream(
    runtime: &mut JsRuntime,
    specifier: &str,
    request_json: &str,
    stream_id: u32,
) -> Result<JsPageStream, String> {
    let code = format!(
        "globalThis.__beaterPreparePageStream({}, {}, {stream_id})",
        serde_json::Value::String(specifier.to_string()),
        request_json,
    );
    let (promise, deadline) =
        execute_script_with_deadline(runtime, "beater:prepare-page-stream", code, "page render")?;
    let resolved = runtime.resolve(promise);
    let global = match runtime
        .with_event_loop_promise(resolved, PollEventLoopOptions::default())
        .await
    {
        Ok(global) => global,
        Err(e) => {
            let error = format_core_error_for_deadline(&deadline, e);
            return deadline.finish(runtime, Err(error));
        }
    };

    let result = {
        deno_core::scope!(scope, runtime);
        let local = v8::Local::new(scope, global);
        deno_core::serde_v8::from_v8(scope, local)
            .map_err(|e| format!("page stream response did not match {{ status, headers }}: {e}"))
    };
    deadline.finish(runtime, result)
}

async fn prepare_rsc_flight_stream(
    runtime: &mut JsRuntime,
    specifier: &str,
    request_json: &str,
    stream_id: u32,
) -> Result<JsPageStream, String> {
    let code = format!(
        "globalThis.__beaterPrepareRscFlightStream({}, {}, {stream_id})",
        serde_json::Value::String(specifier.to_string()),
        request_json,
    );
    let (promise, deadline) = execute_script_with_deadline(
        runtime,
        "beater:prepare-rsc-flight-stream",
        code,
        "RSC flight render",
    )?;
    let resolved = runtime.resolve(promise);
    let global = match runtime
        .with_event_loop_promise(resolved, PollEventLoopOptions::default())
        .await
    {
        Ok(global) => global,
        Err(e) => {
            let error = format_core_error_for_deadline(&deadline, e);
            return deadline.finish(runtime, Err(error));
        }
    };

    let result = {
        deno_core::scope!(scope, runtime);
        let local = v8::Local::new(scope, global);
        deno_core::serde_v8::from_v8(scope, local).map_err(|e| {
            format!("RSC flight stream response did not match {{ status, headers }}: {e}")
        })
    };
    deadline.finish(runtime, result)
}

async fn cancel_page_stream(runtime: &mut JsRuntime, stream_id: u32) -> Result<(), String> {
    let code = format!("globalThis.__beaterCancelPageStream({stream_id})");
    let (promise, deadline) =
        execute_script_with_deadline(runtime, "beater:cancel-page-stream", code, "stream cancel")?;
    let resolved = runtime.resolve(promise);
    let result = match runtime
        .with_event_loop_promise(resolved, PollEventLoopOptions::default())
        .await
    {
        Ok(_) => Ok(()),
        Err(e) => Err(format_core_error_for_deadline(&deadline, e)),
    };
    deadline.finish(runtime, result)
}

async fn route_meta(runtime: &mut JsRuntime, specifier: &str) -> Result<Option<RouteMeta>, String> {
    let code = format!(
        "globalThis.__beaterRouteMeta({})",
        serde_json::Value::String(specifier.to_string()),
    );
    let (promise, deadline) =
        execute_script_with_deadline(runtime, "beater:route-meta", code, "route metadata")?;
    let resolved = runtime.resolve(promise);
    let global = match runtime
        .with_event_loop_promise(resolved, PollEventLoopOptions::default())
        .await
    {
        Ok(global) => global,
        Err(e) => {
            let error = format_core_error_for_deadline(&deadline, e);
            return deadline.finish(runtime, Err(error));
        }
    };
    let result = {
        deno_core::scope!(scope, runtime);
        let local = v8::Local::new(scope, global);
        deno_core::serde_v8::from_v8(scope, local).map_err(|e| format!("bad agent metadata: {e}"))
    };
    deadline.finish(runtime, result)
}

/// Render a JS exception with its (source-mapped) stack for dev output.
pub(crate) fn format_js_error(err: &JsError) -> String {
    match &err.stack {
        Some(stack) if !stack.is_empty() => stack.clone(),
        _ => err.exception_message.clone(),
    }
}

fn format_core_error(err: CoreError) -> String {
    match *err.0 {
        CoreErrorKind::Js(js) => format_js_error(&js),
        other => format!("{other}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new() -> Self {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let path = std::env::temp_dir()
                .join(format!("beater-worker-test-{}-{nanos}", std::process::id()));
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

    fn runtime_with_bootstrap() -> JsRuntime {
        let mut runtime = JsRuntime::new(RuntimeOptions {
            module_loader: Some(Rc::new(BeaterModuleLoader)),
            extensions: vec![beater_ext::init()],
            ..Default::default()
        });
        runtime
            .execute_script(
                "beater:test-clear-web-shims",
                "globalThis.TextEncoder = undefined; globalThis.ReadableStream = undefined",
            )
            .expect("clear native TextEncoder");
        runtime
            .execute_script("beater:bootstrap", include_str!("bootstrap.js"))
            .expect("bootstrap.js must evaluate");
        runtime
    }

    fn eval_bool(runtime: &mut JsRuntime, source: &'static str) -> bool {
        let global = runtime
            .execute_script("beater:test-bool", source)
            .expect("boolean test expression should evaluate");
        deno_core::scope!(scope, runtime);
        let local = v8::Local::new(scope, global);
        deno_core::serde_v8::from_v8(scope, local).expect("test expression should return bool")
    }

    #[test]
    fn stream_body_channel_is_bounded() {
        let (tx, _rx) = stream_body_channel();
        for _ in 0..STREAM_CHUNK_QUEUE_CAPACITY {
            tx.try_send(Ok(Bytes::new()))
                .expect("stream channel should accept capacity-sized burst");
        }

        assert!(matches!(
            tx.try_send(Ok(Bytes::new())),
            Err(mpsc::error::TrySendError::Full(_))
        ));
    }

    #[test]
    fn stream_ids_wrap_without_reusing_zero_or_saturating() {
        let mut next = u32::MAX;

        assert_eq!(allocate_stream_id(&mut next), u32::MAX);
        assert_eq!(next, 1);
        assert_eq!(allocate_stream_id(&mut next), 1);
        assert_eq!(next, 2);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn idle_worker_drives_timers_without_a_followup_request() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let (tx, rx) = mpsc::channel::<WorkerMsg>(4);
                let worker = tokio::task::spawn_local(worker_main(rx));

                let (reply, done) = oneshot::channel();
                tx.send(WorkerMsg::EvalBool {
                    code: "globalThis.__idleTimerFired = false; setTimeout(() => { globalThis.__idleTimerFired = true; }, 0); true".to_string(),
                    reply,
                })
                .await
                .unwrap();
                assert!(done.await.unwrap().unwrap());

                tokio::time::sleep(Duration::from_millis(30)).await;

                let (reply, done) = oneshot::channel();
                tx.send(WorkerMsg::EvalBool {
                    code: "globalThis.__idleTimerFired === true".to_string(),
                    reply,
                })
                .await
                .unwrap();
                assert!(
                    done.await.unwrap().unwrap(),
                    "idle worker should poll the JS event loop while no streams are active"
                );

                drop(tx);
                worker.await.unwrap();
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn wedged_route_handler_times_out_and_worker_recovers() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let app = TempDir::new();
                app.write(
                    "app/routes/api/spin.ts",
                    "export function GET() { while (true) {} }",
                );
                app.write(
                    "app/routes/api/ok.ts",
                    "export function GET() { return { status: 200, body: 'ok' }; }",
                );
                let spin_specifier = deno_core::ModuleSpecifier::from_file_path(
                    app.path().join("app/routes/api/spin.ts"),
                )
                .unwrap()
                .to_string();
                let ok_specifier = deno_core::ModuleSpecifier::from_file_path(
                    app.path().join("app/routes/api/ok.ts"),
                )
                .unwrap()
                .to_string();

                let (tx, rx) = mpsc::channel::<WorkerMsg>(4);
                let worker = tokio::task::spawn_local(worker_main(rx));

                let (reply, done) = oneshot::channel();
                tx.send(WorkerMsg::Route {
                    specifier: spin_specifier,
                    method: "GET".to_string(),
                    request_json: "{}".to_string(),
                    page: false,
                    reply,
                })
                .await
                .unwrap();
                let err = tokio::time::timeout(Duration::from_millis(500), done)
                    .await
                    .expect("wedged route should be interrupted")
                    .unwrap()
                    .expect_err("wedged route should fail");
                assert!(
                    err.contains(WORKER_EXECUTION_TIMEOUT_ERROR),
                    "unexpected route error: {err}"
                );

                let (reply, done) = oneshot::channel();
                tx.send(WorkerMsg::Route {
                    specifier: ok_specifier,
                    method: "GET".to_string(),
                    request_json: "{}".to_string(),
                    page: false,
                    reply,
                })
                .await
                .unwrap();
                let response = tokio::time::timeout(Duration::from_millis(500), done)
                    .await
                    .expect("worker should recover after rebuilding the isolate")
                    .unwrap()
                    .expect("healthy route should pass after timeout recycle");
                assert_eq!(response.status, 200);
                match response.body {
                    RouteBody::Full(body) => assert_eq!(body, "ok"),
                    other => panic!("expected full response body, got {other:?}"),
                }

                drop(tx);
                worker.await.unwrap();
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn readable_stream_desired_size_tracks_queue_depth() {
        let mut runtime = runtime_with_bootstrap();
        let promise = runtime
            .execute_script(
                "beater:readable-stream-desired-size",
                r#"
                (async () => {
                const assert = (condition, message) => {
                  if (!condition) throw new Error(message);
                };
                let controller;
                const stream = new ReadableStream({
                  start(c) {
                    controller = c;
                  },
                });
                await Promise.resolve();

                assert(controller.desiredSize === 1, `initial desiredSize ${controller.desiredSize}`);
                controller.enqueue("a");
                assert(controller.desiredSize === 0, `queued desiredSize ${controller.desiredSize}`);

                const reader = stream.getReader();
                reader.read();
                assert(controller.desiredSize === 1, `drained desiredSize ${controller.desiredSize}`);

                controller.enqueue("b");
                assert(controller.desiredSize === 0, `requeued desiredSize ${controller.desiredSize}`);
                })()
                "#,
            )
            .expect("ReadableStream desiredSize regression script");
        let resolved = runtime.resolve(promise);
        runtime
            .with_event_loop_promise(resolved, PollEventLoopOptions::default())
            .await
            .expect("ReadableStream desiredSize regression should pass");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn stream_chunk_op_waits_for_bounded_channel_capacity() {
        let mut runtime = runtime_with_bootstrap();
        let (tx, mut rx) = stream_body_channel();
        for _ in 0..STREAM_CHUNK_QUEUE_CAPACITY {
            tx.try_send(Ok(Bytes::from_static(b"queued")))
                .expect("test stream channel should fill");
        }
        runtime
            .op_state()
            .borrow_mut()
            .borrow_mut::<WorkerState>()
            .streams
            .insert(42, tx);

        runtime
            .execute_script(
                "beater:bounded-stream-send",
                r#"
                globalThis.__beaterChunkSent = false;
                Deno.core.ops.op_beater_stream_chunk(42, new Uint8Array([9]))
                  .then((ok) => { globalThis.__beaterChunkSent = ok; });
                "#,
            )
            .expect("schedule bounded stream send");

        for _ in 0..3 {
            poll_event_loop_once(&mut runtime).expect("pending stream send should not fail");
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        assert!(!eval_bool(&mut runtime, "globalThis.__beaterChunkSent"));

        rx.recv()
            .await
            .expect("free one slot in the bounded stream channel")
            .expect("queued stream item should be ok");

        tokio::time::timeout(Duration::from_millis(100), async {
            while !eval_bool(&mut runtime, "globalThis.__beaterChunkSent") {
                poll_event_loop_once(&mut runtime).expect("bounded stream send should complete");
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .expect("stream send should wait until receiver capacity is available");

        let mut saw_backpressured_chunk = false;
        for _ in 0..STREAM_CHUNK_QUEUE_CAPACITY {
            let chunk = rx
                .recv()
                .await
                .expect("bounded channel should be refilled")
                .expect("stream item should be ok");
            if chunk == Bytes::from_static(&[9]) {
                saw_backpressured_chunk = true;
            }
        }
        assert!(saw_backpressured_chunk);
    }

    #[test]
    fn text_encoder_encode_into_reports_only_consumed_code_units() {
        let mut runtime = runtime_with_bootstrap();
        runtime
            .execute_script(
                "beater:text-encoder-encode-into",
                r#"
                const encoder = new TextEncoder();
                const assert = (condition, message) => {
                  if (!condition) throw new Error(message);
                };

                const ascii = new Uint8Array(2);
                const asciiResult = encoder.encodeInto("abcd", ascii);
                assert(asciiResult.read === 2, `ascii read ${asciiResult.read}`);
                assert(asciiResult.written === 2, `ascii written ${asciiResult.written}`);
                assert(ascii[0] === 97 && ascii[1] === 98, `ascii bytes ${Array.from(ascii)}`);

                const piPartial = new Uint8Array(2);
                const piPartialResult = encoder.encodeInto("a\u03c0", piPartial);
                assert(piPartialResult.read === 1, `pi partial read ${piPartialResult.read}`);
                assert(piPartialResult.written === 1, `pi partial written ${piPartialResult.written}`);
                assert(piPartial[0] === 97 && piPartial[1] === 0, `pi partial bytes ${Array.from(piPartial)}`);

                const emojiTooSmall = new Uint8Array(3);
                const emojiTooSmallResult = encoder.encodeInto("\ud83d\ude00a", emojiTooSmall);
                assert(emojiTooSmallResult.read === 0, `emoji small read ${emojiTooSmallResult.read}`);
                assert(emojiTooSmallResult.written === 0, `emoji small written ${emojiTooSmallResult.written}`);

                const emojiFits = new Uint8Array(5);
                const emojiFitsResult = encoder.encodeInto("\ud83d\ude00a", emojiFits);
                assert(emojiFitsResult.read === 3, `emoji fit read ${emojiFitsResult.read}`);
                assert(emojiFitsResult.written === 5, `emoji fit written ${emojiFitsResult.written}`);
                assert(
                  Array.from(emojiFits).join(",") === "240,159,152,128,97",
                  `emoji bytes ${Array.from(emojiFits)}`,
                );
                "#,
            )
            .expect("TextEncoder.encodeInto regression script");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn unhandled_rejections_are_reported_without_poisoning_the_runtime() {
        let mut runtime = runtime_with_bootstrap();
        runtime
            .execute_script(
                "beater:unhandled-rejection",
                "Promise.reject(new Error('stray rejection'));",
            )
            .expect("schedule unhandled rejection");
        runtime
            .run_event_loop(Default::default())
            .await
            .expect("handled unhandled rejection should not fail the event loop");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn unprintable_unhandled_rejections_are_still_handled() {
        let mut runtime = runtime_with_bootstrap();
        runtime
            .execute_script(
                "beater:unprintable-rejection",
                r#"
                const badObject = {
                  toJSON() { throw new Error("toJSON exploded"); },
                  [Symbol.toPrimitive]() { throw new Error("toPrimitive exploded"); },
                  toString() { throw new Error("toString exploded"); },
                };
                Promise.reject(badObject);

                const badError = new Error("bad stack");
                Object.defineProperty(badError, "stack", {
                  get() { throw new Error("stack exploded"); },
                });
                Promise.reject(badError);
                "#,
            )
            .expect("schedule unprintable rejection");
        runtime
            .run_event_loop(Default::default())
            .await
            .expect("unprintable rejection should not fail the event loop");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn throwing_timer_callbacks_are_reported_without_rejecting_timer_promises() {
        let mut runtime = runtime_with_bootstrap();
        runtime
            .execute_script(
                "beater:throwing-timer",
                "setTimeout(() => { throw new Error('timer boom'); }, 0);",
            )
            .expect("schedule throwing timer");
        runtime
            .run_event_loop(Default::default())
            .await
            .expect("throwing timer callback should not fail the event loop");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn clearing_stale_timer_ids_does_not_block_live_timers() {
        let mut runtime = runtime_with_bootstrap();
        let promise = runtime
            .execute_script(
                "beater:stale-timer-clears",
                r#"
                (async () => {
                  let fired = 0;
                  clearTimeout(1);
                  const first = setTimeout(() => { fired += 1; }, 0);
                  await Deno.core.ops.op_beater_sleep(1);
                  if (fired !== 1) throw new Error(`first timer fired ${fired} times`);

                  clearTimeout(first);
                  for (let id = 10_000; id < 11_000; id += 1) clearTimeout(id);

                  const second = setTimeout(() => { fired += 1; }, 0);
                  await Deno.core.ops.op_beater_sleep(1);
                  if (fired !== 2) throw new Error(`second timer fired ${fired} times`);

                  clearInterval(3);
                  const interval = setInterval(() => {
                    fired += 1;
                    clearInterval(interval);
                  }, 0);
                  await Deno.core.ops.op_beater_sleep(1);
                  if (fired !== 3) throw new Error(`interval fired ${fired} times`);
                })()
                "#,
            )
            .expect("schedule stale timer clear regression");
        let resolved = runtime.resolve(promise);
        runtime
            .with_event_loop_promise(resolved, PollEventLoopOptions::default())
            .await
            .expect("stale timer clears should not block live timers");
    }

    #[test]
    fn shutdown_failure_aborts_active_streams() {
        let mut runtime = JsRuntime::new(RuntimeOptions {
            extensions: vec![beater_ext::init()],
            ..Default::default()
        });
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        register_page_stream(&mut runtime, 7, tx);

        assert!(active_streams(&runtime));

        fail_active_streams_for_shutdown(&mut runtime);

        assert!(!active_streams(&runtime));
        let err = rx
            .try_recv()
            .expect("stream should receive a shutdown result")
            .expect_err("shutdown should abort the stream");
        assert_eq!(err.to_string(), WORKER_SHUTDOWN_STREAM_ERROR);
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn worker_channel_close_aborts_active_streams() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let (tx, rx) = mpsc::channel::<WorkerMsg>(4);
                let worker = tokio::task::spawn_local(worker_main(rx));
                let mut streams = Vec::new();

                for stream_id in [7, 8] {
                    let (reply, done) = oneshot::channel();
                    tx.send(WorkerMsg::RegisterTestStream { stream_id, reply })
                        .await
                        .unwrap();
                    streams.push(done.await.unwrap());
                }

                drop(tx);

                for mut stream in streams {
                    let err = stream
                        .recv()
                        .await
                        .expect("stream should receive shutdown result")
                        .expect_err("shutdown should abort active stream");
                    assert_eq!(err.to_string(), WORKER_SHUTDOWN_STREAM_ERROR);
                    assert!(stream.recv().await.is_none());
                }

                worker.await.unwrap();
            })
            .await;
    }
}
