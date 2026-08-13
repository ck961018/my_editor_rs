//! 视图实例的交互会话：按照 View definition 绑定一组具名 content，持有
//! document binding 的独立 content view state，并控制一个或多个直属 Pane
//! （正文、状态栏等）。由 `ViewWorkspace` 按 ViewId 索引，同一 Content 可被
//! 多个独立 View 引用；同一 View 可占据多个 Content Space。

use std::collections::{BTreeMap, HashMap};

use vell_core::content_view_state::ContentViewState;
use vell_protocol::ids::{ContentId, SpaceId, ViewId};
use vell_protocol::revision::Revision;
use vell_protocol::selection::Selections;
use vell_protocol::view::{BindingKey, DOCUMENT_BINDING, ViewDefinition, ViewDefinitionId};

/// View 内部识别直属 Pane 的稳定语义键；布局协议仍只使用 SpaceId。
pub type PaneKey = String;

/// 正文 Pane：view 的主编辑区域。
pub const BODY_PANE: &str = "body";
/// BufferView 内建行号 gutter Pane。
pub const GUTTER_PANE: &str = "builtin.gutter";
/// 内建状态栏 Pane。
pub const STATUS_PANE: &str = "builtin.status";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContentBindingError {
    Duplicate(BindingKey),
    Unknown(BindingKey),
    Missing(BindingKey),
    DocumentStateMismatch,
}

/// View definition 声明的命名 Content 角色及其当前绑定。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ContentBindings {
    by_key: BTreeMap<BindingKey, ContentId>,
}

impl ContentBindings {
    pub fn new(
        definition: &ViewDefinition,
        bindings: impl IntoIterator<Item = (BindingKey, ContentId)>,
    ) -> Result<Self, ContentBindingError> {
        let mut by_key = BTreeMap::new();
        for (key, content) in bindings {
            if !definition.declares(key.as_str()) {
                return Err(ContentBindingError::Unknown(key));
            }
            if by_key.contains_key(key.as_str()) {
                return Err(ContentBindingError::Duplicate(key));
            }
            by_key.insert(key, content);
        }
        if let Some(missing) = definition
            .bindings()
            .find(|binding| !by_key.contains_key(binding.as_str()))
        {
            return Err(ContentBindingError::Missing(missing.clone()));
        }
        Ok(Self { by_key })
    }

    pub fn get(&self, key: &str) -> Option<ContentId> {
        self.by_key.get(key).copied()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&BindingKey, ContentId)> {
        self.by_key.iter().map(|(key, content)| (key, *content))
    }

    pub fn references(&self, content: ContentId) -> bool {
        self.by_key.values().any(|candidate| *candidate == content)
    }

    fn rebind(
        &mut self,
        key: &BindingKey,
        content: ContentId,
    ) -> Result<ContentId, ContentBindingError> {
        let target = self
            .by_key
            .get_mut(key.as_str())
            .ok_or_else(|| ContentBindingError::Unknown(key.clone()))?;
        Ok(std::mem::replace(target, content))
    }
}

/// SpaceId 与 PaneKey 的双向映射。每个 PaneKey 在一个 view 内唯一，
/// 每个 Space 同一时刻只属于一个 view 的一个 Pane。
#[derive(Clone, Default)]
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

#[derive(Clone)]
pub struct View {
    definition: ViewDefinitionId,
    bindings: ContentBindings,
    document_state: Option<ContentViewState>,
    binding_revision: Revision,
    revision: Revision,
    panes: ViewPaneMap,
    /// 通用 View 切换操作是否可替换本实例；DiffView 等复合 view 的子 view 为 false。
    switchable: bool,
    /// 语义父 view（复合 view 的组合关系），不同于 Space 布局父子。
    parent: Option<ViewId>,
    children: Vec<ViewId>,
}

impl View {
    pub fn buffer(content: ContentId, state: ContentViewState) -> Self {
        let definition = ViewDefinition::buffer();
        Self::with_definition(
            &definition,
            [(BindingKey::new(DOCUMENT_BINDING), content)],
            Some(state),
        )
        .expect("BufferView definition and bindings are valid")
    }

    pub(crate) fn with_definition(
        definition: &ViewDefinition,
        bindings: impl IntoIterator<Item = (BindingKey, ContentId)>,
        document_state: Option<ContentViewState>,
    ) -> Result<Self, ContentBindingError> {
        if definition.declares(DOCUMENT_BINDING) != document_state.is_some() {
            return Err(ContentBindingError::DocumentStateMismatch);
        }
        let bindings = ContentBindings::new(definition, bindings)?;
        Ok(Self {
            definition: definition.id().clone(),
            bindings,
            document_state,
            binding_revision: Revision::default(),
            revision: Revision::default(),
            panes: ViewPaneMap::default(),
            switchable: true,
            parent: None,
            children: Vec::new(),
        })
    }

