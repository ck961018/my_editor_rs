//! 视图实例的交互会话：绑定一个 content，并持有独立 content view state。
//! 按 ViewId 索引（App.views），同一 Content 可被多个独立 View 绑定。

use vell_core::content_view_state::ContentViewState;
use vell_protocol::ids::{ContentId, ViewId};
use vell_protocol::revision::Revision;
use vell_protocol::selection::Selections;

pub struct View {
    /// 绑定的 content；当前仅 View::new 写入，同一 content 可由多个 View 独立呈现。
    content: ContentId,
    state: ContentViewState,
    revision: Revision,
    /// 状态栏 view 的服务目标：Some(editor_view) 表示本 view 是绑定在
    /// editor_view 上的状态栏呈现位；None 表示普通内容 view。
    status_target: Option<ViewId>,
}

impl View {
    pub fn new(content: ContentId, state: ContentViewState) -> Self {
        Self {
            content,
            state,
            revision: Revision::default(),
            status_target: None,
        }
    }

    /// 创建一个状态栏呈现位：绑定 editor_view 的内容，呈现该 editor view
    /// 的状态栏，而不是作为可聚焦内容 view。
    ///
    /// 状态栏 view 使用普通的 BufferViewState（含默认 selection），但该
    /// selection 永不使用：所有遍历内容 view 的路径都以 `is_status_bar()`
    /// 排除状态栏位，`transform_content_views` 也跳过其 selection 变换。
    pub fn status_bar(content: ContentId, target_view: ViewId) -> Self {
        Self {
            content,
            state: ContentViewState::buffer(),
            revision: Revision::default(),
            status_target: Some(target_view),
        }
    }

    pub fn content(&self) -> ContentId {
        self.content
    }

    /// 本 view 是否承载状态栏呈现（而非内容 view）。
    pub fn is_status_bar(&self) -> bool {
        self.status_target.is_some()
    }

    /// 状态栏 view 服务的 editor view；普通 view 返回 None。
    pub fn status_target(&self) -> Option<ViewId> {
        self.status_target
    }

    /// 重定向状态栏 view 的服务目标；仅在状态栏 view 上调用。
    pub fn set_status_target(&mut self, target_view: ViewId) {
        if self.status_target != Some(target_view) {
            self.status_target = Some(target_view);
            self.touch();
        }
    }

    /// 重绑定状态栏 view 的内容（跟随其服务目标的内容变化）。
    pub fn set_content(&mut self, content: ContentId) {
        if self.content != content {
            self.content = content;
            self.touch();
        }
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

    #[test]
    fn status_bar_view_marks_its_target() {
        let v = View::status_bar(ContentId(1), ViewId(1));
        assert_eq!(v.content(), ContentId(1));
        assert!(v.is_status_bar());
        assert_eq!(v.status_target(), Some(ViewId(1)));
        assert!(v.selections().is_some());
    }

    #[test]
    fn retarget_updates_status_bar_target() {
        let mut v = View::status_bar(ContentId(1), ViewId(1));
        v.set_status_target(ViewId(2));
        assert_eq!(v.status_target(), Some(ViewId(2)));
    }

    #[test]
    fn touch_advances_view_revision() {
        let mut view = View::new(ContentId(0), ContentViewState::buffer());

        view.touch();

        assert_eq!(view.revision(), Revision(1));
    }
}
