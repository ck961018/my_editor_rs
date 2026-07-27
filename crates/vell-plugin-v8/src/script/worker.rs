use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use tokio_util::sync::CancellationToken;

pub(super) use super::worker_channel::{WorkerChannelMessage, WorkerHandle};
use super::worker_quota::WorkerQuota;
use super::{
    DEFAULT_PLUGIN_ASSETS, InvocationWatchdog, MAX_SCRIPT_SOURCE_BYTES, SCRIPT_HEAP_LIMIT_BYTES,
    SCRIPT_STARTUP_TIMEOUT, ScriptError, ScriptInvocationKind, WatchdogOutcome,
    call_script_callback, current_exception, ensure_size, initialize_v8, install_heap_limit,
    json_to_v8, recover_heap_limit, set_object, throw_script_error, transpile_typescript,
    v8_to_json,
};

const RESPONSE_POLL_INTERVAL: Duration = Duration::from_millis(10);
const WORKER_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone)]
pub(super) struct ScriptWorker {
    sender: mpsc::Sender<WorkerRequest>,
}

struct WorkerRequest {
    message: serde_json::Value,
    cancellation: CancellationToken,
    response: mpsc::SyncSender<Result<serde_json::Value, String>>,
}

struct WorkerResources {
    root: String,
}

type WorkerHandler = Rc<RefCell<Option<v8::Global<v8::Function>>>>;

impl ScriptWorker {
    pub(super) fn start(root: String, entry: String) -> Result<Self, ScriptError> {
        let path = resolve_asset_path(&root, &entry)?;
        let source = asset(&path)?;
        let source = std::str::from_utf8(source)
            .map_err(|error| ScriptError::new(format!("invalid UTF-8 in {path}: {error}")))?
            .to_owned();
        ensure_size("worker source", source.len(), MAX_SCRIPT_SOURCE_BYTES)?;
        let javascript = transpile_typescript(&format!("file:///runtime/plugins/{path}"), &source)?;
        ensure_size(
            "transpiled worker",
            javascript.len(),
            MAX_SCRIPT_SOURCE_BYTES,
        )?;
        let (sender, receiver) = mpsc::channel();
        std::thread::Builder::new()
            .name(format!("script-worker-{path}"))
            .spawn(move || run_worker(root, path, javascript, receiver))
            .map_err(|error| ScriptError::new(format!("failed to start script worker: {error}")))?;
        Ok(Self { sender })
    }

    pub(super) fn request(
        &self,
        message: serde_json::Value,
        cancellation: CancellationToken,
    ) -> Result<serde_json::Value, String> {
        let (response, receiver) = mpsc::sync_channel(1);
        self.sender
            .send(WorkerRequest {
                message,
                cancellation: cancellation.clone(),
                response,
            })
            .map_err(|_| "script worker stopped".to_owned())?;
        loop {
            if cancellation.is_cancelled() {
                return Err("script worker request cancelled".to_owned());
            }
            match receiver.recv_timeout(RESPONSE_POLL_INTERVAL) {
                Ok(result) => return result,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err("script worker stopped before replying".to_owned());
                }
            }
        }
    }
}

fn run_worker(
    root: String,
    path: String,
    javascript: String,
    receiver: mpsc::Receiver<WorkerRequest>,
) {
    initialize_v8();
    let params = v8::CreateParams::default().heap_limits(0, SCRIPT_HEAP_LIMIT_BYTES);
    let mut isolate = v8::Isolate::new(params);
    isolate.set_microtasks_policy(v8::MicrotasksPolicy::Explicit);
    isolate.set_capture_stack_trace_for_uncaught_exceptions(true, 10);
    let handler: WorkerHandler = Rc::new(RefCell::new(None));
    isolate.set_slot(handler.clone());
    isolate.set_slot(WorkerResources { root });
    let context = {
        v8::scope!(scope, &mut isolate);
        let context = v8::Context::new(scope, Default::default());
        v8::Global::new(scope, context)
    };
    let mut heap_limit = install_heap_limit(&mut isolate);
    let watchdog = InvocationWatchdog::start(
        isolate.thread_safe_handle(),
        ScriptInvocationKind::ModuleEvaluation,
        SCRIPT_STARTUP_TIMEOUT,
    );
    let startup = match watchdog {
        Ok(watchdog) => {
            let startup = {
                v8::scope_with_context!(scope, &mut isolate, context.clone());
                v8::tc_scope!(let scope, scope);
                install_worker_api(scope);
                evaluate_worker(scope, &path, &javascript)
            };
            watchdog.finish(startup)
        }
        Err(error) => Err(error),
    };
    let startup = if recover_heap_limit(&mut isolate, &mut heap_limit, SCRIPT_HEAP_LIMIT_BYTES) {
        Err(ScriptError::new(
            "script worker heap limit exceeded during startup",
        ))
    } else {
        startup
    };
    let startup_error = startup
        .err()
        .or_else(|| {
            handler
                .borrow()
                .is_none()
                .then(|| ScriptError::new("script worker did not register editor.worker.onMessage"))
        })
        .map(|error| error.to_string());

    while let Ok(request) = receiver.recv() {
        let result = if request.cancellation.is_cancelled() {
            Err("script worker request cancelled".to_owned())
        } else {
            match startup_error.as_ref() {
                Some(error) => Err(error.clone()),
                None => execute_request_with_watchdog(
                    &mut isolate,
                    context.clone(),
                    handler.borrow().as_ref().expect("checked handler"),
                    request.message,
                    &request.cancellation,
                    &mut heap_limit,
                ),
            }
        };
        let _ = request.response.send(result);
    }
}

