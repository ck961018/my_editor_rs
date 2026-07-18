use std::collections::HashMap;
use std::io;
use std::path::Path;

use tokio::sync::mpsc;

use crate::app::command::ModeCommand;
use crate::app::message::AppMessage;
use crate::app::mode::{
    ModeContentStore, ModeError, ModeId, ModeRegistry, ModeResult, ModeStateSnapshot,
};
use crate::app::mode_name::ModeName;
use crate::app::tasks::AppTasks;
use crate::app::transaction::{
    TransactionManager, TransactionManagerError, TransactionRecord, TransactionSnapshot,
};
use crate::core::action::{ContentAction, ContentEditPlan};
use crate::core::content::{
    ContentActionResult, ContentChange, ContentEvent, ContentInput, ContentResult,
    ContentTransactionError, SaveSnapshot,
};
use crate::core::content_store::{ContentSnapshot, ContentStore};
use crate::core::transaction::{TextStateId, TransactionDirection};
use crate::protocol::ids::{ContentId, ViewId};
use crate::protocol::selection::Selections;

pub(super) struct Kernel {
    contents: ContentStore,
    modes: ModeRegistry,
    content_modes: ModeContentStore,
    transactions: TransactionManager,
    new_view_modes: HashMap<ContentId, ModeName>,
    message_tx: mpsc::UnboundedSender<AppMessage>,
    message_rx: mpsc::UnboundedReceiver<AppMessage>,
    tasks: AppTasks,
    pending_saves: HashMap<ContentId, PendingSave>,
    command_transaction: Option<CommandTransaction>,
}

impl Kernel {
    pub(super) fn new(
        contents: ContentStore,
        modes: ModeRegistry,
        new_view_modes: HashMap<ContentId, ModeName>,
    ) -> Self {
        let (message_tx, message_rx) = mpsc::unbounded_channel();
        Self {
            contents,
            modes,
            content_modes: ModeContentStore::default(),
            transactions: TransactionManager::default(),
            new_view_modes,
            message_tx,
            message_rx,
            tasks: AppTasks::new(),
            pending_saves: HashMap::new(),
            command_transaction: None,
        }
    }

    pub(super) fn contents(&self) -> &ContentStore {
        &self.contents
    }

    pub(super) fn snapshot_content(&self, content: ContentId) -> Option<ContentSnapshot> {
        self.contents.snapshot(content)
    }

    pub(super) fn restore_content(&mut self, snapshot: ContentSnapshot) {
        self.contents.restore(snapshot);
    }

    #[cfg(test)]
    pub(super) fn contents_mut(&mut self) -> &mut ContentStore {
        &mut self.contents
    }

    pub(super) fn modes(&self) -> &ModeRegistry {
        &self.modes
    }

    pub(super) fn mode_chain_for_new_view(&self, content: ContentId) -> Vec<ModeName> {
        self.new_view_modes
            .get(&content)
            .cloned()
            .into_iter()
            .collect()
    }

    pub(super) fn content_modes(&self) -> &ModeContentStore {
        &self.content_modes
    }

    pub(super) fn mode_runtime_parts(&mut self) -> (&ContentStore, &mut ModeContentStore) {
        (&self.contents, &mut self.content_modes)
    }

    pub(super) fn mode_attachment_parts(
        &mut self,
    ) -> (&ContentStore, &ModeRegistry, &mut ModeContentStore) {
        (&self.contents, &self.modes, &mut self.content_modes)
    }

    pub(super) fn execute_mode_content_action(
        &mut self,
        content: ContentId,
        command: &ModeCommand,
    ) -> Result<ModeResult, ModeError> {
        self.content_modes
            .execute(&self.modes, &self.contents, content, command)
    }

    pub(super) fn snapshot_mode_content_for(
        &self,
        mode: ModeId,
        content: ContentId,
    ) -> Option<ModeStateSnapshot> {
        self.content_modes.snapshot_for(mode, content)
    }

