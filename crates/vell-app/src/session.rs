use std::collections::{HashMap, HashSet};
use std::time::Instant;

use crate::action::ViewAction;
use crate::command::ModeCommand;
use crate::command_resolver::default_global_keymap;
use crate::dispatcher::{DispatchInput, DispatchOutcome, Dispatcher, DispatcherInputSnapshot};
use crate::layout::{
    LayoutError, NewView, StatusBarHandle, StatusBarPlacement, create_view, focusable_view_count,
    resolve_focus, scene_views, space_for_view, view_for_space, view_space_focusable,
};
use crate::mode::{
    CursorDomain, FaceRegistry, ModeAttachmentError, ModeContentContext, ModeContentStore,
    ModeDraftJournal, ModeError, ModeRegistry, ModeResult, ModeViewContext, ModeViewStore,
};
use crate::presentation::PresentationLayerStore;
use crate::scene_model::{CloseResult, SceneBuilder, SceneError, SplitResult, build_editor_scene};
use crate::view::View;
use vell_core::content::ContentChange;
use vell_core::content_store::ContentStore;
use vell_core::content_view_state::{ContentViewState, ContentViewStateError};
use vell_protocol::content_query::RowRange;
use vell_protocol::ids::{ContentId, SpaceId, ViewId};
use vell_protocol::revision::Revision;
use vell_protocol::scene::Scene;
use vell_protocol::space::{Sizing, SplitDirection};

pub(super) struct ClientSession {
    scene: Scene,
    scene_builder: SceneBuilder,
    scene_revision: Revision,
    views: HashMap<ViewId, View>,
    mode_profiles: HashMap<ContentId, Vec<crate::mode_name::ModeName>>,
    view_modes: ModeViewStore,
    faces: FaceRegistry,
    presentation: PresentationLayerStore,
    next_view_id: u64,
    focused: SpaceId,
    dispatcher: Dispatcher,
    status_content: ContentId,
    status_placement: StatusBarPlacement,
    global_status_view: Option<ViewId>,
    status_by_editor: HashMap<ViewId, ViewId>,
}

