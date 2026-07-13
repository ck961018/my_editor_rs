//! 视图实例的交互会话：绑定一个 content，并持有独立 mode 与 content view state。
//! 按 ViewId 索引（App.views），同一 Content 可被多个独立 View 绑定。

use crate::core::content_view_state::ContentViewState;
use crate::core::mode::ModeInstance;
use crate::protocol::ids::ContentId;
use crate::protocol::remote::Revision;
use crate::protocol::selection::Selections;

pub struct View {
    /// 绑定的 content；当前仅 View::new 写入，预留给同 content 多视图解析（spec §10）。
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
    pub fn mode(&self) -> Option<&ModeInstance> {
        self.mode.as_ref()
    }
    pub fn mode_mut(&mut self) -> Option<&mut ModeInstance> {
        self.mode.as_mut()
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
        assert!(v.mode().is_none());
        assert!(v.selections().is_none());
    }

    #[test]
    fn touch_advances_view_revision() {
        let mut view = View::new(ContentId(0), ContentViewState::StatusBar, None);

        view.touch();

        assert_eq!(view.revision(), Revision(1));
    }
}
