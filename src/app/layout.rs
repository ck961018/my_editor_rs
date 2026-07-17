use std::collections::HashMap;

use crate::app::application::App;
use crate::app::dispatcher::{Dispatcher, default_global_keymap};
use crate::app::scene_model::{
    CloseResult, SceneBuilder, SceneError, SplitResult, build_editor_scene,
};
use crate::app::session::ClientSession;
use crate::app::view::View;
use crate::core::content_store::ContentStore;
use crate::core::mode::ModeRegistry;
use crate::frontend::Frontend;
use crate::protocol::ids::{ContentId, SpaceId, ViewId};
use crate::protocol::scene::Scene;
use crate::protocol::space::{Sizing, SpaceKind, SplitDirection};

impl<F: Frontend> App<F> {
    fn insert_view(&mut self, content: ContentId) -> ViewId {
        let id = ViewId(self.session.next_view_id);
        self.session.next_view_id = self
            .session
            .next_view_id
            .checked_add(1)
            .expect("view id overflow");
        let view = create_view(content, &self.kernel.contents, &self.kernel.modes);
        assert!(
            self.session.views.insert(id, view).is_none(),
            "view id must be unique"
        );
        id
    }

    #[allow(dead_code)] // 本轮只提供后端入口，不接入按键或 UI。
    pub(super) fn split_space(
        &mut self,
        target: SpaceId,
        content: ContentId,
        focusable: bool,
        direction: SplitDirection,
        focus_new: bool,
    ) -> Result<SplitResult, LayoutError> {
        if !self.kernel.contents.contains(content) {
            return Err(LayoutError::MissingContent(content));
        }

        let previous = self.session.focused;
        let previous_view =
            view_for_space(&self.session.scene, previous).expect("focused space hosts a view");
        let view = self.insert_view(content);
        let result = match self.session.scene_builder.split(
            &mut self.session.scene,
            target,
            view,
            focusable,
            direction,
        ) {
            Ok(result) => result,
            Err(error) => {
                self.session.views.remove(&view);
                self.session.next_view_id = view.0;
                return Err(error.into());
            }
        };
        if focus_new {
            self.session
                .dispatcher
                .invalidate_view(previous_view, &mut self.session.views);
        }
        self.reconcile_layout(if focus_new {
            Some(result.new_space)
        } else {
            Some(previous)
        });
        self.session.scene_revision.next();
        Ok(result)
    }

    #[allow(dead_code)] // 本轮只提供后端入口，不接入按键或 UI。
    pub(super) fn close_space(&mut self, target: SpaceId) -> Result<CloseResult, LayoutError> {
        if view_space_focusable(&self.session.scene, target) == Some(true)
            && focusable_view_count(&self.session.scene) == 1
        {
            return Err(LayoutError::WouldRemoveLastFocusable(target));
        }

        let removed_view = view_for_space(&self.session.scene, target)
            .ok_or(SceneError::ExpectedContentLeaf(target))?;
        let result = self
            .session
            .scene_builder
            .close(&mut self.session.scene, target)?;
        self.session
            .dispatcher
            .invalidate_view(removed_view, &mut self.session.views);
        self.session.views.remove(&removed_view);
        self.reconcile_layout(result.surviving_neighbor);
        self.session.scene_revision.next();
        Ok(result)
    }

    #[allow(dead_code)] // 本轮只提供后端入口，不接入按键或 UI。
    pub(super) fn replace_space_content(
        &mut self,
        target: SpaceId,
        content: ContentId,
        focusable: bool,
    ) -> Result<(), LayoutError> {
        if !self.kernel.contents.contains(content) {
            return Err(LayoutError::MissingContent(content));
        }
        if view_space_focusable(&self.session.scene, target) == Some(true)
            && !focusable
            && focusable_view_count(&self.session.scene) == 1
        {
            return Err(LayoutError::NoFocusableSpace);
        }

        let old_view = view_for_space(&self.session.scene, target)
            .ok_or(SceneError::ExpectedContentLeaf(target))?;
        let new_view = self.insert_view(content);
        if let Err(error) = self.session.scene_builder.replace_view(
            &mut self.session.scene,
            target,
            new_view,
            focusable,
        ) {
            self.session.views.remove(&new_view);
            self.session.next_view_id = new_view.0;
            return Err(error.into());
        }
        self.session
            .dispatcher
            .invalidate_view(old_view, &mut self.session.views);
        self.session.views.remove(&old_view);
        self.reconcile_layout(Some(target));
        self.session.scene_revision.next();
        Ok(())
    }

    #[allow(dead_code)] // 本轮只提供后端入口，不接入按键或 UI。
    pub(super) fn set_space_sizing(
        &mut self,
        target: SpaceId,
        sizing: Sizing,
    ) -> Result<(), LayoutError> {
        self.session
            .scene_builder
            .set_sizing(&mut self.session.scene, target, sizing)?;
        self.session.scene_revision.next();
        Ok(())
    }

    #[allow(dead_code)] // 由预留布局入口统一调用。
    fn reconcile_layout(&mut self, preferred: Option<SpaceId>) {
        let previous = self.session.focused;
        debug_assert!(
            scene_views(&self.session.scene)
                .into_iter()
                .all(|(_, view)| self.session.views.contains_key(&view))
        );
        self.session.focused = resolve_focus(&self.session.scene, previous, preferred)
            .expect("App rejects layouts without focusable content spaces");
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

fn create_view(content: ContentId, contents: &ContentStore, modes: &ModeRegistry) -> View {
    let state = contents
        .create_view_state(content)
        .expect("view content exists");
    let mode = contents.default_mode(content).map(|name| {
        modes
            .instantiate(&name)
            .expect("content default mode must be registered")
    });
    View::new(content, state, mode)
}

pub(super) fn create_editor_session(
    contents: &ContentStore,
    modes: &ModeRegistry,
    width: usize,
    height: usize,
) -> ClientSession {
    let editor_view = ViewId(0);
    let status_view = ViewId(1);
    let mut views = HashMap::new();
    views.insert(editor_view, create_view(ContentId(0), contents, modes));
    views.insert(status_view, create_view(ContentId(1), contents, modes));
    let mut scene_builder = SceneBuilder::new();
    let (scene, editor_space) = build_editor_scene(
        &mut scene_builder,
        width as i32,
        height as i32,
        editor_view,
        status_view,
    )
    .expect("valid editor scene");
    let focused = resolve_focus(&scene, editor_space, Some(editor_space))
        .expect("initial scene has a focusable content space");
    ClientSession::new(
        scene,
        scene_builder,
        views,
        2,
        focused,
        Dispatcher::new(default_global_keymap()),
    )
}

fn collect_view_spaces(scene: &Scene, sid: SpaceId, out: &mut Vec<(SpaceId, ViewId)>) {
    let node = scene.node(sid);
    match &node.space.kind {
        SpaceKind::Content { view, .. } => {
            out.push((sid, *view));
        }
        SpaceKind::Container { .. } => {
            for c in &node.children {
                collect_view_spaces(scene, *c, out);
            }
        }
    }
}

fn scene_views(scene: &Scene) -> Vec<(SpaceId, ViewId)> {
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

fn view_space_focusable(scene: &Scene, space: SpaceId) -> Option<bool> {
    if !scene.contains(space) {
        return None;
    }
    match &scene.node(space).space.kind {
        SpaceKind::Content { focusable, .. } => Some(*focusable),
        SpaceKind::Container { .. } => None,
    }
}

#[allow(dead_code)] // 由尚未接入 UI 的 close/replace 预检使用。
fn focusable_view_count(scene: &Scene) -> usize {
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
