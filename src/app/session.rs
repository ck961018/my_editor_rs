use std::collections::HashMap;
use std::time::Instant;

use crate::app::action::ViewAction;
use crate::app::command::ModeCommand;
use crate::app::command_resolver::default_global_keymap;
use crate::app::dispatcher::{DispatchInput, DispatchOutcome, Dispatcher, DispatcherInputSnapshot};
use crate::app::layout::{
    LayoutError, NewView, create_view, focusable_view_count, resolve_focus, scene_views,
    view_for_space, view_space_focusable,
};
use crate::app::mode::{
    CursorDomain, FaceRegistry, ModeContentContext, ModeContentStore, ModeDraftJournal, ModeError,
    ModeRegistry, ModeResult, ModeViewContext, ModeViewStore,
};
use crate::app::presentation::{PresentationLayerStore, PresentationRefresh};
use crate::app::scene_model::{
    CloseResult, SceneBuilder, SceneError, SplitResult, build_editor_scene,
};
use crate::app::view::View;
use crate::core::content::ContentChange;
use crate::core::content_store::ContentStore;
use crate::protocol::content_query::RowRange;
use crate::protocol::ids::{ContentId, SpaceId, ViewId};
use crate::protocol::revision::Revision;
use crate::protocol::scene::Scene;
use crate::protocol::space::{Sizing, SplitDirection};

pub(super) struct ClientSession {
    scene: Scene,
    scene_builder: SceneBuilder,
    scene_revision: Revision,
    views: HashMap<ViewId, View>,
    view_modes: ModeViewStore,
    faces: FaceRegistry,
    presentation: PresentationLayerStore,
    next_view_id: u64,
    focused: SpaceId,
    dispatcher: Dispatcher,
}

pub(super) struct InitialView {
    pub view: ViewId,
    pub content: ContentId,
    pub modes: Vec<crate::app::mode_name::ModeName>,
}

pub(super) struct EditorSessionInit {
    pub editor: InitialView,
    pub status: InitialView,
    pub next_view_id: u64,
}

