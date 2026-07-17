use std::collections::HashMap;
use std::time::Instant;

use crate::app::dispatcher::{
    DispatchCommand, DispatchInput, DispatchOutcome, Dispatcher, default_global_keymap,
};
use crate::app::layout::{
    LayoutError, create_view, focusable_view_count, resolve_focus, scene_views, view_for_space,
    view_space_focusable,
};
use crate::app::scene_model::{
    CloseResult, SceneBuilder, SceneError, SplitResult, build_editor_scene,
};
use crate::app::view::View;
use crate::core::command::Command;
use crate::core::content::ContentChange;
use crate::core::content_store::ContentStore;
use crate::core::mode::ModeRegistry;
use crate::protocol::ids::{ContentId, SpaceId, ViewId};
use crate::protocol::revision::Revision;
use crate::protocol::scene::Scene;
use crate::protocol::space::{Sizing, SplitDirection};

pub(super) struct ClientSession {
    scene: Scene,
    scene_builder: SceneBuilder,
    scene_revision: Revision,
    views: HashMap<ViewId, View>,
    next_view_id: u64,
    focused: SpaceId,
    dispatcher: Dispatcher,
}

impl ClientSession {
    pub(super) fn editor(
        contents: &ContentStore,
        modes: &ModeRegistry,
        width: usize,
        height: usize,
    ) -> Self {
        let editor_view = ViewId(0);
        let status_view = ViewId(1);
        let mut views = HashMap::new();
        views.insert(
            editor_view,
            create_view(ContentId(0), contents, modes).expect("editor content exists"),
        );
        views.insert(
            status_view,
            create_view(ContentId(1), contents, modes).expect("status content exists"),
        );
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
        Self {
            scene,
            scene_builder,
            scene_revision: Revision::default(),
            views,
            next_view_id: 2,
            focused,
            dispatcher: Dispatcher::new(default_global_keymap()),
        }
    }

    pub(super) fn scene(&self) -> &Scene {
        &self.scene
    }

    pub(super) fn scene_revision(&self) -> Revision {
        self.scene_revision
    }

    pub(super) fn focused(&self) -> SpaceId {
        self.focused
    }

    pub(super) fn views(&self) -> &HashMap<ViewId, View> {
        &self.views
    }

    pub(super) fn view_mut(&mut self, view: ViewId) -> Option<&mut View> {
        self.views.get_mut(&view)
    }

    pub(super) fn view_for_space(&self, space: SpaceId) -> Option<ViewId> {
        view_for_space(&self.scene, space)
    }

    pub(super) fn resize(&mut self, width: u16, height: u16) {
        self.scene.size.width = width as i32;
        self.scene.size.height = height as i32;
        self.scene_revision.next();
    }

    pub(super) fn next_input_deadline(&self) -> Option<Instant> {
        self.dispatcher.next_deadline(&self.views)
    }

    pub(super) fn dispatch(&mut self, input: DispatchInput, now: Instant) -> DispatchOutcome {
        self.dispatcher
            .dispatch(input, now, self.focused, &self.scene, &mut self.views)
    }

    pub(super) fn dispatch_timeout(&mut self, now: Instant) -> DispatchOutcome {
        self.dispatcher
            .dispatch_timeout(now, self.focused, &self.scene, &mut self.views)
    }

    pub(super) fn resolve_from_view(
        &self,
        command: Command,
        view: ViewId,
    ) -> Option<DispatchCommand> {
        self.dispatcher
            .resolve_from_view(command, view, &self.views)
    }

    pub(super) fn sync_focused_input(&mut self, now: Instant) {
        let Some(view_id) = self.view_for_space(self.focused) else {
            return;
        };
        let status = self
            .views
            .get(&view_id)
            .map_or(crate::core::input::InputStatus::Ready, View::input_status);
        self.dispatcher.sync_view(view_id, status, true, now);
    }

    pub(super) fn transform_content_views(
        &mut self,
        contents: &ContentStore,
        content: ContentId,
        except: Option<ViewId>,
        change: &ContentChange,
    ) {
        for (view_id, view) in &mut self.views {
            if Some(*view_id) == except || view.content() != content {
                continue;
            }
            if contents
                .transform_view_state(content, view.state_mut(), change)
                .expect("view content exists")
            {
                view.touch();
            }
        }
    }

