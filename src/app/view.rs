//! 视图实例的交互会话：绑定一个 content，并持有独立 mode 与 content view state。
//! 按 ViewId 索引（App.views），同一 Content 可被多个独立 View 绑定。

use crate::core::command::{Command, ContentCommand};
use crate::core::content_view_state::ContentViewState;
use crate::core::input::{InputContext, InputDecision, InputStatus};
use crate::core::keymap::Keymap;
use crate::core::mode::{ModeActionName, ModeInstance, ModeName, ModeRegistry};
use crate::protocol::content_query::CursorStyle;
use crate::protocol::ids::ContentId;
use crate::protocol::key_event::KeyEvent;
use crate::protocol::revision::Revision;
use crate::protocol::selection::Selections;

pub struct View {
    /// 绑定的 content；当前仅 View::new 写入，预留给同 content 多视图解析（spec §10）。
    content: ContentId,
    state: ContentViewState,
    mode: Option<ModeInstance>,
    revision: Revision,
}

pub(crate) enum ModeCommandResult {
    Unknown,
    Handled(Option<ContentCommand>),
}

impl View {
    pub fn new(content: ContentId, state: ContentViewState, mode: Option<ModeInstance>) -> Self {
        Self {
            content,
            state,
            mode,
            revision: Revision::default(),
        }
    }
    pub fn content(&self) -> ContentId {
        self.content
    }
    pub fn selections(&self) -> Option<&Selections> {
        self.state.selections()
    }
    pub fn state_mut(&mut self) -> &mut ContentViewState {
        &mut self.state
    }
    pub fn keymap(&self) -> Option<&Keymap> {
        self.mode.as_ref().map(ModeInstance::keymap)
    }

    pub fn input_status(&self) -> InputStatus {
        self.mode
            .as_ref()
            .map_or(InputStatus::Ready, InputContext::status)
    }

    pub fn capture(&mut self, key: KeyEvent) -> InputDecision<Command> {
        self.mode
            .as_mut()
            .map_or(InputDecision::Pass, |mode| mode.capture(key))
    }

    pub fn fallback(&self, key: KeyEvent) -> Option<Command> {
        self.mode.as_ref().and_then(|mode| mode.fallback(key))
    }

    pub fn on_input_timeout(&mut self) {
        if let Some(mode) = self.mode.as_mut() {
            mode.on_timeout();
        }
    }

    pub fn cancel_input(&mut self) {
        if let Some(mode) = self.mode.as_mut() {
            mode.cancel();
        }
    }

    pub fn cursor_style(&self) -> CursorStyle {
        self.mode
            .as_ref()
            .map_or(CursorStyle::Default, ModeInstance::cursor_style)
    }

    pub(crate) fn execute_mode_command(
        &mut self,
        registry: &ModeRegistry,
        mode: &ModeName,
        action: &ModeActionName,
    ) -> ModeCommandResult {
        let Some((mode, action)) = registry.resolve_command(mode, action) else {
            return ModeCommandResult::Unknown;
        };
        let Some(instance) = self.mode.as_mut() else {
            return ModeCommandResult::Unknown;
        };
        ModeCommandResult::Handled(instance.execute(mode, action))
    }
    pub fn revision(&self) -> Revision {
        self.revision
    }
    pub fn touch(&mut self) {
        self.revision.next();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::content_view_state::ContentViewState;

    #[test]
    fn status_bar_view_has_no_mode_or_selections() {
        let v = View::new(ContentId(0), ContentViewState::StatusBar, None);
        assert_eq!(v.content(), ContentId(0));
        assert!(v.keymap().is_none());
        assert!(v.selections().is_none());
    }

    #[test]
    fn touch_advances_view_revision() {
        let mut view = View::new(ContentId(0), ContentViewState::StatusBar, None);

        view.touch();

        assert_eq!(view.revision(), Revision(1));
    }
}
