use std::collections::HashMap;
use std::fmt;

use crate::application::App;
use crate::scene_model::{CloseResult, SceneError, SplitResult};
use crate::view::View;
use vell_core::content_store::ContentStore;
use vell_frontend::Frontend;
use vell_protocol::ids::{ContentId, SpaceId, ViewId};
use vell_protocol::scene::Scene;
use vell_protocol::space::{Sizing, SpaceKind, SplitDirection};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum StatusBarPlacement {
    #[default]
    Global,
    PerPane,
}

/// 状态栏 Pane 的定位信息：状态栏不再是独立 view，而是某个 editor view
/// 的 STATUS_PANE 直属 Pane。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StatusBarHandle {
    /// 状态栏 Pane 所在的 Space。
    pub space: SpaceId,
    /// 当前拥有该 Pane 的 editor view。
    pub target_view: ViewId,
    /// target_view 绑定的 content。
    pub content: ContentId,
}

impl<F: Frontend> App<F> {
    pub fn status_bar_placement(&self) -> StatusBarPlacement {
        self.session.status_bar_placement()
    }

    pub fn status_bar_for_view(&self, editor: ViewId) -> Option<StatusBarHandle> {
        self.session.status_bar_for_view(editor)
    }

    pub fn status_bars_for_content(&self, content: ContentId) -> Vec<StatusBarHandle> {
        self.session.status_bars_for_content(content)
    }

    /// 焦点 Pane 所属 view 的通用切换目标（最近 switchable 祖先）。
    pub fn switch_target(&self) -> Option<ViewId> {
        self.session.switch_target(self.session.focused())
    }

    pub fn set_status_bar_placement(
        &mut self,
        placement: StatusBarPlacement,
    ) -> std::io::Result<()> {
        self.session
            .set_status_bar_placement(placement)
            .map_err(std::io::Error::other)?;
        self.session
            .refresh_presentation(self.kernel.contents(), self.kernel.content_modes());
        Ok(())
    }

    pub fn set_status_bar_visible(
        &mut self,
        editor: Option<ViewId>,
        visible: bool,
    ) -> std::io::Result<()> {
        self.session
            .set_status_bar_visible(editor, visible)
            .map_err(std::io::Error::other)
    }

    pub(super) fn split_space(
        &mut self,
        target: SpaceId,
        content: ContentId,
        focusable: bool,
        direction: SplitDirection,
        focus_new: bool,
    ) -> Result<SplitResult, LayoutError> {
        let inherited_state = self
            .session
            .view_for_space(target)
            .and_then(|view| self.session.view(view))
            .filter(|view| view.content() == content)
            .map(|view| view.state().clone());
        let mode_names = self.session.mode_chain_for_new_view(content);
        let mut view = create_view(content, self.kernel.contents(), &mode_names)
            .ok_or(LayoutError::MissingContent(content))?;
        if let Some(state) = inherited_state {
            *view.view.state_mut() = state;
        }
        let (contents, modes, content_modes) = self.kernel.mode_attachment_parts();
        let result = self.session.split_space(
            target,
            view,
            focusable,
            direction,
            focus_new,
            modes,
            content_modes,
            contents,
        )?;
        self.kernel.schedule_mode_jobs();
        self.session
            .refresh_presentation(self.kernel.contents(), self.kernel.content_modes());
        Ok(result)
    }

    pub(super) fn close_space(&mut self, target: SpaceId) -> Result<CloseResult, LayoutError> {
        let removed = self
            .session
            .view_for_space(target)
            .and_then(|view| self.session.view(view).map(|data| (view, data.content())));
        let (contents, content_modes) = self.kernel.mode_runtime_parts();
        let result = self.session.close_space(target, content_modes, contents)?;
        if let Some((view, content)) = removed
            && self.kernel.active_transaction_owner(content) == Some(Some(view))
        {
            self.kernel.commit_transaction(content);
        }
        if let Some((view, _)) = removed {
            self.cancel_pending_commands_for_view(view);
        }
        self.session
            .refresh_presentation(self.kernel.contents(), self.kernel.content_modes());
        Ok(result)
    }