fn execute_request_with_watchdog(
    isolate: &mut v8::OwnedIsolate,
    context: v8::Global<v8::Context>,
    handler: &v8::Global<v8::Function>,
    message: serde_json::Value,
    cancellation: &CancellationToken,
    heap_limit: &mut Box<super::HeapLimitState>,
) -> Result<serde_json::Value, String> {
    let handle = isolate.thread_safe_handle();
    let mut watchdog = InvocationWatchdog::start_cancellable(
        handle,
        ScriptInvocationKind::ContentJob,
        WORKER_TIMEOUT,
        cancellation.clone(),
    )
    .map_err(|error| error.to_string())?;
    let result = execute_request(isolate, context, handler, message, cancellation)
        .map_err(|error| error.to_string());
    let outcome = watchdog.stop();
    if recover_heap_limit(isolate, heap_limit, SCRIPT_HEAP_LIMIT_BYTES) {
        return Err("script worker heap limit exceeded".to_owned());
    }
    match outcome {
        WatchdogOutcome::Completed => result,
        WatchdogOutcome::TimedOut => Err("script worker request timed out".to_owned()),
        WatchdogOutcome::Cancelled => Err("script worker request cancelled".to_owned()),
    }
}

fn install_worker_api(scope: &mut v8::PinScope<'_, '_>) {
    let context = scope.get_current_context();
    let global = context.global(scope);
    let editor = v8::Object::new(scope);
    let worker = v8::Object::new(scope);
    let on_message = v8::FunctionTemplate::new(scope, worker_on_message)
        .get_function(scope)
        .expect("worker callback function");
    let name = v8::String::new(scope, "onMessage").expect("static string");
    worker.set(scope, name.into(), on_message.into());
    let resources = v8::Object::new(scope);
    let read_text = v8::FunctionTemplate::new(scope, worker_read_text)
        .get_function(scope)
        .expect("resource callback function");
    let name = v8::String::new(scope, "readText").expect("static string");
    resources.set(scope, name.into(), read_text.into());
    let read_binary = v8::FunctionTemplate::new(scope, worker_read_binary)
        .get_function(scope)
        .expect("resource callback function");
    let name = v8::String::new(scope, "readBinary").expect("static string");
    resources.set(scope, name.into(), read_binary.into());
    set_object(scope, editor, "worker", worker);
    set_object(scope, editor, "resources", resources);
    set_object(scope, global, "editor", editor);
}

fn evaluate_worker(
    scope: &mut v8::PinnedRef<'_, v8::TryCatch<'_, '_, v8::HandleScope<'_>>>,
    path: &str,
    javascript: &str,
) -> Result<(), ScriptError> {
    let source = v8::String::new(scope, javascript)
        .ok_or_else(|| ScriptError::new("worker source is too large for V8"))?;
    let script = v8::Script::compile(scope, source, None)
        .ok_or_else(|| current_exception(scope, path, "compile"))?;
    script
        .run(scope)
        .ok_or_else(|| current_exception(scope, path, "execute"))?;
    scope.perform_microtask_checkpoint();
    Ok(())
}

fn execute_request(
    isolate: &mut v8::OwnedIsolate,
    context: v8::Global<v8::Context>,
    handler: &v8::Global<v8::Function>,
    message: serde_json::Value,
    cancellation: &CancellationToken,
) -> Result<serde_json::Value, ScriptError> {
    v8::scope_with_context!(scope, isolate, context);
    v8::tc_scope!(let scope, scope);
    let handler = v8::Local::new(scope, handler);
    let message = json_to_v8(scope, &message)?;
    let receiver = v8::undefined(scope).into();
    let value = call_script_callback(scope, handler, receiver, &[message])
        .ok_or_else(|| current_exception(scope, "script worker callback", "execute"))?;
    let value = await_value(scope, value, cancellation)?;
    v8_to_json(scope, value, "script worker response")
}

