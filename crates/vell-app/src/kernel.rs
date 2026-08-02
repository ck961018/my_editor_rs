use std::collections::HashMap;
use std::io;
use std::path::Path;
use std::path::PathBuf;

use tokio::sync::mpsc;

use crate::buffer_lifecycle::normalize_path;
use crate::command::ModeCommand;
use crate::message::{AppMessage, OpenedBuffer, OpenedPath};
use crate::mode::{
    ModeBackground, ModeContentStore, ModeDraftJournal, ModeError, ModeId, ModeJobKey,
    ModeJobRequest, ModeJobResult, ModeJobRunner, ModeRegistry, ModeResult,
};
use crate::native_commands::native_command_registry;
use crate::tasks::AppTasks;
use crate::transaction::{
    TransactionManager, TransactionManagerError, TransactionRecord, TransactionSnapshot,
};
use vell_core::action::{ContentAction, ContentEditPlan};
use vell_core::clipboard::{ClipboardKind, ClipboardPayload, PastePlacement};
use vell_core::content::{
    Content, ContentActionResult, ContentChange, ContentEvent, ContentInput, ContentKind,
    ContentResult, ContentTransactionError, SaveSnapshot,
};
use vell_core::content_store::{ContentSnapshot, ContentStore};
use vell_core::search::{SearchError, SearchSnapshot};
use vell_core::transaction::{TextStateId, TransactionDirection};
use vell_mode::command_registry::CommandRegistry;
use vell_protocol::ids::{ContentId, SpaceId, ViewId};
use vell_protocol::selection::Selections;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum FileBaseline {
    Missing,
    Materialized(String),
}

#[derive(Clone, Debug)]
enum SaveGuard {
    Force,
    Matches(String),
    CreateNew,
}

#[derive(Clone, Debug)]
struct BufferPathRecord {
    identity: PathBuf,
    path: PathBuf,
    baseline: FileBaseline,
}

pub(super) struct Kernel {
    contents: ContentStore,
    buffer_paths: HashMap<ContentId, BufferPathRecord>,
    path_contents: HashMap<PathBuf, ContentId>,
    reserved_paths: HashMap<PathBuf, ContentId>,
    pending_opens: HashMap<ContentId, PendingOpen>,
    latest_opens: HashMap<SpaceId, ContentId>,
    modes: ModeRegistry,
    commands: CommandRegistry,
    content_modes: ModeContentStore,
    transactions: TransactionManager,
    message_tx: mpsc::UnboundedSender<AppMessage>,
    message_rx: mpsc::UnboundedReceiver<AppMessage>,
    tasks: AppTasks,
    mode_jobs: HashMap<ModeJobKey, ModeJobSlot>,
    pending_saves: HashMap<ContentId, PendingSave>,
    command_transaction: Option<CommandTransaction>,
    clipboard: ClipboardPayload,
    next_content_id: u64,
}

impl Kernel {
    pub(super) fn new(contents: ContentStore, modes: ModeRegistry) -> Self {
        let next_content_id = contents
            .ids()
            .map(|id| id.0)
            .max()
            .map_or(0, |id| id.checked_add(1).expect("content id overflow"));
        let (message_tx, message_rx) = mpsc::unbounded_channel();
        Self {
            contents,
            buffer_paths: HashMap::new(),
            path_contents: HashMap::new(),
            reserved_paths: HashMap::new(),
            pending_opens: HashMap::new(),
            latest_opens: HashMap::new(),
            modes,
            commands: native_command_registry(),
            content_modes: ModeContentStore::default(),
            transactions: TransactionManager::default(),
            message_tx,
            message_rx,
            tasks: AppTasks::new(),
            mode_jobs: HashMap::new(),
            pending_saves: HashMap::new(),
            command_transaction: None,
            clipboard: ClipboardPayload::character(""),
            next_content_id,
        }
    }

    pub(super) fn create_content(&mut self, kind: ContentKind) -> ContentId {
        self.insert_content(Content::empty(kind))
    }

    pub(super) fn insert_content(&mut self, value: Content) -> ContentId {
        let id = self.reserve_content_id();
        self.contents
            .insert(id, value)
            .expect("allocated content id is unique");
        id
    }

    fn reserve_content_id(&mut self) -> ContentId {
        loop {
            let id = ContentId(self.next_content_id);
            self.next_content_id = self
                .next_content_id
                .checked_add(1)
                .expect("content id overflow");
            if !self.contents.contains(id) {
                return id;
            }
        }
    }

    pub(super) fn content_for_path(&self, identity: &Path) -> Option<ContentId> {
        self.path_contents.get(identity).copied()
    }

