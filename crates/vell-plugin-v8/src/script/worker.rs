use std::cell::RefCell;
use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use deno_ast::ModuleSpecifier;
use tokio_util::sync::CancellationToken;

pub(super) use super::worker_channel::{WorkerChannelMessage, WorkerHandle};
pub(crate) use super::worker_quota::{QuotaError, QuotaHandle, WorkerQuota};
use super::{
    AssetSource, DEFAULT_PLUGIN_ASSETS, InvocationWatchdog, MAX_SCRIPT_INPUT_BYTES, ModuleMap,
    SCRIPT_HEAP_LIMIT_BYTES, SCRIPT_STARTUP_TIMEOUT, ScriptError, ScriptInvocationKind,
    WatchdogOutcome, call_script_callback, current_exception, ensure_size,
    host_import_module_dynamically, host_initialize_import_meta, initialize_v8, install_heap_limit,
    json_to_v8, load_module_tree, recover_heap_limit, resolve_module, set_object,
    throw_dom_exception, throw_script_error, throw_type_error, v8_to_json,
};
#[cfg(test)]
use super::{MAX_SCRIPT_SOURCE_BYTES, transpile_typescript};

const WORKER_TIMEOUT: Duration = Duration::from_secs(30);

struct WorkerResources {
    root: String,
    source: AssetSource,
}

#[derive(Clone)]
struct WorkerUrlOrigin {
    source: AssetSource,
    root: String,
    plugin_id: String,
}

type WorkerUrlRegistrySlot = Rc<RefCell<HashMap<usize, WorkerUrlOrigin>>>;

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

/// Evaluate a worker as an ES module using `load_module_tree`.
/// This replaces `evaluate_worker` (single-file script) for workers
/// that need `import`, `export`, `import.meta.url`, and dynamic
/// `import()`.
fn evaluate_worker_module(
    scope: &mut v8::PinnedRef<'_, v8::TryCatch<'_, '_, v8::HandleScope<'_>>>,
    path: &str,
    modules: &Rc<RefCell<ModuleMap>>,
) -> Result<(), ScriptError> {
    let entry = PathBuf::from(path);
    let module = load_module_tree(scope, &entry, modules)?;
    match module.instantiate_module(scope, resolve_module) {
        Some(true) => {}
        _ => {
            return Err(current_exception(scope, path, "link"));
        }
    }
    if module.evaluate(scope).is_none() {
        return Err(current_exception(scope, path, "execute"));
    }
    scope.perform_microtask_checkpoint();
    match module.get_status() {
        v8::ModuleStatus::Evaluated => {}
        v8::ModuleStatus::Errored => {
            let message = module.get_exception().to_rust_string_lossy(scope);
            return Err(ScriptError::new(format!(
                "failed to execute {path}: {message}"
            )));
        }
        _ => {
            return Err(ScriptError::new(format!(
                "worker module did not evaluate: {path}"
            )));
        }
    }
    Ok(())
}