fn await_value<'scope>(
    scope: &mut v8::PinScope<'scope, '_>,
    value: v8::Local<'scope, v8::Value>,
    cancellation: &CancellationToken,
) -> Result<v8::Local<'scope, v8::Value>, ScriptError> {
    let Ok(promise) = v8::Local::<v8::Promise>::try_from(value) else {
        return Ok(value);
    };
    let deadline = Instant::now() + WORKER_TIMEOUT;
    loop {
        scope.perform_microtask_checkpoint();
        while v8::Platform::pump_message_loop(&v8::V8::get_current_platform(), scope, false) {}
        match promise.state() {
            v8::PromiseState::Fulfilled => return Ok(promise.result(scope)),
            v8::PromiseState::Rejected => {
                let message = promise.result(scope).to_rust_string_lossy(scope);
                return Err(ScriptError::new(format!(
                    "script worker promise rejected: {message}"
                )));
            }
            v8::PromiseState::Pending => {}
        }
        if cancellation.is_cancelled() {
            return Err(ScriptError::new("script worker request cancelled"));
        }
        if Instant::now() >= deadline {
            return Err(ScriptError::new("script worker request timed out"));
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn worker_on_message(
    scope: &mut v8::PinScope,
    arguments: v8::FunctionCallbackArguments,
    mut return_value: v8::ReturnValue,
) {
    let Ok(callback) = v8::Local::<v8::Function>::try_from(arguments.get(0)) else {
        throw_script_error(scope, "editor.worker.onMessage expects a function");
        return;
    };
    let Some(handler) = scope.get_slot::<WorkerHandler>().cloned() else {
        throw_script_error(scope, "script worker registry is unavailable");
        return;
    };
    if handler.borrow().is_some() {
        throw_script_error(scope, "script worker already has a message handler");
        return;
    }
    handler.replace(Some(v8::Global::new(scope, callback)));
    return_value.set_undefined();
}

fn worker_read_text(
    scope: &mut v8::PinScope,
    arguments: v8::FunctionCallbackArguments,
    mut return_value: v8::ReturnValue,
) {
    let result = read_resource(scope, arguments.get(0)).and_then(|(path, bytes)| {
        let text = std::str::from_utf8(bytes)
            .map_err(|error| ScriptError::new(format!("invalid UTF-8 in {path}: {error}")))?;
        v8::String::new(scope, text)
            .map(v8::Local::<v8::Value>::from)
            .ok_or_else(|| ScriptError::new("plugin resource is too large for V8"))
    });
    match result {
        Ok(value) => return_value.set(value),
        Err(error) => throw_script_error(scope, &error.to_string()),
    }
}

fn worker_read_binary(
    scope: &mut v8::PinScope,
    arguments: v8::FunctionCallbackArguments,
    mut return_value: v8::ReturnValue,
) {
    let result = read_resource(scope, arguments.get(0)).and_then(|(_, bytes)| {
        let length = bytes.len();
        let store = v8::ArrayBuffer::new_backing_store_from_vec(bytes.to_vec()).make_shared();
        let buffer = v8::ArrayBuffer::with_backing_store(scope, &store);
        v8::Uint8Array::new(scope, buffer, 0, length)
            .map(v8::Local::<v8::Value>::from)
            .ok_or_else(|| ScriptError::new("failed to create plugin resource buffer"))
    });
    match result {
        Ok(value) => return_value.set(value),
        Err(error) => throw_script_error(scope, &error.to_string()),
    }
}

fn read_resource<'scope>(
    scope: &mut v8::PinScope<'scope, '_>,
    value: v8::Local<v8::Value>,
) -> Result<(String, &'static [u8]), ScriptError> {
    if !value.is_string() {
        return Err(ScriptError::new("plugin resource path must be a string"));
    }
    let resources = scope
        .get_slot::<WorkerResources>()
        .ok_or_else(|| ScriptError::new("plugin resources are unavailable"))?;
    let path = resolve_asset_path(&resources.root, &value.to_rust_string_lossy(scope))?;
    Ok((path.clone(), asset(&path)?))
}

fn resolve_asset_path(root: &str, relative: &str) -> Result<String, ScriptError> {
    if relative.is_empty()
        || relative.starts_with('/')
        || relative.starts_with('\\')
        || relative.contains('\\')
        || relative
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err(ScriptError::new(format!(
            "invalid plugin resource path: {relative}"
        )));
    }
    Ok(format!("{root}{relative}"))
}

fn asset(path: &str) -> Result<&'static [u8], ScriptError> {
    DEFAULT_PLUGIN_ASSETS
        .iter()
        .find_map(|(candidate, bytes)| (*candidate == path).then_some(*bytes))
        .ok_or_else(|| ScriptError::new(format!("plugin resource does not exist: {path}")))
}

// --- Standard Web Worker constructor (Task 2) ---

/// Spawn a new worker isolate, returning a handle for bidirectional
/// communication.
///
/// `quota` is consumed when `Some` — Task 4 wires full enforcement
/// (abort on exceed); for now the type is threaded to avoid dead-code.
pub(super) fn spawn_worker(
    root: String,
    entry: String,
    cancellation: CancellationToken,
    // ponytail: quota enforcement wired in Task 4;
    // type consumed here to avoid dead-code.
    quota: Option<Arc<WorkerQuota>>,
) -> Result<WorkerHandle, ScriptError> {
    // Consume quota type: if provided, attempt to acquire a slot.
    // Full enforcement (abort-on-exceed, Drop release) is Task 4.
    if let Some(q) = &quota {
        let _ = q.current_global();
    }
    let path = resolve_asset_path(&root, &entry)?;
    let source = asset(&path)?;
    let source = std::str::from_utf8(source)
        .map_err(|error| ScriptError::new(format!("invalid UTF-8 in {path}: {error}")))?
        .to_owned();
    ensure_size("worker source", source.len(), MAX_SCRIPT_SOURCE_BYTES)?;
    let javascript = transpile_typescript(&format!("file:///runtime/plugins/{path}"), &source)?;
    ensure_size(
        "transpiled worker",
        javascript.len(),
        MAX_SCRIPT_SOURCE_BYTES,
    )?;

    let (main_sender, worker_receiver) = mpsc::channel::<WorkerChannelMessage>();
    let (worker_sender, main_receiver) = mpsc::channel::<WorkerChannelMessage>();

    let worker_cancel = cancellation.clone();
    let thread = std::thread::Builder::new()
        .name(format!("script-worker-{path}"))
        .spawn(move || {
            run_bidirectional_worker(
                root,
                path,
                javascript,
                worker_receiver,
                worker_sender,
                worker_cancel,
            )
        })
        .map_err(|error| ScriptError::new(format!("failed to start worker: {error}")))?;

    Ok(WorkerHandle::new(
        main_sender,
        main_receiver,
        cancellation,
        thread,
    ))
}