pub(super) struct InitialView {
    pub view: ViewId,
    pub content: ContentId,
    pub modes: Vec<crate::mode_name::ModeName>,
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
        let editor = create_view(init.editor.content, contents, &init.editor.modes)
            .expect("editor content exists");
        let mut status = create_view(init.status.content, contents, &init.status.modes)
            .expect("status content exists");
        *status.view.state_mut() = vell_core::content_view_state::ContentViewState::status_bar(
            init.editor.view,
            init.editor.content,
        );
        let mode_profiles = HashMap::from([
            (init.editor.content, init.editor.modes),
            (init.status.content, init.status.modes),
        ]);
        let mut views = HashMap::new();
        let mut view_modes = ModeViewStore::default();
        let mut faces = FaceRegistry::default();
        let editor_content = editor.view.content();
        views.insert(init.editor.view, editor.view);
        for name in editor.mode_names {
            let content_context = ModeContentContext::new(editor_content, contents);
            let view_data = &views[&init.editor.view];
            let view_context = ModeViewContext::new(
                init.editor.view,
                view_data.content(),
                view_data.state(),
                contents,
            )
            .expect("editor view state matches editor content");
            let mode = modes
                .instantiate_with_context(
                    &name,
                    editor_content,
                    contents
                        .kind(editor_content)
                        .expect("editor content exists"),
                    mode_contents,
                    &content_context,
                    &view_context,
                )
                .expect("initial mode must be registered");
            mode.register_faces(&mut faces);
            view_modes.insert(init.editor.view, mode);
        }
        let status_content = status.view.content();
        views.insert(init.status.view, status.view);
        for name in status.mode_names {
            let content_context = ModeContentContext::new(status_content, contents);
            let view_data = &views[&init.status.view];
            let view_context = ModeViewContext::new(
                init.status.view,
                view_data.content(),
                view_data.state(),
                contents,
            )
            .expect("status view state matches status content");
            let mode = modes
                .instantiate_with_context(
                    &name,
                    status_content,
                    contents
                        .kind(status_content)
                        .expect("status content exists"),
                    mode_contents,
                    &content_context,
                    &view_context,
                )
                .expect("initial mode must be registered");
            mode.register_faces(&mut faces);
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
            mode_profiles,
            view_modes,
            faces,
            presentation: PresentationLayerStore::default(),
            next_view_id: init.next_view_id,
            focused,
            dispatcher: Dispatcher::new(default_global_keymap()),
            status_content: init.status.content,
            status_placement: StatusBarPlacement::Global,
            global_status_view: Some(init.status.view),
            status_by_editor: HashMap::new(),
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

    pub(super) fn status_bar_placement(&self) -> StatusBarPlacement {
        self.status_placement
    }

    pub(super) fn status_bar_for_view(&self, editor: ViewId) -> Option<StatusBarHandle> {
        let editor_view = self.views.get(&editor)?;
        if editor_view.state().status_bar_state().is_some() {
            return None;
        }
        let view = match self.status_placement {
            StatusBarPlacement::Global => self.global_status_view?,
            StatusBarPlacement::PerPane => *self.status_by_editor.get(&editor)?,
        };
        let target_view = self
            .views
            .get(&view)?
            .state()
            .status_bar_state()?
            .target()?
            .0;
        Some(StatusBarHandle {
            view,
            content: self.status_content,
            target_view,
        })
    }

    pub(super) fn status_bars_for_content(&self, content: ContentId) -> Vec<StatusBarHandle> {
        let mut bars = self
            .views
            .iter()
            .filter_map(|(view, data)| {
                (data.content() == content && data.state().status_bar_state().is_none())
                    .then(|| self.status_bar_for_view(*view))
                    .flatten()
            })
            .collect::<Vec<_>>();
        bars.sort_by_key(|bar| bar.view.0);
        bars.dedup_by_key(|bar| bar.view);
        bars
    }

    fn status_view_for_target(
        &self,
        target_view: ViewId,
        target_content: ContentId,
        contents: &ContentStore,
    ) -> NewView {
        let modes = self.mode_chain_for_new_view(self.status_content);
        let mut view =
            create_view(self.status_content, contents, &modes).expect("status-bar content exists");
        *view.view.state_mut() = ContentViewState::status_bar(target_view, target_content);
        view
    }

    fn retarget_status_view(&mut self, status: ViewId, editor: ViewId) {
        let target_content = self.views[&editor].content();
        let view = self.views.get_mut(&status).expect("status view exists");
        let changed = view
            .state_mut()
            .status_bar_state_mut()
            .expect("status view has status state")
            .set_target(editor, target_content);
        if changed {
            view.touch();
        }
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "placement conversion coordinates session-owned scene and mode stores"
    )]
    pub(super) fn set_status_bar_placement(
        &mut self,
        placement: StatusBarPlacement,
        registry: &ModeRegistry,
        content_modes: &mut ModeContentStore,
        contents: &ContentStore,
    ) -> Result<(), LayoutError> {
        if placement == self.status_placement {
            return Ok(());
        }
        match placement {
            StatusBarPlacement::PerPane => {
                let global = self.global_status_view.expect("global status view exists");
                let global_space = space_for_view(&self.scene, global)
                    .expect("global status view belongs to scene");
                self.scene_builder.close(&mut self.scene, global_space)?;
                let editors = scene_views(&self.scene)
                    .into_iter()
                    .filter(|(_, view)| self.views[view].state().status_bar_state().is_none())
                    .collect::<Vec<_>>();
                self.status_by_editor.clear();
                for (index, (editor_space, editor_view)) in editors.into_iter().enumerate() {
                    let status_view = if index == 0 {
                        global
                    } else {
                        let target_content = self.views[&editor_view].content();
                        let status =
                            self.status_view_for_target(editor_view, target_content, contents);
                        self.insert_view(status, registry, content_modes, contents)?
                    };
                    self.retarget_status_view(status_view, editor_view);
                    self.scene_builder.wrap_with_status(
                        &mut self.scene,
                        editor_space,
                        status_view,
                    )?;
                    self.status_by_editor.insert(editor_view, status_view);
                }
                self.global_status_view = None;
            }
            StatusBarPlacement::Global => {
                let focused_editor = self
                    .view_for_space(self.focused)
                    .expect("focused space hosts editor view");
                let chosen = self.status_by_editor[&focused_editor];
                let bars = self.status_by_editor.values().copied().collect::<Vec<_>>();
                for status in &bars {
                    let space = space_for_view(&self.scene, *status)
                        .expect("per-pane status view belongs to scene");
                    self.scene_builder.close(&mut self.scene, space)?;
                }
                for status in bars {
                    if status != chosen {
                        self.remove_view(status, content_modes);
                    }
                }
                self.status_by_editor.clear();
                self.retarget_status_view(chosen, focused_editor);
                self.scene_builder
                    .attach_global_status(&mut self.scene, chosen)?;
                self.global_status_view = Some(chosen);
            }
        }
        self.status_placement = placement;
        self.reconcile_layout(Some(self.focused));
        self.scene_revision.next();
        Ok(())
    }

