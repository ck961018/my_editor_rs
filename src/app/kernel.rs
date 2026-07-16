use std::collections::HashMap;

use tokio::sync::mpsc;

use crate::app::message::AppMessage;
use crate::app::tasks::AppTasks;
use crate::core::content::SaveSnapshot;
use crate::core::content_store::ContentStore;
use crate::core::mode::ModeRegistry;
use crate::core::transaction::TextStateId;
use crate::protocol::ids::ContentId;

pub(super) struct Kernel {
    pub(super) contents: ContentStore,
    pub(super) modes: ModeRegistry,
    pub(super) message_tx: mpsc::UnboundedSender<AppMessage>,
    pub(super) message_rx: mpsc::UnboundedReceiver<AppMessage>,
    pub(super) tasks: AppTasks,
    pub(super) pending_saves: HashMap<ContentId, PendingSave>,
}

impl Kernel {
    pub fn new(contents: ContentStore, modes: ModeRegistry) -> Self {
        let (message_tx, message_rx) = mpsc::unbounded_channel();
        Self {
            contents,
            modes,
            message_tx,
            message_rx,
            tasks: AppTasks::new(),
            pending_saves: HashMap::new(),
        }
    }
}

pub(super) struct PendingSave {
    pub(super) revision: u64,
    pub(super) state: TextStateId,
    pub(super) queued: Option<SaveSnapshot>,
}