    pub(super) fn restore_mode_content_for(
        &mut self,
        mode: ModeId,
        content: ContentId,
        snapshot: ModeStateSnapshot,
    ) {
        self.content_modes.restore_for(mode, content, snapshot);
    }

    #[cfg(test)]
    pub(super) fn modes_mut(&mut self) -> &mut ModeRegistry {
        &mut self.modes
    }

    pub(super) fn execute(&mut self, content: ContentId, input: ContentInput) -> ContentResult {
        self.contents.execute(content, input)
    }

    pub(super) fn plan_edit(
        &self,
        content: ContentId,
        command: crate::core::command::EditCommand,
        selections: &Selections,
    ) -> Option<ContentEditPlan> {
        self.contents.plan_edit(content, command, selections)
    }

    pub(super) fn apply_content_action(
        &mut self,
        content: ContentId,
        action: ContentAction,
    ) -> ContentActionResult {
        self.contents.apply(content, action)
    }

    pub(super) fn begin_transaction(
        &mut self,
        content: ContentId,
        owner: Option<ViewId>,
    ) -> Option<TransactionRecord> {
        self.checkpoint_transaction(content);
        self.preserve_truncated_history();
        self.transactions.begin(content, owner)
    }

    pub(super) fn record_transaction(
        &mut self,
        record: TransactionRecord,
    ) -> Result<(), TransactionManagerError> {
        self.checkpoint_transaction(record.target);
        self.transactions.record(record)
    }

    pub(super) fn commit_transaction(&mut self, content: ContentId) -> Option<TransactionRecord> {
        self.checkpoint_transaction(content);
        self.preserve_truncated_history();
        self.transactions.commit(content)
    }

    pub(super) fn rollback_transaction(&mut self, content: ContentId) -> Option<TransactionRecord> {
        self.checkpoint_transaction(content);
        self.transactions.rollback(content)
    }

    pub(super) fn undo_transaction(&mut self, content: ContentId) -> Option<TransactionRecord> {
        self.checkpoint_transaction(content);
        self.transactions.undo(content)
    }

    pub(super) fn redo_transaction(&mut self, content: ContentId) -> Option<TransactionRecord> {
        self.checkpoint_transaction(content);
        self.transactions.redo(content)
    }

    pub(super) fn active_transaction_owner(&self, content: ContentId) -> Option<Option<ViewId>> {
        self.transactions.active_owner(content)
    }

    pub(super) fn start_command_transaction(&mut self, target: Option<ContentId>) {
        assert!(self.command_transaction.is_none());
        self.command_transaction = target.map(|target| CommandTransaction {
            target,
            snapshot: None,
        });
    }

    pub(super) fn finish_command_transaction(&mut self, success: bool) {
        let Some(command) = self.command_transaction.take() else {
            return;
        };
        if !success && let Some(snapshot) = command.snapshot {
            self.transactions.restore(snapshot);
        }
    }

    fn checkpoint_transaction(&mut self, content: ContentId) {
        let Some(command) = self.command_transaction.as_mut() else {
            return;
        };
        assert_eq!(
            command.target, content,
            "command changed transaction target"
        );
        if command.snapshot.is_none() {
            command.snapshot = Some(self.transactions.snapshot(content));
        }
    }

    fn preserve_truncated_history(&mut self) {
        let Some(snapshot) = self
            .command_transaction
            .as_mut()
            .and_then(|command| command.snapshot.as_mut())
        else {
            return;
        };
        self.transactions.preserve_truncated_history(snapshot);
    }

    pub(super) fn apply_transaction_record(
        &mut self,
        record: &TransactionRecord,
        direction: TransactionDirection,
    ) -> Result<Option<ContentChange>, ContentTransactionError> {
        self.contents
            .apply_transaction(record.target, &record.data.content, direction)
    }

    pub(super) fn cancel(&self) {
        self.tasks.cancel();
    }

    pub(super) fn is_cancelled(&self) -> bool {
        self.tasks.is_cancelled()
    }

    pub(super) fn cancellation_token(&self) -> tokio_util::sync::CancellationToken {
        self.tasks.cancellation_token()
    }

