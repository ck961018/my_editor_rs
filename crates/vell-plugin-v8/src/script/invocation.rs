use std::ffi::c_void;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use tokio_util::sync::CancellationToken;

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
    cancellation: Option<CancellationToken>,
    handle: v8::IsolateHandle,
    outcome: Arc<AtomicU8>,
    completed: Option<WatchdogOutcome>,
    stop: Option<mpsc::Sender<()>>,
    thread: Option<thread::JoinHandle<()>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub(super) enum WatchdogOutcome {
    Completed,
    TimedOut,
    Cancelled,
}

impl WatchdogOutcome {
    fn load(outcome: &AtomicU8) -> Self {
        match outcome.load(Ordering::Acquire) {
            value if value == Self::TimedOut as u8 => Self::TimedOut,
            value if value == Self::Cancelled as u8 => Self::Cancelled,
            _ => Self::Completed,
        }
    }
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
        Self::start_inner(
            handle,
            kind,
            timeout,
            None,
            "script-watchdog",
            "script watchdog",
        )
    }

    pub(super) fn start_cancellable(
        handle: v8::IsolateHandle,
        kind: ScriptInvocationKind,
        timeout: Duration,
        cancellation: CancellationToken,
    ) -> Result<Self, ScriptError> {
        Self::start_inner(
            handle,
            kind,
            timeout,
            Some(cancellation),
            "script-worker-watchdog",
            "script worker watchdog",
        )
    }

    fn start_inner(
        handle: v8::IsolateHandle,
        kind: ScriptInvocationKind,
        timeout: Duration,
        cancellation: Option<CancellationToken>,
        thread_name: &str,
        watchdog_name: &str,
    ) -> Result<Self, ScriptError> {
        let (stop, receiver) = mpsc::channel();
        let (arm, armed) = mpsc::sync_channel(1);
        let outcome = Arc::new(AtomicU8::new(WatchdogOutcome::Completed as u8));
        let watcher_outcome = outcome.clone();
        let watcher_handle = handle.clone();
        let watcher_cancellation = cancellation.clone();
        let thread = thread::Builder::new()
            .name(thread_name.to_owned())
            .spawn(move || {
                let Ok(started) = armed.recv() else {
                    return;
                };
                let outcome =
                    wait_for_watchdog(&receiver, started, timeout, watcher_cancellation.as_ref());
                if outcome != WatchdogOutcome::Completed {
                    watcher_outcome.store(outcome as u8, Ordering::Release);
                    watcher_handle.terminate_execution();
                }
            })
            .map_err(|error| {
                ScriptError::new(format!("failed to start {watchdog_name}: {error}"))
            })?;
        let started = Instant::now();
        if arm.send(started).is_err() {
            let _ = thread.join();
            return Err(ScriptError::new(format!("failed to arm {watchdog_name}")));
        }
        Ok(Self {
            kind,
            timeout,
            started,
            cancellation,
            handle,
            outcome,
            completed: None,
            stop: Some(stop),
            thread: Some(thread),
        })
    }

    pub(super) fn finish<T>(mut self, result: Result<T, ScriptError>) -> Result<T, ScriptError> {
        match self.stop() {
            WatchdogOutcome::Completed => {}
            WatchdogOutcome::TimedOut => {
                return Err(ScriptError::new(format!(
                    "script timeout during {}",
                    self.kind.label()
                )));
            }
            WatchdogOutcome::Cancelled => {
                return Err(ScriptError::new(format!(
                    "script cancelled during {}",
                    self.kind.label()
                )));
            }
        }
        result.map_err(|error| {
            ScriptError::new(format!(
                "script execution failed during {}: {error}",
                self.kind.label()
            ))
        })
    }

    pub(super) fn stop(&mut self) -> WatchdogOutcome {
        if let Some(outcome) = self.completed {
            return outcome;
        }
        let cancelled = self
            .cancellation
            .as_ref()
            .is_some_and(CancellationToken::is_cancelled);
        let timed_out = self.started.elapsed() >= self.timeout;
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        let watcher_outcome = WatchdogOutcome::load(&self.outcome);
        if watcher_outcome != WatchdogOutcome::Completed {
            self.handle.cancel_terminate_execution();
        }
        let outcome = resolve_watchdog_outcome(watcher_outcome, cancelled, timed_out);
        self.completed = Some(outcome);
        outcome
    }
}

fn resolve_watchdog_outcome(
    watcher_outcome: WatchdogOutcome,
    cancelled: bool,
    timed_out: bool,
) -> WatchdogOutcome {
    if cancelled {
        WatchdogOutcome::Cancelled
    } else if watcher_outcome != WatchdogOutcome::Completed {
        watcher_outcome
    } else if timed_out {
        WatchdogOutcome::TimedOut
    } else {
        WatchdogOutcome::Completed
    }
}

impl Drop for InvocationWatchdog {
    fn drop(&mut self) {
        self.stop();
    }
}

fn wait_for_watchdog(
    stop: &mpsc::Receiver<()>,
    started: Instant,
    timeout: Duration,
    cancellation: Option<&CancellationToken>,
) -> WatchdogOutcome {
    loop {
        if cancellation.is_some_and(CancellationToken::is_cancelled) {
            return WatchdogOutcome::Cancelled;
        }
        let remaining = timeout.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return WatchdogOutcome::TimedOut;
        }
        let wait = if cancellation.is_some() {
            remaining.min(Duration::from_millis(1))
        } else {
            remaining
        };
        match stop.recv_timeout(wait) {
            Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                return WatchdogOutcome::Completed;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn watchdog_wait_distinguishes_stop_timeout_and_cancellation() {
        let (stop, receiver) = mpsc::channel();
        stop.send(()).unwrap();
        assert_eq!(
            wait_for_watchdog(&receiver, Instant::now(), Duration::from_secs(1), None,),
            WatchdogOutcome::Completed
        );

        let (_stop, receiver) = mpsc::channel();
        assert_eq!(
            wait_for_watchdog(&receiver, Instant::now(), Duration::ZERO, None),
            WatchdogOutcome::TimedOut
        );

        let (_stop, receiver) = mpsc::channel();
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        assert_eq!(
            wait_for_watchdog(
                &receiver,
                Instant::now(),
                Duration::from_secs(1),
                Some(&cancellation),
            ),
            WatchdogOutcome::Cancelled
        );
    }

    #[test]
    fn completion_snapshot_classifies_deadline_and_prioritizes_cancellation() {
        assert_eq!(
            resolve_watchdog_outcome(WatchdogOutcome::Completed, false, false),
            WatchdogOutcome::Completed
        );
        assert_eq!(
            resolve_watchdog_outcome(WatchdogOutcome::Completed, false, true),
            WatchdogOutcome::TimedOut
        );
        assert_eq!(
            resolve_watchdog_outcome(WatchdogOutcome::TimedOut, true, true),
            WatchdogOutcome::Cancelled
        );
    }
}
