use std::collections::{HashMap, HashSet};
use std::time::Instant;

use crate::action::ViewAction;
use crate::command::ModeCommand;
use crate::command_resolver::default_global_keymap;
use crate::dispatcher::{DispatchInput, DispatchOutcome, Dispatcher, DispatcherInputSnapshot};
use crate::layout::{LayoutError, NewView, StatusBarHandle, StatusBarPlacement, create_view};
use crate::mode::{
    CursorDomain, FaceRegistry, ModeAttachmentError, ModeContentContext, ModeContentStore,
    ModeDraftJournal, ModeError, ModeRegistry, ModeResult, ModeViewContext, ModeViewStore,
};
use crate::presentation::PresentationLayerStore;
use crate::scene_model::{CloseResult, SplitResult};
use crate::theme::{FaceEnvironment, SessionFaces};
use crate::view::View;
use crate::view_workspace::{RemovedView, ViewWorkspace};
use vell_core::content::ContentChange;
use vell_core::content_store::ContentStore;
use vell_core::content_view_state::ContentViewStateError;
use vell_protocol::content_query::RowRange;
use vell_protocol::ids::{ContentId, SpaceId, ViewId};
use vell_protocol::revision::Revision;
use vell_protocol::scene::Scene;
use vell_protocol::space::{Sizing, SplitDirection};

pub(super) struct ClientSession {
    workspace: ViewWorkspace,
    mode_profiles: HashMap<ContentId, Vec<crate::mode_name::ModeName>>,
    default_mode_profiles:
        HashMap<vell_core::content::ContentKind, Vec<crate::mode_name::ModeName>>,
    view_modes: ModeViewStore,
    faces: SessionFaces,
    presentation: PresentationLayerStore,
    dispatcher: Dispatcher,
}

pub(super) struct InitialView {
    pub view: ViewId,
    pub content: ContentId,
    pub modes: Vec<crate::mode_name::ModeName>,
}

pub(super) struct EditorSessionInit {
    pub editor: InitialView,
    pub next_view_id: u64,
}

pub(super) struct SessionWorkspaceMutation<T> {
    pub output: T,
    pub removed: Vec<(ViewId, ContentId)>,
}

impl ClientSession {
    pub(super) fn editor(
        contents: &ContentStore,
        modes: &ModeRegistry,
        mode_contents: &mut ModeContentStore,
        width: usize,
        height: usize,
        init: EditorSessionInit,
        face_environment: FaceEnvironment,
    ) -> Self {
        let editor = create_view(init.editor.content, contents, &init.editor.modes)
            .expect("editor content exists");
        let default_mode_profiles = HashMap::from([(
            vell_core::content::ContentKind::Buffer,
            init.editor.modes.clone(),
        )]);
        let mode_profiles = HashMap::from([(init.editor.content, init.editor.modes)]);
        let workspace = ViewWorkspace::editor(
            width,
            height,
            init.editor.view,
            editor.view,
            init.next_view_id,
        );
        let mut view_modes = ModeViewStore::default();
        let mut faces = FaceRegistry::default();
        let editor_content = workspace
            .view(init.editor.view)
            .expect("initial editor view exists")
            .content();
        for name in editor.mode_names {
            let content_context = ModeContentContext::new(editor_content, contents);
            let view_data = workspace
                .view(init.editor.view)
                .expect("initial editor view exists");
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
        let mut session = Self {
            workspace,
            mode_profiles,
            default_mode_profiles,
            view_modes,
            faces: SessionFaces::new(faces, face_environment),
            presentation: PresentationLayerStore::default(),
            dispatcher: Dispatcher::new(default_global_keymap()),
        };
        session.refresh_presentation(contents, mode_contents);
        session
    }

    pub(super) fn scene(&self) -> &Scene {
        self.workspace.scene()
    }

    pub(super) fn scene_revision(&self) -> Revision {
        self.workspace.scene_revision()
    }

    pub(super) fn focused(&self) -> SpaceId {
        self.workspace.focused()
    }

    pub(super) fn views(&self) -> &HashMap<ViewId, View> {
        self.workspace.views()
    }

    pub(super) fn status_bar_placement(&self) -> StatusBarPlacement {
        self.workspace.status_bar_placement()
    }

    pub(super) fn status_bar_for_view(&self, editor: ViewId) -> Option<StatusBarHandle> {
        self.workspace.status_bar_for_view(editor)
    }

    pub(super) fn status_bars_for_content(&self, content: ContentId) -> Vec<StatusBarHandle> {
        self.workspace.status_bars_for_content(content)
    }

    pub(super) fn set_status_bar_placement(
        &mut self,
        placement: StatusBarPlacement,
    ) -> Result<(), LayoutError> {
        self.workspace.set_status_bar_placement(placement)
    }

    pub(super) fn set_status_bar_visible(
        &mut self,
        editor: Option<ViewId>,
        visible: bool,
    ) -> Result<(), LayoutError> {
        self.workspace.set_status_bar_visible(editor, visible)
    }

    pub(super) fn view_modes(&self) -> &ModeViewStore {
        &self.view_modes
    }

    pub(super) fn view_modes_mut(&mut self) -> &mut ModeViewStore {
        &mut self.view_modes
    }

    pub(super) fn register_content_profile(
        &mut self,
        content: ContentId,
        kind: vell_core::content::ContentKind,
    ) {
        let profile = self
            .default_mode_profiles
            .get(&kind)
            .cloned()
            .unwrap_or_default();
        self.mode_profiles.insert(content, profile);
    }

    pub(super) fn forget_content(&mut self, content: ContentId) {
        self.mode_profiles.remove(&content);
        self.faces.remove_content_remaps(content);
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
                .workspace
                .view_mut(view)
                .expect("touched view still exists");
            if target.revision() == revision_before {
                target.touch();
            }
        }
    }

