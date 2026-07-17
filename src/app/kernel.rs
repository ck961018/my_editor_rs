use std::collections::HashMap;
use std::io;
use std::path::Path;

use tokio::sync::mpsc;

use crate::app::message::AppMessage;
use crate::app::tasks::AppTasks;
use crate::core::content::{ContentEvent, ContentInput, ContentResult, SaveSnapshot};
use crate::core::content_store::ContentStore;
use crate::core::mode::ModeRegistry;
use crate::core::transaction::TextStateId;
use crate::protocol::ids::ContentId;

pub(super) struct Kernel {
    contents: ContentStore,
    modes: ModeRegistry,
    message_tx: mpsc::UnboundedSender<AppMessage>,
    message_rx: mpsc::UnboundedReceiver<AppMessage>,
    tasks: AppTasks,
    pending_saves: HashMap<ContentId, PendingSave>,
}

impl Kernel {
    pub(super) fn new(contents: ContentStore, modes: ModeRegistry) -> Self {
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

    pub(super) fn contents(&self) -> &ContentStore {
        &self.contents
    }

    #[cfg(test)]
    pub(super) fn contents_mut(&mut self) -> &mut ContentStore {
        &mut self.contents
    }

    pub(super) fn modes(&self) -> &ModeRegistry {
        &self.modes
    }

    #[cfg(test)]
    pub(super) fn modes_mut(&mut self) -> &mut ModeRegistry {
        &mut self.modes
    }

    pub(super) fn execute(&mut self, content: ContentId, input: ContentInput<'_>) -> ContentResult {
        self.contents.execute(content, input)
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
