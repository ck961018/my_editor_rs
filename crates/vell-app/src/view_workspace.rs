//! View 与 Scene 的结构工作区。
//!
//! `ViewWorkspace` 是 View 语义树、直属 Pane、Scene 快照、焦点与结构 ID 的
//! 唯一所有者。结构变更先在完整副本上完成并校验，成功后才一次发布；调用方
//! 只消费被移除 View 的生命周期事件，不参与 Space 或 Pane 的分步清理。

use std::collections::{HashMap, HashSet};

use crate::layout::{LayoutError, StatusBarHandle, StatusBarPlacement};
use crate::mode::CompoundViewDefinition;
use crate::scene_model::{CloseResult, SceneBuilder, SceneError, SplitResult, build_editor_scene};
use crate::view::{BODY_PANE, ContentBindingError, STATUS_PANE, View};
use vell_core::content_view_state::ContentViewState;
use vell_protocol::ids::{ContentId, SpaceId, ViewId};
use vell_protocol::revision::Revision;
use vell_protocol::scene::Scene;
use vell_protocol::space::{Sizing, SpaceKind, SplitDirection};
use vell_protocol::view::{
    BUFFER_VIEW_DEFINITION, BindingKey, DIFF_VIEW_DEFINITION, LEFT_BINDING, RIGHT_BINDING,
};

#[derive(Clone)]
pub(super) struct ViewWorkspace {
    scene: Scene,
    scene_builder: SceneBuilder,
    scene_revision: Revision,
    views: HashMap<ViewId, View>,
    next_view_id: u64,
    focused: SpaceId,
    status_placement: StatusBarPlacement,
    /// Global 布局下唯一的状态栏 Space。它始终归当前焦点 View 所有。
    global_status_space: Option<SpaceId>,
    /// PerPane 布局下每个 View 的状态栏 Space。
    status_by_editor: HashMap<ViewId, SpaceId>,
}

pub(super) struct RemovedView {
    pub id: ViewId,
    pub view: View,
}

pub(super) struct WorkspaceMutation<T> {
    pub output: T,
    pub removed: Vec<RemovedView>,
}

pub(super) struct CompoundViewResult {
    pub parent: ViewId,
    pub children: [ViewId; 2],
}

