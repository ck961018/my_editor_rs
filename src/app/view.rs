//! 视图实例的交互会话：绑定一个 content，并持有独立 mode 与 content view state。
//! 按 ViewId 索引（App.views），同一 Content 可被多个独立 View 绑定。

use crate::core::command::{Command, ModeCommand};
use crate::core::content_view_state::ContentViewState;
use crate::core::input::{InputContext, InputDecision, InputStatus};
use crate::core::keymap::Keymap;
use crate::core::mode::{ModeError, ModeInstance, ModeRegistry};
use crate::protocol::content_query::{CursorStyle, SelectionShape};
use crate::protocol::ids::ContentId;
use crate::protocol::key_event::KeyEvent;
use crate::protocol::revision::Revision;
use crate::protocol::selection::Selections;

pub struct View {
    /// 绑定的 content；当前仅 View::new 写入，同一 content 可由多个 View 独立呈现。
    content: ContentId,
    state: ContentViewState,
    mode: Option<ModeInstance>,
    revision: Revision,
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

    pub fn selection_shape(&self) -> SelectionShape {
        self.mode
            .as_ref()
            .map_or(SelectionShape::Character, ModeInstance::selection_shape)
    }

    pub(crate) fn execute_mode_command(
        &mut self,
        registry: &ModeRegistry,
        command: &ModeCommand,
    ) -> Result<Option<Command>, ModeError> {
        let (mode, action) = registry.resolve_command_checked(&command.mode, &command.action)?;
        let Some(instance) = self.mode.as_mut() else {
            return Err(ModeError::InactiveMode {
                requested: command.mode.clone(),
                active: None,
            });
        };
        if instance.name() != &command.mode {
            return Err(ModeError::InactiveMode {
                requested: command.mode.clone(),
                active: Some(instance.name().clone()),
            });
        }
        instance.execute(mode, action)
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
