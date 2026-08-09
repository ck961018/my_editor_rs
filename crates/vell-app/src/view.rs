//! 视图实例的交互会话：绑定一个 content，持有独立 content view state，并
//! 控制一个或多个直属 Pane（正文、状态栏等）。按 ViewId 索引（App.views），
//! 同一 Content 可被多个独立 View 绑定；同一 View 可占据多个 Content Space。

use std::collections::HashMap;

use vell_core::content_view_state::ContentViewState;
use vell_protocol::ids::{ContentId, SpaceId, ViewId};
use vell_protocol::revision::Revision;
use vell_protocol::selection::Selections;

/// View 内部识别直属 Pane 的稳定语义键；布局协议仍只使用 SpaceId。
pub type PaneKey = String;

/// 正文 Pane：view 的主编辑区域。
pub const BODY_PANE: &str = "body";
/// 内建状态栏 Pane。
pub const STATUS_PANE: &str = "builtin.status";

/// SpaceId 与 PaneKey 的双向映射。每个 PaneKey 在一个 view 内唯一，
/// 每个 Space 同一时刻只属于一个 view 的一个 Pane。
#[derive(Default)]
pub struct ViewPaneMap {
    by_space: HashMap<SpaceId, PaneKey>,
    by_key: HashMap<PaneKey, SpaceId>,
}

impl ViewPaneMap {
    pub fn key_for_space(&self, space: SpaceId) -> Option<&str> {
        self.by_space.get(&space).map(String::as_str)
    }

    pub fn space_for_key(&self, key: &str) -> Option<SpaceId> {
        self.by_key.get(key).copied()
    }

    pub fn spaces(&self) -> impl Iterator<Item = SpaceId> + '_ {
        self.by_space.keys().copied()
    }

    fn insert(&mut self, space: SpaceId, key: PaneKey) {
        if let Some(previous_key) = self.by_space.insert(space, key.clone()) {
            self.by_key.remove(&previous_key);
        }
        if let Some(previous_space) = self.by_key.insert(key, space) {
            self.by_space.remove(&previous_space);
        }
    }

    fn remove_space(&mut self, space: SpaceId) -> Option<PaneKey> {
        let key = self.by_space.remove(&space)?;
        self.by_key.remove(&key);
        Some(key)
    }
}

pub struct View {
    /// 绑定的 content；当前仅 View::new 写入，同一 content 可由多个 View 独立呈现。
    content: ContentId,
    state: ContentViewState,
    revision: Revision,
    panes: ViewPaneMap,
    /// 通用 View 切换操作是否可替换本实例；DiffView 等复合 view 的子 view 为 false。
    switchable: bool,
    /// 语义父 view（复合 view 的组合关系），不同于 Space 布局父子。
    parent: Option<ViewId>,
    children: Vec<ViewId>,
}

impl View {
    pub fn new(content: ContentId, state: ContentViewState) -> Self {
        Self {
            content,
            state,
            revision: Revision::default(),
            panes: ViewPaneMap::default(),
            switchable: true,
            parent: None,
            children: Vec::new(),
        }
    }

    pub fn content(&self) -> ContentId {
        self.content
    }

    pub fn panes(&self) -> &ViewPaneMap {
        &self.panes
    }

    /// 登记直属 Pane：space 由本 view 决定显示内容。
    pub fn assign_pane(&mut self, space: SpaceId, key: impl Into<PaneKey>) {
        self.panes.insert(space, key.into());
    }

    /// 释放 space 的 Pane 归属（space 关闭或移交给其他 view）。
    pub fn release_pane_space(&mut self, space: SpaceId) -> Option<PaneKey> {
        self.panes.remove_space(space)
    }

    pub fn switchable(&self) -> bool {
        self.switchable
    }

    pub fn set_switchable(&mut self, switchable: bool) {
        self.switchable = switchable;
    }

    pub fn parent(&self) -> Option<ViewId> {
        self.parent
    }

    pub fn set_parent(&mut self, parent: Option<ViewId>) {
        self.parent = parent;
    }

    pub fn children(&self) -> &[ViewId] {
        &self.children
    }

    pub fn push_child(&mut self, child: ViewId) {
        if !self.children.contains(&child) {
            self.children.push(child);
        }
    }

    pub fn remove_child(&mut self, child: ViewId) {
        self.children.retain(|candidate| *candidate != child);
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
    fn pane_map_tracks_spaces_and_keys_bidirectionally() {
        let mut view = View::new(ContentId(1), ContentViewState::buffer());
        view.assign_pane(SpaceId(0), BODY_PANE);
        view.assign_pane(SpaceId(1), STATUS_PANE);

        assert_eq!(view.panes().key_for_space(SpaceId(0)), Some(BODY_PANE));
        assert_eq!(view.panes().space_for_key(STATUS_PANE), Some(SpaceId(1)));
        assert_eq!(
            view.release_pane_space(SpaceId(1)),
            Some(STATUS_PANE.to_owned())
        );
        assert_eq!(view.panes().space_for_key(STATUS_PANE), None);
    }

    #[test]
    fn reassigning_a_pane_key_moves_it_to_the_new_space() {
        let mut view = View::new(ContentId(1), ContentViewState::buffer());
        view.assign_pane(SpaceId(1), STATUS_PANE);
        view.assign_pane(SpaceId(9), STATUS_PANE);

        assert_eq!(view.panes().space_for_key(STATUS_PANE), Some(SpaceId(9)));
        assert_eq!(view.panes().key_for_space(SpaceId(1)), None);
    }

    #[test]
    fn views_default_to_switchable_without_semantic_parent() {
        let view = View::new(ContentId(0), ContentViewState::buffer());

        assert!(view.switchable());
        assert_eq!(view.parent(), None);
        assert!(view.children().is_empty());
    }

    #[test]
    fn touch_advances_view_revision() {
        let mut view = View::new(ContentId(0), ContentViewState::buffer());

        view.touch();

        assert_eq!(view.revision(), Revision(1));
    }
}
