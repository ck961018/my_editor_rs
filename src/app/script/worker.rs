use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

use tokio_util::sync::CancellationToken;

use super::{
    DEFAULT_PLUGIN_ASSETS, ScriptError, current_exception, initialize_v8, json_to_v8, set_object,
    throw_script_error, transpile_typescript, v8_to_json,
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
        let javascript = transpile_typescript(&format!("file:///runtime/plugins/{path}"), &source)?;
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
    let mut isolate = v8::Isolate::new(v8::CreateParams::default());
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
    let startup = {
        v8::scope_with_context!(scope, &mut isolate, context.clone());
        v8::tc_scope!(let scope, scope);
        install_worker_api(scope);
        evaluate_worker(scope, &path, &javascript)
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
        let result = match startup_error.as_ref() {
            Some(error) => Err(error.clone()),
            None => execute_request_with_watchdog(
                &mut isolate,
                context.clone(),
                handler.borrow().as_ref().expect("checked handler"),
                request.message,
                &request.cancellation,
            ),
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
) -> Result<serde_json::Value, String> {
    let handle = isolate.thread_safe_handle();
    let finished = Arc::new(AtomicBool::new(false));
    let interrupted = Arc::new(AtomicBool::new(false));
    let watcher_finished = finished.clone();
    let watcher_interrupted = interrupted.clone();
    let watcher_cancellation = cancellation.clone();
    let watcher_handle = handle.clone();
    let watchdog = std::thread::spawn(move || {
        let deadline = Instant::now() + WORKER_TIMEOUT;
        while !watcher_finished.load(Ordering::Acquire) {
            if watcher_cancellation.is_cancelled() || Instant::now() >= deadline {
                watcher_interrupted.store(true, Ordering::Release);
                watcher_handle.terminate_execution();
                return;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    });
    let result = execute_request(isolate, context, handler, message, cancellation)
        .map_err(|error| error.to_string());
    finished.store(true, Ordering::Release);
    let _ = watchdog.join();
    if interrupted.load(Ordering::Acquire) {
        handle.cancel_terminate_execution();
        return Err(if cancellation.is_cancelled() {
            "script worker request cancelled".to_owned()
        } else {
            "script worker request timed out".to_owned()
        });
    }
    result
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
    let value = handler
        .call(scope, receiver, &[message])
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
            let snapshot =
                crate::core::text_snapshot::TextSnapshot::new(&ropey::Rope::from_str(text));
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
}