    pub(super) async fn receive_message(&mut self) -> Option<AppMessage> {
        self.message_rx.recv().await
    }

    pub(super) fn try_receive_message(&mut self) -> Option<AppMessage> {
        self.message_rx.try_recv().ok()
    }

    pub(super) fn begin_shutdown(&self) {
        self.tasks.cancel();
        self.tasks.close_detached();
    }

    pub(super) fn close_critical_tasks(&self) {
        self.tasks.close_critical();
    }

    pub(super) async fn wait_for_critical_tasks(&self) {
        self.tasks.wait_critical().await;
    }

    pub(super) fn has_pending_saves(&self) -> bool {
        !self.pending_saves.is_empty()
    }

    #[cfg(test)]
    pub(super) fn has_pending_save(&self, content: ContentId) -> bool {
        self.pending_saves.contains_key(&content)
    }

    /// 发起异步保存；同一 content 已在保存时，仅保留最新的后续快照。
    pub(super) fn queue_save(&mut self, content: ContentId, snapshot: SaveSnapshot) -> bool {
        if let Some(pending) = self.pending_saves.get_mut(&content) {
            let queued_revision = pending
                .queued
                .as_ref()
                .map_or(pending.revision, |queued| queued.revision);
            if snapshot.revision > queued_revision {
                pending.queued = Some(snapshot);
            }
            return false;
        }

        let tx = self.message_tx.clone();
        let revision = snapshot.revision;
        let state = snapshot.state;
        self.pending_saves.insert(
            content,
            PendingSave {
                revision,
                state,
                queued: None,
            },
        );
        self.tasks.spawn_critical(async move {
            let result = atomic_write(snapshot).await;
            let _ = tx.send(AppMessage::SaveCompleted {
                content,
                revision,
                state,
                result,
            });
        });
        true
    }

    pub(super) fn complete_save(
        &mut self,
        content: ContentId,
        revision: u64,
        state: TextStateId,
        result: io::Result<()>,
    ) -> SaveCompletion {
        let pending = self
            .pending_saves
            .remove(&content)
            .expect("save completion must match a pending save");
        assert_eq!(pending.revision, revision, "save revision mismatch");
        assert_eq!(pending.state, state, "save state mismatch");
        let result = self.contents.execute(
            content,
            ContentInput::Event(ContentEvent::SaveFinished { state, result }),
        );
        SaveCompletion {
            result,
            queued: pending.queued,
        }
    }

    #[cfg(test)]
    pub(super) fn track_pending_save_for_test(
        &mut self,
        content: ContentId,
        revision: u64,
        state: TextStateId,
        queued: Option<SaveSnapshot>,
    ) {
        self.pending_saves.insert(
            content,
            PendingSave {
                revision,
                state,
                queued,
            },
        );
    }
}

pub(super) struct SaveCompletion {
    result: ContentResult,
    queued: Option<SaveSnapshot>,
}

impl SaveCompletion {
    pub(super) fn into_parts(self) -> (ContentResult, Option<SaveSnapshot>) {
        (self.result, self.queued)
    }
}

struct PendingSave {
    revision: u64,
    state: TextStateId,
    queued: Option<SaveSnapshot>,
}

struct CommandTransaction {
    target: ContentId,
    snapshot: Option<TransactionSnapshot>,
}

async fn atomic_write(snapshot: SaveSnapshot) -> io::Result<()> {
    tokio::task::spawn_blocking(move || {
        use std::io::Write;

        let parent = snapshot
            .path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
        temporary.write_all(snapshot.bytes.as_bytes())?;
        if let Ok(metadata) = std::fs::metadata(&snapshot.path) {
            temporary
                .as_file()
                .set_permissions(metadata.permissions())?;
        }
        temporary.as_file().sync_all()?;
        temporary
            .persist(&snapshot.path)
            .map_err(|error| error.error)?;
        Ok(())
    })
    .await
    .map_err(io::Error::other)?
}