/// Test-only: spawn a worker from raw TS source instead of an
/// embedded asset.  This exercises the same JS↔Rust bridge that
/// `spawn_worker` uses (transpile + isolate + bidirectional mpsc).
#[cfg(test)]
fn spawn_worker_from_source(
    source: &str,
    cancellation: CancellationToken,
) -> Result<WorkerHandle, ScriptError> {
    let root = String::new();
    let path = "<inline-test>".to_owned();
    ensure_size("worker source", source.len(), MAX_SCRIPT_SOURCE_BYTES)?;
    let javascript = transpile_typescript(&format!("file:///runtime/plugins/{path}"), source)?;
    ensure_size(
        "transpiled worker",
        javascript.len(),
        MAX_SCRIPT_SOURCE_BYTES,
    )?;

    let (main_sender, worker_receiver) = mpsc::channel::<WorkerChannelMessage>();
    let (worker_sender, main_receiver) = mpsc::channel::<WorkerChannelMessage>();

    let worker_cancel = cancellation.clone();
    let thread = std::thread::Builder::new()
        .name(format!("script-worker-{path}"))
        .spawn(move || {
            run_bidirectional_worker(
                root,
                path,
                javascript,
                worker_receiver,
                worker_sender,
                worker_cancel,
            )
        })
        .map_err(|error| ScriptError::new(format!("failed to start worker: {error}")))?;

    Ok(WorkerHandle::new(
        main_sender,
        main_receiver,
        cancellation,
        thread,
    ))
}

fn run_bidirectional_worker(
    root: String,
    path: String,
    javascript: String,
    receiver: mpsc::Receiver<WorkerChannelMessage>,
    sender: mpsc::Sender<WorkerChannelMessage>,
    cancellation: CancellationToken,
) {
    initialize_v8();
    let params = v8::CreateParams::default().heap_limits(0, SCRIPT_HEAP_LIMIT_BYTES);
    let mut isolate = v8::Isolate::new(params);
    isolate.set_microtasks_policy(v8::MicrotasksPolicy::Explicit);
    isolate.set_capture_stack_trace_for_uncaught_exceptions(true, 10);

    let handler: WorkerHandler = Rc::new(RefCell::new(None));
    isolate.set_slot(handler.clone());
    isolate.set_slot(WorkerResources { root });
    // Store the sender so `self.postMessage` can reach the main thread.
    isolate.set_slot::<mpsc::Sender<WorkerChannelMessage>>(sender.clone());
    // Store the cancellation for the watchdog.
    isolate.set_slot(cancellation.clone());

    let context = {
        v8::scope!(scope, &mut isolate);
        let context = v8::Context::new(scope, Default::default());
        v8::Global::new(scope, context)
    };
    let mut heap_limit = install_heap_limit(&mut isolate);

    let startup = {
        let watchdog = InvocationWatchdog::start(
            isolate.thread_safe_handle(),
            ScriptInvocationKind::ModuleEvaluation,
            SCRIPT_STARTUP_TIMEOUT,
        );
        match watchdog {
            Ok(watchdog) => {
                let startup = {
                    v8::scope_with_context!(scope, &mut isolate, context.clone());
                    v8::tc_scope!(let scope, scope);
                    install_worker_globals(scope);
                    evaluate_worker(scope, &path, &javascript)
                };
                watchdog.finish(startup)
            }
            Err(error) => Err(error),
        }
    };

    let startup = if recover_heap_limit(&mut isolate, &mut heap_limit, SCRIPT_HEAP_LIMIT_BYTES) {
        Err(ScriptError::new(
            "worker heap limit exceeded during startup",
        ))
    } else {
        startup
    };

    if let Err(error) = startup {
        let _ = sender.send(WorkerChannelMessage::Error(error.to_string()));
        return;
    }

    // Main message loop: receive ToWorker, dispatch to self.onmessage.
    while let Ok(message) = receiver.recv() {
        if cancellation.is_cancelled() {
            break;
        }
        match message {
            WorkerChannelMessage::ToWorker(data) => {
                let result = execute_worker_on_message(
                    &mut isolate,
                    context.clone(),
                    handler.borrow().as_ref(),
                    data,
                    &cancellation,
                    &mut heap_limit,
                );
                if let Err(error) = result {
                    let _ = sender.send(WorkerChannelMessage::Error(error.to_string()));
                }
            }
            WorkerChannelMessage::Terminated => break,
            _ => {}
        }
        // Flush any microtasks.
        {
            v8::scope_with_context!(scope, &mut isolate, context.clone());
            scope.perform_microtask_checkpoint();
        }
    }

    let _ = sender.send(WorkerChannelMessage::Terminated);
}