impl ViewWorkspace {
    pub(super) fn editor(
        width: usize,
        height: usize,
        editor_id: ViewId,
        mut editor: View,
        next_view_id: u64,
    ) -> Self {
        let mut scene_builder = SceneBuilder::new();
        let (scene, editor_space, status_space) =
            build_editor_scene(&mut scene_builder, width as i32, height as i32, editor_id);
        editor.assign_pane(editor_space, BODY_PANE);
        editor.assign_pane(status_space, STATUS_PANE);
        let focused = resolve_focus(&scene, editor_space, Some(editor_space))
            .expect("initial scene has a focusable content space");
        let workspace = Self {
            scene,
            scene_builder,
            scene_revision: Revision::default(),
            views: HashMap::from([(editor_id, editor)]),
            next_view_id,
            focused,
            status_placement: StatusBarPlacement::Global,
            global_status_space: Some(status_space),
            status_by_editor: HashMap::new(),
        };
        debug_assert!(workspace.validate().is_ok());
        workspace
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

    pub(super) fn view(&self, view: ViewId) -> Option<&View> {
        self.views.get(&view)
    }

    pub(super) fn view_mut(&mut self, view: ViewId) -> Option<&mut View> {
        self.views.get_mut(&view)
    }

    pub(super) fn views_mut(&mut self) -> impl Iterator<Item = (&ViewId, &mut View)> {
        self.views.iter_mut()
    }

    pub(super) fn next_view_id(&self) -> ViewId {
        ViewId(self.next_view_id)
    }

    pub(super) fn status_bar_placement(&self) -> StatusBarPlacement {
        self.status_placement
    }

    pub(super) fn status_bar_for_view(&self, editor: ViewId) -> Option<StatusBarHandle> {
        if !self.views.contains_key(&editor) {
            return None;
        }
        let space = match self.status_placement {
            StatusBarPlacement::Global => self.global_status_space?,
            StatusBarPlacement::PerPane => *self.status_by_editor.get(&editor)?,
        };
        let target_view = view_for_space(&self.scene, space)?;
        let content = self.views.get(&target_view)?.document_content()?;
        Some(StatusBarHandle {
            space,
            target_view,
            content,
        })
    }

    pub(super) fn status_bars_for_content(&self, content: ContentId) -> Vec<StatusBarHandle> {
        let mut bars = self
            .views
            .iter()
            .filter_map(|(view, data)| {
                (data.document_content() == Some(content))
                    .then(|| self.status_bar_for_view(*view))
                    .flatten()
            })
            .collect::<Vec<_>>();
        bars.sort_by_key(|bar| bar.space.0);
        bars.dedup_by_key(|bar| bar.space);
        bars
    }

    pub(super) fn install_extension_pane(
        &mut self,
        view: ViewId,
        key: &str,
        side: crate::mode::ViewExtensionPaneSide,
        size: u16,
    ) -> Result<SpaceId, LayoutError> {
        if matches!(key, BODY_PANE | STATUS_PANE) {
            return Err(LayoutError::InvalidWorkspace(format!(
                "view extension pane key '{key}' is reserved"
            )));
        }
        let target = self
            .views
            .get(&view)
            .ok_or(LayoutError::MissingView(view))?;
        if let Some(space) = target.panes().space_for_key(key) {
            return Ok(space);
        }
        let anchor = self.replacement_space_for_view(view).ok_or_else(|| {
            LayoutError::InvalidWorkspace(format!(
                "View {} has no Pane anchor for extension '{key}'",
                view.0
            ))
        })?;
        let direction = match side {
            crate::mode::ViewExtensionPaneSide::Left => SplitDirection::Left,
            crate::mode::ViewExtensionPaneSide::Right => SplitDirection::Right,
            crate::mode::ViewExtensionPaneSide::Above => SplitDirection::Up,
            crate::mode::ViewExtensionPaneSide::Below => SplitDirection::Down,
        };
        let mut draft = self.clone();
        let result = draft
            .scene_builder
            .split(&mut draft.scene, anchor, view, false, direction)?;
        draft.scene_builder.set_sizing(
            &mut draft.scene,
            result.new_space,
            Sizing::Fixed(i32::from(size)),
        )?;
        draft
            .views
            .get_mut(&view)
            .expect("extension target View was prevalidated")
            .assign_pane(result.new_space, key);
        draft.scene_revision.next();
        draft.validate()?;
        *self = draft;
        Ok(result.new_space)
    }

    pub(super) fn remove_extension_panes(
        &mut self,
        keys: &HashSet<String>,
    ) -> Result<usize, LayoutError> {
        if keys.is_empty() {
            return Ok(0);
        }
        let mut targets = self
            .views
            .iter()
            .flat_map(|(view, data)| {
                data.panes().spaces().filter_map(|space| {
                    let key = data.panes().key_for_space(space)?;
                    keys.contains(key).then_some((*view, space, key.to_owned()))
                })
            })
            .collect::<Vec<_>>();
        if targets.is_empty() {
            return Ok(0);
        }
        targets.sort_by_key(|(_, space, _)| std::cmp::Reverse(space.0));
        let mut draft = self.clone();
        for (view, space, key) in &targets {
            if matches!(key.as_str(), BODY_PANE | STATUS_PANE) {
                return Err(LayoutError::InvalidWorkspace(format!(
                    "cannot remove reserved Pane '{key}' as a view extension"
                )));
            }
            draft.scene_builder.close(&mut draft.scene, *space)?;
            let removed = draft
                .views
                .get_mut(view)
                .ok_or(LayoutError::MissingView(*view))?
                .release_pane_space(*space);
            if removed.as_deref() != Some(key) {
                return Err(LayoutError::InvalidWorkspace(
                    "extension Pane ownership changed during removal".to_owned(),
                ));
            }
        }
        draft.reconcile_layout(Some(draft.focused))?;
        draft.sync_global_status_target()?;
        draft.scene_revision.next();
        draft.validate()?;
        *self = draft;
        Ok(targets.len())
    }

    pub(super) fn set_status_bar_placement(
        &mut self,
        placement: StatusBarPlacement,
    ) -> Result<(), LayoutError> {
        if placement == self.status_placement {
            return Ok(());
        }
        let mut draft = self.clone();
        draft.set_status_bar_placement_in_place(placement)?;
        draft.scene_revision.next();
        draft.validate()?;
        *self = draft;
        Ok(())
    }

    fn set_status_bar_placement_in_place(
        &mut self,
        placement: StatusBarPlacement,
    ) -> Result<(), LayoutError> {
        match placement {
            StatusBarPlacement::PerPane => {
                let global_space = self.global_status_space.ok_or(LayoutError::NoStatusBar)?;
                let global_target = view_for_space(&self.scene, global_space)
                    .ok_or(SceneError::ExpectedContentLeaf(global_space))?;
                self.scene_builder.close(&mut self.scene, global_space)?;
                self.views
                    .get_mut(&global_target)
                    .ok_or(LayoutError::MissingView(global_target))?
                    .release_pane_space(global_space);
                self.global_status_space = None;
                let editors = scene_views(&self.scene)
                    .into_iter()
                    .filter(|(space, view)| {
                        self.views[view].panes().key_for_space(*space) == Some(BODY_PANE)
                    })
                    .collect::<Vec<_>>();
                self.status_by_editor.clear();
                for (editor_space, editor_view) in editors {
                    let pane = self.scene_builder.wrap_with_status(
                        &mut self.scene,
                        editor_space,
                        editor_view,
                    )?;
                    self.views
                        .get_mut(&editor_view)
                        .ok_or(LayoutError::MissingView(editor_view))?
                        .assign_pane(pane.status_space, STATUS_PANE);
                    self.status_by_editor.insert(editor_view, pane.status_space);
                }
            }
            StatusBarPlacement::Global => {
                let focused_editor = self
                    .view_for_space(self.focused)
                    .ok_or(LayoutError::NoFocusableSpace)?;
                let bars = std::mem::take(&mut self.status_by_editor);
                for (editor, space) in bars {
                    self.scene_builder.close(&mut self.scene, space)?;
                    if let Some(view) = self.views.get_mut(&editor) {
                        view.release_pane_space(space);
                    }
                }
                let status_space = self
                    .scene_builder
                    .attach_global_status(&mut self.scene, focused_editor)?;
                self.views
                    .get_mut(&focused_editor)
                    .ok_or(LayoutError::MissingView(focused_editor))?
                    .assign_pane(status_space, STATUS_PANE);
                self.global_status_space = Some(status_space);
            }
        }
        self.status_placement = placement;
        self.reconcile_layout(Some(self.focused))?;
        Ok(())
    }

    pub(super) fn set_status_bar_visible(
        &mut self,
        editor: Option<ViewId>,
        visible: bool,
    ) -> Result<(), LayoutError> {
        let mut draft = self.clone();
        let space = match draft.status_placement {
            StatusBarPlacement::Global => draft.global_status_space,
            StatusBarPlacement::PerPane => {
                editor.and_then(|editor| draft.status_by_editor.get(&editor).copied())
            }
        }
        .ok_or(LayoutError::NoStatusBar)?;
        draft.scene_builder.set_sizing(
            &mut draft.scene,
            space,
            if visible {
                Sizing::Fixed(1)
            } else {
                Sizing::Fixed(0)
            },
        )?;
        draft.scene_revision.next();
        draft.validate()?;
        *self = draft;
        Ok(())
    }

    pub(super) fn view_for_space(&self, space: SpaceId) -> Option<ViewId> {
        view_for_space(&self.scene, space)
    }

    pub(super) fn switch_target(&self, space: SpaceId) -> Option<ViewId> {
        resolve_switch_target(&self.scene, &self.views, space)
    }

    pub(super) fn switch_target_from_view(&self, view: ViewId) -> Option<ViewId> {
        resolve_switch_target_from_view(&self.views, view)
    }

    pub(super) fn body_space_for_view(&self, view: ViewId) -> Option<SpaceId> {
        self.views.get(&view)?.panes().space_for_key(BODY_PANE)
    }

    pub(super) fn replacement_space_for_view(&self, view: ViewId) -> Option<SpaceId> {
        if let Some(space) = self.body_space_for_view(view) {
            return Some(space);
        }
        let subtree = self.semantic_subtree(view).ok()?;
        if subtree.contains(&self.view_for_space(self.focused)?)
            && self.views[&self.view_for_space(self.focused)?]
                .panes()
                .key_for_space(self.focused)
                == Some(BODY_PANE)
        {
            return Some(self.focused);
        }
        subtree
            .into_iter()
            .find_map(|child| self.body_space_for_view(child))
    }

    /// 原子改变一个具名 binding。Scene 和 ViewId 均保持不变。
    pub(super) fn rebind(
        &mut self,
        view: ViewId,
        binding: &BindingKey,
        content: ContentId,
        state: Option<ContentViewState>,
    ) -> Result<ContentId, LayoutError> {
        let mut draft = self.clone();
        let previous =
            draft
                .views
                .get_mut(&view)
                .ok_or(LayoutError::MissingView(view))?
                .rebind(binding, content, state)
                .map_err(|error| match error {
                    ContentBindingError::Unknown(binding) => {
                        LayoutError::MissingBinding { view, binding }
                    }
                    ContentBindingError::Duplicate(binding)
                    | ContentBindingError::Missing(binding) => LayoutError::InvalidWorkspace(
                        format!("View {} has invalid binding schema at {binding}", view.0),
                    ),
                    ContentBindingError::DocumentStateMismatch => LayoutError::InvalidWorkspace(
                        format!("View {} has invalid document state", view.0),
                    ),
                })?;
        draft.validate()?;
        *self = draft;
        Ok(previous)
    }

    pub(super) fn rebind_compound_binding(
        &mut self,
        parent: ViewId,
        definition: &CompoundViewDefinition,
        binding: &BindingKey,
        content: ContentId,
        state: ContentViewState,
    ) -> Result<(ContentId, ViewId), LayoutError> {
        let children = self.validate_compound_view(parent, definition)?;
        let (child_index, child_binding) = definition
            .child_binding_for_parent(binding)
            .ok_or_else(|| LayoutError::MissingBinding {
                view: parent,
                binding: binding.clone(),
            })?;
        let child = children[child_index];
        let previous = self
            .views
            .get(&parent)
            .expect("compound View parent was prevalidated")
            .binding(binding.as_str())
            .ok_or_else(|| LayoutError::MissingBinding {
                view: parent,
                binding: binding.clone(),
            })?;
        if previous == content {
            return Ok((previous, child));
        }

        let mut draft = self.clone();
        draft
            .views
            .get_mut(&parent)
            .expect("compound View parent was prevalidated")
            .rebind(binding, content, None)
            .map_err(|error| LayoutError::InvalidWorkspace(format!("{error:?}")))?;
        draft
            .views
            .get_mut(&child)
            .expect("compound View child was prevalidated")
            .rebind(child_binding, content, Some(state))
            .map_err(|error| LayoutError::InvalidWorkspace(format!("{error:?}")))?;
        draft.validate()?;
        draft.validate_compound_view(parent, definition)?;
        *self = draft;
        Ok((previous, child))
    }

    pub(super) fn validate_compound_view(
        &self,
        parent: ViewId,
        definition: &CompoundViewDefinition,
    ) -> Result<[ViewId; 2], LayoutError> {
        let parent_view = self
            .views
            .get(&parent)
            .ok_or(LayoutError::MissingView(parent))?;
        let [first, second] = parent_view.children() else {
            return Err(LayoutError::InvalidWorkspace(format!(
                "compound View {} does not own exactly two children",
                parent.0
            )));
        };
        let children = [*first, *second];
        let invalid = |message: String| {
            LayoutError::InvalidWorkspace(format!(
                "compound View '{}' {message}",
                definition.definition().id()
            ))
        };
        if parent_view.definition() != definition.definition().id() || !parent_view.switchable() {
            return Err(invalid("has an invalid semantic root".to_owned()));
        }
        for (index, child_definition) in definition.children().iter().enumerate() {
            let child = self
                .views
                .get(&children[index])
                .ok_or(LayoutError::MissingView(children[index]))?;
            if child.definition() != child_definition.definition()
                || child.switchable()
                || child.parent() != Some(parent)
            {
                return Err(invalid(format!(
                    "child '{}' has an invalid View identity",
                    child_definition.key()
                )));
            }
            for binding in child_definition.bindings() {
                if child.binding(binding.child().as_str())
                    != parent_view.binding(binding.parent().as_str())
                {
                    return Err(invalid(format!(
                        "child '{}' binding '{}' is inconsistent with parent binding '{}'",
                        child_definition.key(),
                        binding.child(),
                        binding.parent()
                    )));
                }
            }
        }
        Ok(children)
    }

    pub(super) fn validate_new_compound_view(
        &self,
        parent: ViewId,
        definition: &CompoundViewDefinition,
    ) -> Result<[ViewId; 2], LayoutError> {
        let children = self.validate_compound_view(parent, definition)?;
        if self
            .views
            .get(&parent)
            .expect("compound View parent was prevalidated")
            .panes()
            .spaces()
            .next()
            .is_some()
        {
            return Err(LayoutError::InvalidWorkspace(format!(
                "new compound View '{}' must not own a recipe Pane",
                definition.definition().id()
            )));
        }
        Ok(children)
    }

    /// 返回关闭 Content 后仍会存活的第一个 binding 引用。
    pub(super) fn blocking_content_reference(
        &self,
        content: ContentId,
    ) -> Result<Option<(ViewId, BindingKey)>, LayoutError> {
        let mut removed = HashSet::new();
        for root in self.content_removal_roots(content)? {
            removed.extend(self.semantic_subtree(root)?);
        }
        let mut views = self.views.iter().collect::<Vec<_>>();
        views.sort_by_key(|(view, _)| view.0);
        Ok(views.into_iter().find_map(|(view, data)| {
            if removed.contains(view) {
                return None;
            }
            data.bindings()
                .iter()
                .find_map(|(binding, target)| (target == content).then(|| (*view, binding.clone())))
        }))
    }

    /// 建立一棵已存在 View 的语义子树。布局 Pane 可以先创建，但 parent、
    /// children 与子 View 的 switchable 属性必须在同一次发布中生效。
    pub(super) fn compose(
        &mut self,
        parent: ViewId,
        children: &[(ViewId, bool)],
    ) -> Result<(), LayoutError> {
        let parent_view = self
            .views
            .get(&parent)
            .ok_or(LayoutError::MissingView(parent))?;
        if !parent_view.children().is_empty() {
            return Err(LayoutError::InvalidWorkspace(
                "compound View parent already owns children".to_owned(),
            ));
        }
        let mut unique = HashSet::new();
        for (child, _) in children {
            if *child == parent || !unique.insert(*child) {
                return Err(LayoutError::InvalidWorkspace(
                    "compound View children must be distinct from the parent".to_owned(),
                ));
            }
            let child_view = self
                .views
                .get(child)
                .ok_or(LayoutError::MissingView(*child))?;
            if child_view.parent().is_some() {
                return Err(LayoutError::InvalidWorkspace(format!(
                    "View {} already has a semantic parent",
                    child.0
                )));
            }
        }

        let mut draft = self.clone();
        for (child, switchable) in children {
            let child_view = draft
                .views
                .get_mut(child)
                .expect("composition children were prevalidated");
            child_view.set_parent(Some(parent));
            child_view.set_switchable(*switchable);
            draft
                .views
                .get_mut(&parent)
                .expect("composition parent was prevalidated")
                .push_child(*child);
        }
        draft.scene_revision.next();
        draft.validate()?;
        *self = draft;
        Ok(())
    }

    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "M5 Native DiffView will configure replacement boundaries"
        )
    )]
    pub(super) fn set_switchable(
        &mut self,
        view: ViewId,
        switchable: bool,
    ) -> Result<(), LayoutError> {
        let mut draft = self.clone();
        draft
            .views
            .get_mut(&view)
            .ok_or(LayoutError::MissingView(view))?
            .set_switchable(switchable);
        draft.scene_revision.next();
        draft.validate()?;
        *self = draft;
        Ok(())
    }

    pub(super) fn resize(&mut self, width: u16, height: u16) {
        self.scene.size.width = width as i32;
        self.scene.size.height = height as i32;
        self.scene_revision.next();
    }

    pub(super) fn focus(&mut self, target: SpaceId) -> Result<bool, LayoutError> {
        if view_space_focusable(&self.scene, target) != Some(true) {
            return Err(LayoutError::NoFocusableSpace);
        }
        if target == self.focused {
            return Ok(false);
        }
        let mut draft = self.clone();
        draft.focused = target;
        draft.sync_global_status_target()?;
        draft.scene_revision.next();
        draft.validate()?;
        *self = draft;
        Ok(true)
    }

    pub(super) fn is_focusable(&self, target: SpaceId) -> bool {
        view_space_focusable(&self.scene, target) == Some(true)
    }

    pub(super) fn split(
        &mut self,
        target: SpaceId,
        mut view: View,
        focusable: bool,
        direction: SplitDirection,
        focus_new: bool,
    ) -> Result<(ViewId, SplitResult), LayoutError> {
        let mut draft = self.clone();
        draft.reject_non_body_space(target)?;
        let previous = draft.focused;
        let id = draft.alloc_view_id();
        let result = match draft.status_placement {
            StatusBarPlacement::Global => {
                let result = draft.scene_builder.split(
                    &mut draft.scene,
                    target,
                    id,
                    focusable,
                    direction,
                )?;
                view.assign_pane(result.new_space, BODY_PANE);
                result
            }
            StatusBarPlacement::PerPane => {
                let target_pane = draft
                    .scene
                    .node(target)
                    .parent
                    .ok_or(SceneError::InvalidTree)?;
                let pane = draft.scene_builder.split_pane(
                    &mut draft.scene,
                    target_pane,
                    id,
                    id,
                    focusable,
                    direction,
                )?;
                view.assign_pane(pane.editor_space, BODY_PANE);
                view.assign_pane(pane.status_space, STATUS_PANE);
                draft.status_by_editor.insert(id, pane.status_space);
                SplitResult {
                    new_space: pane.editor_space,
                }
            }
        };
        assert!(
            draft.views.insert(id, view).is_none(),
            "view id must be unique"
        );
        draft.reconcile_layout(Some(if focus_new {
            result.new_space
        } else {
            previous
        }))?;
        draft.sync_global_status_target()?;
        draft.scene_revision.next();
        draft.validate()?;
        *self = draft;
        Ok((id, result))
    }

    pub(super) fn replace_with_compound(
        &mut self,
        target: SpaceId,
        parent: View,
        children: [View; 2],
        direction: SplitDirection,
    ) -> Result<WorkspaceMutation<CompoundViewResult>, LayoutError> {
        let mut draft = self.clone();
        let base_revision = draft.scene_revision;
        let [first, second] = children;
        let replacement = draft.replace(target, first, true)?;
        let first = replacement.output;
        let first_space = draft
            .body_space_for_view(first)
            .expect("new compound child owns its body Pane");
        let (second, _) = draft.split(first_space, second, true, direction, false)?;
        let parent_id = draft.alloc_view_id();
        assert!(
            draft.views.insert(parent_id, parent).is_none(),
            "view id must be unique"
        );
        draft.compose(parent_id, &[(first, false), (second, false)])?;
        draft.scene_revision = base_revision;
        draft.scene_revision.next();
        draft.validate()?;
        *self = draft;
        Ok(WorkspaceMutation {
            output: CompoundViewResult {
                parent: parent_id,
                children: [first, second],
            },
            removed: replacement.removed,
        })
    }

    pub(super) fn close(
        &mut self,
        target: SpaceId,
    ) -> Result<WorkspaceMutation<CloseResult>, LayoutError> {
        let mut draft = self.clone();
        let root = draft.validate_subtree_target(target)?;
        let subtree = draft.semantic_subtree(root)?;
        let subtree_set = subtree.iter().copied().collect::<HashSet<_>>();
        if !scene_views(&draft.scene).into_iter().any(|(space, view)| {
            !subtree_set.contains(&view) && view_space_focusable(&draft.scene, space) == Some(true)
        }) {
            return Err(LayoutError::WouldRemoveLastFocusable(target));
        }

        let previous_focus = draft.focused;
        let focused_removed = draft
            .view_for_space(previous_focus)
            .is_some_and(|view| subtree_set.contains(&view));
        let preserved_global = draft.global_status_space.filter(|space| {
            draft
                .view_for_space(*space)
                .is_some_and(|view| subtree_set.contains(&view))
        });
        let preserved = preserved_global.into_iter().collect::<HashSet<_>>();
        for space in draft.ordered_subtree_spaces(&subtree, target, &preserved, false)? {
            draft.scene_builder.close(&mut draft.scene, space)?;
        }
        let result = draft.scene_builder.close(&mut draft.scene, target)?;
        draft
            .status_by_editor
            .retain(|view, _| !subtree_set.contains(view));
        let removed = draft.remove_semantic_subtree(root, &subtree)?;
        let preferred = if focused_removed {
            result.surviving_neighbor
        } else {
            Some(previous_focus)
        };
        draft.reconcile_layout(preferred)?;
        draft.sync_global_status_target()?;
        draft.scene_revision.next();
        draft.validate()?;
        *self = draft;
        Ok(WorkspaceMutation {
            output: result,
            removed,
        })
    }

    pub(super) fn validate_close(&self, target: SpaceId) -> Result<(), LayoutError> {
        let root = self.validate_subtree_target(target)?;
        let subtree = self.semantic_subtree(root)?;
        let subtree = subtree.into_iter().collect::<HashSet<_>>();
        if !scene_views(&self.scene).into_iter().any(|(space, view)| {
            !subtree.contains(&view) && view_space_focusable(&self.scene, space) == Some(true)
        }) {
            return Err(LayoutError::WouldRemoveLastFocusable(target));
        }
        Ok(())
    }

    pub(super) fn closing_content_needs_replacement(
        &self,
        content: ContentId,
    ) -> Result<bool, LayoutError> {
        let roots = self.content_removal_roots(content)?;
        if roots.is_empty() {
            return Ok(false);
        }
        let mut removed = HashSet::new();
        for root in roots {
            removed.extend(self.semantic_subtree(root)?);
        }
        Ok(!scene_views(&self.scene).into_iter().any(|(space, view)| {
            !removed.contains(&view) && view_space_focusable(&self.scene, space) == Some(true)
        }))
    }

    /// 删除引用同一 Content 的所有最小、不重叠语义子树。若这些子树覆盖
    /// 全部可聚焦 Pane，则在其中一个根位置安装 replacement。所有步骤只在
    /// workspace 草稿上发生，任一步失败都不会发布部分拓扑。
    pub(super) fn close_content_views(
        &mut self,
        content: ContentId,
        replacement: Option<View>,
    ) -> Result<WorkspaceMutation<Option<ViewId>>, LayoutError> {
        let roots = self.content_removal_roots(content)?;
        if roots.is_empty() {
            return Ok(WorkspaceMutation {
                output: None,
                removed: Vec::new(),
            });
        }
        let needs_replacement = self.closing_content_needs_replacement(content)?;
        if needs_replacement != replacement.is_some() {
            return Err(LayoutError::InvalidWorkspace(
                "content close replacement does not match focusability plan".to_owned(),
            ));
        }

        let replacement_root = if replacement.is_some() {
            let focused_view = self.view_for_space(self.focused);
            roots
                .iter()
                .copied()
                .find(|root| {
                    focused_view.is_some_and(|focused| {
                        self.semantic_subtree(*root)
                            .is_ok_and(|subtree| subtree.contains(&focused))
                    })
                })
                .or_else(|| roots.first().copied())
        } else {
            None
        };

        let base_revision = self.scene_revision;
        let mut draft = self.clone();
        let mut removed = Vec::new();
        let mut replacement_id = None;
        if let (Some(root), Some(replacement)) = (replacement_root, replacement) {
            let target = draft
                .replacement_space_for_view(root)
                .ok_or(LayoutError::MissingView(root))?;
            let mutation = draft.replace(target, replacement, true)?;
            replacement_id = Some(mutation.output);
            removed.extend(mutation.removed);
        }
        for root in roots {
            if Some(root) == replacement_root {
                continue;
            }
            let target = draft
                .replacement_space_for_view(root)
                .ok_or(LayoutError::MissingView(root))?;
            let mutation = draft.close(target)?;
            removed.extend(mutation.removed);
        }
        draft.scene_revision = base_revision;
        draft.scene_revision.next();
        draft.validate()?;
        *self = draft;
        Ok(WorkspaceMutation {
            output: replacement_id,
            removed,
        })
    }

    pub(super) fn replace(
        &mut self,
        target: SpaceId,
        mut replacement: View,
        focusable: bool,
    ) -> Result<WorkspaceMutation<ViewId>, LayoutError> {
        let mut draft = self.clone();
        let root = draft.validate_subtree_target(target)?;
        let subtree = draft.semantic_subtree(root)?;
        let subtree_set = subtree.iter().copied().collect::<HashSet<_>>();
        let focusable_outside = scene_views(&draft.scene).into_iter().any(|(space, view)| {
            !subtree_set.contains(&view) && view_space_focusable(&draft.scene, space) == Some(true)
        });
        if !focusable && !focusable_outside {
            return Err(LayoutError::NoFocusableSpace);
        }

        let previous_focus = draft.focused;
        let focused_replaced = draft
            .view_for_space(previous_focus)
            .is_some_and(|view| subtree_set.contains(&view));
        let target_view = draft
            .view_for_space(target)
            .ok_or(SceneError::ExpectedContentLeaf(target))?;
        let per_pane_status = (draft.status_placement == StatusBarPlacement::PerPane)
            .then(|| draft.status_by_editor.get(&target_view).copied())
            .flatten();
        let global_status = draft.global_status_space.filter(|space| {
            draft
                .view_for_space(*space)
                .is_some_and(|view| subtree_set.contains(&view))
        });
        let preserved = per_pane_status
            .into_iter()
            .chain(global_status)
            .collect::<HashSet<_>>();
        for space in draft.ordered_subtree_spaces(&subtree, target, &preserved, false)? {
            draft.scene_builder.close(&mut draft.scene, space)?;
        }

        let new_view = draft.alloc_view_id();
        draft
            .scene_builder
            .replace_view(&mut draft.scene, target, new_view, focusable)?;
        replacement.assign_pane(target, BODY_PANE);
        if let Some(status) = per_pane_status {
            draft
                .scene_builder
                .replace_view(&mut draft.scene, status, new_view, false)?;
            replacement.assign_pane(status, STATUS_PANE);
        }

        let parent = draft.views[&root].parent();
        if let Some(parent) = parent {
            replacement.set_parent(Some(parent));
            let parent_view = draft
                .views
                .get_mut(&parent)
                .ok_or(LayoutError::MissingView(parent))?;
            if !parent_view.replace_child(root, new_view) {
                return Err(LayoutError::InvalidWorkspace(
                    "semantic parent does not own replacement root".to_owned(),
                ));
            }
        }
        draft
            .status_by_editor
            .retain(|view, _| !subtree_set.contains(view));
        if let Some(status) = per_pane_status {
            draft.status_by_editor.insert(new_view, status);
        }
        let removed = draft.remove_semantic_subtree_without_parent_repair(&subtree)?;
        assert!(
            draft.views.insert(new_view, replacement).is_none(),
            "view id must be unique"
        );
        draft.reconcile_layout(Some(if focused_replaced {
            target
        } else {
            previous_focus
        }))?;
        draft.sync_global_status_target()?;
        draft.scene_revision.next();
        draft.validate()?;
        *self = draft;
        Ok(WorkspaceMutation {
            output: new_view,
            removed,
        })
    }

    pub(super) fn set_sizing(
        &mut self,
        target: SpaceId,
        sizing: Sizing,
    ) -> Result<(), LayoutError> {
        let mut draft = self.clone();
        draft.reject_non_body_space(target)?;
        draft
            .scene_builder
            .set_sizing(&mut draft.scene, target, sizing)?;
        draft.scene_revision.next();
        draft.validate()?;
        *self = draft;
        Ok(())
    }

    fn alloc_view_id(&mut self) -> ViewId {
        let id = ViewId(self.next_view_id);
        self.next_view_id = self.next_view_id.checked_add(1).expect("view id overflow");
        id
    }

    fn validate_subtree_target(&self, target: SpaceId) -> Result<ViewId, LayoutError> {
        self.reject_non_body_space(target)?;
        let source = self
            .view_for_space(target)
            .ok_or(SceneError::ExpectedContentLeaf(target))?;
        self.switch_target_from_view(source).ok_or_else(|| {
            LayoutError::InvalidWorkspace(format!(
                "View {} has no switchable lifecycle owner",
                source.0
            ))
        })
    }

    fn reject_non_body_space(&self, target: SpaceId) -> Result<(), LayoutError> {
        let view = self
            .view_for_space(target)
            .ok_or(SceneError::ExpectedContentLeaf(target))?;
        if self.views[&view].panes().key_for_space(target) != Some(BODY_PANE) {
            return Err(LayoutError::StatusBarSpace(target));
        }
        Ok(())
    }

    fn semantic_subtree(&self, root: ViewId) -> Result<Vec<ViewId>, LayoutError> {
        let mut result = Vec::new();
        let mut seen = HashSet::new();
        let mut stack = vec![root];
        while let Some(view) = stack.pop() {
            if !seen.insert(view) {
                return Err(LayoutError::InvalidWorkspace(
                    "semantic View tree contains a cycle or duplicate child".to_owned(),
                ));
            }
            let data = self
                .views
                .get(&view)
                .ok_or(LayoutError::MissingView(view))?;
            result.push(view);
            stack.extend(data.children().iter().rev().copied());
        }
        Ok(result)
    }

    fn content_removal_roots(&self, content: ContentId) -> Result<Vec<ViewId>, LayoutError> {
        let targets = self
            .views
            .iter()
            .filter_map(|(view, data)| (data.document_content() == Some(content)).then_some(*view))
            .map(|view| {
                self.switch_target_from_view(view).ok_or_else(|| {
                    LayoutError::InvalidWorkspace(format!(
                        "View {} has no switchable lifecycle owner",
                        view.0
                    ))
                })
            })
            .collect::<Result<HashSet<_>, _>>()?;
        let mut roots = Vec::new();
        for target in &targets {
            let mut current = self
                .views
                .get(target)
                .ok_or(LayoutError::MissingView(*target))?
                .parent();
            let mut visited = HashSet::new();
            let mut covered = false;
            while let Some(parent) = current {
                if !visited.insert(parent) {
                    return Err(LayoutError::InvalidWorkspace(
                        "semantic View graph contains a parent cycle".to_owned(),
                    ));
                }
                if targets.contains(&parent) {
                    covered = true;
                    break;
                }
                current = self
                    .views
                    .get(&parent)
                    .ok_or(LayoutError::MissingView(parent))?
                    .parent();
            }
            if !covered {
                roots.push(*target);
            }
        }
        roots.sort_by_key(|view| view.0);
        Ok(roots)
    }

    fn ordered_subtree_spaces(
        &self,
        subtree: &[ViewId],
        target: SpaceId,
        preserved: &HashSet<SpaceId>,
        include_target: bool,
    ) -> Result<Vec<SpaceId>, LayoutError> {
        let mut spaces = Vec::new();
        for view in subtree {
            let data = self
                .views
                .get(view)
                .ok_or(LayoutError::MissingView(*view))?;
            for space in data.panes().spaces() {
                if preserved.contains(&space) || (!include_target && space == target) {
                    continue;
                }
                let key = data.panes().key_for_space(space).ok_or_else(|| {
                    LayoutError::InvalidWorkspace("Pane map is not bidirectional".to_owned())
                })?;
                spaces.push((key == BODY_PANE, space));
            }
        }
        // 先移除附属 Pane，再移除正文；目标正文始终由调用方最后处理。
        spaces.sort_by_key(|(body, space)| (*body, space.0));
        Ok(spaces.into_iter().map(|(_, space)| space).collect())
    }

    fn remove_semantic_subtree(
        &mut self,
        root: ViewId,
        subtree: &[ViewId],
    ) -> Result<Vec<RemovedView>, LayoutError> {
        if let Some(parent) = self.views[&root].parent() {
            self.views
                .get_mut(&parent)
                .ok_or(LayoutError::MissingView(parent))?
                .remove_child(root);
        }
        self.remove_semantic_subtree_without_parent_repair(subtree)
    }

    fn remove_semantic_subtree_without_parent_repair(
        &mut self,
        subtree: &[ViewId],
    ) -> Result<Vec<RemovedView>, LayoutError> {
        subtree
            .iter()
            .map(|id| {
                self.views
                    .remove(id)
                    .map(|view| RemovedView { id: *id, view })
                    .ok_or(LayoutError::MissingView(*id))
            })
            .collect()
    }

    fn reconcile_layout(&mut self, preferred: Option<SpaceId>) -> Result<(), LayoutError> {
        self.focused = resolve_focus(&self.scene, self.focused, preferred)
            .ok_or(LayoutError::NoFocusableSpace)?;
        Ok(())
    }

    fn sync_global_status_target(&mut self) -> Result<(), LayoutError> {
        if self.status_placement != StatusBarPlacement::Global {
            return Ok(());
        }
        let Some(space) = self.global_status_space else {
            return Ok(());
        };
        let editor = self
            .view_for_space(self.focused)
            .ok_or(LayoutError::NoFocusableSpace)?;
        self.retarget_status_space(space, editor)
    }

    fn retarget_status_space(&mut self, space: SpaceId, editor: ViewId) -> Result<(), LayoutError> {
        let previous =
            view_for_space(&self.scene, space).ok_or(SceneError::ExpectedContentLeaf(space))?;
        if previous == editor {
            return Ok(());
        }
        self.scene_builder
            .replace_view(&mut self.scene, space, editor, false)?;
        if let Some(view) = self.views.get_mut(&previous) {
            view.release_pane_space(space);
        }
        self.views
            .get_mut(&editor)
            .ok_or(LayoutError::MissingView(editor))?
            .assign_pane(space, STATUS_PANE);
        Ok(())
    }

    fn validate(&self) -> Result<(), LayoutError> {
        if view_space_focusable(&self.scene, self.focused) != Some(true) {
            return Err(LayoutError::InvalidWorkspace(
                "focus does not reference a focusable View Pane".to_owned(),
            ));
        }

        let mut scene_spaces = HashSet::new();
        for (space, view) in scene_views(&self.scene) {
            let data = self.views.get(&view).ok_or_else(|| {
                LayoutError::InvalidWorkspace(format!(
                    "Scene space {} references missing View {}",
                    space.0, view.0
                ))
            })?;
            if data.panes().key_for_space(space).is_none() {
                return Err(LayoutError::InvalidWorkspace(format!(
                    "Scene space {} has no owning PaneKey",
                    space.0
                )));
            }
            if !scene_spaces.insert(space) {
                return Err(LayoutError::InvalidWorkspace(
                    "Scene contains duplicate Space identity".to_owned(),
                ));
            }
        }
        for (id, view) in &self.views {
            for space in view.panes().spaces() {
                if view_for_space(&self.scene, space) != Some(*id) {
                    return Err(LayoutError::InvalidWorkspace(format!(
                        "View {} owns stale Pane space {}",
                        id.0, space.0
                    )));
                }
            }
            if let Some(parent) = view.parent() {
                let parent_id = parent;
                let parent = self.views.get(&parent_id).ok_or_else(|| {
                    LayoutError::InvalidWorkspace(format!(
                        "View {} has missing semantic parent {}",
                        id.0, parent_id.0
                    ))
                })?;
                if !parent.children().contains(id) {
                    return Err(LayoutError::InvalidWorkspace(format!(
                        "semantic parent {} does not own child {}",
                        parent_id.0, id.0
                    )));
                }
            }
            let mut children = HashSet::new();
            for child in view.children() {
                if !children.insert(*child)
                    || self.views.get(child).and_then(View::parent) != Some(*id)
                {
                    return Err(LayoutError::InvalidWorkspace(format!(
                        "View {} has an invalid semantic child {}",
                        id.0, child.0
                    )));
                }
            }
            if view.definition().as_str() == DIFF_VIEW_DEFINITION {
                self.validate_diff_view(*id, view)?;
            }
        }
        let mut reachable = HashSet::new();
        for root in self
            .views
            .iter()
            .filter_map(|(id, view)| view.parent().is_none().then_some(*id))
        {
            reachable.extend(self.semantic_subtree(root)?);
        }
        if reachable.len() != self.views.len() {
            return Err(LayoutError::InvalidWorkspace(
                "semantic View graph contains a parent cycle".to_owned(),
            ));
        }
        let status_panes = self
            .views
            .iter()
            .filter_map(|(view, data)| {
                data.panes()
                    .space_for_key(STATUS_PANE)
                    .map(|space| (*view, space))
            })
            .collect::<HashMap<_, _>>();
        if self.status_placement == StatusBarPlacement::Global {
            if self.global_status_space.is_none() || !self.status_by_editor.is_empty() {
                return Err(LayoutError::InvalidWorkspace(
                    "global status ownership is inconsistent".to_owned(),
                ));
            }
            let global = self.global_status_space.expect("validated global status");
            if status_panes.len() != 1 || !status_panes.values().any(|space| *space == global) {
                return Err(LayoutError::InvalidWorkspace(
                    "global status has stale View Pane ownership".to_owned(),
                ));
            }
        } else if self.global_status_space.is_some() {
            return Err(LayoutError::InvalidWorkspace(
                "per-pane workspace retains a global status Space".to_owned(),
            ));
        } else if status_panes != self.status_by_editor {
            return Err(LayoutError::InvalidWorkspace(
                "per-pane status index does not match View Pane ownership".to_owned(),
            ));
        }
        if let Some(space) = self.global_status_space {
            let owner = self.view_for_space(space).ok_or_else(|| {
                LayoutError::InvalidWorkspace(
                    "global status does not reference a View Pane".to_owned(),
                )
            })?;
            if self.views[&owner].panes().key_for_space(space) != Some(STATUS_PANE) {
                return Err(LayoutError::InvalidWorkspace(
                    "global status Space has the wrong PaneKey".to_owned(),
                ));
            }
        }
        for (view, space) in &self.status_by_editor {
            if self
                .views
                .get(view)
                .and_then(|view| view.panes().key_for_space(*space))
                != Some(STATUS_PANE)
            {
                return Err(LayoutError::InvalidWorkspace(format!(
                    "View {} has an invalid per-pane status mapping",
                    view.0
                )));
            }
        }
        Ok(())
    }

    fn validate_diff_view(&self, id: ViewId, view: &View) -> Result<(), LayoutError> {
        let invalid =
            |message: &str| LayoutError::InvalidWorkspace(format!("DiffView {} {message}", id.0));
        if !view.switchable()
            || view.panes().space_for_key(BODY_PANE).is_some()
            || view.panes().space_for_key(STATUS_PANE).is_some()
        {
            return Err(invalid(
                "must be a switchable parent without body or status Pane",
            ));
        }
        let [left, right] = view.children() else {
            return Err(invalid("must own exactly left and right child Views"));
        };
        for (binding, child) in [(LEFT_BINDING, *left), (RIGHT_BINDING, *right)] {
            let child = self
                .views
                .get(&child)
                .ok_or_else(|| invalid("references a missing child View"))?;
            if child.definition().as_str() != BUFFER_VIEW_DEFINITION
                || child.switchable()
                || child.document_content() != view.binding(binding)
            {
                return Err(invalid("binding and BufferView child are inconsistent"));
            }
        }
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn next_view_id_for_test(&self) -> u64 {
        self.next_view_id
    }
}

fn collect_view_spaces(scene: &Scene, sid: SpaceId, out: &mut Vec<(SpaceId, ViewId)>) {
    let node = scene.node(sid);
    match &node.space.kind {
        SpaceKind::Content { view, .. } => out.push((sid, *view)),
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

fn resolve_switch_target(
    scene: &Scene,
    views: &HashMap<ViewId, View>,
    focused: SpaceId,
) -> Option<ViewId> {
    resolve_switch_target_from_view(views, view_for_space(scene, focused)?)
}

fn resolve_switch_target_from_view(
    views: &HashMap<ViewId, View>,
    mut current: ViewId,
) -> Option<ViewId> {
    let mut visited = HashSet::new();
    loop {
        if !visited.insert(current) {
            return None;
        }
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

#[cfg(test)]
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
