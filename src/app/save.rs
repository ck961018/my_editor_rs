use std::io;

use crate::app::application::App;
use crate::app::message::AppMessage;
use crate::core::content::{ContentEffect, ContentResult};
use crate::frontend::Frontend;
use crate::protocol::ids::ContentId;

impl<F: Frontend> App<F> {
    pub(super) fn handle_app_message(&mut self, message: AppMessage) -> io::Result<bool> {
        let changed = match message {
            AppMessage::SaveCompleted {
                content,
                revision,
                state,
                result,
            } => {
                let completion = self.kernel.complete_save(content, revision, state, result);
                let (result, queued) = completion.into_parts();
                self.handle_content_result(content, result);
                if let Some(snapshot) = queued {
                    self.kernel.queue_save(content, snapshot);
                }
                true
            }
            AppMessage::ModeJobCompleted {
                key,
                version,
                result,
            } => {
                let content = key.content;
                let changed = self.kernel.complete_mode_job(key, version, result);
                if changed {
                    self.session.touch_content_views(content);
                }
                changed
            }
        };
        Ok(changed)
    }

    pub(super) fn handle_content_result(&mut self, content: ContentId, result: ContentResult) {
        if let ContentResult::Handled(outcome) = result
            && let ContentEffect::Save(snapshot) = outcome.effect
        {
            self.kernel.queue_save(content, snapshot);
        }
    }
}