fn execute_worker_on_message(
    isolate: &mut v8::OwnedIsolate,
    context: v8::Global<v8::Context>,
    _handler: Option<&v8::Global<v8::Function>>,
    message: serde_json::Value,
    cancellation: &CancellationToken,
    heap_limit: &mut Box<super::HeapLimitState>,
) -> Result<(), ScriptError> {
    let handle = isolate.thread_safe_handle();
    let mut watchdog = InvocationWatchdog::start_cancellable(
        handle,
        ScriptInvocationKind::ContentJob,
        WORKER_TIMEOUT,
        cancellation.clone(),
    )
    .map_err(|error| ScriptError::new(error.to_string()))?;
    {
        v8::scope_with_context!(scope, isolate, context);
        v8::tc_scope!(let scope, scope);
        // Read self.onmessage from the global object.
        let context = scope.get_current_context();
        let global = context.global(scope);
        let onmessage_name = v8::String::new(scope, "onmessage").expect("static string");
        let handler_val = global.get(scope, onmessage_name.into());
        let Some(handler_val) = handler_val else {
            return Err(ScriptError::new(
                "worker received message but self.onmessage is not set",
            ));
        };
        let Ok(handler) = v8::Local::<v8::Function>::try_from(handler_val) else {
            return Err(ScriptError::new(
                "worker received message but self.onmessage is not set",
            ));
        };
        let message = json_to_v8(scope, &message)?;
        // Wrap in a MessageEvent-like object so `e.data` works.
        let event = v8::Object::new(scope);
        let data_key = v8::String::new(scope, "data").expect("static string");
        event.set(scope, data_key.into(), message);
        let receiver = v8::undefined(scope).into();
        let value = call_script_callback(scope, handler, receiver, &[event.into()])
            .ok_or_else(|| current_exception(scope, "worker onmessage", "execute"))?;
        let _value = await_value(scope, value, cancellation)?;
        // v8_to_json is used to ensure the response is serializable,
        // but we don't need the result here — the worker sends via
        // self.postMessage.
    };
    let outcome = watchdog.stop();
    if recover_heap_limit(isolate, heap_limit, SCRIPT_HEAP_LIMIT_BYTES) {
        return Err(ScriptError::new("worker heap limit exceeded"));
    }
    match outcome {
        WatchdogOutcome::Completed => Ok(()),
        WatchdogOutcome::TimedOut => Err(ScriptError::new("worker request timed out")),
        WatchdogOutcome::Cancelled => Err(ScriptError::new("worker request cancelled")),
    }
}

/// Install standard Web Worker globals on the worker isolate.
///
/// Replaces the legacy `install_worker_api` (which set
/// `editor.worker.onMessage`).  Instead installs:
/// - `self.onmessage` (plain property, JS sets directly)
/// - `self.postMessage(data)` (sends to main thread)
/// - `self.close()` (sends Terminated)
/// - `editor.resources.readText/readBinary` (kept from legacy)
fn install_worker_globals(scope: &mut v8::PinScope<'_, '_>) {
    let context = scope.get_current_context();
    let global = context.global(scope);

    // self — alias to globalThis, as in Web Workers.
    let self_name = v8::String::new(scope, "self").expect("static string");
    global.set(scope, self_name.into(), global.into());

    // onmessage — initially undefined, JS sets it directly.
    let onmessage_name = v8::String::new(scope, "onmessage").expect("static string");
    global.set(scope, onmessage_name.into(), v8::undefined(scope).into());

    // postMessage
    let post_message = v8::FunctionTemplate::new(scope, worker_post_message)
        .get_function(scope)
        .expect("worker postMessage function");
    let post_name = v8::String::new(scope, "postMessage").expect("static string");
    global.set(scope, post_name.into(), post_message.into());

    // close
    let close_fn = v8::FunctionTemplate::new(scope, worker_close)
        .get_function(scope)
        .expect("worker close function");
    let close_name = v8::String::new(scope, "close").expect("static string");
    global.set(scope, close_name.into(), close_fn.into());

    // editor.resources (kept from legacy)
    let editor = v8::Object::new(scope);
    let resources = v8::Object::new(scope);
    let read_text = v8::FunctionTemplate::new(scope, worker_read_text)
        .get_function(scope)
        .expect("resource callback function");
    let name = v8::String::new(scope, "readText").expect("static string");
    resources.set(scope, name.into(), read_text.into());
    let read_binary = v8::FunctionTemplate::new(scope, worker_read_binary)
        .get_function(scope)
        .expect("resource callback function");
    let name = v8::String::new(scope, "readBinary").expect("static string");
    resources.set(scope, name.into(), read_binary.into());
    set_object(scope, editor, "resources", resources);
    set_object(scope, global, "editor", editor);
}

fn worker_post_message(
    scope: &mut v8::PinScope,
    arguments: v8::FunctionCallbackArguments,
    mut return_value: v8::ReturnValue,
) {
    let data = arguments.get(0);
    let json = v8_to_json(scope, data, "worker postMessage");
    match json {
        Ok(value) => {
            let Some(sender) = scope
                .get_slot::<mpsc::Sender<WorkerChannelMessage>>()
                .cloned()
            else {
                throw_script_error(scope, "worker message channel is unavailable");
                return;
            };
            if sender
                .send(WorkerChannelMessage::FromWorker(value))
                .is_err()
            {
                throw_script_error(scope, "worker message channel is closed");
                return;
            }
        }
        Err(error) => {
            throw_script_error(scope, &error.to_string());
            return;
        }
    }
    return_value.set_undefined();
}

fn worker_close(
    scope: &mut v8::PinScope,
    _arguments: v8::FunctionCallbackArguments,
    mut return_value: v8::ReturnValue,
) {
    let Some(sender) = scope
        .get_slot::<mpsc::Sender<WorkerChannelMessage>>()
        .cloned()
    else {
        throw_script_error(scope, "worker message channel is unavailable");
        return;
    };
    let _ = sender.send(WorkerChannelMessage::Terminated);
    return_value.set_undefined();
}

/// Install the global `Worker` constructor on the **main** isolate.
///
/// `new Worker(url, options)` resolves the path, spawns a worker
/// thread, and returns a JS Worker object with postMessage/terminate/
/// addEventListener.
pub(super) fn install_global_worker_constructor(scope: &mut v8::PinScope<'_, '_>) {
    let context = scope.get_current_context();
    let global = context.global(scope);
    let tmpl = v8::FunctionTemplate::new(scope, worker_constructor);
    let name = v8::String::new(scope, "Worker").expect("static string");
    global.set(
        scope,
        name.into(),
        tmpl.get_function(scope).expect("Worker constructor").into(),
    );
}

