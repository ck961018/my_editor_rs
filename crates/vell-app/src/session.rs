use std::collections::{HashMap, HashSet};
use std::time::Instant;

use crate::action::ViewAction;
use crate::command::ModeCommand;
use crate::command_resolver::default_global_keymap;
use crate::content_classifier::ContentClassifier;
use crate::dispatcher::{DispatchInput, DispatchOutcome, Dispatcher, DispatcherInputSnapshot};
use crate::layout::{LayoutError, NewView, StatusBarHandle, StatusBarPlacement, create_view};
use crate::mode::{
    CursorDomain, FaceRegistry, ModeAttachmentError, ModeContentContext, ModeContentStore,
    ModeDraftJournal, ModeError, ModeId, ModeRegistry, ModeResult, ModeViewStore,
};
use crate::mode_resolver::{AttachmentPlanError, ModeAttachmentPlan, ModeOverride, ModeResolver};
use crate::presentation::PresentationLayerStore;
use crate::scene_model::{CloseResult, SplitResult};
use crate::theme::{FaceEnvironment, SessionFaces};
use crate::view::View;
use crate::view_context::require_mode_view_context;
use crate::view_workspace::{DiffViewResult, RemovedView, ViewWorkspace, WorkspaceMutation};
use vell_core::content::ContentChange;
use vell_core::content_store::ContentStore;
use vell_core::content_view_state::ContentViewStateError;
use vell_protocol::content_query::RowRange;
use vell_protocol::ids::{ContentId, SpaceId, ViewId};
use vell_protocol::revision::Revision;
use vell_protocol::scene::Scene;
use vell_protocol::space::{Sizing, SplitDirection};
use vell_protocol::view::ViewDefinition;
use vell_protocol::view::{BindingKey, DOCUMENT_BINDING};

pub(super) struct ClientSession {
    workspace: ViewWorkspace,
    mode_resolver: ModeResolver,
    view_modes: ModeViewStore,
    faces: SessionFaces,
    presentation: PresentationLayerStore,
    dispatcher: Dispatcher,
}

pub(super) struct InitialView {
    pub view: ViewId,
    pub content: ContentId,
}

pub(super) struct EditorSessionInit {
    pub editor: InitialView,
    pub next_view_id: u64,
    pub buffer_definition: ViewDefinition,
}

pub(super) struct SessionWorkspaceMutation<T> {
    pub output: T,
    pub removed: Vec<RemovedSessionView>,
}

pub(super) struct RemovedSessionView {
    pub view: ViewId,
    pub document: Option<ContentId>,
}

struct PreparedAttachment {
    plan: ModeAttachmentPlan,
    target: Vec<ModeId>,
    view: View,
}

pub(super) struct PreparedDiffReplacement {
    workspace: ViewWorkspace,
    mutation: WorkspaceMutation<DiffViewResult>,
    attachments: Vec<PreparedAttachment>,
    previous_view: ViewId,
}