    #[allow(dead_code)] // 本轮只提供后端入口，不接入按键或 UI。
    pub(super) fn split_space(
        &mut self,
        target: SpaceId,
        view: View,
        focusable: bool,
        direction: SplitDirection,
        focus_new: bool,
    ) -> Result<SplitResult, LayoutError> {
        let previous = self.focused;
        let previous_view = self
            .view_for_space(previous)
            .expect("focused space hosts a view");
        let view = self.insert_view(view);
        let result =
            match self
                .scene_builder
                .split(&mut self.scene, target, view, focusable, direction)
            {
                Ok(result) => result,
                Err(error) => {
                    self.views.remove(&view);
                    self.next_view_id = view.0;
                    return Err(error.into());
                }
            };
        if focus_new {
            self.dispatcher
                .invalidate_view(previous_view, &mut self.views);
        }
        self.reconcile_layout(if focus_new {
            Some(result.new_space)
        } else {
            Some(previous)
        });
        self.scene_revision.next();
        Ok(result)
    }

    #[allow(dead_code)] // 本轮只提供后端入口，不接入按键或 UI。
    pub(super) fn close_space(&mut self, target: SpaceId) -> Result<CloseResult, LayoutError> {
        if view_space_focusable(&self.scene, target) == Some(true)
            && focusable_view_count(&self.scene) == 1
        {
            return Err(LayoutError::WouldRemoveLastFocusable(target));
        }

        let removed_view = self
            .view_for_space(target)
            .ok_or(SceneError::ExpectedContentLeaf(target))?;
        let result = self.scene_builder.close(&mut self.scene, target)?;
        self.dispatcher
            .invalidate_view(removed_view, &mut self.views);
        self.views.remove(&removed_view);
        self.reconcile_layout(result.surviving_neighbor);
        self.scene_revision.next();
        Ok(result)
    }

    #[allow(dead_code)] // 本轮只提供后端入口，不接入按键或 UI。
    pub(super) fn replace_space_content(
        &mut self,
        target: SpaceId,
        view: View,
        focusable: bool,
    ) -> Result<(), LayoutError> {
        if view_space_focusable(&self.scene, target) == Some(true)
            && !focusable
            && focusable_view_count(&self.scene) == 1
        {
            return Err(LayoutError::NoFocusableSpace);
        }

        let old_view = self
            .view_for_space(target)
            .ok_or(SceneError::ExpectedContentLeaf(target))?;
        let new_view = self.insert_view(view);
        if let Err(error) =
            self.scene_builder
                .replace_view(&mut self.scene, target, new_view, focusable)
        {
            self.views.remove(&new_view);
            self.next_view_id = new_view.0;
            return Err(error.into());
        }
        self.dispatcher.invalidate_view(old_view, &mut self.views);
        self.views.remove(&old_view);
        self.reconcile_layout(Some(target));
        self.scene_revision.next();
        Ok(())
    }

    #[allow(dead_code)] // 本轮只提供后端入口，不接入按键或 UI。
    pub(super) fn set_space_sizing(
        &mut self,
        target: SpaceId,
        sizing: Sizing,
    ) -> Result<(), LayoutError> {
        self.scene_builder
            .set_sizing(&mut self.scene, target, sizing)?;
        self.scene_revision.next();
        Ok(())
    }

    fn insert_view(&mut self, view: View) -> ViewId {
        let id = ViewId(self.next_view_id);
        self.next_view_id = self.next_view_id.checked_add(1).expect("view id overflow");
        assert!(
            self.views.insert(id, view).is_none(),
            "view id must be unique"
        );
        id
    }

    fn reconcile_layout(&mut self, preferred: Option<SpaceId>) {
        let previous = self.focused;
        debug_assert!(
            scene_views(&self.scene)
                .into_iter()
                .all(|(_, view)| self.views.contains_key(&view))
        );
        self.focused = resolve_focus(&self.scene, previous, preferred)
            .expect("ClientSession rejects layouts without focusable content spaces");
    }

    #[cfg(test)]
    pub(super) fn replace_dispatcher_for_test(&mut self, dispatcher: Dispatcher) {
        self.dispatcher = dispatcher;
    }

    #[cfg(test)]
    pub(super) fn next_view_id_for_test(&self) -> u64 {
        self.next_view_id
    }
}