    pub(super) fn set_status_bar_visible(
        &mut self,
        editor: Option<ViewId>,
        visible: bool,
    ) -> Result<(), LayoutError> {
        let status = match self.status_placement {
            StatusBarPlacement::Global => self.global_status_view,
            StatusBarPlacement::PerPane => {
                editor.and_then(|editor| self.status_by_editor.get(&editor).copied())
            }
        }
        .ok_or(LayoutError::NoStatusBar)?;
        let space = space_for_view(&self.scene, status).ok_or(LayoutError::NoStatusBar)?;
        self.scene_builder.set_sizing(
            &mut self.scene,
            space,
            if visible {
                Sizing::Fixed(1)
            } else {
                Sizing::Fixed(0)
            },
        )?;
        self.scene_revision.next();
        Ok(())
    }

    pub(super) fn view_modes(&self) -> &ModeViewStore {
        &self.view_modes
    }

    pub(super) fn view_modes_mut(&mut self) -> &mut ModeViewStore {
        &mut self.view_modes
    }

    pub(super) fn mode_chain_for_new_view(
        &self,
        content: ContentId,
    ) -> Vec<crate::mode_name::ModeName> {
        self.mode_profiles
            .get(&content)
            .cloned()
            .unwrap_or_default()
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
        let mut active_content = HashSet::new();
        let mut active_views = HashSet::new();
        let mut visited_content = HashSet::new();
        self.presentation.begin_refresh();
        for (&view, view_data) in &self.views {
            let content = view_data.content();
            let order = self.view_modes.mode_ids(view).to_vec();
            self.presentation.set_view(view, content, order.clone());
            let source_rows =
                contents
                    .text_snapshot(content)
                    .map_or(RowRange { start: 0, end: 0 }, |snapshot| RowRange {
                        start: 0,
                        end: snapshot.len_lines(),
                    });
            for mode in order {
                let content_key = (mode, content);
                if visited_content.insert(content_key)
                    && let (Some(source_revision), Some(mode_revision)) = (
                        contents.revision(content),
                        mode_contents.revision(mode, content),
                    )
                {
                    if self.presentation.content_is_current(
                        mode,
                        content,
                        source_revision,
                        mode_revision,
                    ) {
                        active_content.insert(content_key);
                    } else if let Some(layer) =
                        mode_contents.presentation_layer(mode, content, contents, source_rows)
                    {
                        self.presentation.set_content_layer(mode, content, layer);
                        active_content.insert(content_key);
                    }
                }
                let Ok(context) =
                    ModeViewContext::new(view, view_data.content(), view_data.state(), contents)
                else {
                    continue;
                };
                let view_key = (mode, view);
                if let (
                    Some(content_revision),
                    Some(content_mode_revision),
                    Some(view_mode_revision),
                ) = (
                    contents.revision(content),
                    mode_contents.revision(mode, content),
                    self.view_modes.revision(mode, view),
                ) {
                    if self.presentation.view_is_current(
                        mode,
                        view,
                        content_revision,
                        view_data.revision(),
                        content_mode_revision,
                        view_mode_revision,
                    ) {
                        active_views.insert(view_key);
                    } else if let Some(layer) = self.view_modes.presentation_layer(
                        mode,
                        view,
                        &context,
                        mode_contents,
                        view_data.revision(),
                        source_rows,
                    ) {
                        self.presentation.set_view_layer(mode, view, layer);
                        active_views.insert(view_key);
                    }
                }
            }
        }
        self.presentation
            .finish_refresh(&active_content, &active_views);
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
            if view_targets_content(view, content) {
                view.touch();
            }
        }
    }

    pub(super) fn content_view_revisions(&self, content: ContentId) -> Vec<(ViewId, Revision)> {
        self.views
            .iter()
            .filter_map(|(id, view)| {
                view_targets_content(view, content).then_some((*id, view.revision()))
            })
            .collect()
    }

    pub(super) fn cursor_domain_in_draft(
        &self,
        view: ViewId,
        mode_contents: &ModeContentStore,
        contents: &ContentStore,
        drafts: &ModeDraftJournal,
    ) -> CursorDomain {
        let view_data = self.views.get(&view).expect("target view exists");
        let Ok(context) = crate::mode::ModeViewContext::new(
            view,
            view_data.content(),
            view_data.state(),
            contents,
        ) else {
            return CursorDomain::InsertionPoint;
        };
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
            vell_protocol::selection::Selections,
            vell_protocol::revision::Revision,
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
                vell_protocol::selection::Selections,
                vell_protocol::revision::Revision,
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
        let context = crate::mode::ModeViewContext::new(
            view,
            view_data.content(),
            view_data.state(),
            contents,
        )
        .map_err(ModeError::InvalidViewContext)?;
        self.view_modes.execute_with_context(
            view,
            registry,
            command,
            &context,
            mode_contents,
            drafts,
        )
    }

    pub(super) fn execute_mode_input(
        &mut self,
        view: ViewId,
        registry: &ModeRegistry,
        contents: &ContentStore,
        input: &crate::command::ModeInputCommand,
        mode_contents: &mut ModeContentStore,
        drafts: &mut ModeDraftJournal,
    ) -> Result<ModeResult, ModeError> {
        let view_data = self.views.get(&view).expect("target view exists");
        let context = crate::mode::ModeViewContext::new(
            view,
            view_data.content(),
            view_data.state(),
            contents,
        )
        .map_err(ModeError::InvalidViewContext)?;
        self.view_modes.execute_input_with_context(
            view,
            registry,
            input,
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

    pub(super) fn focus_space(
        &mut self,
        target: SpaceId,
        content_modes: &mut ModeContentStore,
        contents: &ContentStore,
    ) -> Result<(), LayoutError> {
        if view_space_focusable(&self.scene, target) != Some(true) {
            return Err(LayoutError::NoFocusableSpace);
        }
        if target == self.focused {
            return Ok(());
        }
        let previous_view = self
            .view_for_space(self.focused)
            .expect("focused space hosts a view");
        let previous_content = self.views[&previous_view].content();
        let previous_data = &self.views[&previous_view];
        let presentation_changed = self.dispatcher.invalidate_view(
            previous_view,
            previous_data,
            previous_content,
            &mut self.view_modes,
            content_modes,
            contents,
        );
        if presentation_changed {
            self.views
                .get_mut(&previous_view)
                .expect("previous view exists")
                .touch();
        }
        self.focused = target;
        self.sync_global_status_target();
        self.sync_changed_input_source(previous_content, content_modes, contents);
        self.scene_revision.next();
        Ok(())
    }

    pub(super) fn is_focusable_space(&self, target: SpaceId) -> bool {
        view_space_focusable(&self.scene, target) == Some(true)
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
        let view = &self.views[&view_id];
        let Ok(context) =
            crate::mode::ModeViewContext::new(view_id, view.content(), view.state(), contents)
        else {
            return;
        };
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
    ) -> Result<(), ContentViewStateError> {
        for (view_id, view) in &mut self.views {
            if Some(*view_id) == except || view.content() != content {
                continue;
            }
            if contents.transform_view_state(content, view.state_mut(), change)? {
                view.touch();
            }
        }
        for view in self.views.values_mut() {
            if view
                .state()
                .status_bar_state()
                .and_then(|state| state.target())
                .is_some_and(|(_, target_content)| target_content == content)
            {
                view.touch();
            }
        }
        Ok(())
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
            self.views
                .iter()
                .map(|(&view, data)| (view, data.content(), data.state())),
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
        name: &crate::mode_name::ModeName,
        registry: &ModeRegistry,
        mode_contents: &mut ModeContentStore,
        contents: &ContentStore,
    ) -> Result<(), ModeAttachmentError> {
        let kind = contents
            .kind(content)
            .ok_or(ModeAttachmentError::UnknownContent(content))?;
        if registry.resolve_mode(name).is_none() {
            return Err(ModeAttachmentError::UnknownMode(name.clone()));
        }
        registry.ensure_adapter(name, content, kind)?;
        let views: Vec<_> = self
            .views
            .iter()
            .filter_map(|(view, data)| (data.content() == content).then_some(*view))
            .collect();
        for view in &views {
            let view_data = &self.views[view];
            ModeViewContext::new(*view, view_data.content(), view_data.state(), contents)?;
        }
        let profile = self.mode_profiles.entry(content).or_default();
        if !profile.contains(name) {
            profile.push(name.clone());
        }
        for view in views {
            if self.view_modes.contains(view, name) {
                continue;
            }
            let content_context = ModeContentContext::new(content, contents);
            let view_data = &self.views[&view];
            let view_context =
                ModeViewContext::new(view, view_data.content(), view_data.state(), contents)
                    .expect("attachment prevalidated view context");
            let mode = registry.instantiate_with_context(
                name,
                content,
                kind,
                mode_contents,
                &content_context,
                &view_context,
            )?;
            mode.register_faces(&mut self.faces);
            self.view_modes.insert(view, mode);
            self.dispatcher.invalidate_mode_chain(view);
            self.views
                .get_mut(&view)
                .expect("mode owner exists")
                .touch();
        }
        Ok(())
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
        registry: &ModeRegistry,
        content_modes: &mut ModeContentStore,
        contents: &ContentStore,
    ) -> Result<SplitResult, LayoutError> {
        self.reject_status_bar_space(target)?;
        let previous = self.focused;
        let previous_view = self
            .view_for_space(previous)
            .expect("focused space hosts a view");
        let previous_content = self.views[&previous_view].content();
        let target_pane = match self.status_placement {
            StatusBarPlacement::Global => None,
            StatusBarPlacement::PerPane => {
                if view_space_focusable(&self.scene, target).is_none() {
                    return Err(SceneError::ExpectedContentLeaf(target).into());
                }
                Some(
                    self.scene
                        .node(target)
                        .parent
                        .ok_or(SceneError::InvalidTree)?,
                )
            }
        };
        let next_view_id = self.next_view_id;
        let view = self.insert_view(view, registry, content_modes, contents)?;
        let result = match self.status_placement {
            StatusBarPlacement::Global => {
                self.scene_builder
                    .split(&mut self.scene, target, view, focusable, direction)
            }
            StatusBarPlacement::PerPane => {
                let status =
                    self.status_view_for_target(view, self.views[&view].content(), contents);
                let status = self.insert_view(status, registry, content_modes, contents)?;
                match self.scene_builder.split_pane(
                    &mut self.scene,
                    target_pane.expect("per-pane split prevalidates its target pane"),
                    view,
                    status,
                    focusable,
                    direction,
                ) {
                    Ok(pane) => {
                        self.status_by_editor.insert(view, status);
                        Ok(SplitResult {
                            new_space: pane.editor_space,
                        })
                    }
                    Err(error) => {
                        self.remove_view(status, content_modes);
                        Err(error)
                    }
                }
            }
        };
        let result = match result {
            Ok(result) => result,
            Err(error) => {
                self.remove_view(view, content_modes);
                self.next_view_id = next_view_id;
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
        self.sync_global_status_target();
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
        self.validate_close_space(target)?;
        let previous_focus = self.focused;
        let previous_content = self
            .view_for_space(previous_focus)
            .and_then(|view| self.views.get(&view))
            .map(View::content)
            .expect("focused space hosts a view");
        let removed_view = self
            .view_for_space(target)
            .expect("validated close target hosts a view");
        if self.status_placement == StatusBarPlacement::PerPane
            && let Some(status) = self.status_by_editor.remove(&removed_view)
        {
            let status_space =
                space_for_view(&self.scene, status).expect("per-pane status view belongs to scene");
            self.scene_builder.close(&mut self.scene, status_space)?;
            self.remove_view(status, content_modes);
        }
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
        self.reconcile_layout(if target == previous_focus {
            result.surviving_neighbor
        } else {
            Some(previous_focus)
        });
        self.sync_global_status_target();
        if self.focused != previous_focus {
            self.sync_changed_input_source(previous_content, content_modes, contents);
        }
        self.scene_revision.next();
        Ok(result)
    }

    pub(super) fn validate_close_space(&self, target: SpaceId) -> Result<(), LayoutError> {
        self.reject_status_bar_space(target)?;
        if view_space_focusable(&self.scene, target) == Some(true)
            && focusable_view_count(&self.scene) == 1
        {
            return Err(LayoutError::WouldRemoveLastFocusable(target));
        }
        self.view_for_space(target)
            .ok_or(SceneError::ExpectedContentLeaf(target))?;
        Ok(())
    }

    pub(super) fn replace_space_content(
        &mut self,
        target: SpaceId,
        view: NewView,
        focusable: bool,
        registry: &ModeRegistry,
        content_modes: &mut ModeContentStore,
        contents: &ContentStore,
    ) -> Result<(), LayoutError> {
        self.reject_status_bar_space(target)?;
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
        let new_view = self.insert_view(view, registry, content_modes, contents)?;
        if let Err(error) =
            self.scene_builder
                .replace_view(&mut self.scene, target, new_view, focusable)
        {
            self.remove_view(new_view, content_modes);
            self.next_view_id = new_view.0;
            return Err(error.into());
        }
        if self.status_placement == StatusBarPlacement::PerPane
            && let Some(status) = self.status_by_editor.remove(&old_view)
        {
            self.status_by_editor.insert(new_view, status);
            self.retarget_status_view(status, new_view);
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
        self.sync_global_status_target();
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
        self.reject_status_bar_space(target)?;
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

    fn sync_global_status_target(&mut self) {
        if self.status_placement != StatusBarPlacement::Global {
            return;
        }
        let Some(status) = self.global_status_view else {
            return;
        };
        let Some(editor) = self.view_for_space(self.focused) else {
            return;
        };
        self.retarget_status_view(status, editor);
    }

    fn reject_status_bar_space(&self, target: SpaceId) -> Result<(), LayoutError> {
        let view = self
            .view_for_space(target)
            .ok_or(SceneError::ExpectedContentLeaf(target))?;
        if self.views[&view].state().status_bar_state().is_some() {
            return Err(LayoutError::StatusBarSpace(target));
        }
        Ok(())
    }

    fn insert_view(
        &mut self,
        view: NewView,
        registry: &ModeRegistry,
        mode_contents: &mut ModeContentStore,
        contents: &ContentStore,
    ) -> Result<ViewId, LayoutError> {
        if view
            .view
            .state()
            .status_bar_state()
            .is_some_and(|state| state.target().is_none())
        {
            return Err(LayoutError::UnboundStatusBarView(view.view.content()));
        }
        let id = ViewId(self.next_view_id);
        self.next_view_id = self.next_view_id.checked_add(1).expect("view id overflow");
        let content = view.view.content();
        let kind = contents.kind(content).expect("new-view content exists");
        assert!(
            self.views.insert(id, view.view).is_none(),
            "view id must be unique"
        );
        for name in view.mode_names {
            let content_context = ModeContentContext::new(content, contents);
            let view_data = &self.views[&id];
            let view_context =
                ModeViewContext::new(id, view_data.content(), view_data.state(), contents)
                    .expect("new view state matches content kind");
            let mode = registry
                .instantiate_with_context(
                    &name,
                    content,
                    kind,
                    mode_contents,
                    &content_context,
                    &view_context,
                )
                .expect("new-view mode must be registered");
            mode.register_faces(&mut self.faces);
            self.view_modes.insert(id, mode);
        }
        Ok(id)
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

fn view_targets_content(view: &View, content: ContentId) -> bool {
    view.content() == content
        || view
            .state()
            .status_bar_state()
            .and_then(|state| state.target())
            .is_some_and(|(_, target_content)| target_content == content)
}