impl ClientSession {
    pub(super) fn editor(
        contents: &ContentStore,
        modes: &ModeRegistry,
        mode_contents: &mut ModeContentStore,
        width: usize,
        height: usize,
        init: EditorSessionInit,
    ) -> Self {
        let editor = create_view(init.editor.content, contents, modes, &init.editor.modes)
            .expect("editor content exists");
        let status = create_view(init.status.content, contents, modes, &init.status.modes)
            .expect("status content exists");
        let mut views = HashMap::new();
        let mut view_modes = ModeViewStore::default();
        let mut faces = FaceRegistry::default();
        let editor_content = editor.view.content();
        views.insert(init.editor.view, editor.view);
        for mut mode in editor.modes {
            mode.register_faces(&mut faces);
            let content_context = ModeContentContext::new(editor_content, contents);
            let view_context =
                ModeViewContext::new(init.editor.view, &views[&init.editor.view], contents);
            mode_contents.attach_view_with_context(
                editor_content,
                &mut mode,
                &content_context,
                &view_context,
            );
            view_modes.insert(init.editor.view, mode);
        }
        let status_content = status.view.content();
        views.insert(init.status.view, status.view);
        for mut mode in status.modes {
            mode.register_faces(&mut faces);
            let content_context = ModeContentContext::new(status_content, contents);
            let view_context =
                ModeViewContext::new(init.status.view, &views[&init.status.view], contents);
            mode_contents.attach_view_with_context(
                status_content,
                &mut mode,
                &content_context,
                &view_context,
            );
            view_modes.insert(init.status.view, mode);
        }
        let mut scene_builder = SceneBuilder::new();
        let (scene, editor_space) = build_editor_scene(
            &mut scene_builder,
            width as i32,
            height as i32,
            init.editor.view,
            init.status.view,
        )
        .expect("valid editor scene");
        let focused = resolve_focus(&scene, editor_space, Some(editor_space))
            .expect("initial scene has a focusable content space");
        let mut session = Self {
            scene,
            scene_builder,
            scene_revision: Revision::default(),
            views,
            view_modes,
            faces,
            presentation: PresentationLayerStore::default(),
            next_view_id: init.next_view_id,
            focused,
            dispatcher: Dispatcher::new(default_global_keymap()),
        };
        session.refresh_presentation(contents, mode_contents);
        session
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

    pub(super) fn view_modes(&self) -> &ModeViewStore {
        &self.view_modes
    }

    pub(super) fn commit_mode_drafts(&mut self, drafts: &mut ModeDraftJournal) {
        drafts.commit_views(&mut self.view_modes);
    }

    pub(super) fn commit_view_touches(&mut self, touches: HashMap<ViewId, Revision>) {
        for (view, revision_before) in touches {
            let target = self
                .views
                .get_mut(&view)
                .expect("touched view still exists");
            if target.revision() == revision_before {
                target.touch();
            }
        }
    }

    pub(super) fn faces(&self) -> &FaceRegistry {
        &self.faces
    }

    pub(super) fn presentation(&self) -> &PresentationLayerStore {
        &self.presentation
    }

    pub(super) fn refresh_presentation(
        &mut self,
        contents: &ContentStore,
        mode_contents: &ModeContentStore,
    ) {
        let mut refresh = PresentationRefresh::new();
        let visible_rows = RowRange {
            start: 0,
            end: usize::MAX,
        };
        for (&view, view_data) in &self.views {
            let content = view_data.content();
            refresh.view_contents.insert(view, content);
            let order = self.view_modes.mode_ids(view).to_vec();
            refresh.view_order.insert(view, order.clone());
            for mode in order {
                if refresh.needs_content(mode, content)
                    && let Some(layer) =
                        mode_contents.presentation_layer(mode, content, contents, visible_rows)
                {
                    refresh.content_layers.insert((mode, content), layer);
                }
                let context = ModeViewContext::new(view, view_data, contents);
                if let Some(layer) = self.view_modes.presentation_layer(
                    mode,
                    view,
                    &context,
                    mode_contents,
                    view_data.revision(),
                    visible_rows,
                ) {
                    refresh.view_layers.insert((mode, view), layer);
                }
            }
        }
        self.presentation.replace(
            refresh.content_layers,
            refresh.view_layers,
            refresh.view_contents,
            refresh.view_order,
        );
    }

    pub(super) fn snapshot_input(&self) -> DispatcherInputSnapshot {
        self.dispatcher.snapshot_input()
    }

    pub(super) fn restore_input(&mut self, snapshot: DispatcherInputSnapshot) {
        self.dispatcher.restore_input(snapshot);
    }

    #[cfg(test)]
    pub(super) fn view_modes_mut_for_test(&mut self) -> &mut ModeViewStore {
        &mut self.view_modes
    }

    #[cfg(test)]
    pub(super) fn input_is_pending_for_test(&self) -> bool {
        self.dispatcher.is_pending()
    }

    #[cfg(test)]
    pub(super) fn view_mut(&mut self, view: ViewId) -> Option<&mut View> {
        self.views.get_mut(&view)
    }

    pub(super) fn view(&self, view: ViewId) -> Option<&View> {
        self.views.get(&view)
    }

    pub(super) fn touch_content_views(&mut self, content: ContentId) {
        for view in self.views.values_mut() {
            if view.content() == content {
                view.touch();
            }
        }
    }

    pub(super) fn cursor_domain_in_draft(
        &self,
        view: ViewId,
        mode_contents: &ModeContentStore,
        contents: &ContentStore,
        drafts: &ModeDraftJournal,
    ) -> CursorDomain {
        let view_data = self.views.get(&view).expect("target view exists");
        let context = crate::app::mode::ModeViewContext::new(view, view_data, contents);
        self.view_modes
            .view_policy_in_draft(view, &context, mode_contents, drafts)
            .cursor_domain
            .unwrap_or(CursorDomain::InsertionPoint)
    }

    pub(super) fn apply_view_action(
        &mut self,
        view: ViewId,
        action: ViewAction,
        contents: &ContentStore,
    ) -> Option<bool> {
        let view = self.views.get_mut(&view)?;
        match action {
            ViewAction::SetSelections(selections) => contents
                .selections_are_valid(view.content(), &selections)
                .filter(|valid| *valid)
                .map(|_| view.set_selections(selections)),
        }
    }

    pub(super) fn snapshot_selections(
        &self,
        content: ContentId,
    ) -> HashMap<
        ViewId,
        (
            crate::protocol::selection::Selections,
            crate::protocol::revision::Revision,
        ),
    > {
        self.views
            .iter()
            .filter(|(_, view)| view.content() == content)
            .filter_map(|(id, view)| {
                view.selections()
                    .cloned()
                    .map(|selections| (*id, (selections, view.revision())))
            })
            .collect()
    }

    pub(super) fn restore_selections(
        &mut self,
        snapshot: HashMap<
            ViewId,
            (
                crate::protocol::selection::Selections,
                crate::protocol::revision::Revision,
            ),
        >,
    ) {
        for (id, (selections, revision)) in snapshot {
            if let Some(view) = self.views.get_mut(&id) {
                view.restore_selections_and_revision(selections, revision);
            }
        }
    }

    pub(super) fn execute_mode(
        &mut self,
        view: ViewId,
        registry: &ModeRegistry,
        contents: &ContentStore,
        command: &ModeCommand,
        mode_contents: &mut ModeContentStore,
        drafts: &mut ModeDraftJournal,
    ) -> Result<ModeResult, ModeError> {
        let view_data = self.views.get(&view).expect("target view exists");
        let context = crate::app::mode::ModeViewContext::new(view, view_data, contents);
        self.view_modes.execute_with_context(
            view,
            registry,
            command,
            &context,
            mode_contents,
            drafts,
        )
    }

    pub(super) fn view_for_space(&self, space: SpaceId) -> Option<ViewId> {
        view_for_space(&self.scene, space)
    }

    pub(super) fn resize(&mut self, width: u16, height: u16) {
        self.scene.size.width = width as i32;
        self.scene.size.height = height as i32;
        self.scene_revision.next();
    }

    pub(super) fn next_input_deadline(
        &self,
        content_modes: &ModeContentStore,
        contents: &ContentStore,
    ) -> Option<Instant> {
        self.dispatcher
            .next_deadline(&self.views, &self.view_modes, content_modes, contents)
    }

    pub(super) fn dispatch(
        &mut self,
        input: DispatchInput,
        now: Instant,
        content_modes: &mut ModeContentStore,
        contents: &ContentStore,
        drafts: &mut ModeDraftJournal,
    ) -> (DispatchOutcome, Vec<(ViewId, Revision)>) {
        let outcome = self.dispatcher.dispatch_in_draft(
            input,
            now,
            self.focused,
            &self.scene,
            &self.views,
            &mut self.view_modes,
            content_modes,
            contents,
            drafts,
        );
        (outcome, self.dispatcher.take_view_mode_revisions())
    }

    pub(super) fn dispatch_timeout(
        &mut self,
        now: Instant,
        content_modes: &mut ModeContentStore,
        contents: &ContentStore,
        drafts: &mut ModeDraftJournal,
    ) -> (DispatchOutcome, Vec<(ViewId, Revision)>) {
        let outcome = self.dispatcher.dispatch_timeout_in_draft(
            now,
            self.focused,
            &self.scene,
            &self.views,
            &mut self.view_modes,
            content_modes,
            contents,
            drafts,
        );
        (outcome, self.dispatcher.take_view_mode_revisions())
    }

    pub(super) fn sync_focused_input_in_draft(
        &mut self,
        now: Instant,
        content_modes: &ModeContentStore,
        contents: &ContentStore,
        drafts: &ModeDraftJournal,
    ) {
        let Some(view_id) = self.view_for_space(self.focused) else {
            return;
        };
        let context =
            crate::app::mode::ModeViewContext::new(view_id, &self.views[&view_id], contents);
        for index in 0..self.view_modes.mode_ids(view_id).len() {
            let status = self
                .view_modes
                .status_at(view_id, index, &context, content_modes, drafts);
            self.dispatcher.sync_mode(view_id, index, status, true, now);
        }
    }

    pub(super) fn sync_focused_input(
        &mut self,
        now: Instant,
        content_modes: &ModeContentStore,
        contents: &ContentStore,
    ) {
        self.sync_focused_input_in_draft(
            now,
            content_modes,
            contents,
            &ModeDraftJournal::default(),
        );
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

    pub(super) fn notify_mode_content_changed(
        &mut self,
        content: ContentId,
        mode_contents: &mut ModeContentStore,
        contents: &ContentStore,
        change: &ContentChange,
        drafts: &mut ModeDraftJournal,
    ) {
        self.view_modes.notify_changed(
            &self.views,
            content,
            mode_contents,
            contents,
            change,
            drafts,
        );
    }

    pub(super) fn attach_mode_to_content_views(
        &mut self,
        content: ContentId,
        name: &crate::app::mode_name::ModeName,
        registry: &ModeRegistry,
        mode_contents: &mut ModeContentStore,
        contents: &ContentStore,
    ) -> bool {
        let views: Vec<_> = self
            .views
            .iter()
            .filter_map(|(view, data)| (data.content() == content).then_some(*view))
            .collect();
        if views.is_empty() || registry.resolve_mode(name).is_none() {
            return false;
        }
        for view in views {
            if self.view_modes.contains(view, name) {
                continue;
            }
            let mut mode = registry
                .instantiate(name)
                .expect("resolved mode can create view state");
            mode.register_faces(&mut self.faces);
            let content_context = ModeContentContext::new(content, contents);
            let view_context = ModeViewContext::new(view, &self.views[&view], contents);
            mode_contents.attach_view_with_context(
                content,
                &mut mode,
                &content_context,
                &view_context,
            );
            self.view_modes.insert(view, mode);
            self.dispatcher.invalidate_mode_chain(view);
            self.views
                .get_mut(&view)
                .expect("mode owner exists")
                .touch();
        }
        true
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "session mutation receives split app-owned stores"
    )]
    pub(super) fn split_space(
        &mut self,
        target: SpaceId,
        view: NewView,
        focusable: bool,
        direction: SplitDirection,
        focus_new: bool,
        content_modes: &mut ModeContentStore,
        contents: &ContentStore,
    ) -> Result<SplitResult, LayoutError> {
        let previous = self.focused;
        let previous_view = self
            .view_for_space(previous)
            .expect("focused space hosts a view");
        let previous_content = self.views[&previous_view].content();
        let view = self.insert_view(view, content_modes, contents);
        let result =
            match self
                .scene_builder
                .split(&mut self.scene, target, view, focusable, direction)
            {
                Ok(result) => result,
                Err(error) => {
                    self.remove_view(view, content_modes);
                    self.next_view_id = view.0;
                    return Err(error.into());
                }
            };
        if focus_new {
            let view_data = &self.views[&previous_view];
            let presentation_changed = self.dispatcher.invalidate_view(
                previous_view,
                view_data,
                previous_content,
                &mut self.view_modes,
                content_modes,
                contents,
            );
            if presentation_changed {
                self.views
                    .get_mut(&previous_view)
                    .expect("previous view still exists")
                    .touch();
            }
        }
        self.reconcile_layout(if focus_new {
            Some(result.new_space)
        } else {
            Some(previous)
        });
        if focus_new {
            self.sync_changed_input_source(previous_content, content_modes, contents);
        }
        self.scene_revision.next();
        Ok(result)
    }

    pub(super) fn close_space(
        &mut self,
        target: SpaceId,
        content_modes: &mut ModeContentStore,
        contents: &ContentStore,
    ) -> Result<CloseResult, LayoutError> {
        if view_space_focusable(&self.scene, target) == Some(true)
            && focusable_view_count(&self.scene) == 1
        {
            return Err(LayoutError::WouldRemoveLastFocusable(target));
        }

        let removed_view = self
            .view_for_space(target)
            .ok_or(SceneError::ExpectedContentLeaf(target))?;
        let input_source_changed = target == self.focused;
        let result = self.scene_builder.close(&mut self.scene, target)?;
        let content = self.views[&removed_view].content();
        let view_data = &self.views[&removed_view];
        self.dispatcher.invalidate_view(
            removed_view,
            view_data,
            content,
            &mut self.view_modes,
            content_modes,
            contents,
        );
        self.remove_view(removed_view, content_modes);
        self.reconcile_layout(result.surviving_neighbor);
        if input_source_changed {
            self.sync_changed_input_source(content, content_modes, contents);
        }
        self.scene_revision.next();
        Ok(result)
    }

    pub(super) fn replace_space_content(
        &mut self,
        target: SpaceId,
        view: NewView,
        focusable: bool,
        content_modes: &mut ModeContentStore,
        contents: &ContentStore,
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
        let input_source_changed = target == self.focused;
        let new_view = self.insert_view(view, content_modes, contents);
        if let Err(error) =
            self.scene_builder
                .replace_view(&mut self.scene, target, new_view, focusable)
        {
            self.remove_view(new_view, content_modes);
            self.next_view_id = new_view.0;
            return Err(error.into());
        }
        let content = self.views[&old_view].content();
        let view_data = &self.views[&old_view];
        self.dispatcher.invalidate_view(
            old_view,
            view_data,
            content,
            &mut self.view_modes,
            content_modes,
            contents,
        );
        self.remove_view(old_view, content_modes);
        self.reconcile_layout(Some(target));
        if input_source_changed {
            self.sync_changed_input_source(content, content_modes, contents);
        }
        self.scene_revision.next();
        Ok(())
    }

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

    fn sync_changed_input_source(
        &mut self,
        previous_content: ContentId,
        content_modes: &mut ModeContentStore,
        contents: &ContentStore,
    ) {
        let _ = previous_content;
        self.sync_focused_input(Instant::now(), content_modes, contents);
    }

    fn insert_view(
        &mut self,
        view: NewView,
        mode_contents: &mut ModeContentStore,
        contents: &ContentStore,
    ) -> ViewId {
        let id = ViewId(self.next_view_id);
        self.next_view_id = self.next_view_id.checked_add(1).expect("view id overflow");
        let content = view.view.content();
        assert!(
            self.views.insert(id, view.view).is_none(),
            "view id must be unique"
        );
        for mut mode in view.modes {
            mode.register_faces(&mut self.faces);
            let content_context = ModeContentContext::new(content, contents);
            let view_context = ModeViewContext::new(id, &self.views[&id], contents);
            mode_contents.attach_view_with_context(
                content,
                &mut mode,
                &content_context,
                &view_context,
            );
            self.view_modes.insert(id, mode);
        }
        id
    }

    fn remove_view(&mut self, view: ViewId, mode_contents: &mut ModeContentStore) {
        let content = self
            .views
            .remove(&view)
            .expect("removed view exists")
            .content();
        for mode in self.view_modes.remove(view) {
            mode_contents.detach_view(content, mode);
        }
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
