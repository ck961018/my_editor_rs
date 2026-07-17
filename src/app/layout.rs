use crate::app::application::App;
use crate::app::scene_model::{CloseResult, SceneError, SplitResult};
use crate::app::view::View;
use crate::core::content_store::ContentStore;
use crate::core::mode::ModeRegistry;
use crate::frontend::Frontend;
use crate::protocol::ids::{ContentId, SpaceId, ViewId};
use crate::protocol::scene::Scene;
use crate::protocol::space::{Sizing, SpaceKind, SplitDirection};

impl<F: Frontend> App<F> {
    #[allow(dead_code)] // 本轮只提供后端入口，不接入按键或 UI。
    pub(super) fn split_space(
        &mut self,
        target: SpaceId,
        content: ContentId,
        focusable: bool,
        direction: SplitDirection,
        focus_new: bool,
    ) -> Result<SplitResult, LayoutError> {
        let view = create_view(content, self.kernel.contents(), self.kernel.modes())
            .ok_or(LayoutError::MissingContent(content))?;
        self.session
            .split_space(target, view, focusable, direction, focus_new)
    }

    #[allow(dead_code)] // 本轮只提供后端入口，不接入按键或 UI。
    pub(super) fn close_space(&mut self, target: SpaceId) -> Result<CloseResult, LayoutError> {
        self.session.close_space(target)
    }

    #[allow(dead_code)] // 本轮只提供后端入口，不接入按键或 UI。
    pub(super) fn replace_space_content(
        &mut self,
        target: SpaceId,
        content: ContentId,
        focusable: bool,
    ) -> Result<(), LayoutError> {
        let view = create_view(content, self.kernel.contents(), self.kernel.modes())
            .ok_or(LayoutError::MissingContent(content))?;
        self.session.replace_space_content(target, view, focusable)
    }

    #[allow(dead_code)] // 本轮只提供后端入口，不接入按键或 UI。
    pub(super) fn set_space_sizing(
        &mut self,
        target: SpaceId,
        sizing: Sizing,
    ) -> Result<(), LayoutError> {
        self.session.set_space_sizing(target, sizing)
    }
}

#[allow(dead_code)] // 伴随尚未接入 UI 的布局入口。
#[derive(Debug, PartialEq, Eq)]
pub(super) enum LayoutError {
    MissingContent(ContentId),
    WouldRemoveLastFocusable(SpaceId),
    NoFocusableSpace,
    Scene(SceneError),
}

impl From<SceneError> for LayoutError {
    fn from(error: SceneError) -> Self {
        Self::Scene(error)
    }
}

pub(super) fn create_view(
    content: ContentId,
    contents: &ContentStore,
    modes: &ModeRegistry,
) -> Option<View> {
    if !contents.contains(content) {
        return None;
    }
    let state = contents
        .create_view_state(content)
        .expect("existing content creates view state");
    let mode = contents.default_mode(content).map(|name| {
        modes
            .instantiate(&name)
            .expect("content default mode must be registered")
    });
    Some(View::new(content, state, mode))
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

pub(super) fn view_space_focusable(scene: &Scene, space: SpaceId) -> Option<bool> {
    if !scene.contains(space) {
        return None;
    }
    match &scene.node(space).space.kind {
        SpaceKind::Content { focusable, .. } => Some(*focusable),
        SpaceKind::Container { .. } => None,
    }
}

#[allow(dead_code)] // 由尚未接入 UI 的 close/replace 预检使用。
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
        .filter(|space| view_space_focusable(scene, *space) == Some(true))
        .or_else(|| (view_space_focusable(scene, previous) == Some(true)).then_some(previous))
        .or_else(|| {
            scene_views(scene)
                .into_iter()
                .map(|(space, _)| space)
                .find(|space| view_space_focusable(scene, *space) == Some(true))
        })
}