    pub fn definition(&self) -> &ViewDefinitionId {
        &self.definition
    }

    pub fn bindings(&self) -> &ContentBindings {
        &self.bindings
    }

    pub fn binding(&self, key: &str) -> Option<ContentId> {
        self.bindings.get(key)
    }

    pub fn document_content(&self) -> Option<ContentId> {
        self.binding(DOCUMENT_BINDING)
    }

    pub fn document(&self) -> Option<(ContentId, &ContentViewState)> {
        Some((self.document_content()?, self.document_state.as_ref()?))
    }

    /// 当前只适用于必须是 BufferView 的内部执行路径。
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "test and BufferView assertions")
    )]
    pub(crate) fn require_document(&self) -> ContentId {
        self.document_content()
            .expect("BufferView has a document binding")
    }

    pub fn references(&self, content: ContentId) -> bool {
        self.bindings.references(content)
    }

    pub(crate) fn rebind(
        &mut self,
        key: &BindingKey,
        content: ContentId,
        state: Option<ContentViewState>,
    ) -> Result<ContentId, ContentBindingError> {
        let previous = self
            .binding(key.as_str())
            .ok_or_else(|| ContentBindingError::Unknown(key.clone()))?;
        let document_rebind = key.as_str() == DOCUMENT_BINDING;
        if document_rebind != state.is_some() {
            return Err(ContentBindingError::DocumentStateMismatch);
        }
        if previous == content {
            return Ok(previous);
        }

        if let Some(state) = state {
            self.document_state = Some(state);
        }
        self.bindings.rebind(key, content)?;
        self.binding_revision.next();
        self.touch();
        Ok(previous)
    }

    pub fn panes(&self) -> &ViewPaneMap {
        &self.panes
    }

    /// 登记直属 Pane：space 由本 view 决定显示内容。
    pub(crate) fn assign_pane(&mut self, space: SpaceId, key: impl Into<PaneKey>) {
        self.panes.insert(space, key.into());
    }

    /// 释放 space 的 Pane 归属（space 关闭或移交给其他 view）。
    pub(crate) fn release_pane_space(&mut self, space: SpaceId) -> Option<PaneKey> {
        self.panes.remove_space(space)
    }

    pub fn switchable(&self) -> bool {
        self.switchable
    }

    pub(crate) fn set_switchable(&mut self, switchable: bool) {
        self.switchable = switchable;
    }

    pub fn parent(&self) -> Option<ViewId> {
        self.parent
    }

    pub(crate) fn set_parent(&mut self, parent: Option<ViewId>) {
        self.parent = parent;
    }

    pub fn children(&self) -> &[ViewId] {
        &self.children
    }

    pub(crate) fn push_child(&mut self, child: ViewId) {
        if !self.children.contains(&child) {
            self.children.push(child);
        }
    }

    pub(crate) fn remove_child(&mut self, child: ViewId) {
        self.children.retain(|candidate| *candidate != child);
    }

    pub(crate) fn replace_child(&mut self, current: ViewId, replacement: ViewId) -> bool {
        let Some(child) = self
            .children
            .iter_mut()
            .find(|candidate| **candidate == current)
        else {
            return false;
        };
        *child = replacement;
        true
    }

    pub fn selections(&self) -> Option<&Selections> {
        self.document_state.as_ref()?.selections()
    }
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "M4 generic View attachment will consume this")
    )]
    pub(crate) fn require_document_state(&self) -> &ContentViewState {
        self.document_state
            .as_ref()
            .expect("document-backed View has ContentViewState")
    }
    pub(crate) fn require_document_state_mut(&mut self) -> &mut ContentViewState {
        self.document_state
            .as_mut()
            .expect("document-backed View has ContentViewState")
    }
    pub fn set_selections(&mut self, selections: Selections) -> bool {
        let changed = self
            .document_state
            .as_mut()
            .and_then(|state| state.replace_selections(selections))
            == Some(true);
        if changed {
            self.touch();
        }
        changed
    }
    pub fn revision(&self) -> Revision {
        self.revision
    }
    pub fn binding_revision(&self) -> Revision {
        self.binding_revision
    }
    pub fn touch(&mut self) {
        self.revision.next();
    }

    pub(crate) fn restore_selections_and_revision(
        &mut self,
        selections: Selections,
        revision: Revision,
    ) {
        if self
            .document_state
            .as_mut()
            .and_then(|state| state.replace_selections(selections))
            .is_some()
        {
            self.revision = revision;
        }
    }

    #[cfg(test)]
    pub(crate) fn set_definition_for_test(
        &mut self,
        definition: ViewDefinition,
        bindings: impl IntoIterator<Item = (BindingKey, ContentId)>,
    ) -> Result<(), ContentBindingError> {
        let document_state = if definition.declares(DOCUMENT_BINDING) {
            self.document_state.clone()
        } else {
            None
        };
        let replacement = Self::with_definition(&definition, bindings, document_state)?;
        self.definition = replacement.definition;
        self.bindings = replacement.bindings;
        self.document_state = replacement.document_state;
        self.binding_revision.next();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pane_map_tracks_spaces_and_keys_bidirectionally() {
        let mut view = View::buffer(ContentId(1), ContentViewState::buffer());
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
        let mut view = View::buffer(ContentId(1), ContentViewState::buffer());
        view.assign_pane(SpaceId(1), STATUS_PANE);
        view.assign_pane(SpaceId(9), STATUS_PANE);

        assert_eq!(view.panes().space_for_key(STATUS_PANE), Some(SpaceId(9)));
        assert_eq!(view.panes().key_for_space(SpaceId(1)), None);
    }

    #[test]
    fn views_default_to_switchable_without_semantic_parent() {
        let view = View::buffer(ContentId(0), ContentViewState::buffer());

        assert!(view.switchable());
        assert_eq!(view.parent(), None);
        assert!(view.children().is_empty());
    }

    #[test]
    fn touch_advances_view_revision() {
        let mut view = View::buffer(ContentId(0), ContentViewState::buffer());

        view.touch();

        assert_eq!(view.revision(), Revision(1));
    }

    #[test]
    fn rebinding_one_named_role_preserves_identity_and_other_roles() {
        let left = BindingKey::new("left");
        let right = BindingKey::new("right");
        let definition = ViewDefinition::new(
            vell_protocol::view::ViewDefinitionId::new("test.diff"),
            [left.clone(), right.clone()],
        )
        .unwrap();
        let mut view = View::with_definition(
            &definition,
            [(left.clone(), ContentId(1)), (right.clone(), ContentId(2))],
            None,
        )
        .unwrap();

        let previous = view.rebind(&right, ContentId(3), None).unwrap();

        assert_eq!(previous, ContentId(2));
        assert_eq!(view.binding("left"), Some(ContentId(1)));
        assert_eq!(view.binding("right"), Some(ContentId(3)));
        assert_eq!(view.definition().as_str(), "test.diff");
        assert_eq!(view.binding_revision(), Revision(1));
        assert_eq!(view.revision(), Revision(1));
    }

    #[test]
    fn view_bindings_must_exactly_match_the_definition_schema() {
        let left = BindingKey::new("left");
        let right = BindingKey::new("right");
        let definition = ViewDefinition::new(
            vell_protocol::view::ViewDefinitionId::new("test.diff"),
            [left.clone(), right.clone()],
        )
        .unwrap();

        let missing = View::with_definition(&definition, [(left.clone(), ContentId(1))], None);
        let unknown = View::with_definition(
            &definition,
            [
                (left, ContentId(1)),
                (right, ContentId(2)),
                (BindingKey::new("center"), ContentId(3)),
            ],
            None,
        );

        assert!(matches!(
            missing,
            Err(ContentBindingError::Missing(binding)) if binding.as_str() == "right"
        ));
        assert!(matches!(
            unknown,
            Err(ContentBindingError::Unknown(binding)) if binding.as_str() == "center"
        ));
    }

    #[test]
    fn failed_rebind_does_not_change_document_state() {
        let mut view = View::buffer(ContentId(1), ContentViewState::buffer());
        let original_state = view.require_document_state().clone();

        let result = view.rebind(
            &BindingKey::new("missing"),
            ContentId(2),
            Some(ContentViewState::buffer()),
        );

        assert_eq!(
            result,
            Err(ContentBindingError::Unknown(BindingKey::new("missing")))
        );
        assert_eq!(view.require_document_state(), &original_state);
        assert_eq!(view.revision(), Revision(0));
    }

    #[test]
    fn rebind_rejects_mismatched_document_state_without_mutating() {
        let right = BindingKey::new("right");
        let definition = ViewDefinition::new(
            vell_protocol::view::ViewDefinitionId::new("test.document-and-right"),
            [BindingKey::new(DOCUMENT_BINDING), right.clone()],
        )
        .unwrap();
        let mut view = View::with_definition(
            &definition,
            [
                (BindingKey::new(DOCUMENT_BINDING), ContentId(1)),
                (right.clone(), ContentId(2)),
            ],
            Some(ContentViewState::buffer()),
        )
        .unwrap();

        let missing_document_state =
            view.rebind(&BindingKey::new(DOCUMENT_BINDING), ContentId(3), None);
        let unexpected_secondary_state =
            view.rebind(&right, ContentId(4), Some(ContentViewState::buffer()));

        assert_eq!(
            missing_document_state,
            Err(ContentBindingError::DocumentStateMismatch)
        );
        assert_eq!(
            unexpected_secondary_state,
            Err(ContentBindingError::DocumentStateMismatch)
        );
        assert_eq!(view.document_content(), Some(ContentId(1)));
        assert_eq!(view.binding("right"), Some(ContentId(2)));
        assert_eq!(view.revision(), Revision(0));
    }
}