fn worker_read_text(
    scope: &mut v8::PinScope,
    arguments: v8::FunctionCallbackArguments,
    mut return_value: v8::ReturnValue,
) {
    let result = read_resource(scope, arguments.get(0)).and_then(|(path, bytes)| {
        let text = std::str::from_utf8(&bytes)
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
        let store = v8::ArrayBuffer::new_backing_store_from_vec(bytes).make_shared();
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
) -> Result<(String, Vec<u8>), ScriptError> {
    if !value.is_string() {
        return Err(ScriptError::new("plugin resource path must be a string"));
    }
    let resources = scope
        .get_slot::<WorkerResources>()
        .ok_or_else(|| ScriptError::new("plugin resources are unavailable"))?;
    let path = resolve_asset_path(&resources.root, &value.to_rust_string_lossy(scope))?;
    let bytes = match resources.source {
        AssetSource::Embedded => asset(&path)?.to_vec(),
        AssetSource::Filesystem => std::fs::read(&path).map_err(|error| {
            ScriptError::new(format!("failed to read plugin resource {path}: {error}"))
        })?,
    };
    ensure_size("plugin resource", bytes.len(), MAX_SCRIPT_INPUT_BYTES)?;
    Ok((path, bytes))
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
/// `quota` — when `Some`, enforces per-plugin/global/depth limits.
/// The `QuotaHandle` is held for the worker's lifetime (Drop releases).
/// `plugin_id` — identity for per-plugin quota tracking.
/// `depth` — current nesting depth (0 = top-level).
#[cfg(test)]
pub(super) fn spawn_worker(
    root: String,
    entry: String,
    cancellation: CancellationToken,
    quota: Option<Arc<WorkerQuota>>,
    plugin_id: String,
    depth: usize,
) -> Result<WorkerHandle, ScriptError> {
    spawn_worker_with_source(
        AssetSource::Embedded,
        root,
        entry,
        cancellation,
        quota,
        plugin_id,
        depth,
    )
}

fn spawn_worker_with_source(
    source: AssetSource,
    root: String,
    entry: String,
    cancellation: CancellationToken,
    quota: Option<Arc<WorkerQuota>>,
    plugin_id: String,
    depth: usize,
) -> Result<WorkerHandle, ScriptError> {
    // Enforce quota before spawning.
    let quota_handle: Option<QuotaHandle> = if let Some(q) = &quota {
        Some(q.try_acquire(&plugin_id, depth).map_err(|e| {
            let name = match e {
                QuotaError::PerPluginExceeded => "QuotaExceededError",
                QuotaError::GlobalExceeded => "QuotaExceededError",
                QuotaError::DepthExceeded => "QuotaExceededError",
            };
            ScriptError::new(format!("{name}: worker quota exceeded"))
        })?)
    } else {
        None
    };

    let path = resolve_asset_path(&root, &entry)?;
    // Verify the entry exists before spawning the thread.
    match source {
        AssetSource::Embedded => {
            asset(&path)?;
        }
        AssetSource::Filesystem => {
            std::fs::metadata(&path).map_err(|error| {
                ScriptError::new(format!("failed to read worker entry {path}: {error}"))
            })?;
        }
    }

    let (main_sender, worker_receiver) = mpsc::channel::<WorkerChannelMessage>();
    let (worker_sender, main_receiver) = mpsc::channel::<WorkerChannelMessage>();

    // Pass quota + plugin_id + depth to the worker thread so
    // nested spawns can use the same quota.
    let worker_quota = quota.clone();
    let worker_plugin_id = plugin_id.clone();
    let worker_cancel = cancellation.clone();
    let thread = std::thread::Builder::new()
        .name(format!("script-worker-{path}"))
        .spawn(move || {
            let _quota_handle = quota_handle;
            run_bidirectional_worker(
                source,
                root,
                path,
                None,
                worker_receiver,
                worker_sender,
                worker_cancel,
                worker_quota,
                worker_plugin_id,
                depth,
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
                AssetSource::Embedded,
                root,
                path,
                Some(javascript),
                worker_receiver,
                worker_sender,
                worker_cancel,
                None,
                "test".to_owned(),
                0,
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

#[allow(clippy::too_many_arguments)]
fn run_bidirectional_worker(
    source: AssetSource,
    root: String,
    path: String,
    inline_javascript: Option<String>,
    receiver: mpsc::Receiver<WorkerChannelMessage>,
    sender: mpsc::Sender<WorkerChannelMessage>,
    cancellation: CancellationToken,
    quota: Option<Arc<WorkerQuota>>,
    plugin_id: String,
    depth: usize,
) {
    initialize_v8();
    let params = v8::CreateParams::default().heap_limits(0, SCRIPT_HEAP_LIMIT_BYTES);
    let mut isolate = v8::Isolate::new(params);
    isolate.set_microtasks_policy(v8::MicrotasksPolicy::Explicit);
    isolate.set_capture_stack_trace_for_uncaught_exceptions(true, 10);
    // Wire import.meta.url and dynamic import() support.
    isolate.set_host_initialize_import_meta_object_callback(host_initialize_import_meta);
    isolate.set_host_import_module_dynamically_callback(host_import_module_dynamically);

    isolate.set_slot(WorkerResources { root, source });
    // Store the sender so `self.postMessage` can reach the main thread.
    isolate.set_slot::<mpsc::Sender<WorkerChannelMessage>>(sender.clone());
    // Store the cancellation for the watchdog.
    isolate.set_slot(cancellation.clone());
    isolate.set_slot(source);
    // Set up a ModuleMap for ES module loading.
    let module_root = PathBuf::from(path.rsplit_once('/').map(|(dir, _)| dir).unwrap_or(""));
    let modules = Rc::new(RefCell::new(ModuleMap::default()));
    modules.borrow_mut().reset(module_root);
    isolate.set_slot(modules.clone());
    // Store quota + plugin_id + depth so nested Worker
    // constructor can enforce limits.
    isolate.set_slot(quota);
    isolate.set_slot::<String>(plugin_id);
    isolate.set_slot(depth);
    // Worker registry for nested spawns.
    let worker_registry: WorkerRegistrySlot = Rc::new(RefCell::new(Vec::new()));
    isolate.set_slot(worker_registry);

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
                    match &inline_javascript {
                        Some(js) => evaluate_worker(scope, &path, js),
                        None => evaluate_worker_module(scope, &path, &modules),
                    }
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
        let _ = sender.send(WorkerChannelMessage::Error {
            message: error.to_string(),
            name: "Error".to_owned(),
        });
        return;
    }

    // Main message loop: receive ToWorker, dispatch to self.onmessage.
    // Use recv_timeout so we can periodically pump nested workers.
    // Wrap in catch_unwind so a Rust panic (e.g. from a .unwrap() on an
    // oversized V8 string) is reported as an ErrorEvent to main,
    // not a silent thread death.
    let panic_result = catch_unwind(AssertUnwindSafe(|| {
        while !cancellation.is_cancelled() {
            match receiver.recv_timeout(Duration::from_millis(50)) {
                Ok(message) => match message {
                    WorkerChannelMessage::ToWorker(data) => {
                        let result = execute_worker_on_message(
                            &mut isolate,
                            context.clone(),
                            data,
                            &cancellation,
                            &mut heap_limit,
                        );
                        if let Err(error) = result {
                            let name = error_name_for_worker(&error.to_string());
                            let _ = sender.send(WorkerChannelMessage::Error {
                                message: error.to_string(),
                                name,
                            });
                            break;
                        }
                    }
                    WorkerChannelMessage::Terminated => break,
                    _ => {}
                },
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
            // Flush any microtasks and pump nested workers.
            let registry_slot = isolate.get_slot::<WorkerRegistrySlot>().cloned();
            {
                v8::scope_with_context!(scope, &mut isolate, context.clone());
                scope.perform_microtask_checkpoint();
                if let Some(registry) = &registry_slot {
                    pump_nested_workers(scope, registry);
                }
            }
        }
        // Final pump before exiting.
        let final_registry = isolate.get_slot::<WorkerRegistrySlot>().cloned();
        {
            v8::scope_with_context!(scope, &mut isolate, context.clone());
            scope.perform_microtask_checkpoint();
            if let Some(registry) = &final_registry {
                pump_nested_workers(scope, registry);
                while v8::Platform::pump_message_loop(&v8::V8::get_current_platform(), scope, false)
                {
                }
            }
        }
    }));

    if let Err(panic) = panic_result {
        let msg = panic
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| panic.downcast_ref::<&str>().map(|s| s.to_string()))
            .unwrap_or_else(|| "unknown panic".to_owned());
        let _ = sender.send(WorkerChannelMessage::Error {
            message: format!("worker panic: {msg}"),
            name: "Error".to_owned(),
        });
    }

    let _ = sender.send(WorkerChannelMessage::Terminated);
}

fn execute_worker_on_message(
    isolate: &mut v8::OwnedIsolate,
    context: v8::Global<v8::Context>,
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
                    "worker promise rejected: {message}"
                )));
            }
            v8::PromiseState::Pending => {}
        }
        if cancellation.is_cancelled() {
            return Err(ScriptError::new("worker request cancelled"));
        }
        if Instant::now() >= deadline {
            return Err(ScriptError::new("worker request timed out"));
        }
        std::thread::sleep(Duration::from_millis(1));
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

    // Worker constructor — allows nested spawn inside worker
    // isolates.
    install_global_worker_constructor(scope);

    // AbortController — minimal standard global for signal-based
    // cancellation.
    install_abort_controller(scope);

    // URL — minimal global so workers can use standard
    // `new Worker(new URL("./x.ts", import.meta.url))`.
    install_url_global(scope);

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
    if let Some(cancellation) = scope.get_slot::<CancellationToken>() {
        cancellation.cancel();
    }
    let _ = sender.send(WorkerChannelMessage::Terminated);
    return_value.set_undefined();
}

/// Install a minimal `AbortController` global.
///
/// `new AbortController()` returns `{ signal, abort() }` where
/// `signal` is an `AbortSignal` with `{ aborted: bool }`.
/// Internally links `abort()` to a `CancellationToken` stored in
/// the isolate slot, so worker spawn can check it.
pub(super) fn install_abort_controller_global(scope: &mut v8::PinScope<'_, '_>) {
    install_abort_controller(scope);
}

/// Install a minimal `URL` global so workers and main isolate can use
/// `new URL("./x.ts", import.meta.url)`. Only stores the string and
/// exposes `.href` / `.pathname` / `toString()`.
pub(super) fn install_url_global(scope: &mut v8::PinScope<'_, '_>) {
    if scope.get_slot::<WorkerUrlRegistrySlot>().is_none() {
        scope.set_slot::<WorkerUrlRegistrySlot>(Rc::new(RefCell::new(HashMap::new())));
    }
    let context = scope.get_current_context();
    let global = context.global(scope);
    let tmpl = v8::FunctionTemplate::new(scope, url_constructor);
    let name = v8::String::new(scope, "URL").expect("static string");
    global.set(
        scope,
        name.into(),
        tmpl.get_function(scope).expect("URL constructor").into(),
    );
}

fn url_constructor(
    scope: &mut v8::PinScope,
    arguments: v8::FunctionCallbackArguments,
    mut return_value: v8::ReturnValue,
) {
    let relative = arguments.get(0);
    let base = arguments.get(1);
    // Resolve relative against base if both are strings.
    let resolved = if let Ok(base_str) = v8::Local::<v8::String>::try_from(base) {
        let base_s = base_str.to_rust_string_lossy(scope);
        if let Ok(rel_str) = v8::Local::<v8::String>::try_from(relative) {
            let rel_s = rel_str.to_rust_string_lossy(scope);
            // Simple resolution: if relative starts with
            // "./" or "../", strip the file:// prefix from
            // base, take the directory, and join.
            resolve_url(&base_s, &rel_s)
        } else {
            base_s
        }
    } else if let Ok(rel_str) = v8::Local::<v8::String>::try_from(relative) {
        rel_str.to_rust_string_lossy(scope)
    } else {
        String::new()
    };

    let obj = v8::Object::new(scope);
    let href = v8::String::new(scope, &resolved).unwrap();
    let href_key = v8::String::new(scope, "href").unwrap();
    obj.set(scope, href_key.into(), href.into());
    // pathname — same as href for our purposes.
    let path_key = v8::String::new(scope, "pathname").unwrap();
    obj.set(scope, path_key.into(), href.into());
    // toString — returns href.
    let to_string = v8::FunctionTemplate::new(scope, url_to_string)
        .get_function(scope)
        .expect("URL toString");
    let ts_name = v8::String::new(scope, "toString").unwrap();
    obj.set(scope, ts_name.into(), to_string.into());
    // Store href in a hidden field for toString to read.
    let internal = v8::String::new(scope, "_href").unwrap();
    obj.set(scope, internal.into(), href.into());

    let source = scope
        .get_slot::<AssetSource>()
        .copied()
        .unwrap_or(AssetSource::Embedded);
    let root = match source {
        AssetSource::Filesystem => scope.get_slot::<Rc<RefCell<ModuleMap>>>().map(|modules| {
            format!(
                "{}{separator}",
                modules.borrow().root().display(),
                separator = std::path::MAIN_SEPARATOR
            )
        }),
        AssetSource::Embedded => scope
            .get_slot::<Rc<RefCell<Option<String>>>>()
            .and_then(|root| root.borrow().clone())
            .or_else(|| {
                scope
                    .get_slot::<WorkerResources>()
                    .map(|resources| resources.root.clone())
            }),
    };
    if let Some(root) = root
        && let Some(registry) = scope.get_slot::<WorkerUrlRegistrySlot>()
    {
        let plugin_id = scope
            .get_slot::<String>()
            .cloned()
            .unwrap_or_else(|| root.clone());
        registry.borrow_mut().insert(
            obj.get_identity_hash().get() as usize,
            WorkerUrlOrigin {
                source,
                root,
                plugin_id,
            },
        );
    }
    return_value.set(obj.into());
}

fn url_to_string(
    scope: &mut v8::PinScope,
    arguments: v8::FunctionCallbackArguments,
    mut return_value: v8::ReturnValue,
) {
    let this = arguments.this();
    let key = v8::String::new(scope, "_href").unwrap();
    if let Some(href) = this.get(scope, key.into())
        && let Ok(s) = v8::Local::<v8::String>::try_from(href)
    {
        return_value.set(s.into());
    } else {
        return_value.set(v8::String::new(scope, "").unwrap().into());
    }
}

/// Resolve a relative URL against a base file:// URL.
/// ponytail: naive string join, no RFC 3986. Works for
/// `new URL("./child.ts", import.meta.url)` where
/// import.meta.url is `file:///runtime/plugins/...`.
fn resolve_url(base: &str, relative: &str) -> String {
    // Strip file:// prefix to get a path.
    let base_path = base.strip_prefix("file://").unwrap_or(base);
    // Take the directory of the base path.
    let dir = base_path.rsplit_once('/').map(|(d, _)| d).unwrap_or(".");
    let rel = relative.strip_prefix("./").unwrap_or(relative);
    // Rebuild as file:// URL so the Worker constructor's
    // `strip_prefix("file:///runtime/plugins/")` works.
    format!("file://{dir}/{rel}")
}

fn install_abort_controller(scope: &mut v8::PinScope<'_, '_>) {
    let context = scope.get_current_context();
    let global = context.global(scope);
    let tmpl = v8::FunctionTemplate::new(scope, abort_controller_constructor);
    let name = v8::String::new(scope, "AbortController").expect("static string");
    global.set(
        scope,
        name.into(),
        tmpl.get_function(scope)
            .expect("AbortController constructor")
            .into(),
    );
}

fn abort_controller_constructor(
    scope: &mut v8::PinScope,
    _arguments: v8::FunctionCallbackArguments,
    mut return_value: v8::ReturnValue,
) {
    let controller = v8::Object::new(scope);
    let token = CancellationToken::new();
    // Store the token in the object's internal field via a
    // hidden property.
    let signal = v8::Object::new(scope);
    let aborted_key = v8::String::new(scope, "aborted").unwrap();
    signal.set(
        scope,
        aborted_key.into(),
        v8::Boolean::new(scope, false).into(),
    );
    // Store the token on the signal object so abort() can
    // access it.
    // ponytail: we store the CancellationToken in an isolate slot
    // keyed by the signal object's identity. A production impl
    // would use External/internal fields, but this works for the
    // single-controller-per-call pattern.
    let token_slot: Rc<RefCell<Vec<(usize, CancellationToken)>>> = scope
        .get_slot::<Rc<RefCell<Vec<(usize, CancellationToken)>>>>()
        .cloned()
        .unwrap_or_else(|| {
            let s: Rc<RefCell<Vec<(usize, CancellationToken)>>> = Rc::new(RefCell::new(Vec::new()));
            s
        });
    if scope
        .get_slot::<Rc<RefCell<Vec<(usize, CancellationToken)>>>>()
        .is_none()
    {
        scope.set_slot(token_slot.clone());
    }
    let signal_id = signal.get_identity_hash().get() as usize;
    token_slot.borrow_mut().push((signal_id, token.clone()));

    let signal_name = v8::String::new(scope, "signal").unwrap();
    controller.set(scope, signal_name.into(), signal.into());

    // abort() — sets aborted=true, cancels the token.
    let abort_fn = v8::FunctionTemplate::new(scope, abort_signal_abort)
        .get_function(scope)
        .expect("abort function");
    let abort_name = v8::String::new(scope, "abort").unwrap();
    controller.set(scope, abort_name.into(), abort_fn.into());

    // Store the signal_id on the controller for abort() to find.
    let id_name = v8::String::new(scope, "_signalId").unwrap();
    controller.set(
        scope,
        id_name.into(),
        v8::Number::new(scope, signal_id as f64).into(),
    );

    return_value.set(controller.into());
}

fn abort_signal_abort(
    scope: &mut v8::PinScope,
    arguments: v8::FunctionCallbackArguments,
    mut return_value: v8::ReturnValue,
) {
    let this = arguments.this();
    let id_name = v8::String::new(scope, "_signalId").unwrap();
    let Some(id_val) = this.get(scope, id_name.into()) else {
        return_value.set_undefined();
        return;
    };
    let signal_id = v8::Local::<v8::Number>::try_from(id_val)
        .map(|n| n.value() as usize)
        .unwrap_or(0);

    // Set signal.aborted = true.
    let signal_name = v8::String::new(scope, "signal").unwrap();
    if let Some(signal) = this.get(scope, signal_name.into())
        && let Ok(signal_obj) = v8::Local::<v8::Object>::try_from(signal)
    {
        let aborted_key = v8::String::new(scope, "aborted").unwrap();
        signal_obj.set(
            scope,
            aborted_key.into(),
            v8::Boolean::new(scope, true).into(),
        );
    }

    // Cancel the token.
    if let Some(token_slot) = scope.get_slot::<Rc<RefCell<Vec<(usize, CancellationToken)>>>>() {
        let mut slot = token_slot.borrow_mut();
        if let Some(index) = slot.iter().position(|(id, _)| *id == signal_id) {
            let (_, token) = slot.swap_remove(index);
            token.cancel();
        }
    }
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
pub(super) type WorkerRegistrySlot = Rc<RefCell<Vec<Option<WorkerHandle>>>>;

fn worker_constructor(
    scope: &mut v8::PinScope,
    arguments: v8::FunctionCallbackArguments,
    mut return_value: v8::ReturnValue,
) {
    let url = arguments.get(0);
    let (entry, url_origin) = if let Ok(url_string) = v8::Local::<v8::String>::try_from(url) {
        (url_string.to_rust_string_lossy(scope), None)
    } else if let Ok(url_obj) = v8::Local::<v8::Object>::try_from(url) {
        let href_key = v8::String::new(scope, "href").unwrap();
        let entry = if let Some(href) = url_obj.get(scope, href_key.into())
            && let Ok(href_str) = v8::Local::<v8::String>::try_from(href)
        {
            href_str.to_rust_string_lossy(scope)
        } else {
            url.to_rust_string_lossy(scope)
        };
        let origin = scope
            .get_slot::<WorkerUrlRegistrySlot>()
            .and_then(|registry| {
                registry
                    .borrow()
                    .get(&(url_obj.get_identity_hash().get() as usize))
                    .cloned()
            });
        (entry, origin)
    } else {
        throw_script_error(scope, "Worker constructor expects a URL string");
        return;
    };
    let source = url_origin
        .as_ref()
        .map(|origin| origin.source)
        .or_else(|| scope.get_slot::<AssetSource>().copied())
        .unwrap_or(AssetSource::Embedded);
    let embedded_path = entry.strip_prefix("file:///runtime/plugins/");
    let (source, root, entry) = if source == AssetSource::Filesystem && entry.starts_with("file:") {
        let url = match ModuleSpecifier::parse(&entry) {
            Ok(url) => url,
            Err(error) => {
                throw_script_error(scope, &format!("invalid worker URL: {error}"));
                return;
            }
        };
        let path = match url.to_file_path() {
            Ok(path) => path,
            Err(()) => {
                throw_script_error(scope, "worker URL is not a file path");
                return;
            }
        };
        let path = match std::fs::canonicalize(path) {
            Ok(path) => path,
            Err(_) => {
                throw_script_error(scope, "worker URL is not a readable file path");
                return;
            }
        };
        let root_path = if let Some(origin) = &url_origin {
            PathBuf::from(&origin.root)
        } else if let Some(modules) = scope.get_slot::<Rc<RefCell<ModuleMap>>>() {
            modules.borrow().root().to_owned()
        } else {
            throw_script_error(scope, "worker module root is unavailable");
            return;
        };
        let Ok(relative) = path.strip_prefix(&root_path) else {
            throw_script_error(scope, "script import escapes the config directory");
            return;
        };
        let mut root = root_path.to_string_lossy().into_owned();
        if !root.ends_with(std::path::MAIN_SEPARATOR) {
            root.push(std::path::MAIN_SEPARATOR);
        }
        (
            AssetSource::Filesystem,
            root,
            relative.to_string_lossy().into_owned(),
        )
    } else if let Some(path) = embedded_path {
        let configured_root = url_origin
            .as_ref()
            .map(|origin| origin.root.clone())
            .or_else(|| {
                scope
                    .get_slot::<Rc<RefCell<Option<String>>>>()
                    .and_then(|root| root.borrow().clone())
            })
            .or_else(|| {
                scope
                    .get_slot::<WorkerResources>()
                    .map(|resources| resources.root.clone())
            });
        let (root, entry) = if let Some(root) = configured_root {
            let Some(entry) = path.strip_prefix(&root) else {
                throw_script_error(scope, "worker URL escapes the plugin directory");
                return;
            };
            (root, entry.to_owned())
        } else {
            path.rsplit_once('/')
                .map(|(root, entry)| (format!("{root}/"), entry.to_owned()))
                .unwrap_or_else(|| (String::new(), path.to_owned()))
        };
        (AssetSource::Embedded, root, entry)
    } else {
        let root = scope
            .get_slot::<Rc<RefCell<Option<String>>>>()
            .and_then(|root| root.borrow().clone())
            .or_else(|| {
                scope
                    .get_slot::<WorkerResources>()
                    .map(|resources| resources.root.clone())
            })
            .unwrap_or_default();
        let entry = entry.strip_prefix(&root).unwrap_or(&entry).to_owned();
        (source, root, entry)
    };

    // Parse options for `type: 'module'` and `signal`.
    // If signal is provided, extract its CancellationToken from
    // the isolate slot so abort() actually cancels the worker.
    let mut cancellation = CancellationToken::new();
    if let Ok(opts) = v8::Local::<v8::Object>::try_from(arguments.get(1)) {
        let type_key = v8::String::new(scope, "type").unwrap();
        if let Some(worker_type) = opts.get(scope, type_key.into())
            && !worker_type.is_undefined()
        {
            let Ok(worker_type) = v8::Local::<v8::String>::try_from(worker_type) else {
                throw_type_error(scope, "Worker option 'type' must be 'module'");
                return;
            };
            if worker_type.to_rust_string_lossy(scope) != "module" {
                throw_type_error(scope, "only module workers are supported");
                return;
            }
        }
        let key = v8::String::new(scope, "signal").unwrap();
        if let Some(signal) = opts.get(scope, key.into())
            && let Ok(signal_obj) = v8::Local::<v8::Object>::try_from(signal)
        {
            let aborted_key = v8::String::new(scope, "aborted").unwrap();
            if let Some(aborted) = signal_obj.get(scope, aborted_key.into())
                && aborted.is_true()
            {
                throw_dom_exception(scope, "AbortError", "Worker signal already aborted");
                return;
            }
            // Look up the signal's CancellationToken from the
            // isolate slot (stored by AbortController
            // constructor, keyed by signal identity hash).
            let signal_id = signal_obj.get_identity_hash().get() as usize;
            if let Some(token_slot) =
                scope.get_slot::<Rc<RefCell<Vec<(usize, CancellationToken)>>>>()
            {
                let slot = token_slot.borrow();
                if let Some((_, token)) = slot.iter().find(|(id, _)| *id == signal_id) {
                    cancellation = token.clone();
                }
            }
        }
    }

    // Determine plugin_id for quota tracking.
    let plugin_id = url_origin
        .map(|origin| origin.plugin_id)
        .or_else(|| scope.get_slot::<String>().cloned())
        .unwrap_or_else(|| root.clone());

    // Get or create the WorkerRegistry slot.
    let registry_slot = scope
        .get_slot::<WorkerRegistrySlot>()
        .cloned()
        .unwrap_or_else(|| Rc::new(RefCell::new(Vec::new())));
    if scope.get_slot::<WorkerRegistrySlot>().is_none() {
        scope.set_slot(registry_slot.clone());
    }

    // Get quota from the isolate slot.
    let quota = scope
        .get_slot::<Option<Arc<WorkerQuota>>>()
        .cloned()
        .flatten();

    // Get current depth for nested-spawn tracking.
    // Nested workers increment depth.
    let current_depth = scope.get_slot::<usize>().copied().unwrap_or(0);
    let depth = current_depth + 1;

    let mut registry = registry_slot.borrow_mut();
    match spawn_worker_with_source(source, root, entry, cancellation, quota, plugin_id, depth) {
        Ok(handle) => {
            // Create the JS Worker object wrapping the handle.
            let worker_obj = v8::Object::new(scope);
            // Store the handle index in an internal field.
            let handle_index = registry.len();
            registry.push(Some(handle));

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
            // removeEventListener
            let remove_listener = v8::FunctionTemplate::new(scope, worker_js_remove_event_listener)
                .get_function(scope)
                .expect("removeEventListener function");
            let remove_name = v8::String::new(scope, "removeEventListener").unwrap();
            worker_obj.set(scope, remove_name.into(), remove_listener.into());

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
            let msg = error.to_string();
            if msg.contains("QuotaExceededError") {
                throw_dom_exception(
                    scope,
                    "QuotaExceededError",
                    &msg.replace("QuotaExceededError: ", ""),
                );
            } else {
                throw_script_error(scope, &msg);
            }
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
            let Some(handle) = registry.get(index).and_then(Option::as_ref) else {
                throw_dom_exception(scope, "InvalidStateError", "worker is terminated");
                return;
            };
            if handle.post_message(value).is_err() {
                throw_dom_exception(
                    scope,
                    "InvalidStateError",
                    "worker terminated before postMessage",
                );
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
    if let Some(handle) = registry.get_mut(index).and_then(Option::take) {
        drop(handle);
    }
    let worker_key = v8::String::new(scope, &format!("_worker_{index}")).unwrap();
    let global = scope.get_current_context().global(scope);
    global.delete(scope, worker_key.into());
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
    let prop_name = v8::String::new(scope, &format!("_listeners_{event_name}")).unwrap();
    let listeners = this
        .get(scope, prop_name.into())
        .and_then(|value| v8::Local::<v8::Array>::try_from(value).ok())
        .unwrap_or_else(|| v8::Array::new(scope, 0));
    for index in 0..listeners.length() {
        if listeners
            .get_index(scope, index)
            .is_some_and(|listener| listener.strict_equals(callback_fn.into()))
        {
            return_value.set_undefined();
            return;
        }
    }
    listeners.set_index(scope, listeners.length(), callback_fn.into());
    this.set(scope, prop_name.into(), listeners.into());
    return_value.set_undefined();
}

fn worker_js_remove_event_listener(
    scope: &mut v8::PinScope,
    arguments: v8::FunctionCallbackArguments,
    mut return_value: v8::ReturnValue,
) {
    let event_type = arguments.get(0);
    let callback = arguments.get(1);
    let this = arguments.this();
    let Ok(event_str) = v8::Local::<v8::String>::try_from(event_type) else {
        throw_script_error(scope, "removeEventListener expects a string event type");
        return;
    };
    let Ok(callback_fn) = v8::Local::<v8::Function>::try_from(callback) else {
        throw_script_error(scope, "removeEventListener expects a function callback");
        return;
    };
    let event_name = event_str.to_rust_string_lossy(scope);
    let prop_name = v8::String::new(scope, &format!("_listeners_{event_name}")).unwrap();
    let Some(listeners) = this
        .get(scope, prop_name.into())
        .and_then(|value| v8::Local::<v8::Array>::try_from(value).ok())
    else {
        return_value.set_undefined();
        return;
    };
    let retained = v8::Array::new(scope, 0);
    for index in 0..listeners.length() {
        let Some(listener) = listeners.get_index(scope, index) else {
            continue;
        };
        if !listener.strict_equals(callback_fn.into()) {
            retained.set_index(scope, retained.length(), listener);
        }
    }
    this.set(scope, prop_name.into(), retained.into());
    return_value.set_undefined();
}

/// Pump messages from nested child workers to their JS listeners.
fn pump_nested_workers(scope: &mut v8::PinScope<'_, '_>, registry: &WorkerRegistrySlot) {
    let len = registry.borrow().len();
    for index in 0..len {
        let messages = {
            let r = registry.borrow();
            r.get(index)
                .and_then(Option::as_ref)
                .map(WorkerHandle::drain)
                .unwrap_or_default()
        };
        let mut terminated = false;
        for msg in messages {
            match msg {
                WorkerChannelMessage::FromWorker(data) => {
                    let _ = dispatch_message_event(scope, registry, index, data);
                }
                WorkerChannelMessage::Error { message, name } => {
                    let _ = dispatch_error_event(scope, registry, index, message, name);
                }
                WorkerChannelMessage::Terminated => terminated = true,
                WorkerChannelMessage::ToWorker(_) => {}
            }
        }
        if terminated && let Some(handle) = registry.borrow_mut().get_mut(index) {
            handle.take();
        }
    }
}

fn dispatch_worker_event<'scope>(
    scope: &mut v8::PinScope<'scope, '_>,
    worker: v8::Local<'scope, v8::Object>,
    event_name: &str,
    event: v8::Local<'scope, v8::Object>,
) {
    let receiver = worker.into();
    let handler_key = v8::String::new(scope, &format!("on{event_name}")).unwrap();
    if let Some(handler) = worker.get(scope, handler_key.into())
        && let Ok(callback) = v8::Local::<v8::Function>::try_from(handler)
    {
        call_script_callback(scope, callback, receiver, &[event.into()]);
    }
    let listeners_key = v8::String::new(scope, &format!("_listeners_{event_name}")).unwrap();
    let Some(listeners) = worker
        .get(scope, listeners_key.into())
        .and_then(|value| v8::Local::<v8::Array>::try_from(value).ok())
    else {
        return;
    };
    for index in 0..listeners.length() {
        if let Some(listener) = listeners.get_index(scope, index)
            && let Ok(callback) = v8::Local::<v8::Function>::try_from(listener)
        {
            call_script_callback(scope, callback, receiver, &[event.into()]);
        }
    }
}

/// Dispatch a `message` event to a worker's JS listener.
pub(super) fn dispatch_message_event(
    scope: &mut v8::PinScope<'_, '_>,
    registry: &WorkerRegistrySlot,
    index: usize,
    data: serde_json::Value,
) -> Result<(), ScriptError> {
    let registry_ref = registry.borrow();
    if registry_ref.get(index).and_then(Option::as_ref).is_none() {
        return Ok(());
    }
    drop(registry_ref);
    // We can't access the JS Worker object from here directly.
    // Instead, dispatch via a global helper.
    let context = scope.get_current_context();
    let global = context.global(scope);
    // Create a MessageEvent-like object with `data`.
    let event = v8::Object::new(scope);
    let data_val = json_to_v8(scope, &data)?;
    let data_key = v8::String::new(scope, "data").unwrap();
    event.set(scope, data_key.into(), data_val);
    let type_key = v8::String::new(scope, "type").unwrap();
    let event_type = v8::String::new(scope, "message").unwrap();
    event.set(scope, type_key.into(), event_type.into());
    // Call the worker's message listeners if they exist.
    // The worker object is stored in global as `_worker_<index>`.
    let worker_key = v8::String::new(scope, &format!("_worker_{index}")).unwrap();
    if let Some(worker_obj) = global.get(scope, worker_key.into())
        && let Ok(worker) = v8::Local::<v8::Object>::try_from(worker_obj)
    {
        dispatch_worker_event(scope, worker, "message", event);
    }
    Ok(())
}

/// Classify a worker error string into a standard
/// DOMException-like `name` for the ErrorEvent.
fn error_name_for_worker(message: &str) -> String {
    if message.contains("timed out") || message.contains("timeout") {
        "TimeoutError".to_owned()
    } else if message.contains("heap limit") {
        "ResourceExhausted".to_owned()
    } else {
        "Error".to_owned()
    }
}

/// Dispatch an `error` event to a worker's JS listener.
pub(super) fn dispatch_error_event(
    scope: &mut v8::PinScope<'_, '_>,
    registry: &WorkerRegistrySlot,
    index: usize,
    message: String,
    name: String,
) -> Result<(), ScriptError> {
    let registry_ref = registry.borrow();
    if registry_ref.get(index).and_then(Option::as_ref).is_none() {
        return Ok(());
    }
    drop(registry_ref);
    let context = scope.get_current_context();
    let global = context.global(scope);
    let event = v8::Object::new(scope);
    let msg_val = v8::String::new(scope, &message).unwrap();
    let msg_key = v8::String::new(scope, "message").unwrap();
    event.set(scope, msg_key.into(), msg_val.into());
    let name_val = v8::String::new(scope, &name).unwrap();
    let name_key = v8::String::new(scope, "name").unwrap();
    event.set(scope, name_key.into(), name_val.into());
    let type_key = v8::String::new(scope, "type").unwrap();
    let event_type = v8::String::new(scope, "error").unwrap();
    event.set(scope, type_key.into(), event_type.into());
    // ponytail: filename/lineno/colno use sentinels because the
    // WorkerChannelMessage::Error path does not carry the V8 Message
    // (stack/line info). Wire real values when the channel carries them.
    let filename_val = v8::String::new(scope, "unknown").unwrap();
    let filename_key = v8::String::new(scope, "filename").unwrap();
    event.set(scope, filename_key.into(), filename_val.into());
    let lineno_key = v8::String::new(scope, "lineno").unwrap();
    event.set(scope, lineno_key.into(), v8::Number::new(scope, 0.0).into());
    let colno_key = v8::String::new(scope, "colno").unwrap();
    event.set(scope, colno_key.into(), v8::Number::new(scope, 0.0).into());
    let worker_key = v8::String::new(scope, &format!("_worker_{index}")).unwrap();
    if let Some(worker_obj) = global.get(scope, worker_key.into())
        && let Ok(worker) = v8::Local::<v8::Object>::try_from(worker_obj)
    {
        dispatch_worker_event(scope, worker, "error", event);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn start_tree_sitter_worker(cancellation: CancellationToken) -> WorkerHandle {
        spawn_worker(
            "tree-sitter/".to_owned(),
            "worker.ts".to_owned(),
            cancellation,
            None,
            "tree-sitter".to_owned(),
            1,
        )
        .unwrap()
    }

    fn worker_response(
        worker: &WorkerHandle,
        message: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        worker
            .post_message(message)
            .map_err(|error| error.to_string())?;
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while std::time::Instant::now() < deadline {
            for message in worker.drain() {
                match message {
                    WorkerChannelMessage::FromWorker(value) => return Ok(value),
                    WorkerChannelMessage::Error { message, .. } => return Err(message),
                    WorkerChannelMessage::Terminated => return Err("worker terminated".to_owned()),
                    WorkerChannelMessage::ToWorker(_) => {}
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        Err("worker response timed out".to_owned())
    }

    #[test]
    fn worker_loads_embedded_resources_and_resolves_async_response() {
        let worker = start_tree_sitter_worker(CancellationToken::new());
        let result = worker_response(
            &worker,
            serde_json::json!({
                    "contentId": 7,
                    "generation": 0,
                    "language": "markdown",
                    "revision": 0,
                "text": "```rust\nfn main() {}\n```\n",
            }),
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
        let worker = start_tree_sitter_worker(CancellationToken::new());
        let result = worker_response(
            &worker,
            serde_json::json!({
                "contentId": 8,
                "generation": 0,
                "language": "rust",
                "revision": 0,
                "text": "fn show(value: bool) { println!(\"{value}\"); }",
            }),
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
        let worker = start_tree_sitter_worker(CancellationToken::new());
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
            let result = worker_response(
                &worker,
                serde_json::json!({
                    "contentId": 8,
                    "generation": 0,
                    "language": "rust",
                    "revision": 0,
                    "text": text,
                }),
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
    fn tree_sitter_worker_can_be_replaced_after_cancellation() {
        let cancellation = CancellationToken::new();
        let worker = start_tree_sitter_worker(cancellation.clone());
        cancellation.cancel();
        drop(worker);

        let replacement = start_tree_sitter_worker(CancellationToken::new());
        let result = worker_response(
            &replacement,
            serde_json::json!({
                "contentId": 9,
                "language": "rust",
                "revision": 1,
                "text": "fn recovered() {}\n",
            }),
        )
        .unwrap();

        assert_eq!(result["revision"], 1);
        assert!(!result["spans"].as_array().unwrap().is_empty());
    }

    // --- Task 2: Standard Web Worker bidirectional postMessage ---

    /// Test that spawn_worker returns a handle that can postMessage
    /// and drain responses.
    #[test]
    fn spawn_worker_returns_handle_with_post_message() {
        let handle = spawn_worker(
            "tree-sitter/".to_owned(),
            "worker.ts".to_owned(),
            CancellationToken::new(),
            None,
            "test".to_owned(),
            0,
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
    fn worker_self_close_stops_thread() {
        let handle = spawn_worker_from_source(
            "self.onmessage = () => self.close();",
            CancellationToken::new(),
        )
        .expect("worker should start");
        handle.post_message(serde_json::json!(null)).unwrap();
        std::thread::sleep(Duration::from_millis(200));
        assert!(handle.is_finished(), "self.close() must stop the worker");
    }

    #[test]
    fn worker_handle_terminate_stops_worker() {
        let mut handle = spawn_worker(
            "tree-sitter/".to_owned(),
            "worker.ts".to_owned(),
            CancellationToken::new(),
            None,
            "test".to_owned(),
            0,
        )
        .expect("spawn_worker should succeed");

        // Terminate should not panic.
        handle.terminate();

        // Post-message after terminate should fail.
        assert!(handle.post_message(serde_json::json!({})).is_err());
    }

    // --- Task 3: ES module workers with import.meta.url + import ---

    /// Test that a worker can access import.meta.url and it
    /// returns a file:// URL ending with the worker's filename.
    #[test]
    fn worker_import_meta_url_returns_file_url() {
        let handle = spawn_worker(
            "test-worker/".to_owned(),
            "meta-worker.ts".to_owned(),
            CancellationToken::new(),
            None,
            "test".to_owned(),
            0,
        )
        .expect("spawn_worker should succeed");

        handle
            .post_message(serde_json::json!(null))
            .expect("postMessage should succeed");

        std::thread::sleep(Duration::from_millis(300));
        let messages = handle.drain();

        assert_eq!(
            messages.len(),
            1,
            "expected exactly 1 message, got {messages:?}"
        );
        match &messages[0] {
            WorkerChannelMessage::FromWorker(data) => {
                let url = data.as_str().expect("import.meta.url should be a string");
                assert!(
                    url.starts_with("file:///"),
                    "expected file:// URL, got {url}"
                );
                assert!(
                    url.ends_with("/meta-worker.ts"),
                    "expected URL ending with /meta-worker.ts, got {url}"
                );
            }
            other => panic!("expected FromWorker, got {other:?}"),
        }
    }

    /// Test that a worker can statically import a sibling module.
    #[test]
    fn worker_es_module_imports_sibling() {
        let handle = spawn_worker(
            "test-worker/".to_owned(),
            "import-worker.ts".to_owned(),
            CancellationToken::new(),
            None,
            "test".to_owned(),
            0,
        )
        .expect("spawn_worker should succeed");

        handle
            .post_message(serde_json::json!(null))
            .expect("postMessage should succeed");

        std::thread::sleep(Duration::from_millis(300));
        let messages = handle.drain();

        assert_eq!(
            messages.len(),
            1,
            "expected exactly 1 message, got {messages:?}"
        );
        match &messages[0] {
            WorkerChannelMessage::FromWorker(data) => {
                assert_eq!(
                    data,
                    &serde_json::json!(42),
                    "worker should send back imported value 42"
                );
            }
            other => panic!("expected FromWorker, got {other:?}"),
        }
    }

    /// Test that a worker can use dynamic import() to load a module.
    #[test]
    fn worker_dynamic_import_resolves() {
        let handle = spawn_worker(
            "test-worker/".to_owned(),
            "dynamic-import-worker.ts".to_owned(),
            CancellationToken::new(),
            None,
            "test".to_owned(),
            0,
        )
        .expect("spawn_worker should succeed");

        handle
            .post_message(serde_json::json!(null))
            .expect("postMessage should succeed");

        // Dynamic import is async — give it more time.
        std::thread::sleep(Duration::from_millis(500));
        let messages = handle.drain();

        assert_eq!(
            messages.len(),
            1,
            "expected exactly 1 message, got {messages:?}"
        );
        match &messages[0] {
            WorkerChannelMessage::FromWorker(data) => {
                assert_eq!(
                    data,
                    &serde_json::json!(42),
                    "worker should send back dynamically imported value 42"
                );
            }
            other => panic!("expected FromWorker, got {other:?}"),
        }
    }

    // --- Task 4: AbortSignal, nested spawn, quota enforcement ---

    /// Test that terminating a worker via WorkerHandle
    /// releases the quota slot.
    #[test]
    fn worker_terminate_releases_quota() {
        let quota = Arc::new(WorkerQuota::new(8, 32, 4));
        let mut handle = spawn_worker(
            "test-worker/".to_owned(),
            "meta-worker.ts".to_owned(),
            CancellationToken::new(),
            Some(quota.clone()),
            "test".to_owned(),
            0,
        )
        .expect("spawn_worker should succeed");

        assert_eq!(quota.current_global(), 1);
        handle.terminate();
        assert_eq!(quota.current_global(), 0);
    }

    #[test]
    fn worker_pump_reaps_self_closed_handle() {
        use super::super::host::ScriptHost;

        let mut host = ScriptHost::new();
        host.execute_embedded_plugin(
            "test-worker/close-main.ts",
            "const worker = new Worker('close-worker.ts', { type: 'module' });\n\
             worker.postMessage(null);",
        )
        .unwrap();
        std::thread::sleep(Duration::from_millis(200));
        host.pump_worker_messages();

        let context = host.context.clone();
        let live = host
            .invoke(ScriptInvocationKind::Action, |isolate| {
                v8::scope_with_context!(scope, isolate, context);
                let registry = scope
                    .get_slot::<WorkerRegistrySlot>()
                    .expect("worker registry");
                Ok(registry.borrow().iter().flatten().count())
            })
            .unwrap();
        assert_eq!(live, 0, "finished worker handle must be reaped");
    }

    #[test]
    fn worker_self_close_releases_quota_before_handle_drop() {
        let quota = Arc::new(WorkerQuota::new(1, 1, 4));
        let handle = spawn_worker(
            "test-worker/".to_owned(),
            "close-worker.ts".to_owned(),
            CancellationToken::new(),
            Some(quota.clone()),
            "test".to_owned(),
            1,
        )
        .expect("worker should start");
        handle.post_message(serde_json::json!(null)).unwrap();
        std::thread::sleep(Duration::from_millis(200));
        assert!(handle.is_finished(), "self.close() must stop the worker");

        let replacement = spawn_worker(
            "test-worker/".to_owned(),
            "meta-worker.ts".to_owned(),
            CancellationToken::new(),
            Some(quota),
            "test".to_owned(),
            1,
        );
        assert!(
            replacement.is_ok(),
            "finished worker must release quota before its handle is dropped"
        );
    }

    /// Test that nested spawn works: a worker can `new Worker()`
    /// inside its own isolate, and messages flow through.
    #[test]
    fn worker_dynamic_import_resolves_from_importer_directory() {
        let handle = spawn_worker(
            "test-worker/".to_owned(),
            "nested-dynamic-entry.ts".to_owned(),
            CancellationToken::new(),
            None,
            "test".to_owned(),
            1,
        )
        .expect("spawn_worker should succeed");

        let result = worker_response(&handle, serde_json::Value::Null).unwrap();
        assert_eq!(result, serde_json::json!(43));
    }

    #[test]
    fn worker_nested_spawn_child() {
        let quota = Arc::new(WorkerQuota::new(8, 32, 4));
        let handle = spawn_worker(
            "test-worker/".to_owned(),
            "parent.ts".to_owned(),
            CancellationToken::new(),
            Some(quota.clone()),
            "test".to_owned(),
            0,
        )
        .expect("spawn_worker should succeed");

        handle
            .post_message(serde_json::json!("hello from parent"))
            .expect("postMessage to parent should succeed");

        // Parent spawns child, forwards message, child echoes back.
        std::thread::sleep(Duration::from_millis(500));
        let messages = handle.drain();

        assert_eq!(
            messages.len(),
            1,
            "expected 1 message from child via parent, got {messages:?}"
        );
        match &messages[0] {
            WorkerChannelMessage::FromWorker(data) => {
                assert_eq!(
                    data,
                    &serde_json::json!("hello from parent"),
                    "child should echo back parent's message"
                );
            }
            other => panic!("expected FromWorker, got {other:?}"),
        }
    }

    /// Test that per-plugin quota limit throws QuotaExceededError.
    #[test]
    fn worker_quota_per_plugin_exceeded() {
        let quota = Arc::new(WorkerQuota::new(2, 32, 4));
        // Spawn 2 (at limit).
        let _h1 = spawn_worker(
            "test-worker/".to_owned(),
            "meta-worker.ts".to_owned(),
            CancellationToken::new(),
            Some(quota.clone()),
            "test".to_owned(),
            0,
        )
        .expect("first 2 should succeed");
        let _h2 = spawn_worker(
            "test-worker/".to_owned(),
            "meta-worker.ts".to_owned(),
            CancellationToken::new(),
            Some(quota.clone()),
            "test".to_owned(),
            0,
        )
        .expect("second should succeed");
        // Third should fail.
        let result = spawn_worker(
            "test-worker/".to_owned(),
            "meta-worker.ts".to_owned(),
            CancellationToken::new(),
            Some(quota.clone()),
            "test".to_owned(),
            0,
        );
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.to_string().contains("QuotaExceededError"),
            "expected QuotaExceededError, got {err}"
        );
    }

    /// Test that depth quota limit throws QuotaExceededError.
    /// Test uses reduced limits for speed; production is 8/32/4.
    #[test]
    fn worker_quota_depth_exceeded() {
        // depth=2: worker depths 1 and 2 succeed; depth 3 fails.
        let quota = Arc::new(WorkerQuota::new(2, 8, 2));
        let result = spawn_worker(
            "test-worker/".to_owned(),
            "meta-worker.ts".to_owned(),
            CancellationToken::new(),
            Some(quota.clone()),
            "test".to_owned(),
            3,
        );
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.to_string().contains("QuotaExceededError"),
            "expected QuotaExceededError for depth, got {err}"
        );
    }

    /// Test that aborting the cancellation token actually
    /// stops a running worker. This verifies the AbortSignal
    /// wiring: the token passed to spawn_worker is linked to
    /// the JS AbortController.abort() call.
    #[test]
    fn worker_aborts_on_signal() {
        let token = CancellationToken::new();
        let handle = spawn_worker(
            "test-worker/".to_owned(),
            "meta-worker.ts".to_owned(),
            token.clone(),
            None,
            "test".to_owned(),
            0,
        )
        .expect("spawn_worker should succeed");

        // Send a message so the worker starts processing.
        handle
            .post_message(serde_json::json!("hello"))
            .expect("postMessage should succeed");

        // Abort the token — simulates AbortController.abort().
        token.cancel();

        // The worker thread should terminate. Give it a moment,
        // then verify the handle's thread is no longer alive.
        std::thread::sleep(Duration::from_millis(100));
        assert!(
            handle.is_finished(),
            "worker thread should be terminated after abort"
        );
    }

    /// Test that the global quota limit is enforced.
    /// Test uses reduced limits for speed; production is
    /// per-plugin=8, global=32, depth=4.
    #[test]
    fn worker_quota_global_exceeded() {
        // per-plugin=2, global=3, depth=4.
        let quota = Arc::new(WorkerQuota::new(2, 3, 4));
        // Spawn 2 from plugin "a" (at per-plugin limit).
        let _h1 = spawn_worker(
            "test-worker/".to_owned(),
            "meta-worker.ts".to_owned(),
            CancellationToken::new(),
            Some(quota.clone()),
            "a".to_owned(),
            0,
        )
        .expect("first from plugin a should succeed");
        let _h2 = spawn_worker(
            "test-worker/".to_owned(),
            "meta-worker.ts".to_owned(),
            CancellationToken::new(),
            Some(quota.clone()),
            "a".to_owned(),
            0,
        )
        .expect("second from plugin a should succeed");
        // Spawn 1 from plugin "b" (global now at 3).
        let _h3 = spawn_worker(
            "test-worker/".to_owned(),
            "meta-worker.ts".to_owned(),
            CancellationToken::new(),
            Some(quota.clone()),
            "b".to_owned(),
            0,
        )
        .expect("first from plugin b should succeed");
        // 4th spawn should fail — global limit (3) exceeded.
        let result = spawn_worker(
            "test-worker/".to_owned(),
            "meta-worker.ts".to_owned(),
            CancellationToken::new(),
            Some(quota.clone()),
            "b".to_owned(),
            0,
        );
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.to_string().contains("QuotaExceededError"),
            "expected QuotaExceededError for global, got {err}"
        );
    }

    /// JS-level AbortSignal integration test: exercises the full
    /// bridge — `new AbortController()` → `new Worker(url, {signal})`
    /// → `controller.abort()` → worker terminates. This covers the
    /// isolate-slot storage + `get_identity_hash` token extraction in
    /// `worker_constructor`, which the Rust-level
    /// `worker_aborts_on_signal` test bypasses.
    #[test]
    fn worker_aborts_on_signal_via_js() {
        use super::super::host::ScriptHost;

        let mut host = ScriptHost::new();

        // Use execute_embedded_plugin so plugin_root is set to
        // `test-worker/` during evaluation. The root is read by
        // worker_constructor at spawn time; clearing it after is
        // fine — the worker already has its root.
        host.execute_embedded_plugin(
            "test-worker/abort-inline.ts",
            "const ctrl = new AbortController();\n\
globalThis.w = new Worker(\n\
  \"abort-fixture.ts\",\n\
  { type: \"module\", signal: ctrl.signal },\n\
);\n\
ctrl.abort();",
        )
        .expect("plugin evaluation should succeed");

        // Give the worker thread time to see the cancellation
        // and exit its recv loop (50ms poll interval).
        std::thread::sleep(Duration::from_millis(200));

        // Access the WorkerRegistrySlot from the isolate to
        // check the worker thread has terminated.
        let context = host.context.clone();
        let finished = host
            .invoke(ScriptInvocationKind::Action, |isolate| {
                v8::scope_with_context!(scope, isolate, context);
                let registry = scope.get_slot::<WorkerRegistrySlot>().cloned();
                if let Some(registry) = registry {
                    let registry = registry.borrow();
                    Ok(registry.iter().flatten().all(WorkerHandle::is_finished))
                } else {
                    Ok(true)
                }
            })
            .expect("isolate access should succeed");

        assert!(
            finished,
            "worker thread should be terminated after JS abort()"
        );
    }

    #[test]
    fn worker_spawned_after_plugin_evaluation_keeps_plugin_root() {
        use super::super::host::ScriptHost;

        let mut host = ScriptHost::new();
        host.execute_embedded_plugin(
            "test-worker/deferred-spawn.ts",
            "globalThis.workerResult = null;\n\
const workerUrl = new URL(\n\
  './meta-worker.ts',\n\
  'file:///runtime/plugins/test-worker/deferred-spawn.ts',\n\
);\n\
globalThis.spawnWorker = () => {\n\
  const w = new Worker(workerUrl, { type: 'module' });\n\
  w.onmessage = (e) => { globalThis.workerResult = e.data; };\n\
  w.postMessage({});\n\
};",
        )
        .expect("plugin evaluation should succeed");
        let directory = tempfile::tempdir().unwrap();
        let config = directory.path().join("config.ts");
        std::fs::write(&config, "globalThis.userConfigLoaded = true;").unwrap();
        host.execute_module(&config)
            .expect("filesystem module should load");
        host.evaluate_script("globalThis.spawnWorker(); null")
            .expect("deferred worker construction should succeed");

        std::thread::sleep(Duration::from_millis(300));
        host.pump_worker_messages();

        let result = host
            .evaluate_script("globalThis.workerResult")
            .expect("eval should succeed");
        assert!(
            result.is_string(),
            "worker should post its module URL: {result:?}"
        );
    }

    #[test]
    fn worker_url_cannot_escape_filesystem_plugin_root() {
        use super::super::host::ScriptHost;

        let directory = tempfile::tempdir().unwrap();
        let plugin = directory.path().join("plugin");
        std::fs::create_dir(&plugin).unwrap();
        std::fs::write(
            directory.path().join("outside.ts"),
            "self.onmessage = () => {};",
        )
        .unwrap();
        let config = plugin.join("config.ts");
        std::fs::write(
            &config,
            "new Worker(new URL('../outside.ts', import.meta.url), \
             { type: 'module' });",
        )
        .unwrap();

        let mut host = ScriptHost::new();
        let error = host
            .execute_module(&config)
            .expect_err("worker URL must stay inside the plugin root");
        assert!(error.to_string().contains("escapes the config directory"));
    }

    // --- Task 5: ErrorEvent dispatch ---

    /// A worker that throws in `self.onmessage` should send
    /// an `Error` message back to the main thread.
    #[test]
    fn worker_error_on_uncaught_exception() {
        let handle = spawn_worker(
            "test-worker/".to_owned(),
            "throw-worker.ts".to_owned(),
            CancellationToken::new(),
            None,
            "test".to_owned(),
            0,
        )
        .expect("spawn_worker should succeed");

        handle
            .post_message(serde_json::json!(null))
            .expect("postMessage should succeed");

        // Give the worker thread time to throw and send the
        // error message back.
        std::thread::sleep(Duration::from_millis(300));
        let messages = handle.drain();

        let found_error = messages.iter().any(|msg| {
            matches!(
                msg,
                WorkerChannelMessage::Error { message, .. }
                    if message.contains("boom")
            )
        });
        assert!(
            found_error,
            "expected an Error message containing 'boom', \
             got {messages:?}"
        );
        assert!(
            handle.is_finished(),
            "uncaught worker error must terminate the worker"
        );
    }

    /// A worker timeout should classify as `TimeoutError`.
    /// We verify the `error_name_for_worker` classifier directly
    /// rather than waiting 30s for a real timeout.
    #[test]
    fn worker_error_name_classifies_timeout() {
        assert_eq!(
            error_name_for_worker("worker request timed out"),
            "TimeoutError"
        );
        assert_eq!(
            error_name_for_worker("worker heap limit exceeded"),
            "ResourceExhausted"
        );
        assert_eq!(error_name_for_worker("some other error"), "Error");
    }

    /// JS-level test: spawn a throw-worker via `ScriptHost`,
    /// pump messages, and verify the `error` event listener
    /// was called with the right `message`.
    #[test]
    fn worker_error_event_dispatched_to_js_listener() {
        use super::super::host::ScriptHost;

        let mut host = ScriptHost::new();
        host.execute_embedded_plugin(
            "test-worker/throw-listener.ts",
            "globalThis.errMsg = null;\n\
globalThis.errName = null;\n\
const w = new Worker(\"throw-worker.ts\", { type: \"module\" });\n\
w.onerror = (e) => {\n\
  globalThis.errMsg = e.message;\n\
  globalThis.errName = e.name;\n\
};\n\
w.postMessage({});",
        )
        .expect("plugin evaluation should succeed");

        // Give the worker thread time to throw and send the
        // error message back.
        std::thread::sleep(Duration::from_millis(300));
        host.pump_worker_messages();

        // After pump, the error event should have fired.
        let msg = host
            .evaluate_script("globalThis.errMsg")
            .expect("eval should succeed");
        assert!(
            msg.is_string(),
            "error listener should have set errMsg \
             to a string, got {msg:?}"
        );
        let context = host.context.clone();
        let live = host
            .invoke(ScriptInvocationKind::Action, |isolate| {
                v8::scope_with_context!(scope, isolate, context);
                let registry = scope
                    .get_slot::<WorkerRegistrySlot>()
                    .expect("worker registry");
                Ok(registry.borrow().iter().flatten().count())
            })
            .unwrap();
        assert_eq!(live, 0, "failed worker handle must be reaped");
    }

    #[test]
    fn worker_event_listeners_support_multiple_callbacks_and_removal() {
        use super::super::host::ScriptHost;

        let mut host = ScriptHost::new();
        host.execute_embedded_plugin(
            "test-worker/listeners.ts",
            "globalThis.calls = [];\n\
const w = new Worker(\"echo-worker.ts\", { type: \"module\" });\n\
const first = () => globalThis.calls.push(\"first\");\n\
const second = () => globalThis.calls.push(\"second\");\n\
w.addEventListener(\"message\", first);\n\
w.addEventListener(\"message\", second);\n\
w.removeEventListener(\"message\", first);\n\
w.postMessage({});",
        )
        .expect("plugin evaluation should succeed");

        std::thread::sleep(Duration::from_millis(100));
        host.pump_worker_messages();
        let calls = host.evaluate_script("globalThis.calls").unwrap();
        assert_eq!(calls, serde_json::json!(["second"]));
    }

    #[test]
    fn worker_rejects_unsupported_classic_option_synchronously() {
        use super::super::host::ScriptHost;

        let mut host = ScriptHost::new();
        host.execute_embedded_plugin(
            "test-worker/classic-option.ts",
            "globalThis.classicTypeError = false;\n\
try {\n\
  new Worker(\"echo-worker.ts\", { type: \"classic\" });\n\
} catch (error) { globalThis.classicTypeError = error instanceof TypeError; }",
        )
        .unwrap();
        assert_eq!(
            host.evaluate_script("globalThis.classicTypeError").unwrap(),
            serde_json::json!(true)
        );
    }

    #[test]
    fn worker_startup_syntax_error_is_reported_asynchronously() {
        use super::super::host::ScriptHost;

        let mut host = ScriptHost::new();
        host.execute_embedded_plugin(
            "test-worker/startup-error-listener.ts",
            "globalThis.constructorThrew = false;\n\
globalThis.startupError = null;\n\
try {\n\
  const worker = new Worker(\"invalid-worker.ts\", { type: \"module\" });\n\
  worker.onerror = (event) => { globalThis.startupError = event.message; };\n\
} catch (_) { globalThis.constructorThrew = true; }",
        )
        .expect("worker construction should not throw for module load errors");

        std::thread::sleep(Duration::from_millis(100));
        host.pump_worker_messages();
        assert_eq!(
            host.evaluate_script("globalThis.constructorThrew").unwrap(),
            serde_json::json!(false)
        );
        assert!(
            host.evaluate_script("globalThis.startupError")
                .unwrap()
                .is_string(),
            "module parse failure must dispatch an asynchronous error event"
        );
    }

    #[test]
    fn worker_error_event_has_standard_fields() {
        use super::super::host::ScriptHost;

        let mut host = ScriptHost::new();
        host.execute_embedded_plugin(
            "test-worker/throw-listener.ts",
            "globalThis.fields = null;\n\
const w = new Worker(\"throw-worker.ts\", { type: \"module\" });\n\
w.addEventListener(\"error\", (e) => {\n\
  globalThis.fields = {\n\
    message: typeof e.message,\n\
    name: typeof e.name,\n\
    filename: typeof e.filename,\n\
    lineno: typeof e.lineno,\n\
    colno: typeof e.colno,\n\
  };\n\
});\n\
w.postMessage({});",
        )
        .expect("plugin evaluation should succeed");

        std::thread::sleep(Duration::from_millis(300));
        host.pump_worker_messages();

        let fields = host
            .evaluate_script("globalThis.fields")
            .expect("eval should succeed");
        let obj = fields.as_object().expect("fields should be an object");
        assert_eq!(
            obj.get("message").and_then(|v| v.as_str()),
            Some("string"),
            "message should be a string"
        );
        assert_eq!(
            obj.get("name").and_then(|v| v.as_str()),
            Some("string"),
            "name should be a string"
        );
        assert_eq!(
            obj.get("filename").and_then(|v| v.as_str()),
            Some("string"),
            "filename should be a string"
        );
        assert_eq!(
            obj.get("lineno").and_then(|v| v.as_str()),
            Some("number"),
            "lineno should be a number"
        );
        assert_eq!(
            obj.get("colno").and_then(|v| v.as_str()),
            Some("number"),
            "colno should be a number"
        );
    }
}