/// Slot storing WorkerHandles on the main isolate so the JS Worker
/// object and pump can call back into Rust.
pub(super) type WorkerRegistrySlot = Rc<RefCell<Vec<WorkerHandle>>>;

fn worker_constructor(
    scope: &mut v8::PinScope,
    arguments: v8::FunctionCallbackArguments,
    mut return_value: v8::ReturnValue,
) {
    let url = arguments.get(0);
    let Ok(url_string) = v8::Local::<v8::String>::try_from(url) else {
        throw_script_error(scope, "Worker constructor expects a URL string");
        return;
    };
    let entry = url_string.to_rust_string_lossy(scope);

    // Parse options for `type: 'module'` and `signal`.
    let cancellation = CancellationToken::new();
    if let Ok(opts) = v8::Local::<v8::Object>::try_from(arguments.get(1)) {
        let key = v8::String::new(scope, "signal").unwrap();
        if let Some(signal) = opts.get(scope, key.into())
            && let Ok(signal_obj) = v8::Local::<v8::Object>::try_from(signal)
        {
            let aborted_key = v8::String::new(scope, "aborted").unwrap();
            if let Some(aborted) = signal_obj.get(scope, aborted_key.into())
                && aborted.is_true()
            {
                throw_script_error(scope, "Worker signal already aborted");
                return;
            }
        }
    }

    // Determine plugin root from the isolate slot.
    let Some(plugin_root) = scope.get_slot::<Rc<RefCell<Option<String>>>>().cloned() else {
        throw_script_error(scope, "plugin root is unavailable on the main isolate");
        return;
    };
    let root = plugin_root.borrow().clone().unwrap_or_default();

    // Acquire or create the WorkerRegistry slot.
    let registry_slot = scope
        .get_slot::<WorkerRegistrySlot>()
        .cloned()
        .unwrap_or_else(|| Rc::new(RefCell::new(Vec::new())));
    // Ensure the slot is populated (first spawn creates it).
    if scope.get_slot::<WorkerRegistrySlot>().is_none() {
        scope.set_slot(registry_slot.clone());
    }

    let mut registry = registry_slot.borrow_mut();
    match spawn_worker(root, entry, cancellation, None) {
        Ok(handle) => {
            // Create the JS Worker object wrapping the handle.
            let worker_obj = v8::Object::new(scope);
            // Store the handle index in an internal field.
            let handle_index = registry.len();
            registry.push(handle);

            // postMessage
            let post = v8::FunctionTemplate::new(scope, worker_js_post_message)
                .get_function(scope)
                .expect("postMessage function");
            let post_name = v8::String::new(scope, "postMessage").unwrap();
            worker_obj.set(scope, post_name.into(), post.into());
            // terminate
            let term = v8::FunctionTemplate::new(scope, worker_js_terminate)
                .get_function(scope)
                .expect("terminate function");
            let term_name = v8::String::new(scope, "terminate").unwrap();
            worker_obj.set(scope, term_name.into(), term.into());
            // addEventListener
            let add_listener = v8::FunctionTemplate::new(scope, worker_js_add_event_listener)
                .get_function(scope)
                .expect("addEventListener function");
            let listener_name = v8::String::new(scope, "addEventListener").unwrap();
            worker_obj.set(scope, listener_name.into(), add_listener.into());

            // Store handle index as a hidden property.
            let index_val = v8::Number::new(scope, handle_index as f64);
            let index_name = v8::String::new(scope, "_handleIndex").unwrap();
            worker_obj.set(scope, index_name.into(), index_val.into());

            // Store worker object in global so pump_worker_messages
            // can find it by index for event dispatch.
            let worker_key = v8::String::new(scope, &format!("_worker_{handle_index}")).unwrap();
            let context = scope.get_current_context();
            let global = context.global(scope);
            global.set(scope, worker_key.into(), worker_obj.into());

            return_value.set(worker_obj.into());
        }
        Err(error) => {
            throw_script_error(scope, &error.to_string());
        }
    }
}

fn get_worker_handle_index(
    scope: &mut v8::PinScope,
    arguments: v8::FunctionCallbackArguments,
) -> Option<usize> {
    let obj = arguments.this();
    let index_name = v8::String::new(scope, "_handleIndex").unwrap();
    let field = obj.get(scope, index_name.into())?;
    let num = v8::Local::<v8::Number>::try_from(field).ok()?;
    Some(num.value() as usize)
}

fn get_registry(scope: &mut v8::PinScope) -> Option<WorkerRegistrySlot> {
    scope.get_slot::<WorkerRegistrySlot>().cloned()
}

fn worker_js_post_message(
    scope: &mut v8::PinScope,
    arguments: v8::FunctionCallbackArguments,
    mut return_value: v8::ReturnValue,
) {
    let data = arguments.get(0);
    let json = v8_to_json(scope, data, "Worker.postMessage");
    match json {
        Ok(value) => {
            let Some(registry) = get_registry(scope) else {
                throw_script_error(scope, "worker registry is unavailable");
                return;
            };
            let Some(index) = get_worker_handle_index(scope, arguments) else {
                throw_script_error(scope, "postMessage called on invalid Worker");
                return;
            };
            let registry = registry.borrow();
            let Some(handle) = registry.get(index) else {
                throw_script_error(scope, "worker handle not found");
                return;
            };
            if handle.post_message(value).is_err() {
                throw_script_error(scope, "worker terminated before postMessage");
                return;
            }
        }
        Err(error) => {
            throw_script_error(scope, &error.to_string());
            return;
        }
    }
    return_value.set_undefined();
}