    pub(super) fn path_owner(&self, identity: &Path) -> Option<ContentId> {
        self.path_contents
            .get(identity)
            .or_else(|| self.reserved_paths.get(identity))
            .copied()
    }

    pub(super) fn register_buffer_path(
        &mut self,
        content: ContentId,
        identity: PathBuf,
        path: PathBuf,
        baseline: FileBaseline,
    ) -> Result<(), ContentId> {
        if let Some(existing) = self.path_owner(&identity)
            && existing != content
        {
            return Err(existing);
        }
        if let Some(previous) = self.buffer_paths.remove(&content) {
            self.path_contents.remove(&previous.identity);
        }
        self.path_contents.insert(identity.clone(), content);
        self.buffer_paths.insert(
            content,
            BufferPathRecord {
                identity,
                path,
                baseline,
            },
        );
        Ok(())
    }

    pub(super) fn buffer_path_record(
        &self,
        content: ContentId,
    ) -> Option<(&PathBuf, &FileBaseline)> {
        self.buffer_paths
            .get(&content)
            .map(|record| (&record.path, &record.baseline))
    }

    pub(super) fn update_buffer_baseline(
        &mut self,
        content: ContentId,
        path: PathBuf,
        baseline: FileBaseline,
    ) {
        let record = self
            .buffer_paths
            .get_mut(&content)
            .expect("path-backed buffer has a registry record");
        record.path = path;
        record.baseline = baseline;
    }

    pub(super) fn clear_history(&mut self, content: ContentId) {
        self.transactions.remove(content);
    }

    fn save_guard(&self, content: ContentId, path: &Path, force: bool) -> SaveGuard {
        if force {
            return SaveGuard::Force;
        }
        self.buffer_paths
            .get(&content)
            .filter(|record| record.path == path)
            .map_or(SaveGuard::CreateNew, |record| match &record.baseline {
                FileBaseline::Missing => SaveGuard::CreateNew,
                FileBaseline::Materialized(text) => SaveGuard::Matches(text.clone()),
            })
    }

