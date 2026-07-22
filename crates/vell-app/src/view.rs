//! 视图实例的交互会话：绑定一个 content，并持有独立 content view state。
//! 按 ViewId 索引（App.views），同一 Content 可被多个独立 View 绑定。

use vell_core::content_view_state::ContentViewState;
use vell_protocol::ids::ContentId;
use vell_protocol::revision::Revision;
use vell_protocol::selection::Selections;

pub struct View {
    /// 绑定的 content；当前仅 View::new 写入，同一 content 可由多个 View 独立呈现。
    content: ContentId,
    state: ContentViewState,
    revision: Revision,
}

impl View {
    pub fn new(content: ContentId, state: ContentViewState) -> Self {
        Self {
            content,
            state,
            revision: Revision::default(),
        }
    }
    pub fn content(&self) -> ContentId {
        self.content
    }
    pub fn selections(&self) -> Option<&Selections> {
        self.state.selections()
    }
    pub fn state(&self) -> &ContentViewState {
        &self.state
    }
    pub fn state_mut(&mut self) -> &mut ContentViewState {
        &mut self.state
    }
    pub fn set_selections(&mut self, selections: Selections) -> bool {
        let changed = self.state.replace_selections(selections) == Some(true);
        if changed {
            self.touch();
        }
        changed
    }
    pub fn revision(&self) -> Revision {
        self.revision
    }
    pub fn touch(&mut self) {
        self.revision.next();
    }

    pub(crate) fn restore_selections_and_revision(
        &mut self,
        selections: Selections,
        revision: Revision,
    ) {
        if self.state.replace_selections(selections).is_some() {
            self.revision = revision;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vell_core::content_view_state::ContentViewState;
    use vell_protocol::ids::ViewId;

    #[test]
    fn status_bar_view_has_no_selections() {
        let v = View::new(
            ContentId(0),
            ContentViewState::status_bar(ViewId(1), ContentId(1)),
        );
        assert_eq!(v.content(), ContentId(0));
        assert!(v.selections().is_none());
    }

    #[test]
    fn touch_advances_view_revision() {
        let mut view = View::new(
            ContentId(0),
            ContentViewState::status_bar(ViewId(1), ContentId(1)),
        );

        view.touch();

        assert_eq!(view.revision(), Revision(1));
    }
}