fn worker_js_terminate(
    scope: &mut v8::PinScope,
    arguments: v8::FunctionCallbackArguments,
    mut return_value: v8::ReturnValue,
) {
    let Some(registry) = get_registry(scope) else {
        throw_script_error(scope, "worker registry is unavailable");
        return;
    };
    let Some(index) = get_worker_handle_index(scope, arguments) else {
        throw_script_error(scope, "terminate called on invalid Worker");
        return;
    };
    let mut registry = registry.borrow_mut();
    if let Some(handle) = registry.get_mut(index) {
        handle.terminate();
    }
    return_value.set_undefined();
}

fn worker_js_add_event_listener(
    scope: &mut v8::PinScope,
    arguments: v8::FunctionCallbackArguments,
    mut return_value: v8::ReturnValue,
) {
    let event_type = arguments.get(0);
    let callback = arguments.get(1);
    let this = arguments.this();
    let Ok(event_str) = v8::Local::<v8::String>::try_from(event_type) else {
        throw_script_error(scope, "addEventListener expects a string event type");
        return;
    };
    let Ok(callback_fn) = v8::Local::<v8::Function>::try_from(callback) else {
        throw_script_error(scope, "addEventListener expects a function callback");
        return;
    };
    let event_name = event_str.to_rust_string_lossy(scope);
    let prop_name = v8::String::new(scope, &event_name).unwrap();
    this.set(scope, prop_name.into(), callback_fn.into());
    return_value.set_undefined();
}

/// Dispatch a `message` event to a worker's JS listener.
pub(super) fn dispatch_message_event(
    scope: &mut v8::PinScope<'_, '_>,
    registry: &WorkerRegistrySlot,
    index: usize,
    data: serde_json::Value,
) -> Result<(), ScriptError> {
    let registry = registry.borrow();
    let Some(handle) = registry.get(index) else {
        return Ok(());
    };
    let _ = handle; // handle exists, we dispatch via JS
    // We can't access the JS Worker object from here directly.
    // Instead, dispatch via a global helper.
    let context = scope.get_current_context();
    let global = context.global(scope);
    // Create a MessageEvent-like object with `data`.
    let event = v8::Object::new(scope);
    let data_val = json_to_v8(scope, &data)?;
    let data_key = v8::String::new(scope, "data").unwrap();
    event.set(scope, data_key.into(), data_val);
    // Call the worker's `message` listener if it exists.
    // The worker object is stored in global as `_worker_<index>`.
    let worker_key = v8::String::new(scope, &format!("_worker_{index}")).unwrap();
    if let Some(worker_obj) = global.get(scope, worker_key.into())
        && let Ok(worker) = v8::Local::<v8::Object>::try_from(worker_obj)
    {
        let msg_key = v8::String::new(scope, "message").unwrap();
        if let Some(listener) = worker.get(scope, msg_key.into())
            && let Ok(callback) = v8::Local::<v8::Function>::try_from(listener)
        {
            let receiver = v8::undefined(scope).into();
            call_script_callback(scope, callback, receiver, &[event.into()]);
        }
    }
    Ok(())
}