    pub(super) fn replace_space_content(
        &mut self,
        target: SpaceId,
        content: ContentId,
        focusable: bool,
    ) -> Result<(), LayoutError> {
        let mode_names = self.session.mode_chain_for_new_view(content);
        let view = create_view(content, self.kernel.contents(), &mode_names)
            .ok_or(LayoutError::MissingContent(content))?;
        let removed = self
            .session
            .view_for_space(target)
            .and_then(|view| self.session.view(view).map(|data| (view, data.content())));
        let (contents, modes, content_modes) = self.kernel.mode_attachment_parts();
        self.session.replace_space_content(
            target,
            view,
            focusable,
            modes,
            content_modes,
            contents,
        )?;
        if let Some((view, _)) = removed {
            self.cancel_pending_commands_for_view(view);
        }
        self.kernel.schedule_mode_jobs();
        if let Some((view, content)) = removed
            && self.kernel.active_transaction_owner(content) == Some(Some(view))
        {
            self.kernel.commit_transaction(content);
        }
        self.session
            .refresh_presentation(self.kernel.contents(), self.kernel.content_modes());
        Ok(())
    }

    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "layout mutation is exposed as an application backend operation"
        )
    )]
    pub(super) fn set_space_sizing(
        &mut self,
        target: SpaceId,
        sizing: Sizing,
    ) -> Result<(), LayoutError> {
        self.session.set_space_sizing(target, sizing)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum LayoutError {
    MissingContent(ContentId),
    WouldRemoveLastFocusable(SpaceId),
    NoFocusableSpace,
    NoStatusBar,
    StatusBarSpace(SpaceId),
    Scene(SceneError),
}

impl fmt::Display for LayoutError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingContent(content) => {
                write!(formatter, "content {} does not exist", content.0)
            }
            Self::WouldRemoveLastFocusable(space) => {
                write!(formatter, "space {} is the last focusable space", space.0)
            }
            Self::NoFocusableSpace => write!(formatter, "scene has no focusable space"),
            Self::NoStatusBar => write!(formatter, "status bar does not exist"),
            Self::StatusBarSpace(space) => {
                write!(formatter, "space {} is managed by the status bar", space.0)
            }
            Self::Scene(error) => write!(formatter, "scene mutation failed: {error:?}"),
        }
    }
}

impl std::error::Error for LayoutError {}

impl From<SceneError> for LayoutError {
    fn from(error: SceneError) -> Self {
        Self::Scene(error)
    }
}

pub(super) fn create_view(
    content: ContentId,
    contents: &ContentStore,
    mode_names: &[crate::mode_name::ModeName],
) -> Option<NewView> {
    if !contents.contains(content) {
        return None;
    }
    let state = contents
        .create_view_state(content)
        .expect("existing content creates view state");
    Some(NewView {
        view: View::new(content, state),
        mode_names: mode_names.to_vec(),
    })
}

pub(super) struct NewView {
    pub(super) view: View,
    pub(super) mode_names: Vec<crate::mode_name::ModeName>,
}

fn collect_view_spaces(scene: &Scene, sid: SpaceId, out: &mut Vec<(SpaceId, ViewId)>) {
    let node = scene.node(sid);
    match &node.space.kind {
        SpaceKind::Content { view, .. } => {
            out.push((sid, *view));
        }
        SpaceKind::Container { .. } => {
            for child in &node.children {
                collect_view_spaces(scene, *child, out);
            }
        }
    }
}

pub(super) fn scene_views(scene: &Scene) -> Vec<(SpaceId, ViewId)> {
    let mut views = Vec::new();
    collect_view_spaces(scene, scene.root(), &mut views);
    views
}

pub(super) fn view_for_space(scene: &Scene, space: SpaceId) -> Option<ViewId> {
    if !scene.contains(space) {
        return None;
    }
    match &scene.node(space).space.kind {
        SpaceKind::Content { view, .. } => Some(*view),
        SpaceKind::Container { .. } => None,
    }
}

/// 通用 View 切换的目标解析：从焦点 Pane 所属 view 沿语义 parent 链向上，
/// 第一个 switchable 的 view 即切换目标。
pub(super) fn resolve_switch_target(
    scene: &Scene,
    views: &HashMap<ViewId, View>,
    focused: SpaceId,
) -> Option<ViewId> {
    let mut current = view_for_space(scene, focused)?;
    loop {
        let view = views.get(&current)?;
        if view.switchable() {
            return Some(current);
        }
        current = view.parent()?;
    }
}

pub(super) fn view_space_focusable(scene: &Scene, space: SpaceId) -> Option<bool> {
    if !scene.contains(space) {
        return None;
    }
    match &scene.node(space).space.kind {
        SpaceKind::Content { focusable, .. } => Some(*focusable),
        SpaceKind::Container { .. } => None,
    }
}

pub(super) fn focusable_view_count(scene: &Scene) -> usize {
    scene_views(scene)
        .into_iter()
        .filter(|(space, _)| view_space_focusable(scene, *space) == Some(true))
        .count()
}

pub(super) fn resolve_focus(
    scene: &Scene,
    previous: SpaceId,
    preferred: Option<SpaceId>,
) -> Option<SpaceId> {
    preferred
        .and_then(|space| first_focusable_at_or_below(scene, space))
        .or_else(|| (view_space_focusable(scene, previous) == Some(true)).then_some(previous))
        .or_else(|| {
            scene_views(scene)
                .into_iter()
                .map(|(space, _)| space)
                .find(|space| view_space_focusable(scene, *space) == Some(true))
        })
}

fn first_focusable_at_or_below(scene: &Scene, space: SpaceId) -> Option<SpaceId> {
    if !scene.contains(space) {
        return None;
    }
    if view_space_focusable(scene, space) == Some(true) {
        return Some(space);
    }
    let mut views = Vec::new();
    collect_view_spaces(scene, space, &mut views);
    views
        .into_iter()
        .map(|(space, _)| space)
        .find(|space| view_space_focusable(scene, *space) == Some(true))
}
