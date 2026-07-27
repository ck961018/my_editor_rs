use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use tokio_util::sync::CancellationToken;

use super::ScriptError;
use super::worker_quota::QuotaHandle;

/// Bidirectional messages flowing between main thread and worker.
#[derive(Debug)]
pub(super) enum WorkerChannelMessage {
    /// Main to worker: a `postMessage(data)` call.
    ToWorker(serde_json::Value),
    /// Worker to main: a `self.postMessage(data)` call.
    FromWorker(serde_json::Value),
    /// Worker to main: uncaught error.
    Error(String),
    /// Worker to main: `self.close()` or isolate terminated.
    Terminated,
}

/// Rust-side handle to a live worker isolate.
///
/// Owns the main-to-worker channel and a shared receiver for
/// worker-to-main messages. The JS `Worker` object wraps this via
/// an internal slot.
#[derive(Debug)]
pub(super) struct WorkerHandle {
    sender: mpsc::Sender<WorkerChannelMessage>,
    receiver: Arc<Mutex<mpsc::Receiver<WorkerChannelMessage>>>,
    cancellation: CancellationToken,
    thread: Option<JoinHandle<()>>,
    #[allow(dead_code)]
    quota: Option<QuotaHandle>,
}

impl WorkerHandle {
    pub(super) fn new(
        sender: mpsc::Sender<WorkerChannelMessage>,
        receiver: mpsc::Receiver<WorkerChannelMessage>,
        cancellation: CancellationToken,
        thread: JoinHandle<()>,
        quota: Option<QuotaHandle>,
    ) -> Self {
        Self {
            sender,
            receiver: Arc::new(Mutex::new(receiver)),
            cancellation,
            thread: Some(thread),
            quota,
        }
    }

    /// Send a message to the worker (main to worker).
    pub(super) fn post_message(&self, data: serde_json::Value) -> Result<(), ScriptError> {
        self.sender
            .send(WorkerChannelMessage::ToWorker(data))
            .map_err(|_| ScriptError::new("worker terminated before postMessage"))
    }

    /// Non-blocking drain of pending worker-to-main messages.
    pub(super) fn drain(&self) -> Vec<WorkerChannelMessage> {
        let Ok(receiver) = self.receiver.lock() else {
            return Vec::new();
        };
        let mut messages = Vec::new();
        while let Ok(message) = receiver.try_recv() {
            messages.push(message);
        }
        messages
    }

    /// Terminate the worker: cancel + join thread.
    pub(super) fn terminate(&mut self) {
        self.cancellation.cancel();
        let _ = self.sender.send(WorkerChannelMessage::Terminated);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        // Release the quota slot.
        self.quota.take();
    }
}

impl Drop for WorkerHandle {
    fn drop(&mut self) {
        self.terminate();
    }
}
