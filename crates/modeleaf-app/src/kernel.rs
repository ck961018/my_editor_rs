use std::collections::HashMap;
use std::io;
use std::path::Path;

use tokio::sync::mpsc;

use crate::command::ModeCommand;
use crate::message::AppMessage;
use crate::mode::{
    ModeContentStore, ModeDraftJournal, ModeError, ModeId, ModeJobKey, ModeJobRequest,
    ModeJobResult, ModeJobRunner, ModeRegistry, ModeResult,
};
use crate::tasks::AppTasks;
use crate::transaction::{
    TransactionManager, TransactionManagerError, TransactionRecord, TransactionSnapshot,
};
use modeleaf_core::action::{ContentAction, ContentEditPlan};
use modeleaf_core::content::{
    ContentActionResult, ContentChange, ContentEvent, ContentInput, ContentResult,
    ContentTransactionError, SaveSnapshot,
};
use modeleaf_core::content_store::{ContentSnapshot, ContentStore};
use modeleaf_core::transaction::{TextStateId, TransactionDirection};
use modeleaf_protocol::ids::{ContentId, ViewId};
use modeleaf_protocol::selection::Selections;

pub(super) struct Kernel {
    contents: ContentStore,
    modes: ModeRegistry,
    content_modes: ModeContentStore,
    transactions: TransactionManager,
    message_tx: mpsc::UnboundedSender<AppMessage>,
    message_rx: mpsc::UnboundedReceiver<AppMessage>,
    tasks: AppTasks,
    mode_jobs: HashMap<ModeJobKey, ModeJobSlot>,
    pending_saves: HashMap<ContentId, PendingSave>,
    command_transaction: Option<CommandTransaction>,
}