    pub(super) fn faces(&self) -> &SessionFaces {
        self.faces
            .set_active_view(self.workspace.view_for_space(self.workspace.focused()));
        &self.faces
    }

    pub(super) fn faces_mut(&mut self) -> &mut SessionFaces {
        &mut self.faces
    }

    pub(super) fn presentation(&self) -> &PresentationLayerStore {
        &self.presentation
    }

    pub(super) fn set_status_message(&mut self, message: String) {
        self.presentation.set_status_message(message);
    }

    pub(super) fn clear_status_message(&mut self) {
        self.presentation.clear_status_message();
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
        for (&view, view_data) in self.workspace.views() {
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
    pub(super) fn has_content_face_remaps_for_test(&self, content: ContentId) -> bool {
        self.faces.has_content_remaps_for_test(content)
    }

    #[cfg(test)]
    pub(super) fn view_modes_mut_for_test(&mut self) -> &mut ModeViewStore {
        &mut self.view_modes
    }

    #[cfg(test)]
    pub(super) fn input_is_pending_for_test(&self) -> bool {
        self.dispatcher.is_pending()
    }

    pub(super) fn view(&self, view: ViewId) -> Option<&View> {
        self.workspace.view(view)
    }

    #[cfg(test)]
    pub(super) fn view_mut(&mut self, view: ViewId) -> Option<&mut View> {
        self.workspace.view_mut(view)
    }

    #[cfg(test)]
    pub(super) fn compose_views_for_test(
        &mut self,
        parent: ViewId,
        children: &[(ViewId, bool)],
    ) -> Result<(), LayoutError> {
        self.workspace.compose(parent, children)
    }

    #[cfg(test)]
    pub(super) fn set_view_switchable_for_test(
        &mut self,
        view: ViewId,
        switchable: bool,
    ) -> Result<(), LayoutError> {
        self.workspace.set_switchable(view, switchable)
    }

    pub(super) fn touch_content_views(&mut self, content: ContentId) {
        for (_, view) in self.workspace.views_mut() {
            if view.content() == content {
                view.touch();
            }
        }
    }

    pub(super) fn content_view_revisions(&self, content: ContentId) -> Vec<(ViewId, Revision)> {
        self.workspace
            .views()
            .iter()
            .filter_map(|(id, view)| (view.content() == content).then_some((*id, view.revision())))
            .collect()
    }

    pub(super) fn cursor_domain_in_draft(
        &self,
        view: ViewId,
        mode_contents: &ModeContentStore,
        contents: &ContentStore,
        drafts: &ModeDraftJournal,
    ) -> CursorDomain {
        let view_data = self.workspace.view(view).expect("target view exists");
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
        let view = self.workspace.view_mut(view)?;
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
        self.workspace
            .views()
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
            if let Some(view) = self.workspace.view_mut(id) {
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
        let view_data = self.workspace.view(view).expect("target view exists");
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
        let view_data = self.workspace.view(view).expect("target view exists");
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
        self.workspace.view_for_space(space)
    }

    /// 通用切换目标：焦点 Pane 所属 view 的最近 switchable 祖先。
    pub(super) fn switch_target(&self, space: SpaceId) -> Option<ViewId> {
        self.workspace.switch_target(space)
    }

    pub(super) fn switch_target_from_view(&self, view: ViewId) -> Option<ViewId> {
        self.workspace.switch_target_from_view(view)
    }

    /// view 的正文 Space（BODY_PANE 直属 Pane）。
    pub(super) fn body_space_for_view(&self, view: ViewId) -> Option<SpaceId> {
        self.workspace.body_space_for_view(view)
    }

    pub(super) fn resize(&mut self, width: u16, height: u16) {
        self.workspace.resize(width, height);
    }

    pub(super) fn focus_space(
        &mut self,
        target: SpaceId,
        content_modes: &mut ModeContentStore,
        contents: &ContentStore,
    ) -> Result<(), LayoutError> {
        if !self.workspace.is_focusable(target) {
            return Err(LayoutError::NoFocusableSpace);
        }
        if target == self.workspace.focused() {
            return Ok(());
        }
        let previous_view = self
            .view_for_space(self.workspace.focused())
            .expect("focused space hosts a view");
        let previous_data = self
            .workspace
            .view(previous_view)
            .expect("focused view exists");
        let previous_content = previous_data.content();
        let presentation_changed = self.dispatcher.invalidate_view(
            previous_view,
            previous_data,
            previous_content,
            &mut self.view_modes,
            content_modes,
            contents,
        );
        if presentation_changed {
            self.workspace
                .view_mut(previous_view)
                .expect("previous view exists")
                .touch();
        }
        self.workspace.focus(target)?;
        self.sync_changed_input_source(previous_content, content_modes, contents);
        Ok(())
    }

    pub(super) fn is_focusable_space(&self, target: SpaceId) -> bool {
        self.workspace.is_focusable(target)
    }

    pub(super) fn next_input_deadline(
        &self,
        content_modes: &ModeContentStore,
        contents: &ContentStore,
    ) -> Option<Instant> {
        self.dispatcher.next_deadline(
            self.workspace.views(),
            &self.view_modes,
            content_modes,
            contents,
        )
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
            self.workspace.focused(),
            self.workspace.scene(),
            self.workspace.views(),
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
            self.workspace.focused(),
            self.workspace.scene(),
            self.workspace.views(),
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
        let Some(view_id) = self.view_for_space(self.workspace.focused()) else {
            return;
        };
        let view = self.workspace.view(view_id).expect("focused view exists");
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
        for (view_id, view) in self.workspace.views_mut() {
            if Some(*view_id) == except || view.content() != content {
                continue;
            }
            if contents.transform_view_state(content, view.state_mut(), change)? {
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
            self.workspace
                .views()
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
            .workspace
            .views()
            .iter()
            .filter_map(|(view, data)| (data.content() == content).then_some(*view))
            .collect();
        for view in &views {
            let view_data = self.workspace.view(*view).expect("target view exists");
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
            let view_data = self.workspace.view(view).expect("target view exists");
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
            mode.register_faces(self.faces.registry_mut());
            self.view_modes.insert(view, mode);
            self.dispatcher.invalidate_mode_chain(view);
            self.workspace
                .view_mut(view)
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
        let previous = self.workspace.focused();
        let previous_view = self
            .view_for_space(previous)
            .expect("focused space hosts a view");
        let previous_content = self
            .workspace
            .view(previous_view)
            .expect("focused view exists")
            .content();
        let NewView { view, mode_names } = view;
        let (view, result) = self
            .workspace
            .split(target, view, focusable, direction, focus_new)?;
        self.attach_new_view_modes(view, mode_names, registry, content_modes, contents);
        if focus_new {
            let view_data = self
                .workspace
                .view(previous_view)
                .expect("previous view exists");
            let presentation_changed = self.dispatcher.invalidate_view(
                previous_view,
                view_data,
                previous_content,
                &mut self.view_modes,
                content_modes,
                contents,
            );
            if presentation_changed {
                self.workspace
                    .view_mut(previous_view)
                    .expect("previous view still exists")
                    .touch();
            }
        }
        if focus_new {
            self.sync_changed_input_source(previous_content, content_modes, contents);
        }
        Ok(result)
    }

    pub(super) fn close_space(
        &mut self,
        target: SpaceId,
        content_modes: &mut ModeContentStore,
        contents: &ContentStore,
    ) -> Result<SessionWorkspaceMutation<CloseResult>, LayoutError> {
        let previous_focus = self.workspace.focused();
        let previous_content = self
            .view_for_space(previous_focus)
            .and_then(|view| self.workspace.view(view))
            .map(View::content)
            .expect("focused space hosts a view");
        let mutation = self.workspace.close(target)?;
        let removed = self.cleanup_removed_views(mutation.removed, content_modes, contents);
        if self.workspace.focused() != previous_focus {
            self.sync_changed_input_source(previous_content, content_modes, contents);
        }
        Ok(SessionWorkspaceMutation {
            output: mutation.output,
            removed,
        })
    }

    pub(super) fn validate_close_space(&self, target: SpaceId) -> Result<(), LayoutError> {
        self.workspace.validate_close(target)
    }

    pub(super) fn closing_content_needs_replacement(
        &self,
        content: ContentId,
    ) -> Result<bool, LayoutError> {
        self.workspace.closing_content_needs_replacement(content)
    }

    pub(super) fn close_content_views(
        &mut self,
        content: ContentId,
        replacement: Option<NewView>,
        registry: &ModeRegistry,
        content_modes: &mut ModeContentStore,
        contents: &ContentStore,
    ) -> Result<SessionWorkspaceMutation<Option<ViewId>>, LayoutError> {
        let previous_focus = self.workspace.focused();
        let previous_view = self
            .view_for_space(previous_focus)
            .expect("focused space hosts a view");
        let previous_content = self
            .workspace
            .view(previous_view)
            .expect("focused view exists")
            .content();
        let (replacement, mode_names) = replacement.map_or((None, Vec::new()), |replacement| {
            (Some(replacement.view), replacement.mode_names)
        });
        let mutation = self.workspace.close_content_views(content, replacement)?;
        if let Some(view) = mutation.output {
            self.attach_new_view_modes(view, mode_names, registry, content_modes, contents);
        }
        let removed = self.cleanup_removed_views(mutation.removed, content_modes, contents);
        if removed.iter().any(|(view, _)| *view == previous_view) {
            self.sync_changed_input_source(previous_content, content_modes, contents);
        }
        Ok(SessionWorkspaceMutation {
            output: mutation.output,
            removed,
        })
    }

    pub(super) fn replace_space_content(
        &mut self,
        target: SpaceId,
        view: NewView,
        focusable: bool,
        registry: &ModeRegistry,
        content_modes: &mut ModeContentStore,
        contents: &ContentStore,
    ) -> Result<SessionWorkspaceMutation<ViewId>, LayoutError> {
        let previous_focus = self.workspace.focused();
        let previous_view = self
            .view_for_space(previous_focus)
            .expect("focused space hosts a view");
        let previous_content = self
            .workspace
            .view(previous_view)
            .expect("focused view exists")
            .content();
        let NewView { view, mode_names } = view;
        let mutation = self.workspace.replace(target, view, focusable)?;
        self.attach_new_view_modes(
            mutation.output,
            mode_names,
            registry,
            content_modes,
            contents,
        );
        let removed = self.cleanup_removed_views(mutation.removed, content_modes, contents);
        if removed.iter().any(|(view, _)| *view == previous_view) {
            self.sync_changed_input_source(previous_content, content_modes, contents);
        }
        Ok(SessionWorkspaceMutation {
            output: mutation.output,
            removed,
        })
    }

    pub(super) fn set_space_sizing(
        &mut self,
        target: SpaceId,
        sizing: Sizing,
    ) -> Result<(), LayoutError> {
        self.workspace.set_sizing(target, sizing)
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

    fn attach_new_view_modes(
        &mut self,
        id: ViewId,
        mode_names: Vec<crate::mode_name::ModeName>,
        registry: &ModeRegistry,
        mode_contents: &mut ModeContentStore,
        contents: &ContentStore,
    ) {
        let content = self
            .workspace
            .view(id)
            .expect("workspace published new view")
            .content();
        let kind = contents.kind(content).expect("new-view content exists");
        for name in mode_names {
            let content_context = ModeContentContext::new(content, contents);
            let view_data = self.workspace.view(id).expect("new view exists");
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
            mode.register_faces(self.faces.registry_mut());
            self.view_modes.insert(id, mode);
        }
    }

    fn cleanup_removed_views(
        &mut self,
        removed: Vec<RemovedView>,
        mode_contents: &mut ModeContentStore,
        contents: &ContentStore,
    ) -> Vec<(ViewId, ContentId)> {
        let mut removed_contents = HashSet::new();
        let mut removed_modes = HashSet::new();
        let mut result = Vec::with_capacity(removed.len());
        for RemovedView { id, view } in removed {
            let content = view.content();
            self.dispatcher.invalidate_view(
                id,
                &view,
                content,
                &mut self.view_modes,
                mode_contents,
                contents,
            );
            self.faces.remove_view_remaps(id);
            for mode in self.view_modes.remove(id) {
                mode_contents.detach_view(content, mode);
                removed_modes.insert(mode);
            }
            removed_contents.insert(content);
            result.push((id, content));
        }
        for content in removed_contents {
            if !self
                .workspace
                .views()
                .values()
                .any(|view| view.content() == content)
            {
                self.faces.remove_content_remaps(content);
            }
        }
        for mode in removed_modes {
            if !self.view_modes.contains_mode(mode) {
                self.faces.remove_mode_remaps(mode);
            }
        }
        result
    }

    #[cfg(test)]
    pub(super) fn replace_dispatcher_for_test(&mut self, dispatcher: Dispatcher) {
        self.dispatcher = dispatcher;
    }

    #[cfg(test)]
    pub(super) fn next_view_id_for_test(&self) -> u64 {
        self.workspace.next_view_id_for_test()
    }
}