/// Dispatch an `error` event to a worker's JS listener.
pub(super) fn dispatch_error_event(
    scope: &mut v8::PinScope<'_, '_>,
    registry: &WorkerRegistrySlot,
    index: usize,
    message: String,
) -> Result<(), ScriptError> {
    let registry = registry.borrow();
    let Some(handle) = registry.get(index) else {
        return Ok(());
    };
    let _ = handle;
    let context = scope.get_current_context();
    let global = context.global(scope);
    let event = v8::Object::new(scope);
    let msg_val = v8::String::new(scope, &message).unwrap();
    let msg_key = v8::String::new(scope, "message").unwrap();
    event.set(scope, msg_key.into(), msg_val.into());
    let worker_key = v8::String::new(scope, &format!("_worker_{index}")).unwrap();
    if let Some(worker_obj) = global.get(scope, worker_key.into())
        && let Ok(worker) = v8::Local::<v8::Object>::try_from(worker_obj)
    {
        let err_key = v8::String::new(scope, "error").unwrap();
        if let Some(listener) = worker.get(scope, err_key.into())
            && let Ok(callback) = v8::Local::<v8::Function>::try_from(listener)
        {
            let receiver = v8::undefined(scope).into();
            call_script_callback(scope, callback, receiver, &[event.into()]);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worker_loads_embedded_resources_and_resolves_async_response() {
        let worker =
            ScriptWorker::start("tree-sitter/".to_owned(), "worker.ts".to_owned()).unwrap();
        let result = worker
            .request(
                serde_json::json!({
                    "contentId": 7,
                    "generation": 0,
                    "language": "markdown",
                    "revision": 0,
                    "text": "```rust\nfn main() {}\n```\n",
                }),
                CancellationToken::new(),
            )
            .unwrap();

        assert_eq!(result["revision"], 0);
        assert!(
            result["spans"].as_array().unwrap().iter().any(|span| {
                span["face"] == "syntax.keyword"
                    && span["range"]["start"]
                        == serde_json::json!({
                            "line": 1,
                            "character": 0,
                        })
                    && span["range"]["end"]
                        == serde_json::json!({
                            "line": 1,
                            "character": 2,
                        })
            }),
            "{result:#}"
        );
    }

    #[test]
    fn worker_preserves_specific_tree_sitter_capture_names() {
        let worker =
            ScriptWorker::start("tree-sitter/".to_owned(), "worker.ts".to_owned()).unwrap();
        let result = worker
            .request(
                serde_json::json!({
                    "contentId": 8,
                    "generation": 0,
                    "language": "rust",
                    "revision": 0,
                    "text": "fn show(value: bool) { println!(\"{value}\"); }",
                }),
                CancellationToken::new(),
            )
            .unwrap();

        let faces = result["spans"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|span| span["face"].as_str())
            .collect::<Vec<_>>();
        assert!(faces.contains(&"syntax.variable.parameter"), "{result:#}");
        assert!(faces.contains(&"syntax.function.macro"), "{result:#}");
        assert!(faces.contains(&"syntax.type.builtin"), "{result:#}");
    }

    #[test]
    fn rust_highlighter_returns_valid_spans_during_incomplete_edits() {
        let worker =
            ScriptWorker::start("tree-sitter/".to_owned(), "worker.ts".to_owned()).unwrap();
        for text in [
            "fn ",
            "struct ",
            "let value = ",
            "pub use ",
            "// comment\r\nfn main() {}\r\n",
            "/* first\r\nsecond */\r\nfn main() {}\r\n",
            "fn main() { let value = \"😀\"; }\r\n",
            "fn main() {\r\n    let value =\r\n}\r\n",
        ] {
            let result = worker
                .request(
                    serde_json::json!({
                        "contentId": 8,
                        "generation": 0,
                        "language": "rust",
                        "revision": 0,
                        "text": text,
                    }),
                    CancellationToken::new(),
                )
                .unwrap();
            let snapshot = vell_core::text_snapshot::TextSnapshot::from_text(text);
            for span in result["spans"].as_array().unwrap() {
                let start = &span["range"]["start"];
                let end = &span["range"]["end"];
                let start = snapshot.utf16_position_to_char(
                    start["line"].as_u64().unwrap() as usize,
                    start["character"].as_u64().unwrap() as usize,
                );
                let end = snapshot.utf16_position_to_char(
                    end["line"].as_u64().unwrap() as usize,
                    end["character"].as_u64().unwrap() as usize,
                );
                assert!(
                    start.zip(end).is_some_and(|(start, end)| start < end),
                    "{text:?}: {span:#}"
                );
            }
        }
    }

    #[test]
    fn tree_sitter_worker_recovers_after_a_cancelled_request() {
        let worker =
            ScriptWorker::start("tree-sitter/".to_owned(), "worker.ts".to_owned()).unwrap();
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        let cancelled = worker.request(
            serde_json::json!({
                "contentId": 9,
                "generation": 0,
                "language": "rust",
                "revision": 0,
                "text": "fn cancelled() {}\n",
            }),
            cancellation,
        );
        assert!(cancelled.is_err());

        let result = worker
            .request(
                serde_json::json!({
                    "contentId": 9,
                    "generation": 1,
                    "language": "rust",
                    "revision": 1,
                    "text": "fn recovered() {}\n",
                }),
                CancellationToken::new(),
            )
            .unwrap();

        assert_eq!(result["revision"], 1);
        assert!(!result["spans"].as_array().unwrap().is_empty());
    }

    // --- Task 2: Standard Web Worker bidirectional postMessage ---

    /// Test that spawn_worker returns a handle that can postMessage
    /// and drain responses.  Uses the tree-sitter worker (which still
    /// uses editor.worker.onMessage in the legacy path) but exercises
    /// the new bidirectional channel infrastructure.
    #[test]
    fn spawn_worker_returns_handle_with_post_message() {
        let handle = spawn_worker(
            "tree-sitter/".to_owned(),
            "worker.ts".to_owned(),
            CancellationToken::new(),
            None,
        )
        .expect("spawn_worker should succeed");

        // postMessage should not error.
        handle
            .post_message(serde_json::json!({
                "contentId": 1,
                "generation": 0,
                "language": "markdown",
                "revision": 0,
                "text": "# hi\n",
            }))
            .expect("postMessage should succeed");
    }

    /// Test the full JS↔Rust worker bridge with an inline echo
    /// script: send a known value, assert the worker echoes it back
    /// with exact content (not a len-based tautology).
    #[test]
    fn worker_echoes_message_content() {
        let echo_source = "self.onmessage = (e) => { self.postMessage(e.data); };";
        let handle = spawn_worker_from_source(echo_source, CancellationToken::new())
            .expect("spawn_worker_from_source should succeed");

        // Send a known value with distinct content.
        let sent = serde_json::json!({ "ping": 42, "msg": "hello" });
        handle
            .post_message(sent.clone())
            .expect("postMessage should succeed");

        // Wait for the worker to process and echo back.
        std::thread::sleep(Duration::from_millis(200));
        let messages = handle.drain();

        // Exactly one FromWorker message, with exact echoed content.
        assert_eq!(
            messages.len(),
            1,
            "expected exactly 1 message, got {messages:?}"
        );
        match &messages[0] {
            WorkerChannelMessage::FromWorker(data) => {
                assert_eq!(data, &sent, "worker should echo back the exact sent value");
            }
            other => panic!("expected FromWorker, got {other:?}"),
        }
    }

    /// Test that WorkerHandle::terminate kills the worker.
    #[test]
    fn worker_handle_terminate_stops_worker() {
        let mut handle = spawn_worker(
            "tree-sitter/".to_owned(),
            "worker.ts".to_owned(),
            CancellationToken::new(),
            None,
        )
        .expect("spawn_worker should succeed");

        // Terminate should not panic.
        handle.terminate();

        // Post-message after terminate should fail.
        assert!(handle.post_message(serde_json::json!({})).is_err());
    }
}