impl Kernel {
    pub(super) fn new(contents: ContentStore, modes: ModeRegistry) -> Self {
        let (message_tx, message_rx) = mpsc::unbounded_channel();
        Self {
            contents,
            modes,
            content_modes: ModeContentStore::default(),
            transactions: TransactionManager::default(),
            message_tx,
            message_rx,
            tasks: AppTasks::new(),
            mode_jobs: HashMap::new(),
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

    pub(super) fn content_modes(&self) -> &ModeContentStore {
        &self.content_modes
    }

    pub(super) fn commit_mode_drafts(&mut self, drafts: &mut ModeDraftJournal) {
        drafts.commit_content(&mut self.content_modes);
    }

    pub(super) fn mode_runtime_parts(&mut self) -> (&ContentStore, &mut ModeContentStore) {
        (&self.contents, &mut self.content_modes)
    }

    pub(super) fn mode_attachment_parts(
        &mut self,
    ) -> (&ContentStore, &ModeRegistry, &mut ModeContentStore) {
        (&self.contents, &self.modes, &mut self.content_modes)
    }

    pub(super) fn execute_mode_content_action_in_draft(
        &mut self,
        content: ContentId,
        command: &ModeCommand,
        drafts: &mut ModeDraftJournal,
    ) -> Result<ModeResult, ModeError> {
        self.content_modes
            .execute(&self.modes, &self.contents, content, command, drafts)
    }

    #[cfg(test)]
    pub(super) fn execute_mode_content_action(
        &mut self,
        content: ContentId,
        command: &ModeCommand,
    ) -> Result<ModeResult, ModeError> {
        let mut drafts = ModeDraftJournal::default();
        let result = self.execute_mode_content_action_in_draft(content, command, &mut drafts);
        if result.is_ok() {
            self.commit_mode_drafts(&mut drafts);
        }
        result
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
        command: modeleaf_core::command::EditCommand,
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

    #[cfg(test)]
    pub(super) fn history_behavior_for_test(
        &self,
        content: ContentId,
    ) -> (bool, Option<ViewId>, usize, usize) {
        self.transactions.behavior_for_test(content)
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

    pub(super) fn schedule_mode_jobs(&mut self) {
        if tokio::runtime::Handle::try_current().is_err() {
            return;
        }
        let jobs = self.content_modes.take_background_jobs(&self.contents);
        for (mode, content, request) in jobs {
            self.queue_mode_job(mode, content, request);
        }
    }

    fn queue_mode_job(&mut self, mode: ModeId, content: ContentId, request: ModeJobRequest) {
        let (slot, version, run) = request.into_parts();
        let key = ModeJobKey {
            mode,
            content,
            slot,
        };
        let pending = PendingModeJob { version, run };
        let entry = self.mode_jobs.entry(key.clone()).or_default();
        if let Some(running) = entry.running.as_ref() {
            if running.version == version {
                return;
            }
            running.cancellation.cancel();
            entry.queued = Some(pending);
            return;
        }
        let cancellation = self.tasks.cancellation_token().child_token();
        entry.running = Some(RunningModeJob {
            version,
            cancellation: cancellation.clone(),
        });
        self.spawn_mode_job(key, pending, cancellation);
    }

    fn spawn_mode_job(
        &self,
        key: ModeJobKey,
        pending: PendingModeJob,
        cancellation: tokio_util::sync::CancellationToken,
    ) {
        let tx = self.message_tx.clone();
        self.tasks.spawn_detached(async move {
            let version = pending.version;
            let result = tokio::task::spawn_blocking(move || (pending.run)(cancellation))
                .await
                .unwrap_or_else(|error| Err(format!("mode job panicked: {error}")));
            let _ = tx.send(AppMessage::ModeJobCompleted {
                key,
                version,
                result,
            });
        });
    }

    pub(super) fn complete_mode_job(
        &mut self,
        key: ModeJobKey,
        version: u64,
        result: ModeJobResult,
    ) -> bool {
        let Some(slot) = self.mode_jobs.get_mut(&key) else {
            return false;
        };
        if slot.running.as_ref().map(|running| running.version) != Some(version) {
            return false;
        }
        slot.running = None;
        let changed = self.content_modes.apply_background_job(
            key.mode,
            key.content,
            &self.contents,
            &key.slot,
            version,
            result,
        );
        let queued = slot.queued.take();
        if let Some(pending) = queued {
            let cancellation = self.tasks.cancellation_token().child_token();
            slot.running = Some(RunningModeJob {
                version: pending.version,
                cancellation: cancellation.clone(),
            });
            self.spawn_mode_job(key, pending, cancellation);
        }
        changed
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

#[derive(Default)]
struct ModeJobSlot {
    running: Option<RunningModeJob>,
    queued: Option<PendingModeJob>,
}

struct RunningModeJob {
    version: u64,
    cancellation: tokio_util::sync::CancellationToken,
}

struct PendingModeJob {
    version: u64,
    run: ModeJobRunner,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mode_name::ModeName;

    struct TestMode(ModeName);

    impl crate::mode::Mode for TestMode {
        fn name(&self) -> &ModeName {
            &self.0
        }

        fn actions(&self) -> &[crate::mode_name::ModeActionName] {
            &[]
        }

        fn adapters(&self) -> crate::mode::ModeAdapters {
            crate::mode::ModeAdapters::buffer()
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn newer_mode_job_cancels_the_running_version() {
        let mut modes = ModeRegistry::new();
        let mode = modes.register(TestMode(ModeName::new("test"))).unwrap();
        let mut kernel = Kernel::new(ContentStore::default(), modes);
        let key = ModeJobKey {
            mode,
            content: ContentId(0),
            slot: "parse".to_owned(),
        };
        let request = |version| {
            ModeJobRequest::new("parse", version, move |cancellation| {
                while !cancellation.is_cancelled() {
                    std::thread::yield_now();
                }
                Err("cancelled".to_owned())
            })
        };

        kernel.queue_mode_job(mode, ContentId(0), request(1));
        kernel.queue_mode_job(mode, ContentId(0), request(2));

        let slot = &kernel.mode_jobs[&key];
        assert!(slot.running.as_ref().unwrap().cancellation.is_cancelled());
        assert_eq!(slot.queued.as_ref().unwrap().version, 2);
        kernel.cancel();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn stale_mode_job_completion_is_ignored() {
        let mut modes = ModeRegistry::new();
        let mode = modes.register(TestMode(ModeName::new("test"))).unwrap();
        let mut kernel = Kernel::new(ContentStore::default(), modes);
        let key = ModeJobKey {
            mode,
            content: ContentId(0),
            slot: "parse".to_owned(),
        };
        kernel.queue_mode_job(
            mode,
            ContentId(0),
            ModeJobRequest::new("parse", 2, |_| Ok(Box::new(()))),
        );

        assert!(!kernel.complete_mode_job(key.clone(), 1, Ok(Box::new(()))));
        assert_eq!(
            kernel.mode_jobs[&key]
                .running
                .as_ref()
                .map(|running| running.version),
            Some(2)
        );
        kernel.cancel();
    }
}
