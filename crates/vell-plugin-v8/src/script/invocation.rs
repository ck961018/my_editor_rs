use std::ffi::c_void;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use super::{
    SCRIPT_CALLBACK_TIMEOUT, SCRIPT_HEAP_RECOVERY_BYTES, SCRIPT_STARTUP_TIMEOUT, ScriptError,
};

pub(super) fn call_script_callback<'scope>(
    scope: &mut v8::PinScope<'scope, '_>,
    callback: v8::Local<'scope, v8::Function>,
    receiver: v8::Local<'scope, v8::Value>,
    arguments: &[v8::Local<'scope, v8::Value>],
) -> Option<v8::Local<'scope, v8::Value>> {
    callback.call(scope, receiver, arguments)
}

pub(super) fn perform_microtask_checkpoint(scope: &mut v8::PinScope<'_, '_>) {
    scope.perform_microtask_checkpoint();
}

#[derive(Clone, Copy)]
pub(super) struct ScriptExecutionBudget {
    pub(super) callback_timeout: Duration,
    pub(super) startup_timeout: Duration,
}

impl Default for ScriptExecutionBudget {
    fn default() -> Self {
        Self {
            callback_timeout: SCRIPT_CALLBACK_TIMEOUT,
            startup_timeout: SCRIPT_STARTUP_TIMEOUT,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) enum ScriptInvocationKind {
    ModuleEvaluation,
    StateFactory,
    Action,
    ContentChanged,
    ContentJob,
    AnalysisInput,
    AnalysisApply,
}

impl ScriptInvocationKind {
    fn label(self) -> &'static str {
        match self {
            Self::ModuleEvaluation => "module evaluation",
            Self::StateFactory => "state factory",
            Self::Action => "action",
            Self::ContentChanged => "content changed callback",
            Self::ContentJob => "content job callback",
            Self::AnalysisInput => "analysis input callback",
            Self::AnalysisApply => "analysis apply callback",
        }
    }

    pub(super) fn timeout(self, budget: ScriptExecutionBudget) -> Duration {
        match self {
            Self::ModuleEvaluation => budget.startup_timeout,
            _ => budget.callback_timeout,
        }
    }
}

pub(super) struct InvocationWatchdog {
    kind: ScriptInvocationKind,
    timeout: Duration,
    started: Instant,
    handle: v8::IsolateHandle,
    interrupted: Arc<AtomicBool>,
    stop: mpsc::Sender<()>,
    thread: Option<thread::JoinHandle<()>>,
}

pub(super) struct HeapLimitState {
    shared: Arc<HeapLimitShared>,
}

struct HeapLimitShared {
    exceeded: AtomicBool,
    handle: v8::IsolateHandle,
}

unsafe extern "C" fn near_heap_limit(
    data: *mut c_void,
    current_heap_limit: usize,
    _initial_heap_limit: usize,
) -> usize {
    // SAFETY: the isolate owns an Arc for this state in its slot storage, which
    // remains alive until V8 has completed isolate teardown.
    let state = unsafe { &*(data as *const HeapLimitShared) };
    state.exceeded.store(true, Ordering::Release);
    state.handle.terminate_execution();
    current_heap_limit.saturating_add(SCRIPT_HEAP_RECOVERY_BYTES)
}

pub(super) fn install_heap_limit(isolate: &mut v8::OwnedIsolate) -> Box<HeapLimitState> {
    let shared = Arc::new(HeapLimitShared {
        exceeded: AtomicBool::new(false),
        handle: isolate.thread_safe_handle(),
    });
    let data = Arc::as_ptr(&shared) as *mut c_void;
    isolate.set_slot(shared.clone());
    isolate.add_near_heap_limit_callback(near_heap_limit, data);
    Box::new(HeapLimitState { shared })
}

pub(super) fn recover_heap_limit(
    isolate: &mut v8::OwnedIsolate,
    state: &mut Box<HeapLimitState>,
    heap_limit_bytes: usize,
) -> bool {
    if !state.shared.exceeded.swap(false, Ordering::AcqRel) {
        return false;
    }
    isolate.cancel_terminate_execution();
    isolate.remove_near_heap_limit_callback(near_heap_limit, heap_limit_bytes);
    let data = Arc::as_ptr(&state.shared) as *mut c_void;
    isolate.add_near_heap_limit_callback(near_heap_limit, data);
    true
}

impl InvocationWatchdog {
    pub(super) fn start(
        handle: v8::IsolateHandle,
        kind: ScriptInvocationKind,
        timeout: Duration,
    ) -> Result<Self, ScriptError> {
        let (stop, receiver) = mpsc::channel();
        let interrupted = Arc::new(AtomicBool::new(false));
        let watcher_interrupted = interrupted.clone();
        let watcher_handle = handle.clone();
        let thread = thread::Builder::new()
            .name("script-watchdog".to_owned())
            .spawn(move || {
                if matches!(
                    receiver.recv_timeout(timeout),
                    Err(mpsc::RecvTimeoutError::Timeout)
                ) {
                    watcher_interrupted.store(true, Ordering::Release);
                    watcher_handle.terminate_execution();
                }
            })
            .map_err(|error| {
                ScriptError::new(format!("failed to start script watchdog: {error}"))
            })?;
        Ok(Self {
            kind,
            timeout,
            started: Instant::now(),
            handle,
            interrupted,
            stop,
            thread: Some(thread),
        })
    }

    pub(super) fn finish<T>(mut self, result: Result<T, ScriptError>) -> Result<T, ScriptError> {
        let _ = self.stop.send(());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        let timed_out =
            self.interrupted.load(Ordering::Acquire) || self.started.elapsed() >= self.timeout;
        if timed_out {
            self.handle.cancel_terminate_execution();
            return Err(ScriptError::new(format!(
                "script timeout during {}",
                self.kind.label()
            )));
        }
        result.map_err(|error| {
            ScriptError::new(format!(
                "script execution failed during {}: {error}",
                self.kind.label()
            ))
        })
    }
}