    pub(super) fn register_mode_background(&mut self, background: Box<dyn ModeBackground>) {
        self.modes.register_background(background);
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

    pub(super) fn commands(&self) -> &CommandRegistry {
        &self.commands
    }

    pub(super) fn commands_mut(&mut self) -> &mut CommandRegistry {
        &mut self.commands
    }

    pub(super) fn content_modes(&self) -> &ModeContentStore {
        &self.content_modes
    }

    pub(super) fn content_modes_mut(&mut self) -> &mut ModeContentStore {
        &mut self.content_modes
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
        command: vell_core::command::EditCommand,
        selections: &Selections,
    ) -> Option<ContentEditPlan> {
        self.contents.plan_edit(content, command, selections)
    }

    pub(super) fn copy_selections(
        &self,
        content: ContentId,
        selections: &Selections,
        kind: ClipboardKind,
    ) -> Option<ClipboardPayload> {
        self.contents.copy_selections(content, selections, kind)
    }

    pub(super) fn copy_for_edit(
        &self,
        content: ContentId,
        selections: &Selections,
        command: vell_core::command::EditCommand,
        kind: ClipboardKind,
    ) -> Option<ClipboardPayload> {
        self.contents
            .copy_for_edit(content, selections, command, kind)
    }

    pub(super) fn plan_cut(
        &self,
        content: ContentId,
        selections: &Selections,
        kind: ClipboardKind,
    ) -> Option<(ClipboardPayload, ContentEditPlan)> {
        self.contents.plan_cut(content, selections, kind)
    }

    pub(super) fn plan_paste(
        &self,
        content: ContentId,
        selections: &Selections,
        payload: &ClipboardPayload,
        placement: PastePlacement,
    ) -> Option<ContentEditPlan> {
        self.contents
            .plan_paste(content, selections, payload, placement)
    }

    pub(super) fn clipboard(&self) -> &ClipboardPayload {
        &self.clipboard
    }

    pub(super) fn set_clipboard(&mut self, payload: ClipboardPayload) {
        self.clipboard = payload;
    }

    pub(super) fn search_snapshot(
        &self,
        content: ContentId,
        expected_revision: vell_protocol::revision::Revision,
    ) -> Result<Option<SearchSnapshot>, SearchError> {
        self.contents.search_snapshot(content, expected_revision)
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

    pub(super) fn retarget_command_transaction(&mut self, target: ContentId) -> bool {
        let Some(command) = self.command_transaction.as_mut() else {
            self.command_transaction = Some(CommandTransaction {
                target,
                snapshot: None,
            });
            return true;
        };
        if command.target == target {
            return true;
        }
        if command.snapshot.is_some() {
            return false;
        }
        command.target = target;
        true
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

    pub(super) fn schedule_mode_jobs(&mut self) -> bool {
        let presentation_changed = self.modes.poll_background();
        if presentation_changed {
            self.content_modes.mark_presentation_dirty();
        }
        if tokio::runtime::Handle::try_current().is_err() {
            return presentation_changed;
        }
        let jobs = self.content_modes.take_background_jobs(&self.contents);
        for (mode, content, request) in jobs {
            self.queue_mode_job(mode, content, request);
        }
        presentation_changed
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
            let _ = tx.send(AppMessage::ModeJobFinished {
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

    pub(super) fn has_pending_save(&self, content: ContentId) -> bool {
        self.pending_saves.contains_key(&content)
    }

    pub(super) fn remove_content(&mut self, content: ContentId) -> bool {
        assert!(
            !self.has_pending_save(content),
            "pending saves must be resolved before removing content"
        );
        self.mode_jobs.retain(|key, slot| {
            if key.content != content {
                return true;
            }
            if let Some(running) = &slot.running {
                running.cancellation.cancel();
            }
            false
        });
        self.transactions.remove(content);
        if let Some(record) = self.buffer_paths.remove(&content) {
            self.path_contents.remove(&record.identity);
        }
        if self
            .command_transaction
            .as_ref()
            .is_some_and(|command| command.target == content)
        {
            self.command_transaction = None;
        }
        self.contents.remove(content)
    }

    pub(super) fn queue_open(
        &mut self,
        target: SpaceId,
        expected_view: Option<ViewId>,
        requested_path: PathBuf,
    ) -> ContentId {
        self.detach_open_target(target);
        let content = self.reserve_content_id();
        self.pending_opens.insert(
            content,
            PendingOpen {
                targets: HashMap::from([(target, expected_view)]),
            },
        );
        self.latest_opens.insert(target, content);
        let tx = self.message_tx.clone();
        self.tasks.spawn_critical(async move {
            let result = tokio::task::spawn_blocking(move || {
                let (path, identity) = normalize_path(&requested_path)?;
                let buffer = match std::fs::read_to_string(&path) {
                    Ok(text) => OpenedBuffer {
                        content: Content::buffer_from_file(path.clone(), text.clone()),
                        baseline: FileBaseline::Materialized(text),
                    },
                    Err(source) if source.kind() == io::ErrorKind::NotFound => OpenedBuffer {
                        content: Content::buffer_for_new_file(path.clone()),
                        baseline: FileBaseline::Missing,
                    },
                    Err(source) => return Err(source),
                };
                Ok(OpenedPath {
                    path,
                    identity,
                    buffer,
                })
            })
            .await
            .map_err(io::Error::other)
            .and_then(|result| result);
            let _ = tx.send(AppMessage::OpenCompleted { content, result });
        });
        content
    }

    fn detach_open_target(&mut self, target: SpaceId) {
        let Some(previous) = self.latest_opens.remove(&target) else {
            return;
        };
        if let Some(pending) = self.pending_opens.get_mut(&previous) {
            pending.targets.remove(&target);
        }
    }

    pub(super) fn complete_open(
        &mut self,
        content: ContentId,
        result: io::Result<OpenedPath>,
    ) -> io::Result<Option<OpenCompletion>> {
        let Some(pending) = self.pending_opens.remove(&content) else {
            return Ok(None);
        };
        let targets = pending
            .targets
            .into_iter()
            .filter_map(|(space, expected_view)| {
                (self.latest_opens.remove(&space) == Some(content)).then_some(OpenTarget {
                    space,
                    expected_view,
                })
            })
            .collect::<Vec<_>>();
        if targets.is_empty() {
            return Ok(None);
        }
        let opened = result?;
        if let Some(existing) = self.path_contents.get(&opened.identity) {
            return Ok(Some(OpenCompletion {
                content: *existing,
                path: opened.path,
                identity: opened.identity,
                opened: None,
                targets,
            }));
        }
        if self.reserved_paths.contains_key(&opened.identity) {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "open path is reserved by a pending Save As",
            ));
        }
        Ok(Some(OpenCompletion {
            content,
            path: opened.path,
            identity: opened.identity,
            opened: Some(opened.buffer),
            targets,
        }))
    }

    pub(super) fn install_open(
        &mut self,
        completion: OpenCompletion,
    ) -> io::Result<(ContentId, bool)> {
        let content = completion.content;
        let Some(opened) = completion.opened else {
            return Ok((content, false));
        };
        self.contents
            .insert(content, opened.content)
            .map_err(|_| io::Error::new(io::ErrorKind::AlreadyExists, "content id is occupied"))?;
        self.path_contents
            .insert(completion.identity.clone(), content);
        self.buffer_paths.insert(
            content,
            BufferPathRecord {
                identity: completion.identity,
                path: completion.path,
                baseline: opened.baseline,
            },
        );
        Ok((content, true))
    }

    /// 发起异步保存；同一 content 已在保存时，仅保留最新的后续快照。
    pub(super) fn queue_save(
        &mut self,
        content: ContentId,
        mut snapshot: SaveSnapshot,
        force: bool,
    ) -> bool {
        if let Some(pending) = self.pending_saves.get_mut(&content) {
            if pending.path_identity.is_some() {
                snapshot.path = pending.path.clone();
            }
            let queued_revision = pending
                .queued
                .as_ref()
                .map_or(pending.revision, |(queued, _)| queued.revision);
            if snapshot.revision > queued_revision {
                pending.queued = Some((snapshot, force));
            } else if snapshot.revision == queued_revision && force {
                if let Some((_, queued_force)) = pending.queued.as_mut() {
                    *queued_force = true;
                } else if !pending.force {
                    pending.queued = Some((snapshot, true));
                }
            }
            return false;
        }

        let guard = self.save_guard(content, &snapshot.path, force);
        let tx = self.message_tx.clone();
        let revision = snapshot.revision;
        let state = snapshot.state;
        let path = snapshot.path.clone();
        let bytes = snapshot.bytes.clone();
        self.pending_saves.insert(
            content,
            PendingSave {
                revision,
                state,
                path,
                bytes,
                path_identity: None,
                force,
                queued: None,
            },
        );
        self.tasks.spawn_critical(async move {
            let result = atomic_write(snapshot, guard).await;
            let _ = tx.send(AppMessage::SaveCompleted {
                content,
                revision,
                state,
                result,
            });
        });
        true
    }

    pub(super) fn queue_save_as(
        &mut self,
        content: ContentId,
        snapshot: SaveSnapshot,
        identity: PathBuf,
        force: bool,
    ) -> Result<bool, ContentId> {
        if self.has_pending_save(content) {
            return Ok(false);
        }
        if let Some(existing) = self.path_owner(&identity)
            && existing != content
        {
            return Err(existing);
        }
        self.reserved_paths.insert(identity.clone(), content);
        let guard = if force {
            SaveGuard::Force
        } else {
            self.buffer_paths
                .get(&content)
                .filter(|record| record.path == snapshot.path)
                .map_or(SaveGuard::CreateNew, |record| match &record.baseline {
                    FileBaseline::Missing => SaveGuard::CreateNew,
                    FileBaseline::Materialized(text) => SaveGuard::Matches(text.clone()),
                })
        };
        let tx = self.message_tx.clone();
        let revision = snapshot.revision;
        let state = snapshot.state;
        let path = snapshot.path.clone();
        let bytes = snapshot.bytes.clone();
        self.pending_saves.insert(
            content,
            PendingSave {
                revision,
                state,
                path,
                bytes,
                path_identity: Some(identity),
                force,
                queued: None,
            },
        );
        self.tasks.spawn_critical(async move {
            let result = atomic_write(snapshot, guard).await;
            let _ = tx.send(AppMessage::SaveCompleted {
                content,
                revision,
                state,
                result,
            });
        });
        Ok(true)
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
        let succeeded = result.is_ok();
        if let Some(identity) = &pending.path_identity {
            self.reserved_paths.remove(identity);
            if succeeded {
                if let Some(previous) = self.buffer_paths.remove(&content) {
                    self.path_contents.remove(&previous.identity);
                }
                self.path_contents.insert(identity.clone(), content);
                self.buffer_paths.insert(
                    content,
                    BufferPathRecord {
                        identity: identity.clone(),
                        path: pending.path.clone(),
                        baseline: FileBaseline::Materialized(pending.bytes.clone()),
                    },
                );
            }
        } else if succeeded && let Some(record) = self.buffer_paths.get_mut(&content) {
            record.path = pending.path.clone();
            record.baseline = FileBaseline::Materialized(pending.bytes.clone());
        }
        let result = self.contents.execute(
            content,
            ContentInput::Event(ContentEvent::SaveFinished {
                path: pending.path,
                state,
                result,
            }),
        );
        let queued = if !succeeded && pending.path_identity.is_some() {
            None
        } else {
            pending.queued
        };
        SaveCompletion { result, queued }
    }

    #[cfg(test)]
    pub(super) fn has_mode_jobs_for_content_for_test(&self, content: ContentId) -> bool {
        self.mode_jobs.keys().any(|key| key.content == content)
    }

    #[cfg(test)]
    pub(super) fn track_mode_job_for_test(
        &mut self,
        mode: ModeId,
        content: ContentId,
    ) -> tokio_util::sync::CancellationToken {
        let cancellation = self.tasks.cancellation_token().child_token();
        self.mode_jobs.insert(
            ModeJobKey {
                mode,
                content,
                slot: "test".into(),
            },
            ModeJobSlot {
                running: Some(RunningModeJob {
                    version: 1,
                    cancellation: cancellation.clone(),
                }),
                queued: None,
            },
        );
        cancellation
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
                path: PathBuf::new(),
                bytes: String::new(),
                path_identity: None,
                force: false,
                queued: queued.map(|snapshot| (snapshot, false)),
            },
        );
    }
}

pub(super) struct SaveCompletion {
    result: ContentResult,
    queued: Option<(SaveSnapshot, bool)>,
}

impl SaveCompletion {
    pub(super) fn into_parts(self) -> (ContentResult, Option<(SaveSnapshot, bool)>) {
        (self.result, self.queued)
    }
}

pub(super) struct OpenTarget {
    pub space: SpaceId,
    pub expected_view: Option<ViewId>,
}

pub(super) struct OpenCompletion {
    pub content: ContentId,
    pub path: PathBuf,
    pub identity: PathBuf,
    pub opened: Option<OpenedBuffer>,
    pub targets: Vec<OpenTarget>,
}

struct PendingOpen {
    targets: HashMap<SpaceId, Option<ViewId>>,
}

struct PendingSave {
    revision: u64,
    state: TextStateId,
    path: PathBuf,
    bytes: String,
    path_identity: Option<PathBuf>,
    force: bool,
    queued: Option<(SaveSnapshot, bool)>,
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

async fn atomic_write(snapshot: SaveSnapshot, guard: SaveGuard) -> io::Result<()> {
    tokio::task::spawn_blocking(move || {
        use std::io::Write;

        let parent = snapshot
            .path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
        temporary.write_all(snapshot.bytes.as_bytes())?;
        if !matches!(guard, SaveGuard::CreateNew)
            && let Ok(metadata) = std::fs::metadata(&snapshot.path)
        {
            temporary
                .as_file()
                .set_permissions(metadata.permissions())?;
        }
        temporary.as_file().sync_all()?;
        match guard {
            SaveGuard::Force => {
                temporary
                    .persist(&snapshot.path)
                    .map_err(|error| error.error)?;
            }
            SaveGuard::Matches(expected) => {
                if !matches!(
                    std::fs::read_to_string(&snapshot.path),
                    Ok(actual) if actual == expected
                ) {
                    return Err(io::Error::new(
                        io::ErrorKind::AlreadyExists,
                        "file changed since it was opened",
                    ));
                }
                temporary
                    .persist(&snapshot.path)
                    .map_err(|error| error.error)?;
            }
            SaveGuard::CreateNew => {
                temporary
                    .persist_noclobber(&snapshot.path)
                    .map_err(|error| error.error)?;
            }
        };
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

    #[tokio::test]
    async fn guarded_atomic_write_does_not_overwrite_a_changed_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("changed.txt");
        std::fs::write(&path, "external").unwrap();
        let snapshot = SaveSnapshot {
            path: path.clone(),
            bytes: "editor".to_owned(),
            revision: 1,
            state: TextStateId(1),
        };

        let error = atomic_write(snapshot, SaveGuard::Matches("opened".to_owned()))
            .await
            .unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read_to_string(path).unwrap(), "external");
    }

    #[tokio::test]
    async fn create_new_atomic_write_does_not_overwrite_a_raced_save_as_target() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("created.txt");
        std::fs::write(&path, "external").unwrap();
        let snapshot = SaveSnapshot {
            path: path.clone(),
            bytes: "editor".to_owned(),
            revision: 1,
            state: TextStateId(1),
        };

        let error = atomic_write(snapshot, SaveGuard::CreateNew)
            .await
            .unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read_to_string(path).unwrap(), "external");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn newer_mode_job_cancels_the_running_version() {
        let mut modes = ModeRegistry::new();
        let mode = modes.register(TestMode(ModeName::new("test"))).unwrap();
        let mut kernel = Kernel::new(ContentStore::default(), modes);
        let key = ModeJobKey {
            mode,
            content: ContentId(0),
            slot: "parse".into(),
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
            slot: "parse".into(),
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