impl ClientSession {
    #[allow(
        clippy::too_many_arguments,
        reason = "session bootstrap receives independent app-owned stores"
    )]
    pub(super) fn editor(
        contents: &ContentStore,
        modes: &ModeRegistry,
        classifier: &ContentClassifier,
        mode_contents: &mut ModeContentStore,
        width: usize,
        height: usize,
        init: EditorSessionInit,
        face_environment: FaceEnvironment,
    ) -> Result<Self, AttachmentPlanError> {
        let editor = create_view(init.editor.content, contents, &init.buffer_definition)
            .expect("editor content exists");
        let workspace = ViewWorkspace::editor(
            width,
            height,
            init.editor.view,
            editor.view,
            init.next_view_id,
        );
        let view_modes = ModeViewStore::default();
        let faces = FaceRegistry::default();
        let mut session = Self {
            workspace,
            mode_resolver: ModeResolver::new(modes)?,
            view_modes,
            faces: SessionFaces::new(faces, face_environment),
            presentation: PresentationLayerStore::default(),
            dispatcher: Dispatcher::new(default_global_keymap()),
        };
        session.reconcile_view_modes(
            init.editor.view,
            modes,
            classifier,
            mode_contents,
            contents,
        )?;
        session.refresh_presentation(contents, mode_contents);
        Ok(session)
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

    pub(super) fn forget_content(&mut self, content: ContentId) {
        self.mode_resolver.forget_content(content);
        self.faces.remove_content_remaps(content);
    }

    pub(super) fn commit_mode_drafts(&mut self, drafts: &mut ModeDraftJournal) {
        drafts.commit_views(&mut self.view_modes);
    }

    pub(super) fn resolve_attachment_plan(
        &self,
        view: ViewId,
        registry: &ModeRegistry,
        classifier: &ContentClassifier,
        contents: &ContentStore,
    ) -> Result<ModeAttachmentPlan, AttachmentPlanError> {
        let view_data = self.workspace.view(view).ok_or({
            ModeAttachmentError::InvalidViewContext(
                crate::mode::ModeContextError::MissingDocument { view },
            )
        })?;
        self.mode_resolver
            .resolve(view, view_data, registry, classifier, contents)
            .map_err(Into::into)
    }

    pub(super) fn apply_attachment_plan(
        &mut self,
        plan: ModeAttachmentPlan,
        registry: &ModeRegistry,
        mode_contents: &mut ModeContentStore,
        contents: &ContentStore,
    ) -> Result<bool, AttachmentPlanError> {
        let view_data = self
            .workspace
            .view(plan.view)
            .ok_or({
                ModeAttachmentError::InvalidViewContext(
                    crate::mode::ModeContextError::MissingDocument { view: plan.view },
                )
            })?
            .clone();
        let target =
            self.validate_attachment_plan_for_view(&plan, &view_data, registry, contents)?;
        Ok(
            self.install_attachment_plan(
                plan,
                target,
                view_data,
                registry,
                mode_contents,
                contents,
            ),
        )
    }

    fn validate_attachment_plan_for_view(
        &self,
        plan: &ModeAttachmentPlan,
        view_data: &View,
        registry: &ModeRegistry,
        contents: &ContentStore,
    ) -> Result<Vec<ModeId>, AttachmentPlanError> {
        if view_data.binding_revision() != plan.binding_revision {
            return Err(AttachmentPlanError::StaleBindings {
                view: plan.view,
                expected: plan.binding_revision,
                actual: view_data.binding_revision(),
            });
        }
        let mut target = Vec::with_capacity(plan.entries.len());
        for entry in &plan.entries {
            let (Some(binding), Some(content)) = (&entry.binding, entry.content) else {
                return Err(AttachmentPlanError::UnsupportedBinding {
                    mode: entry.mode.clone(),
                    binding: entry.binding.clone(),
                });
            };
            if binding.as_str() != DOCUMENT_BINDING || view_data.document_content() != Some(content)
            {
                return Err(AttachmentPlanError::UnsupportedBinding {
                    mode: entry.mode.clone(),
                    binding: entry.binding.clone(),
                });
            }
            let mode = registry
                .resolve_mode(&entry.mode)
                .ok_or_else(|| ModeAttachmentError::UnknownMode(entry.mode.clone()))?;
            registry.ensure_adapter(
                &entry.mode,
                content,
                contents
                    .kind(content)
                    .ok_or(ModeAttachmentError::UnknownContent(content))?,
            )?;
            if target.contains(&mode) {
                return Err(AttachmentPlanError::DuplicateMode(entry.mode.clone()));
            }
            target.push(mode);
        }
        if !target.is_empty() || self.view_modes.is_active(plan.view) {
            require_mode_view_context(plan.view, view_data, contents)
                .map_err(ModeAttachmentError::from)?;
        }
        Ok(target)
    }

    fn install_attachment_plan(
        &mut self,
        plan: ModeAttachmentPlan,
        target: Vec<ModeId>,
        view_data: View,
        registry: &ModeRegistry,
        mode_contents: &mut ModeContentStore,
        contents: &ContentStore,
    ) -> bool {
        let current = self.view_modes.mode_ids(plan.view).to_vec();
        if current == target {
            return false;
        }
        let context = require_mode_view_context(plan.view, &view_data, contents)
            .expect("attachment plan View context was prevalidated");
        let mut additions = Vec::new();
        for (entry, mode) in plan.entries.iter().zip(&target) {
            if current.contains(mode) {
                continue;
            }
            let content = entry
                .content
                .expect("attachment plan content was prevalidated");
            let content_context = ModeContentContext::new(content, contents);
            let instance = registry.instantiate_registered_with_context(
                *mode,
                content,
                contents
                    .kind(content)
                    .expect("attachment content was prevalidated"),
                mode_contents,
                &content_context,
                &context,
            );
            additions.push(instance);
        }
        self.dispatcher.invalidate_view(
            plan.view,
            &view_data,
            &mut self.view_modes,
            mode_contents,
            contents,
        );
        let removed = current
            .iter()
            .copied()
            .filter(|mode| !target.contains(mode))
            .collect::<Vec<ModeId>>();
        let document = view_data
            .document_content()
            .expect("active Mode chain has a document binding");
        for mode in &removed {
            self.view_modes.remove_mode(plan.view, *mode);
            mode_contents.detach_view(document, *mode);
        }
        for instance in additions {
            instance.register_faces(self.faces.registry_mut());
            self.view_modes.insert(plan.view, instance);
        }
        self.view_modes.set_chain_order(plan.view, target);
        self.workspace
            .view_mut(plan.view)
            .expect("planned View still exists")
            .touch();
        for mode in removed {
            if !self.view_modes.contains_mode(mode) {
                self.faces.remove_mode_remaps(mode);
            }
        }
        true
    }

    pub(super) fn reconcile_view_modes(
        &mut self,
        view: ViewId,
        registry: &ModeRegistry,
        classifier: &ContentClassifier,
        mode_contents: &mut ModeContentStore,
        contents: &ContentStore,
    ) -> Result<bool, AttachmentPlanError> {
        let plan = self.resolve_attachment_plan(view, registry, classifier, contents)?;
        self.apply_attachment_plan(plan, registry, mode_contents, contents)
    }

    pub(super) fn reconcile_content_modes(
        &mut self,
        content: ContentId,
        registry: &ModeRegistry,
        classifier: &ContentClassifier,
        mode_contents: &mut ModeContentStore,
        contents: &ContentStore,
    ) -> Result<bool, AttachmentPlanError> {
        let views = self
            .workspace
            .views()
            .iter()
            .filter_map(|(view, data)| data.references(content).then_some(*view))
            .collect::<Vec<_>>();
        let mut prepared = Vec::with_capacity(views.len());
        for view in views {
            let view_data = self
                .workspace
                .view(view)
                .expect("referencing View exists")
                .clone();
            let plan = self
                .mode_resolver
                .resolve(view, &view_data, registry, classifier, contents)?;
            let target =
                self.validate_attachment_plan_for_view(&plan, &view_data, registry, contents)?;
            prepared.push((plan, target, view_data));
        }
        let mut changed = false;
        for (plan, target, view_data) in prepared {
            changed |= self.install_attachment_plan(
                plan,
                target,
                view_data,
                registry,
                mode_contents,
                contents,
            );
        }
        Ok(changed)
    }

    #[allow(
        dead_code,
        clippy::too_many_arguments,
        reason = "per-View override receives independent app-owned stores"
    )]
    pub(super) fn set_view_mode_enabled(
        &mut self,
        view: ViewId,
        mode: crate::mode_name::ModeName,
        enabled: bool,
        registry: &ModeRegistry,
        classifier: &ContentClassifier,
        mode_contents: &mut ModeContentStore,
        contents: &ContentStore,
    ) -> Result<bool, AttachmentPlanError> {
        if registry.resolve_mode(&mode).is_none() {
            return Err(ModeAttachmentError::UnknownMode(mode).into());
        }
        let value = if enabled {
            ModeOverride::Enable
        } else {
            ModeOverride::Disable
        };
        let mut candidate = self.mode_resolver.clone();
        candidate.set_view_override(view, mode, value);
        let view_data = self.workspace.view(view).ok_or({
            ModeAttachmentError::InvalidViewContext(
                crate::mode::ModeContextError::MissingDocument { view },
            )
        })?;
        let plan = candidate.resolve(view, view_data, registry, classifier, contents)?;
        let changed = self.apply_attachment_plan(plan, registry, mode_contents, contents)?;
        self.mode_resolver = candidate;
        Ok(changed)
    }

    #[allow(
        dead_code,
        clippy::too_many_arguments,
        reason = "per-View order override receives independent app-owned stores"
    )]
    pub(super) fn set_view_mode_order(
        &mut self,
        view: ViewId,
        order: Vec<crate::mode_name::ModeName>,
        registry: &ModeRegistry,
        classifier: &ContentClassifier,
        mode_contents: &mut ModeContentStore,
        contents: &ContentStore,
    ) -> Result<bool, AttachmentPlanError> {
        let mut candidate = self.mode_resolver.clone();
        candidate.set_view_order_override(view, order);
        let view_data = self.workspace.view(view).ok_or({
            ModeAttachmentError::InvalidViewContext(
                crate::mode::ModeContextError::MissingDocument { view },
            )
        })?;
        let plan = candidate.resolve(view, view_data, registry, classifier, contents)?;
        let changed = self.apply_attachment_plan(plan, registry, mode_contents, contents)?;
        self.mode_resolver = candidate;
        Ok(changed)
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
            let Some((content, _)) = view_data.document() else {
                continue;
            };
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
                let Ok(context) = require_mode_view_context(view, view_data, contents) else {
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
            if view.document_content() == Some(content) {
                view.touch();
            }
        }
    }

    pub(super) fn content_view_revisions(&self, content: ContentId) -> Vec<(ViewId, Revision)> {
        self.workspace
            .views()
            .iter()
            .filter_map(|(id, view)| {
                (view.document_content() == Some(content)).then_some((*id, view.revision()))
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
        let view_data = self.workspace.view(view).expect("target view exists");
        let Ok(context) = require_mode_view_context(view, view_data, contents) else {
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
            ViewAction::SetSelections(selections) => view
                .document_content()
                .and_then(|content| contents.selections_are_valid(content, &selections))
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
            .filter(|(_, view)| view.document_content() == Some(content))
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
        let context = require_mode_view_context(view, view_data, contents)
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
        let context = require_mode_view_context(view, view_data, contents)
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

    pub(super) fn replacement_space_for_view(&self, view: ViewId) -> Option<SpaceId> {
        self.workspace.replacement_space_for_view(view)
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
        let presentation_changed = self.dispatcher.invalidate_view(
            previous_view,
            previous_data,
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
        self.sync_changed_input_source(content_modes, contents);
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
        let Ok(context) = require_mode_view_context(view_id, view, contents) else {
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
            if Some(*view_id) == except || view.document_content() != Some(content) {
                continue;
            }
            if contents.transform_view_state(content, view.require_document_state_mut(), change)? {
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
            self.workspace.views().iter().filter_map(|(&view, data)| {
                data.document()
                    .map(|(content, state)| (view, content, state))
            }),
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
        classifier: &ContentClassifier,
        mode_contents: &mut ModeContentStore,
        contents: &ContentStore,
    ) -> Result<(), AttachmentPlanError> {
        let kind = contents
            .kind(content)
            .ok_or(ModeAttachmentError::UnknownContent(content))?;
        if registry.resolve_mode(name).is_none() {
            return Err(ModeAttachmentError::UnknownMode(name.clone()).into());
        }
        registry.ensure_adapter(name, content, kind)?;
        let views = self
            .workspace
            .views()
            .iter()
            .filter_map(|(view, data)| (data.document_content() == Some(content)).then_some(*view))
            .collect::<Vec<_>>();
        for view in &views {
            let view_data = self.workspace.view(*view).expect("target view exists");
            require_mode_view_context(*view, view_data, contents)
                .map_err(ModeAttachmentError::from)?;
        }
        let mut candidate = self.mode_resolver.clone();
        candidate.set_content_override(content, name.clone(), ModeOverride::Enable);
        let plans = views
            .iter()
            .map(|view| {
                candidate.resolve(
                    *view,
                    self.workspace.view(*view).expect("target view exists"),
                    registry,
                    classifier,
                    contents,
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        for plan in plans {
            self.apply_attachment_plan(plan, registry, mode_contents, contents)?;
        }
        self.mode_resolver = candidate;
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
        classifier: &ContentClassifier,
        content_modes: &mut ModeContentStore,
        contents: &ContentStore,
    ) -> Result<SplitResult, LayoutError> {
        let previous = self.workspace.focused();
        let previous_view = self
            .view_for_space(previous)
            .expect("focused space hosts a view");
        let NewView { view } = view;
        let planned_view = self.workspace.next_view_id();
        let plan = self
            .mode_resolver
            .resolve(planned_view, &view, registry, classifier, contents)
            .map_err(|error| LayoutError::ModeAttachment(error.to_string()))?;
        let workspace_before = self.workspace.clone();
        let (view, result) = self
            .workspace
            .split(target, view, focusable, direction, focus_new)?;
        assert_eq!(view, planned_view, "workspace allocated the planned ViewId");
        if let Err(error) = self.apply_attachment_plan(plan, registry, content_modes, contents) {
            self.workspace = workspace_before;
            return Err(LayoutError::ModeAttachment(error.to_string()));
        }
        if focus_new {
            let view_data = self
                .workspace
                .view(previous_view)
                .expect("previous view exists");
            let presentation_changed = self.dispatcher.invalidate_view(
                previous_view,
                view_data,
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
            self.sync_changed_input_source(content_modes, contents);
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
        let mutation = self.workspace.close(target)?;
        let removed = self.cleanup_removed_views(mutation.removed, content_modes, contents);
        if self.workspace.focused() != previous_focus {
            self.sync_changed_input_source(content_modes, contents);
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

    pub(super) fn blocking_content_reference(
        &self,
        content: ContentId,
    ) -> Result<Option<(ViewId, BindingKey)>, LayoutError> {
        self.workspace.blocking_content_reference(content)
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "session mutation receives independent app-owned stores"
    )]
    pub(super) fn rebind_view_content(
        &mut self,
        view: ViewId,
        binding: &BindingKey,
        content: ContentId,
        registry: &ModeRegistry,
        classifier: &ContentClassifier,
        content_modes: &mut ModeContentStore,
        contents: &ContentStore,
    ) -> Result<ContentId, LayoutError> {
        if !contents.contains(content) {
            return Err(LayoutError::MissingContent(content));
        }
        let previous_view = self
            .workspace
            .view(view)
            .ok_or(LayoutError::MissingView(view))?
            .clone();
        let previous_content =
            previous_view
                .binding(binding.as_str())
                .ok_or_else(|| LayoutError::MissingBinding {
                    view,
                    binding: binding.clone(),
                })?;
        if previous_content == content {
            return Ok(previous_content);
        }

        let document_rebind = binding.as_str() == DOCUMENT_BINDING;
        let state = if document_rebind {
            Some(
                contents
                    .create_view_state(content)
                    .ok_or(LayoutError::MissingContent(content))?,
            )
        } else {
            None
        };
        let mut planned_view = previous_view.clone();
        planned_view
            .rebind(binding, content, state.clone())
            .map_err(|error| LayoutError::ModeAttachment(format!("{error:?}")))?;
        let plan = self
            .mode_resolver
            .resolve(view, &planned_view, registry, classifier, contents)
            .map_err(|error| LayoutError::ModeAttachment(error.to_string()))?;
        let current_modes = self.view_modes.mode_ids(view).to_vec();
        let target_modes = self
            .validate_attachment_plan_for_view(&plan, &planned_view, registry, contents)
            .map_err(|error| LayoutError::ModeAttachment(error.to_string()))?;
        let retained_modes = current_modes
            .iter()
            .copied()
            .filter(|mode| target_modes.contains(mode))
            .collect::<Vec<_>>();
        let removed_modes = current_modes
            .iter()
            .copied()
            .filter(|mode| !target_modes.contains(mode))
            .collect::<Vec<_>>();

        self.workspace.rebind(view, binding, content, state)?;
        if document_rebind {
            self.dispatcher.invalidate_view(
                view,
                &previous_view,
                &mut self.view_modes,
                content_modes,
                contents,
            );
            let content_context = ModeContentContext::new(content, contents);
            for mode in &retained_modes {
                let instance = self
                    .view_modes
                    .instance(*mode, view)
                    .expect("retained Mode is attached to the View");
                content_modes.attach_retained_view_with_context(
                    content,
                    instance,
                    &content_context,
                );
            }
            for mode in &current_modes {
                content_modes.detach_view(previous_content, *mode);
            }
            for mode in &removed_modes {
                self.view_modes.remove_mode(view, *mode);
            }
        }
        let rebound_view = self
            .workspace
            .view(view)
            .expect("rebound View remains installed")
            .clone();
        self.install_attachment_plan(
            plan,
            target_modes,
            rebound_view,
            registry,
            content_modes,
            contents,
        );
        if !self
            .workspace
            .views()
            .values()
            .any(|view| view.references(previous_content))
        {
            self.faces.remove_content_remaps(previous_content);
        }
        for mode in removed_modes {
            if !self.view_modes.contains_mode(mode) {
                self.faces.remove_mode_remaps(mode);
            }
        }
        if self.view_for_space(self.workspace.focused()) == Some(view) {
            self.sync_changed_input_source(content_modes, contents);
        }
        Ok(previous_content)
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "DiffView rebind receives independent app-owned stores"
    )]
    pub(super) fn rebind_diff_right(
        &mut self,
        parent: ViewId,
        content: ContentId,
        registry: &ModeRegistry,
        classifier: &ContentClassifier,
        content_modes: &mut ModeContentStore,
        contents: &ContentStore,
    ) -> Result<(ContentId, ViewId), LayoutError> {
        if !contents.contains(content) {
            return Err(LayoutError::MissingContent(content));
        }
        let previous_parent = self
            .workspace
            .view(parent)
            .ok_or(LayoutError::MissingView(parent))?
            .clone();
        let [_, right] = previous_parent.children() else {
            return Err(LayoutError::InvalidWorkspace(format!(
                "DiffView {} does not own exactly two children",
                parent.0
            )));
        };
        let right = *right;
        let previous_right = self
            .workspace
            .view(right)
            .ok_or(LayoutError::MissingView(right))?
            .clone();
        let previous_content = previous_parent
            .binding(vell_protocol::view::RIGHT_BINDING)
            .ok_or_else(|| LayoutError::MissingBinding {
                view: parent,
                binding: BindingKey::new(vell_protocol::view::RIGHT_BINDING),
            })?;
        if previous_content == content {
            return Ok((previous_content, right));
        }
        let state = contents
            .create_view_state(content)
            .ok_or(LayoutError::MissingContent(content))?;
        let mut planned_parent = previous_parent.clone();
        planned_parent
            .rebind(
                &BindingKey::new(vell_protocol::view::RIGHT_BINDING),
                content,
                None,
            )
            .map_err(|error| LayoutError::ModeAttachment(format!("{error:?}")))?;
        let mut planned_right = previous_right.clone();
        planned_right
            .rebind(
                &BindingKey::new(DOCUMENT_BINDING),
                content,
                Some(state.clone()),
            )
            .map_err(|error| LayoutError::ModeAttachment(format!("{error:?}")))?;

        let parent_plan = self
            .mode_resolver
            .resolve(parent, &planned_parent, registry, classifier, contents)
            .map_err(|error| LayoutError::ModeAttachment(error.to_string()))?;
        let parent_target = self
            .validate_attachment_plan_for_view(&parent_plan, &planned_parent, registry, contents)
            .map_err(|error| LayoutError::ModeAttachment(error.to_string()))?;
        let right_plan = self
            .mode_resolver
            .resolve(right, &planned_right, registry, classifier, contents)
            .map_err(|error| LayoutError::ModeAttachment(error.to_string()))?;
        let right_target = self
            .validate_attachment_plan_for_view(&right_plan, &planned_right, registry, contents)
            .map_err(|error| LayoutError::ModeAttachment(error.to_string()))?;
        let current_modes = self.view_modes.mode_ids(right).to_vec();
        let retained_modes = current_modes
            .iter()
            .copied()
            .filter(|mode| right_target.contains(mode))
            .collect::<Vec<_>>();
        let removed_modes = current_modes
            .iter()
            .copied()
            .filter(|mode| !right_target.contains(mode))
            .collect::<Vec<_>>();

        self.workspace.rebind_diff_right(parent, content, state)?;
        self.dispatcher.invalidate_view(
            right,
            &previous_right,
            &mut self.view_modes,
            content_modes,
            contents,
        );
        let content_context = ModeContentContext::new(content, contents);
        for mode in &retained_modes {
            let instance = self
                .view_modes
                .instance(*mode, right)
                .expect("retained Mode is attached to the right child");
            content_modes.attach_retained_view_with_context(content, instance, &content_context);
        }
        for mode in &current_modes {
            content_modes.detach_view(previous_content, *mode);
        }
        for mode in &removed_modes {
            self.view_modes.remove_mode(right, *mode);
        }
        let rebound_right = self
            .workspace
            .view(right)
            .expect("rebound right child remains installed")
            .clone();
        self.install_attachment_plan(
            right_plan,
            right_target,
            rebound_right,
            registry,
            content_modes,
            contents,
        );
        let rebound_parent = self
            .workspace
            .view(parent)
            .expect("rebound DiffView parent remains installed")
            .clone();
        self.install_attachment_plan(
            parent_plan,
            parent_target,
            rebound_parent,
            registry,
            content_modes,
            contents,
        );
        if !self
            .workspace
            .views()
            .values()
            .any(|view| view.references(previous_content))
        {
            self.faces.remove_content_remaps(previous_content);
        }
        for mode in removed_modes {
            if !self.view_modes.contains_mode(mode) {
                self.faces.remove_mode_remaps(mode);
            }
        }
        if self.view_for_space(self.workspace.focused()) == Some(right) {
            self.sync_changed_input_source(content_modes, contents);
        }
        Ok((previous_content, right))
    }

    pub(super) fn close_content_views(
        &mut self,
        content: ContentId,
        replacement: Option<NewView>,
        registry: &ModeRegistry,
        classifier: &ContentClassifier,
        content_modes: &mut ModeContentStore,
        contents: &ContentStore,
    ) -> Result<SessionWorkspaceMutation<Option<ViewId>>, LayoutError> {
        let previous_focus = self.workspace.focused();
        let previous_view = self
            .view_for_space(previous_focus)
            .expect("focused space hosts a view");
        let planned_view = self.workspace.next_view_id();
        let (replacement, plan) = match replacement {
            Some(replacement) => {
                let plan = self
                    .mode_resolver
                    .resolve(
                        planned_view,
                        &replacement.view,
                        registry,
                        classifier,
                        contents,
                    )
                    .map_err(|error| LayoutError::ModeAttachment(error.to_string()))?;
                (Some(replacement.view), Some(plan))
            }
            None => (None, None),
        };
        let workspace_before = self.workspace.clone();
        let mutation = self.workspace.close_content_views(content, replacement)?;
        if let (Some(view), Some(plan)) = (mutation.output, plan) {
            assert_eq!(view, planned_view, "workspace allocated the planned ViewId");
            if let Err(error) = self.apply_attachment_plan(plan, registry, content_modes, contents)
            {
                self.workspace = workspace_before;
                return Err(LayoutError::ModeAttachment(error.to_string()));
            }
        }
        let removed = self.cleanup_removed_views(mutation.removed, content_modes, contents);
        if removed.iter().any(|removed| removed.view == previous_view) {
            self.sync_changed_input_source(content_modes, contents);
        }
        Ok(SessionWorkspaceMutation {
            output: mutation.output,
            removed,
        })
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "session mutation receives independent app-owned stores"
    )]
    pub(super) fn replace_space_content(
        &mut self,
        target: SpaceId,
        view: NewView,
        focusable: bool,
        registry: &ModeRegistry,
        classifier: &ContentClassifier,
        content_modes: &mut ModeContentStore,
        contents: &ContentStore,
    ) -> Result<SessionWorkspaceMutation<ViewId>, LayoutError> {
        let previous_focus = self.workspace.focused();
        let previous_view = self
            .view_for_space(previous_focus)
            .expect("focused space hosts a view");
        let NewView { view } = view;
        let planned_view = self.workspace.next_view_id();
        let plan = self
            .mode_resolver
            .resolve(planned_view, &view, registry, classifier, contents)
            .map_err(|error| LayoutError::ModeAttachment(error.to_string()))?;
        let workspace_before = self.workspace.clone();
        let mutation = self.workspace.replace(target, view, focusable)?;
        assert_eq!(
            mutation.output, planned_view,
            "workspace allocated the planned ViewId"
        );
        if let Err(error) = self.apply_attachment_plan(plan, registry, content_modes, contents) {
            self.workspace = workspace_before;
            return Err(LayoutError::ModeAttachment(error.to_string()));
        }
        let removed = self.cleanup_removed_views(mutation.removed, content_modes, contents);
        if removed.iter().any(|removed| removed.view == previous_view) {
            self.sync_changed_input_source(content_modes, contents);
        }
        Ok(SessionWorkspaceMutation {
            output: mutation.output,
            removed,
        })
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "compound View creation receives independent app-owned stores"
    )]
    pub(super) fn prepare_diff_replacement(
        &self,
        target: SpaceId,
        parent: View,
        left: NewView,
        right: NewView,
        registry: &ModeRegistry,
        classifier: &ContentClassifier,
        contents: &ContentStore,
    ) -> Result<PreparedDiffReplacement, LayoutError> {
        let previous_view = self
            .view_for_space(self.workspace.focused())
            .expect("focused space hosts a view");
        let mut candidate = self.workspace.clone();
        let mutation = candidate.replace_with_diff(target, parent, left.view, right.view)?;
        let created = [
            mutation.output.parent,
            mutation.output.left,
            mutation.output.right,
        ];
        let mut attachments = Vec::with_capacity(created.len());
        for view in created {
            let view_data = candidate
                .view(view)
                .expect("candidate workspace contains created DiffView subtree")
                .clone();
            let plan = self
                .mode_resolver
                .resolve(view, &view_data, registry, classifier, contents)
                .map_err(|error| LayoutError::ModeAttachment(error.to_string()))?;
            let target = self
                .validate_attachment_plan_for_view(&plan, &view_data, registry, contents)
                .map_err(|error| LayoutError::ModeAttachment(error.to_string()))?;
            attachments.push(PreparedAttachment {
                plan,
                target,
                view: view_data,
            });
        }

        Ok(PreparedDiffReplacement {
            workspace: candidate,
            mutation,
            attachments,
            previous_view,
        })
    }

    pub(super) fn publish_diff_replacement(
        &mut self,
        prepared: PreparedDiffReplacement,
        registry: &ModeRegistry,
        content_modes: &mut ModeContentStore,
        contents: &ContentStore,
    ) -> SessionWorkspaceMutation<DiffViewResult> {
        let PreparedDiffReplacement {
            workspace,
            mutation,
            attachments,
            previous_view,
        } = prepared;
        self.workspace = workspace;
        for PreparedAttachment { plan, target, view } in attachments {
            self.install_attachment_plan(plan, target, view, registry, content_modes, contents);
        }
        let removed = self.cleanup_removed_views(mutation.removed, content_modes, contents);
        if removed.iter().any(|removed| removed.view == previous_view) {
            self.sync_changed_input_source(content_modes, contents);
        }
        SessionWorkspaceMutation {
            output: mutation.output,
            removed,
        }
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
        content_modes: &mut ModeContentStore,
        contents: &ContentStore,
    ) {
        self.sync_focused_input(Instant::now(), content_modes, contents);
    }

    fn cleanup_removed_views(
        &mut self,
        removed: Vec<RemovedView>,
        mode_contents: &mut ModeContentStore,
        contents: &ContentStore,
    ) -> Vec<RemovedSessionView> {
        let mut removed_contents = HashSet::new();
        let mut removed_modes = HashSet::new();
        let mut result = Vec::with_capacity(removed.len());
        for RemovedView { id, view } in removed {
            self.dispatcher.invalidate_view(
                id,
                &view,
                &mut self.view_modes,
                mode_contents,
                contents,
            );
            self.faces.remove_view_remaps(id);
            self.mode_resolver.forget_view(id);
            for mode in self.view_modes.remove(id) {
                if let Some(content) = view.document_content() {
                    mode_contents.detach_view(content, mode);
                }
                removed_modes.insert(mode);
            }
            if let Some(content) = view.document_content() {
                removed_contents.insert(content);
            }
            result.push(RemovedSessionView {
                view: id,
                document: view.document_content(),
            });
        }
        for content in removed_contents {
            if !self
                .workspace
                .views()
                .values()
                .any(|view| view.references(content))
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
