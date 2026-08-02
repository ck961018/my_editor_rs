use std::io;

use crate::application::App;
use crate::message::AppMessage;
use vell_core::content::{ContentEffect, ContentResult};
use vell_frontend::Frontend;
use vell_protocol::ids::ContentId;

impl<F: Frontend> App<F> {
    pub(super) fn handle_app_message(&mut self, message: AppMessage) -> io::Result<bool> {
        let changed = match message {
            AppMessage::OpenCompleted { content, result } => {
                match self.complete_async_open(content, result) {
                    Ok(changed) => changed,
                    Err(error) => {
                        self.record_recoverable_error(error);
                        true
                    }
                }
            }
            AppMessage::SaveCompleted {
                content,
                revision,
                state,
                result,
            } => {
                let command_result = result
                    .as_ref()
                    .map(|()| vell_mode::command_registry::CommandValue::default())
                    .map_err(|error| {
                        vell_mode::command_registry::CommandError::Failed(format!(
                            "save failed: {error}"
                        ))
                    });
                let completion = self.kernel.complete_save(content, revision, state, result);
                let (result, queued) = completion.into_parts();
                let content_changed = matches!(
                    &result,
                    ContentResult::Handled(outcome) if outcome.content_changed
                );
                self.handle_content_result(content, result);
                if content_changed {
                    self.session.clear_status_message();
                    self.session.touch_content_views(content);
                }
                if let Some((snapshot, force)) = queued {
                    self.kernel.queue_save(content, snapshot, force);
                }
                self.complete_command_tasks(content, revision, command_result);
                true
            }
            AppMessage::ModeJobFinished {
                key,
                version,
                result,
            } => {
                let content = key.content;
                let changed = self.kernel.complete_mode_job(key, version, result);
                if changed {
                    self.session.touch_content_views(content);
                }
                self.kernel.schedule_mode_jobs();
                changed
            }
        };
        self.session
            .refresh_presentation(self.kernel.contents(), self.kernel.content_modes());
        Ok(changed)
    }

    pub(super) fn handle_content_result(&mut self, content: ContentId, result: ContentResult) {
        if let ContentResult::Handled(outcome) = result
            && let ContentEffect::Save(snapshot) = outcome.effect
        {
            self.kernel.queue_save(content, snapshot, false);
        }
    }
}
