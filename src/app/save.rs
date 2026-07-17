use std::io;
use std::path::Path;

use crate::app::application::App;
use crate::app::kernel::PendingSave;
use crate::app::message::AppMessage;
use crate::core::content::{
    ContentEffect, ContentEvent, ContentInput, ContentResult, SaveSnapshot,
};
use crate::frontend::Frontend;
use crate::protocol::ids::ContentId;

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

impl<F: Frontend> App<F> {
    pub(super) fn handle_app_message(&mut self, message: AppMessage) -> io::Result<()> {
        match message {
            AppMessage::SaveCompleted {
                content,
                revision,
                state,
                result,
            } => {
                let pending = self
                    .kernel
                    .pending_saves
                    .remove(&content)
                    .expect("save completion must match a pending save");
                assert_eq!(pending.revision, revision, "save revision mismatch");
                assert_eq!(pending.state, state, "save state mismatch");
                let result = self.kernel.contents.execute(
                    content,
                    ContentInput::Event(ContentEvent::SaveFinished { state, result }),
                );
                self.handle_content_result(content, result);
                if let Some(snapshot) = pending.queued {
                    self.spawn_save(content, snapshot);
                }
            }
        }
        Ok(())
    }

    pub(super) fn handle_content_result(&mut self, id: ContentId, result: ContentResult) {
        if let ContentResult::Handled(outcome) = result {
            if let ContentEffect::Save(snapshot) = outcome.effect {
                self.spawn_save(id, snapshot);
            }
        }
    }

    fn spawn_save(&mut self, id: ContentId, snapshot: SaveSnapshot) -> bool {
        if let Some(pending) = self.kernel.pending_saves.get_mut(&id) {
            let queued_revision = pending
                .queued
                .as_ref()
                .map_or(pending.revision, |queued| queued.revision);
            if snapshot.revision > queued_revision {
                pending.queued = Some(snapshot);
            }
            return false;
        }
        let tx = self.kernel.message_tx.clone();
        let revision = snapshot.revision;
        let state = snapshot.state;
        self.kernel.pending_saves.insert(
            id,
            PendingSave {
                revision,
                state,
                queued: None,
            },
        );
        self.kernel.tasks.spawn_critical(async move {
            let result = atomic_write(snapshot).await;
            let _ = tx.send(AppMessage::SaveCompleted {
                content: id,
                revision,
                state,
                result,
            });
        });
        true
    }
}
