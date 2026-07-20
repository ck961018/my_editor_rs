use std::any::Any;
use std::collections::HashMap;
use std::fmt;
use std::rc::Rc;
use std::sync::LazyLock;

use tokio_util::sync::CancellationToken;

use crate::app::action::{TransactionIntent, ViewAction};
use crate::app::command::{AppCommand, Command, ModeCommand, ModeValue};
use crate::app::mode_name::{ModeActionName, ModeName};
use crate::app::presentation::{ContentPresentationLayer, ViewPresentationLayer};
use crate::app::view::View;
use crate::core::action::ContentAction;
use crate::core::command::EditCommand;
use crate::core::content::ContentChange;
use crate::core::content_store::ContentStore;
use crate::core::input::{InputDecision, InputStatus};
use crate::core::keymap::Keymap;
use crate::protocol::content_query::{
    ContentData, ContentQuery, CursorStyle, Face, FaceName, NamedTextDecoration, RowRange,
    SelectionShape,
};
use crate::protocol::ids::{ContentId, ViewId};
use crate::protocol::key_event::KeyEvent;
use crate::protocol::revision::Revision;
use crate::protocol::selection::Selections;
use crate::protocol::viewport::ViewportCommand;

static EMPTY_KEYMAP: LazyLock<Keymap<Command>> = LazyLock::new(Keymap::new);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ModeId(u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ModeActionId(u32);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModeError {
    #[allow(dead_code, reason = "script adapters map callback failures")]
    CallbackFailed {
        mode: ModeName,
        message: String,
    },
    UnknownMode {
        mode: ModeName,
    },
    UnknownAction {
        mode: ModeName,
        action: ModeActionName,
    },
    InactiveMode {
        requested: ModeName,
        active: Option<ModeName>,
    },
}

impl fmt::Display for ModeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CallbackFailed { mode, message } => {
                write!(
                    formatter,
                    "mode '{}' callback failed: {message}",
                    mode.as_str()
                )
            }
            Self::UnknownMode { mode } => write!(formatter, "unknown mode '{}'", mode.as_str()),
            Self::UnknownAction { mode, action } => write!(
                formatter,
                "unknown action '{}' for mode '{}'",
                action.as_str(),
                mode.as_str()
            ),
            Self::InactiveMode { requested, active } => match active {
                Some(active) => write!(
                    formatter,
                    "mode '{}' is not active; active mode is '{}'",
                    requested.as_str(),
                    active.as_str()
                ),
                None => write!(
                    formatter,
                    "mode '{}' cannot execute because the view has no active mode",
                    requested.as_str()
                ),
            },
        }
    }
}

impl std::error::Error for ModeError {}

pub trait ModeState: Any {
    fn as_any(&self) -> &dyn Any;
    fn as_any_mut(&mut self) -> &mut dyn Any;
    fn clone_box(&self) -> Box<dyn ModeState>;
}

pub type ModeJobResult = Result<Box<dyn Any + Send>, String>;
pub(crate) type ModeJobRunner = Box<dyn FnOnce(CancellationToken) -> ModeJobResult + Send>;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct ModeJobKey {
    pub(crate) mode: ModeId,
    pub(crate) content: ContentId,
    pub(crate) slot: String,
}

pub struct ModeJobRequest {
    slot: String,
    version: u64,
    run: ModeJobRunner,
}

impl ModeJobRequest {
    pub fn new(
        slot: impl Into<String>,
        version: u64,
        run: impl FnOnce(CancellationToken) -> ModeJobResult + Send + 'static,
    ) -> Self {
        Self {
            slot: slot.into(),
            version,
            run: Box::new(run),
        }
    }

    pub(crate) fn into_parts(self) -> (String, u64, ModeJobRunner) {
        (self.slot, self.version, self.run)
    }
}

impl<T: Any + Clone> ModeState for T {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    fn clone_box(&self) -> Box<dyn ModeState> {
        Box::new(self.clone())
    }
}

#[derive(Default)]
pub(crate) struct ModeDraftJournal {
    content: HashMap<(ModeId, ContentId), ModeContentDraft>,
    views: HashMap<(ModeId, ViewId), ModeViewDraft>,
}

struct ModeContentDraft {
    state: Box<dyn ModeState>,
    faulted: bool,
    background_job_dirty: bool,
}

struct ModeViewDraft {
    state: Box<dyn ModeState>,
    faulted: bool,
}

impl ModeDraftJournal {
    fn content<'a>(
        &'a self,
        mode: ModeId,
        content: ContentId,
        persistent: &'a ModeContentInstance,
    ) -> (&'a dyn ModeState, bool) {
        self.content
            .get(&(mode, content))
            .map_or((persistent.state.as_ref(), persistent.faulted), |draft| {
                (draft.state.as_ref(), draft.faulted)
            })
    }

    fn content_mut<'a>(
        &'a mut self,
        mode: ModeId,
        content: ContentId,
        persistent: &ModeContentInstance,
    ) -> &'a mut ModeContentDraft {
        self.content
            .entry((mode, content))
            .or_insert_with(|| ModeContentDraft {
                state: persistent.state.clone_box(),
                faulted: persistent.faulted,
                background_job_dirty: persistent.background_job_dirty,
            })
    }

    fn view<'a>(
        &'a self,
        mode: ModeId,
        view: ViewId,
        persistent: &'a ModeViewInstance,
    ) -> (&'a dyn ModeState, bool) {
        self.views
            .get(&(mode, view))
            .map_or((persistent.state.as_ref(), persistent.faulted), |draft| {
                (draft.state.as_ref(), draft.faulted)
            })
    }

    fn content_and_view_mut<'a>(
        &'a mut self,
        mode: ModeId,
        content: ContentId,
        view: ViewId,
        persistent_content: &ModeContentInstance,
        persistent_view: &ModeViewInstance,
    ) -> (&'a mut ModeContentDraft, &'a mut ModeViewDraft) {
        let content_draft =
            self.content
                .entry((mode, content))
                .or_insert_with(|| ModeContentDraft {
                    state: persistent_content.state.clone_box(),
                    faulted: persistent_content.faulted,
                    background_job_dirty: persistent_content.background_job_dirty,
                });
        let view_draft = self
            .views
            .entry((mode, view))
            .or_insert_with(|| ModeViewDraft {
                state: persistent_view.state.clone_box(),
                faulted: persistent_view.faulted,
            });
        (content_draft, view_draft)
    }

    pub(crate) fn commit_content(&mut self, store: &mut ModeContentStore) {
        for (key, draft) in self.content.drain() {
            let instance = store
                .instances
                .get_mut(&key)
                .expect("drafted mode content still exists");
            instance.state = draft.state;
            instance.faulted = draft.faulted;
            instance.background_job_dirty = draft.background_job_dirty;
        }
    }

    pub(crate) fn commit_views(&mut self, store: &mut ModeViewStore) {
        for (key, draft) in self.views.drain() {
            let instance = store
                .instances
                .get_mut(&key)
                .expect("drafted mode view still exists");
            instance.state = draft.state;
            instance.faulted = draft.faulted;
        }
    }
}

#[derive(Default)]
pub(crate) struct FaceRegistry {
    faces: HashMap<FaceName, Face>,
}

impl FaceRegistry {
    fn register_defaults(&mut self, mode: &dyn Mode) {
        for (name, face) in mode.faces() {
            self.faces.entry(name).or_insert(face);
        }
    }

    pub(crate) fn resolve(&self, name: &FaceName) -> Face {
        self.faces.get(name).cloned().unwrap_or_default()
    }

    #[allow(dead_code, reason = "theme and script adapters override named faces")]
    pub(crate) fn set(&mut self, name: FaceName, face: Face) {
        self.faces.insert(name, face);
    }
}

#[allow(
    dead_code,
    reason = "native Mode content contexts are used by extensions"
)]
pub struct ModeContentContext<'a> {
    content_id: ContentId,
    contents: &'a ContentStore,
}

#[allow(
    dead_code,
    reason = "native Mode content contexts are used by extensions"
)]
impl<'a> ModeContentContext<'a> {
    pub(crate) fn new(content_id: ContentId, contents: &'a ContentStore) -> Self {
        Self {
            content_id,
            contents,
        }
    }

    pub fn content_id(&self) -> ContentId {
        self.content_id
    }

    pub fn query_content(&self, query: ContentQuery) -> ContentData {
        self.contents.query(self.content_id, query)
    }

    pub fn content_revision(&self) -> Option<Revision> {
        self.contents.revision(self.content_id)
    }

    pub fn text_snapshot(&self) -> Option<crate::core::text_snapshot::TextSnapshot> {
        self.contents.text_snapshot(self.content_id)
    }
}

#[allow(dead_code, reason = "reserved for generic Mode extensions")]
pub struct ModeViewContext<'a> {
    view_id: ViewId,
    view: &'a View,
    contents: &'a ContentStore,
}

#[allow(dead_code, reason = "reserved for generic Mode extensions")]
impl<'a> ModeViewContext<'a> {
    pub(crate) fn new(view_id: ViewId, view: &'a View, contents: &'a ContentStore) -> Self {
        Self {
            view_id,
            view,
            contents,
        }
    }

    pub fn view_id(&self) -> ViewId {
        self.view_id
    }

    pub fn content_id(&self) -> ContentId {
        self.view.content()
    }

    pub fn selections(&self) -> Option<&Selections> {
        self.view.selections()
    }

    pub fn query_content(&self, query: ContentQuery) -> ContentData {
        self.contents.query(self.content_id(), query)
    }

    pub fn content_revision(&self) -> Option<Revision> {
        self.contents.revision(self.content_id())
    }

    pub fn text_snapshot(&self) -> Option<crate::core::text_snapshot::TextSnapshot> {
        self.contents.text_snapshot(self.content_id())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModeResult {
    flow: InputFlow,
    operations: Vec<ModeEffect>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputFlow {
    Continue,
    Stop,
}

impl ModeResult {
    #[allow(
        dead_code,
        reason = "empty typed results are part of the mode contract"
    )]
    pub fn none() -> Self {
        Self {
            flow: InputFlow::Stop,
            operations: Vec::new(),
        }
    }

    #[allow(dead_code, reason = "Mode results are an extension-facing API")]
    pub fn operations(operations: Vec<ModeEffect>) -> Self {
        Self {
            flow: InputFlow::Stop,
            operations,
        }
    }

    #[allow(dead_code, reason = "dynamic modes can pass input to the next mode")]
    pub fn continue_with(operations: Vec<ModeEffect>) -> Self {
        Self {
            flow: InputFlow::Continue,
            operations,
        }
    }

    fn into_operations(self) -> Vec<ModeEffect> {
        self.operations
    }

    pub(crate) fn into_parts(self) -> (InputFlow, Vec<ModeEffect>) {
        (self.flow, self.operations)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedViewEdit {
    pub content: Option<ContentAction>,
    pub view: Option<ViewAction>,
    pub before: Selections,
}

#[allow(dead_code, reason = "Mode effects are an extension-facing API")]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModeEffect {
    Operation(crate::app::operation::OperationRequest),
    Edit(ResolvedViewEdit),
    DeferredEdit(EditCommand),
    View(ViewAction),
    Content(ContentAction),
    Transaction(TransactionIntent),
    App(AppCommand),
    Mode(ModeCommand),
    Viewport(ViewportCommand),
    Save,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CursorDomain {
    InsertionPoint,
    Character,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ModeViewPolicy {
    pub cursor_style: Option<CursorStyle>,
    pub cursor_domain: Option<CursorDomain>,
    pub selection_shape: Option<SelectionShape>,
    pub selection_face: Option<FaceName>,
}

impl ModeViewPolicy {
    pub(crate) fn merge_missing(&mut self, next: Self) {
        self.cursor_style = self.cursor_style.or(next.cursor_style);
        self.cursor_domain = self.cursor_domain.or(next.cursor_domain);
        self.selection_shape = self.selection_shape.or(next.selection_shape);
        self.selection_face = self.selection_face.take().or(next.selection_face);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModeActionScope {
    #[allow(dead_code, reason = "content-scoped modes are an extension contract")]
    Content,
    View,
}

pub trait Mode {
    fn name(&self) -> &ModeName;
    fn actions(&self) -> &[ModeActionName];
    fn faces(&self) -> Vec<(FaceName, Face)> {
        Vec::new()
    }
    fn action_scope(&self, _action: &ModeActionName) -> ModeActionScope {
        ModeActionScope::View
    }
    fn new_content_state(&self) -> Box<dyn ModeState> {
        Box::new(())
    }
    fn create_content_state(
        &self,
        _context: &ModeContentContext<'_>,
    ) -> Result<Box<dyn ModeState>, ModeError> {
        Ok(self.new_content_state())
    }
    fn new_view_state(&self) -> Box<dyn ModeState> {
        Box::new(())
    }
    fn create_view_state(
        &self,
        _content_state: &dyn ModeState,
        _context: &ModeViewContext<'_>,
    ) -> Result<Box<dyn ModeState>, ModeError> {
        Ok(self.new_view_state())
    }
    fn execute_content(
        &self,
        _state: &mut dyn ModeState,
        _context: &ModeContentContext<'_>,
        action: &ModeActionName,
    ) -> Result<ModeResult, ModeError> {
        Err(ModeError::UnknownAction {
            mode: self.name().clone(),
            action: action.clone(),
        })
    }
    fn execute_content_with_arguments(
        &self,
        state: &mut dyn ModeState,
        context: &ModeContentContext<'_>,
        action: &ModeActionName,
        _arguments: &ModeValue,
    ) -> Result<ModeResult, ModeError> {
        self.execute_content(state, context, action)
    }
    fn on_content_changed(
        &self,
        _state: &mut dyn ModeState,
        _context: &ModeContentContext<'_>,
        _change: &ContentChange,
    ) -> Result<(), ModeError> {
        Ok(())
    }
    fn take_background_job(
        &self,
        _state: &mut dyn ModeState,
        _context: &ModeContentContext<'_>,
    ) -> Option<ModeJobRequest> {
        None
    }
    fn apply_background_job(
        &self,
        _state: &mut dyn ModeState,
        _context: &ModeContentContext<'_>,
        _version: u64,
        _result: ModeJobResult,
    ) -> Result<bool, ModeError> {
        Ok(false)
    }
    fn on_view_content_changed(
        &self,
        _content_state: &mut dyn ModeState,
        _view_state: &mut dyn ModeState,
        _context: &ModeViewContext<'_>,
        _change: &ContentChange,
    ) -> Result<(), ModeError> {
        Ok(())
    }
    fn decorations(
        &self,
        _content_state: &dyn ModeState,
        _view_state: &dyn ModeState,
        _context: &ModeViewContext<'_>,
        _visible_rows: RowRange,
    ) -> Vec<NamedTextDecoration> {
        Vec::new()
    }
    fn content_decorations(
        &self,
        _content_state: &dyn ModeState,
        _context: &ModeContentContext<'_>,
        _visible_rows: RowRange,
    ) -> Vec<NamedTextDecoration> {
        Vec::new()
    }
    fn view_decorations(
        &self,
        content_state: &dyn ModeState,
        view_state: &dyn ModeState,
        context: &ModeViewContext<'_>,
        visible_rows: RowRange,
    ) -> Vec<NamedTextDecoration> {
        self.decorations(content_state, view_state, context, visible_rows)
    }
    fn view_policy(
        &self,
        _content_state: &dyn ModeState,
        _view_state: &dyn ModeState,
        _context: &ModeViewContext<'_>,
    ) -> ModeViewPolicy {
        ModeViewPolicy::default()
    }
    fn keymap(&self, _state: &dyn ModeState, _context: &ModeViewContext<'_>) -> &Keymap<Command> {
        &EMPTY_KEYMAP
    }
    fn input_keymap<'a>(
        &'a self,
        _content_state: &dyn ModeState,
        view_state: &dyn ModeState,
        context: &ModeViewContext<'_>,
    ) -> &'a Keymap<Command> {
        self.keymap(view_state, context)
    }
    fn typing(
        &self,
        _state: &dyn ModeState,
        _context: &ModeViewContext<'_>,
        _key: KeyEvent,
    ) -> Option<Command> {
        None
    }
    fn input_typing(
        &self,
        _content_state: &dyn ModeState,
        view_state: &dyn ModeState,
        context: &ModeViewContext<'_>,
        key: KeyEvent,
    ) -> Option<Command> {
        self.typing(view_state, context, key)
    }
    fn input_status(&self, _state: &dyn ModeState, _context: &ModeViewContext<'_>) -> InputStatus {
        InputStatus::Ready
    }
    fn mode_input_status(
        &self,
        _content_state: &dyn ModeState,
        view_state: &dyn ModeState,
        context: &ModeViewContext<'_>,
    ) -> InputStatus {
        self.input_status(view_state, context)
    }
    fn capture(
        &self,
        _state: &mut dyn ModeState,
        _context: &ModeViewContext<'_>,
        _key: KeyEvent,
    ) -> InputDecision<Command> {
        InputDecision::Pass
    }
    fn input_capture(
        &self,
        _content_state: &mut dyn ModeState,
        view_state: &mut dyn ModeState,
        context: &ModeViewContext<'_>,
        key: KeyEvent,
    ) -> InputDecision<Command> {
        self.capture(view_state, context, key)
    }
    fn on_timeout(&self, _state: &mut dyn ModeState, _context: &ModeViewContext<'_>) -> ModeResult {
        ModeResult::none()
    }
    fn input_timeout(
        &self,
        _content_state: &mut dyn ModeState,
        view_state: &mut dyn ModeState,
        context: &ModeViewContext<'_>,
    ) -> ModeResult {
        self.on_timeout(view_state, context)
    }
    fn cancel(&self, _state: &mut dyn ModeState, _context: &ModeViewContext<'_>) {}
    fn input_cancel(
        &self,
        _content_state: &mut dyn ModeState,
        view_state: &mut dyn ModeState,
        context: &ModeViewContext<'_>,
    ) {
        self.cancel(view_state, context);
    }
    fn execute_view(
        &self,
        _state: &mut dyn ModeState,
        _context: &ModeViewContext<'_>,
        action: &ModeActionName,
    ) -> Result<ModeResult, ModeError> {
        Err(ModeError::UnknownAction {
            mode: self.name().clone(),
            action: action.clone(),
        })
    }
    fn execute_view_with_content(
        &self,
        _content_state: &mut dyn ModeState,
        view_state: &mut dyn ModeState,
        context: &ModeViewContext<'_>,
        action: &ModeActionName,
    ) -> Result<ModeResult, ModeError> {
        self.execute_view(view_state, context, action)
    }
    fn execute_view_with_arguments(
        &self,
        content_state: &mut dyn ModeState,
        view_state: &mut dyn ModeState,
        context: &ModeViewContext<'_>,
        action: &ModeActionName,
        _arguments: &ModeValue,
    ) -> Result<ModeResult, ModeError> {
        self.execute_view_with_content(content_state, view_state, context, action)
    }
}

pub(crate) struct ModeRegistry {
    definitions: HashMap<ModeId, Rc<ModeRegistration>>,
    ids_by_name: HashMap<ModeName, ModeId>,
    next_id: u32,
}

struct ModeRegistration {
    id: ModeId,
    definition: Box<dyn Mode>,
    action_names: Vec<ModeActionName>,
    actions: HashMap<ModeActionName, ModeActionId>,
}

pub(crate) struct ModeViewInstance {
    registered: Rc<ModeRegistration>,
    state: Box<dyn ModeState>,
    faulted: bool,
}

impl ModeRegistry {
    pub(crate) fn new() -> Self {
        Self {
            definitions: HashMap::new(),
            ids_by_name: HashMap::new(),
            next_id: 0,
        }
    }

    pub(crate) fn register(&mut self, mode: impl Mode + 'static) -> ModeId {
        let name = mode.name().clone();
        let actions = mode.actions().to_vec();
        self.register_definition(name, actions, Box::new(mode))
    }

    fn register_definition(
        &mut self,
        name: ModeName,
        action_names: Vec<ModeActionName>,
        definition: Box<dyn Mode>,
    ) -> ModeId {
        assert!(
            !self.ids_by_name.contains_key(&name),
            "mode name must be unique"
        );
        let mut actions = HashMap::new();
        for (index, name) in action_names.iter().cloned().enumerate() {
            let action = ModeActionId(u32::try_from(index).expect("mode action id overflow"));
            assert!(
                actions.insert(name, action).is_none(),
                "mode action name must be unique"
            );
        }
        let id = ModeId(self.next_id);
        self.next_id = self.next_id.checked_add(1).expect("mode id overflow");
        let registered = Rc::new(ModeRegistration {
            id,
            definition,
            action_names,
            actions,
        });
        self.ids_by_name.insert(name, id);
        self.definitions.insert(id, registered);
        id
    }

    pub(crate) fn resolve_mode(&self, name: &ModeName) -> Option<ModeId> {
        self.ids_by_name.get(name).copied()
    }

    pub(crate) fn resolve_action(
        &self,
        mode: ModeId,
        name: &ModeActionName,
    ) -> Option<ModeActionId> {
        self.definitions.get(&mode)?.actions.get(name).copied()
    }

    pub(crate) fn resolve_command_checked(
        &self,
        mode: &ModeName,
        action: &ModeActionName,
    ) -> Result<(ModeId, ModeActionId), ModeError> {
        let mode_id = self
            .resolve_mode(mode)
            .ok_or_else(|| ModeError::UnknownMode { mode: mode.clone() })?;
        let action_id =
            self.resolve_action(mode_id, action)
                .ok_or_else(|| ModeError::UnknownAction {
                    mode: mode.clone(),
                    action: action.clone(),
                })?;
        Ok((mode_id, action_id))
    }

    pub(crate) fn command_scope(
        &self,
        mode: &ModeName,
        action: &ModeActionName,
    ) -> Result<ModeActionScope, ModeError> {
        let (mode_id, _) = self.resolve_command_checked(mode, action)?;
        Ok(self.definitions[&mode_id].mode().action_scope(action))
    }

    pub(crate) fn instantiate(&self, name: &ModeName) -> Option<ModeViewInstance> {
        let id = self.resolve_mode(name)?;
        let registered = self.definitions.get(&id)?.clone();
        Some(ModeViewInstance {
            state: registered.definition.new_view_state(),
            registered,
            faulted: false,
        })
    }
}

impl ModeRegistration {
    fn mode(&self) -> &dyn Mode {
        self.definition.as_ref()
    }
}

pub(crate) struct ModeContentInstance {
    content: ContentId,
    registered: Rc<ModeRegistration>,
    state: Box<dyn ModeState>,
    attachments: usize,
    faulted: bool,
    background_job_dirty: bool,
}

impl ModeContentInstance {
    fn execute(
        &self,
        state: &mut dyn ModeState,
        faulted: bool,
        mode: ModeId,
        action: ModeActionId,
        arguments: &ModeValue,
        contents: &ContentStore,
    ) -> Result<ModeResult, ModeError> {
        if faulted {
            return Err(ModeError::InactiveMode {
                requested: self.registered.mode().name().clone(),
                active: None,
            });
        }
        assert_eq!(self.registered.id, mode, "resolved mode must be active");
        let action = self
            .registered
            .action_names
            .get(usize::try_from(action.0).expect("mode action index overflow"))
            .expect("mode action id belongs to registered mode");
        let context = ModeContentContext::new(self.content, contents);
        self.registered
            .mode()
            .execute_content_with_arguments(state, &context, action, arguments)
    }
}

#[derive(Default)]
pub(crate) struct ModeContentStore {
    instances: HashMap<(ModeId, ContentId), ModeContentInstance>,
}

impl ModeContentStore {
    #[cfg(test)]
    pub(crate) fn faults_for_test(&self) -> Vec<(String, ContentId)> {
        self.instances
            .values()
            .filter(|instance| instance.faulted)
            .map(|instance| {
                (
                    instance.registered.mode().name().as_str().to_owned(),
                    instance.content,
                )
            })
            .collect()
    }

    #[cfg(test)]
    pub(crate) fn state_for_test<T: 'static>(
        &self,
        mode: ModeId,
        content: ContentId,
    ) -> Option<&T> {
        self.instances
            .get(&(mode, content))?
            .state
            .as_any()
            .downcast_ref()
    }

    pub(crate) fn take_background_jobs(
        &mut self,
        contents: &ContentStore,
    ) -> Vec<(ModeId, ContentId, ModeJobRequest)> {
        let targets: Vec<_> = self.instances.keys().copied().collect();
        let mut drafts = ModeDraftJournal::default();
        let mut jobs = Vec::new();
        for (mode, content) in targets {
            let instance = self
                .instances
                .get(&(mode, content))
                .expect("collected mode content exists");
            let draft = drafts.content_mut(mode, content, instance);
            if draft.faulted || !draft.background_job_dirty {
                continue;
            }
            draft.background_job_dirty = false;
            let context = ModeContentContext::new(content, contents);
            if let Some(job) = instance
                .registered
                .mode()
                .take_background_job(draft.state.as_mut(), &context)
            {
                jobs.push((mode, content, job));
            }
        }
        drafts.commit_content(self);
        jobs
    }

    pub(crate) fn apply_background_job(
        &mut self,
        mode: ModeId,
        content: ContentId,
        contents: &ContentStore,
        version: u64,
        result: ModeJobResult,
    ) -> bool {
        let Some(instance) = self.instance(mode, content) else {
            return false;
        };
        let mut drafts = ModeDraftJournal::default();
        let draft = drafts.content_mut(mode, content, instance);
        if draft.faulted {
            return false;
        }
        let checkpoint = draft.state.clone_box();
        let context = ModeContentContext::new(content, contents);
        let changed = match instance.registered.mode().apply_background_job(
            draft.state.as_mut(),
            &context,
            version,
            result,
        ) {
            Ok(changed) => {
                draft.background_job_dirty |= changed;
                changed
            }
            Err(_) => {
                draft.state = checkpoint;
                draft.faulted = true;
                false
            }
        };
        drafts.commit_content(self);
        changed
    }

    pub(crate) fn presentation_layer(
        &self,
        mode: ModeId,
        content: ContentId,
        contents: &ContentStore,
        visible_rows: RowRange,
    ) -> Option<ContentPresentationLayer> {
        let instance = self.instance(mode, content)?;
        if instance.faulted {
            return None;
        }
        let context = ModeContentContext::new(content, contents);
        Some(ContentPresentationLayer {
            source_revision: context.content_revision()?,
            decorations: instance.registered.mode().content_decorations(
                instance.state.as_ref(),
                &context,
                visible_rows,
            ),
        })
    }

    #[cfg(test)]
    pub(crate) fn attach_view(&mut self, content: ContentId, mode: &ModeViewInstance) {
        let id = mode.registered.id;
        if let Some(existing) = self.instances.get_mut(&(id, content)) {
            existing.attachments += 1;
            return;
        }
        self.instances.insert(
            (id, content),
            ModeContentInstance {
                content,
                registered: mode.registered.clone(),
                state: mode.registered.mode().new_content_state(),
                attachments: 1,
                faulted: false,
                background_job_dirty: true,
            },
        );
    }

    pub(crate) fn attach_view_with_context(
        &mut self,
        content: ContentId,
        mode: &mut ModeViewInstance,
        content_context: &ModeContentContext<'_>,
        view_context: &ModeViewContext<'_>,
    ) {
        let id = mode.registered.id;
        if let Some(existing) = self.instances.get_mut(&(id, content)) {
            existing.attachments += 1;
        } else {
            let (state, faulted) =
                match mode.registered.mode().create_content_state(content_context) {
                    Ok(state) => (state, false),
                    Err(_) => (Box::new(()) as Box<dyn ModeState>, true),
                };
            self.instances.insert(
                (id, content),
                ModeContentInstance {
                    content,
                    registered: mode.registered.clone(),
                    state,
                    attachments: 1,
                    faulted,
                    background_job_dirty: !faulted,
                },
            );
        }
        let content_state = self
            .instances
            .get(&(id, content))
            .expect("attached mode has content state");
        mode.initialize(
            content_state.state.as_ref(),
            content_state.faulted,
            view_context,
        );
    }

    pub(crate) fn detach_view(&mut self, content: ContentId, mode: ModeId) {
        let key = (mode, content);
        let remove = self.instances.get_mut(&key).is_some_and(|instance| {
            instance.attachments -= 1;
            instance.attachments == 0
        });
        if remove {
            self.instances.remove(&key);
        }
    }

    pub(crate) fn notify_changed(
        &self,
        content: ContentId,
        contents: &ContentStore,
        change: &ContentChange,
        drafts: &mut ModeDraftJournal,
    ) {
        let modes: Vec<_> = self
            .instances
            .keys()
            .filter_map(|(mode, candidate)| (*candidate == content).then_some(*mode))
            .collect();
        for mode in modes {
            let instance = self
                .instances
                .get(&(mode, content))
                .expect("collected mode exists");
            let draft = drafts.content_mut(mode, content, instance);
            if draft.faulted {
                continue;
            }
            let checkpoint = draft.state.clone_box();
            let context = ModeContentContext::new(content, contents);
            if instance
                .registered
                .mode()
                .on_content_changed(draft.state.as_mut(), &context, change)
                .is_err()
            {
                draft.state = checkpoint;
                draft.faulted = true;
            } else {
                draft.background_job_dirty = true;
            }
        }
    }

    fn active_instance(&self, content: ContentId) -> Option<&ModeContentInstance> {
        self.instances
            .iter()
            .find_map(|((_, candidate), instance)| (*candidate == content).then_some(instance))
    }

    fn instance(&self, mode: ModeId, content: ContentId) -> Option<&ModeContentInstance> {
        self.instances.get(&(mode, content))
    }

    pub(crate) fn execute(
        &mut self,
        registry: &ModeRegistry,
        contents: &ContentStore,
        content: ContentId,
        command: &crate::app::command::ModeCommand,
        drafts: &mut ModeDraftJournal,
    ) -> Result<ModeResult, ModeError> {
        let (mode, action) = registry.resolve_command_checked(&command.mode, &command.action)?;
        let Some(instance) = self.instances.get(&(mode, content)) else {
            return Err(ModeError::InactiveMode {
                requested: command.mode.clone(),
                active: self
                    .active_instance(content)
                    .map(|instance| instance.registered.mode().name().clone()),
            });
        };
        let draft = drafts.content_mut(mode, content, instance);
        let result = instance.execute(
            draft.state.as_mut(),
            draft.faulted,
            mode,
            action,
            &command.arguments,
            contents,
        );
        if result.is_ok() {
            draft.background_job_dirty = true;
        }
        result
    }
}

impl ModeViewInstance {
    fn initialize(
        &mut self,
        content_state: &dyn ModeState,
        content_faulted: bool,
        context: &ModeViewContext<'_>,
    ) {
        if content_faulted {
            return;
        }
        match self
            .registered
            .mode()
            .create_view_state(content_state, context)
        {
            Ok(state) => self.state = state,
            Err(_) => self.faulted = true,
        }
    }

    pub(crate) fn name(&self) -> &ModeName {
        self.registered.mode().name()
    }

    pub(crate) fn register_faces(&self, faces: &mut FaceRegistry) {
        faces.register_defaults(self.registered.mode());
    }

    pub(crate) fn execute_with_context(
        &self,
        content_state: &mut dyn ModeState,
        view_state: &mut dyn ModeState,
        faulted: bool,
        action: ModeActionId,
        arguments: &ModeValue,
        context: &ModeViewContext<'_>,
    ) -> Result<ModeResult, ModeError> {
        if faulted {
            return Err(ModeError::InactiveMode {
                requested: self.name().clone(),
                active: None,
            });
        }
        let action = self
            .registered
            .action_names
            .get(usize::try_from(action.0).expect("mode action index overflow"))
            .expect("mode action id belongs to registered mode");
        self.registered.mode().execute_view_with_arguments(
            content_state,
            view_state,
            context,
            action,
            arguments,
        )
    }
}

impl ModeViewInstance {
    fn input_cancel_with_content(
        &self,
        content_state: &mut dyn ModeState,
        view_state: &mut dyn ModeState,
        faulted: bool,
        context: &ModeViewContext<'_>,
    ) {
        if faulted {
            return;
        }
        self.registered
            .mode()
            .input_cancel(content_state, view_state, context);
    }
}

#[derive(Default)]
pub(crate) struct ModeViewStore {
    chains: HashMap<ViewId, Vec<ModeId>>,
    instances: HashMap<(ModeId, ViewId), ModeViewInstance>,
}

impl ModeViewStore {
    #[cfg(test)]
    pub(crate) fn faults_for_test(&self) -> Vec<(String, ViewId)> {
        self.instances
            .iter()
            .filter(|(_, instance)| instance.faulted)
            .map(|((_, view), instance)| (instance.name().as_str().to_owned(), *view))
            .collect()
    }

    #[cfg(test)]
    pub(crate) fn state_for_test<T: 'static>(&self, mode: ModeId, view: ViewId) -> Option<&T> {
        self.instances
            .get(&(mode, view))?
            .state
            .as_any()
            .downcast_ref()
    }

    pub(crate) fn is_active(&self, view: ViewId) -> bool {
        self.chains
            .get(&view)
            .is_some_and(|chain| !chain.is_empty())
    }

    pub(crate) fn insert(&mut self, view: ViewId, mode: ModeViewInstance) {
        let id = mode.registered.id;
        let chain = self.chains.entry(view).or_default();
        assert!(
            !chain.contains(&id),
            "a mode may only be attached to a view once"
        );
        chain.push(id);
        assert!(self.instances.insert((id, view), mode).is_none());
    }

    pub(crate) fn remove(&mut self, view: ViewId) -> Vec<ModeId> {
        let modes = self.chains.remove(&view).unwrap_or_default();
        for mode in &modes {
            self.instances.remove(&(*mode, view));
        }
        modes
    }

    pub(crate) fn mode_ids(&self, view: ViewId) -> &[ModeId] {
        self.chains.get(&view).map_or(&[], Vec::as_slice)
    }

    pub(crate) fn mode_names(&self, view: ViewId) -> Vec<ModeName> {
        self.mode_ids(view)
            .iter()
            .filter_map(|mode| self.instances.get(&(*mode, view)))
            .map(|instance| instance.name().clone())
            .collect()
    }

    pub(crate) fn notify_changed(
        &self,
        views: &HashMap<ViewId, View>,
        content: ContentId,
        mode_contents: &ModeContentStore,
        contents: &ContentStore,
        change: &ContentChange,
        drafts: &mut ModeDraftJournal,
    ) {
        let targets: Vec<_> = views
            .iter()
            .filter_map(|(view, data)| (data.content() == content).then_some(*view))
            .collect();
        for view in targets {
            let context = ModeViewContext::new(view, &views[&view], contents);
            let modes = self.mode_ids(view).to_vec();
            for mode in modes {
                let Some(content_instance) = mode_contents.instance(mode, content) else {
                    continue;
                };
                let Some(view_instance) = self.instances.get(&(mode, view)) else {
                    continue;
                };
                let (content_draft, view_draft) = drafts.content_and_view_mut(
                    mode,
                    content,
                    view,
                    content_instance,
                    view_instance,
                );
                if content_draft.faulted || view_draft.faulted {
                    continue;
                }
                let content_checkpoint = content_draft.state.clone_box();
                let view_checkpoint = view_draft.state.clone_box();
                if view_instance
                    .registered
                    .mode()
                    .on_view_content_changed(
                        content_draft.state.as_mut(),
                        view_draft.state.as_mut(),
                        &context,
                        change,
                    )
                    .is_err()
                {
                    content_draft.state = content_checkpoint;
                    view_draft.state = view_checkpoint;
                    view_draft.faulted = true;
                }
            }
        }
    }

    pub(crate) fn presentation_layer(
        &self,
        mode: ModeId,
        view: ViewId,
        context: &ModeViewContext<'_>,
        mode_contents: &ModeContentStore,
        view_revision: Revision,
        visible_rows: RowRange,
    ) -> Option<ViewPresentationLayer> {
        let content_instance = mode_contents.instance(mode, context.content_id())?;
        let view_instance = self.instances.get(&(mode, view))?;
        if content_instance.faulted || view_instance.faulted {
            return None;
        }
        let definition = view_instance.registered.mode();
        Some(ViewPresentationLayer {
            content_revision: context.content_revision()?,
            view_revision,
            policy: definition.view_policy(
                content_instance.state.as_ref(),
                view_instance.state.as_ref(),
                context,
            ),
            decorations: definition.view_decorations(
                content_instance.state.as_ref(),
                view_instance.state.as_ref(),
                context,
                visible_rows,
            ),
        })
    }

    pub(crate) fn contains(&self, view: ViewId, name: &ModeName) -> bool {
        self.mode_ids(view)
            .iter()
            .filter_map(|mode| self.instances.get(&(*mode, view)))
            .any(|instance| instance.name() == name)
    }

    fn first(&self, view: ViewId) -> Option<&ModeViewInstance> {
        let mode = *self.mode_ids(view).first()?;
        self.instances.get(&(mode, view))
    }

    pub(crate) fn keymap_at<'a>(
        &'a self,
        view: ViewId,
        index: usize,
        context: &ModeViewContext<'_>,
        mode_contents: &'a ModeContentStore,
        drafts: &'a ModeDraftJournal,
    ) -> Option<&'a Keymap<Command>> {
        let mode = *self.mode_ids(view).get(index)?;
        let content_state = mode_contents.instance(mode, context.content_id())?;
        let instance = self.instances.get(&(mode, view))?;
        let (content_state, content_faulted) =
            drafts.content(mode, context.content_id(), content_state);
        let (view_state, view_faulted) = drafts.view(mode, view, instance);
        (!content_faulted && !view_faulted).then(|| {
            instance
                .registered
                .mode()
                .input_keymap(content_state, view_state, context)
        })
    }

    pub(crate) fn fallback_at(
        &self,
        view: ViewId,
        index: usize,
        context: &ModeViewContext<'_>,
        mode_contents: &ModeContentStore,
        drafts: &ModeDraftJournal,
        key: KeyEvent,
    ) -> Option<Command> {
        let mode = *self.mode_ids(view).get(index)?;
        let content_state = mode_contents.instance(mode, context.content_id())?;
        let instance = self.instances.get(&(mode, view))?;
        let (content_state, content_faulted) =
            drafts.content(mode, context.content_id(), content_state);
        let (view_state, view_faulted) = drafts.view(mode, view, instance);
        if content_faulted || view_faulted {
            return None;
        }
        instance
            .registered
            .mode()
            .input_typing(content_state, view_state, context, key)
    }

    pub(crate) fn status_at(
        &self,
        view: ViewId,
        index: usize,
        context: &ModeViewContext<'_>,
        mode_contents: &ModeContentStore,
        drafts: &ModeDraftJournal,
    ) -> InputStatus {
        let Some(mode) = self.mode_ids(view).get(index).copied() else {
            return InputStatus::Ready;
        };
        let Some(content_state) = mode_contents.instance(mode, context.content_id()) else {
            return InputStatus::Ready;
        };
        let Some(instance) = self.instances.get(&(mode, view)) else {
            return InputStatus::Ready;
        };
        let (content_state, content_faulted) =
            drafts.content(mode, context.content_id(), content_state);
        let (view_state, view_faulted) = drafts.view(mode, view, instance);
        if content_faulted || view_faulted {
            return InputStatus::Ready;
        }
        instance
            .registered
            .mode()
            .mode_input_status(content_state, view_state, context)
    }

    pub(crate) fn capture_at(
        &self,
        view: ViewId,
        index: usize,
        context: &ModeViewContext<'_>,
        mode_contents: &ModeContentStore,
        drafts: &mut ModeDraftJournal,
        key: KeyEvent,
    ) -> InputDecision<Command> {
        let Some(mode) = self.mode_ids(view).get(index).copied() else {
            return InputDecision::Pass;
        };
        let Some(content_state) = mode_contents.instance(mode, context.content_id()) else {
            return InputDecision::Pass;
        };
        let Some(instance) = self.instances.get(&(mode, view)) else {
            return InputDecision::Pass;
        };
        let (content_draft, view_draft) =
            drafts.content_and_view_mut(mode, context.content_id(), view, content_state, instance);
        if content_draft.faulted || view_draft.faulted {
            return InputDecision::Pass;
        }
        instance.registered.mode().input_capture(
            content_draft.state.as_mut(),
            view_draft.state.as_mut(),
            context,
            key,
        )
    }

    pub(crate) fn timeout_at(
        &self,
        view: ViewId,
        index: usize,
        context: &ModeViewContext<'_>,
        mode_contents: &ModeContentStore,
        drafts: &mut ModeDraftJournal,
    ) -> Option<Vec<ModeEffect>> {
        let mode = self.mode_ids(view).get(index).copied()?;
        let content_state = mode_contents.instance(mode, context.content_id())?;
        let instance = self.instances.get(&(mode, view))?;
        let (content_draft, view_draft) =
            drafts.content_and_view_mut(mode, context.content_id(), view, content_state, instance);
        if content_draft.faulted || view_draft.faulted {
            return None;
        }
        Some(
            instance
                .registered
                .mode()
                .input_timeout(
                    content_draft.state.as_mut(),
                    view_draft.state.as_mut(),
                    context,
                )
                .into_operations(),
        )
    }

    pub(crate) fn fallback_in_chain(
        &self,
        view: ViewId,
        start_mode: usize,
        context: &ModeViewContext<'_>,
        mode_contents: &ModeContentStore,
        drafts: &ModeDraftJournal,
        key: KeyEvent,
    ) -> Option<(usize, Command)> {
        self.mode_ids(view)
            .iter()
            .enumerate()
            .skip(start_mode)
            .find_map(|(index, mode)| {
                let content_instance = mode_contents.instance(*mode, context.content_id())?;
                let view_instance = self.instances.get(&(*mode, view))?;
                let (content_state, content_faulted) =
                    drafts.content(*mode, context.content_id(), content_instance);
                let (view_state, view_faulted) = drafts.view(*mode, view, view_instance);
                if content_faulted || view_faulted {
                    return None;
                }
                view_instance
                    .registered
                    .mode()
                    .input_typing(content_state, view_state, context, key)
                    .map(|command| (index, command))
            })
    }

    pub(crate) fn cancel_chain(
        &mut self,
        view: ViewId,
        context: &ModeViewContext<'_>,
        mode_contents: &mut ModeContentStore,
        _contents: &ContentStore,
    ) {
        let modes = self.mode_ids(view).to_vec();
        let mut drafts = ModeDraftJournal::default();
        for mode in modes {
            let Some(content_state) = mode_contents.instance(mode, context.content_id()) else {
                continue;
            };
            let Some(instance) = self.instances.get(&(mode, view)) else {
                continue;
            };
            let (content_draft, view_draft) = drafts.content_and_view_mut(
                mode,
                context.content_id(),
                view,
                content_state,
                instance,
            );
            instance.input_cancel_with_content(
                content_draft.state.as_mut(),
                view_draft.state.as_mut(),
                view_draft.faulted,
                context,
            );
        }
        drafts.commit_content(mode_contents);
        drafts.commit_views(self);
    }

    pub(crate) fn view_policy_in_draft(
        &self,
        view: ViewId,
        context: &ModeViewContext<'_>,
        mode_contents: &ModeContentStore,
        drafts: &ModeDraftJournal,
    ) -> ModeViewPolicy {
        let mut policy = ModeViewPolicy::default();
        for mode in self.mode_ids(view) {
            let Some(content_instance) = mode_contents.instance(*mode, context.content_id()) else {
                continue;
            };
            let Some(view_instance) = self.instances.get(&(*mode, view)) else {
                continue;
            };
            let (content_state, content_faulted) =
                drafts.content(*mode, context.content_id(), content_instance);
            let (view_state, view_faulted) = drafts.view(*mode, view, view_instance);
            if content_faulted || view_faulted {
                continue;
            }
            policy.merge_missing(view_instance.registered.mode().view_policy(
                content_state,
                view_state,
                context,
            ));
        }
        policy
    }

    pub(crate) fn execute_with_context(
        &mut self,
        view: ViewId,
        registry: &ModeRegistry,
        command: &crate::app::command::ModeCommand,
        context: &ModeViewContext<'_>,
        mode_contents: &mut ModeContentStore,
        drafts: &mut ModeDraftJournal,
    ) -> Result<ModeResult, ModeError> {
        let (mode, action) = registry.resolve_command_checked(&command.mode, &command.action)?;
        let Some(instance) = self.instances.get(&(mode, view)) else {
            return Err(ModeError::InactiveMode {
                requested: command.mode.clone(),
                active: self.first(view).map(|instance| instance.name().clone()),
            });
        };
        let content_state = mode_contents
            .instance(mode, context.content_id())
            .expect("attached mode has content state");
        let (content_draft, view_draft) =
            drafts.content_and_view_mut(mode, context.content_id(), view, content_state, instance);
        let result = instance.execute_with_context(
            content_draft.state.as_mut(),
            view_draft.state.as_mut(),
            view_draft.faulted || content_draft.faulted,
            action,
            &command.arguments,
            context,
        );
        if result.is_ok() {
            content_draft.background_job_dirty = true;
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    struct CountingJobMode {
        name: ModeName,
        calls: Rc<Cell<usize>>,
    }

    struct DraftStateMode {
        name: ModeName,
        actions: Vec<ModeActionName>,
        fail_observer: bool,
    }

    impl Mode for DraftStateMode {
        fn name(&self) -> &ModeName {
            &self.name
        }

        fn actions(&self) -> &[ModeActionName] {
            &self.actions
        }

        fn action_scope(&self, _action: &ModeActionName) -> ModeActionScope {
            ModeActionScope::Content
        }

        fn new_content_state(&self) -> Box<dyn ModeState> {
            Box::new(0_u8)
        }

        fn execute_content(
            &self,
            state: &mut dyn ModeState,
            _context: &ModeContentContext<'_>,
            _action: &ModeActionName,
        ) -> Result<ModeResult, ModeError> {
            *state.as_any_mut().downcast_mut::<u8>().unwrap() += 1;
            Ok(ModeResult::none())
        }

        fn on_content_changed(
            &self,
            state: &mut dyn ModeState,
            _context: &ModeContentContext<'_>,
            _change: &ContentChange,
        ) -> Result<(), ModeError> {
            *state.as_any_mut().downcast_mut::<u8>().unwrap() += 1;
            if self.fail_observer {
                Err(ModeError::CallbackFailed {
                    mode: self.name.clone(),
                    message: "observer failed".to_owned(),
                })
            } else {
                Ok(())
            }
        }
    }

    impl Mode for CountingJobMode {
        fn name(&self) -> &ModeName {
            &self.name
        }

        fn actions(&self) -> &[ModeActionName] {
            &[]
        }

        fn take_background_job(
            &self,
            _state: &mut dyn ModeState,
            _context: &ModeContentContext<'_>,
        ) -> Option<ModeJobRequest> {
            self.calls.set(self.calls.get() + 1);
            None
        }
    }

    #[test]
    fn unchanged_content_does_not_poll_background_jobs_again() {
        let calls = Rc::new(Cell::new(0));
        let name = ModeName::new("counting-jobs");
        let mut registry = ModeRegistry::new();
        registry.register(CountingJobMode {
            name: name.clone(),
            calls: calls.clone(),
        });
        let mode = registry.instantiate(&name).unwrap();
        let mut content_modes = ModeContentStore::default();
        content_modes.attach_view(ContentId(1), &mode);
        let contents = ContentStore::default();

        assert!(content_modes.take_background_jobs(&contents).is_empty());
        assert!(content_modes.take_background_jobs(&contents).is_empty());
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn content_state_draft_is_visible_in_frame_and_published_only_on_commit() {
        let name = ModeName::new("draft-state");
        let action = ModeActionName::new("advance");
        let mut registry = ModeRegistry::new();
        registry.register(DraftStateMode {
            name: name.clone(),
            actions: vec![action.clone()],
            fail_observer: false,
        });
        let mode = registry.instantiate(&name).unwrap();
        let mode_id = mode.registered.id;
        let content = ContentId(1);
        let mut content_modes = ModeContentStore::default();
        content_modes.attach_view(content, &mode);
        let contents = ContentStore::default();
        let command = crate::app::command::ModeCommand::new(name, action);
        let mut drafts = ModeDraftJournal::default();

        content_modes
            .execute(&registry, &contents, content, &command, &mut drafts)
            .unwrap();
        content_modes
            .execute(&registry, &contents, content, &command, &mut drafts)
            .unwrap();

        assert_eq!(
            content_modes.state_for_test::<u8>(mode_id, content),
            Some(&0)
        );
        let draft = drafts.content.get(&(mode_id, content)).unwrap();
        assert_eq!(draft.state.as_any().downcast_ref::<u8>(), Some(&2));

        drafts.commit_content(&mut content_modes);
        assert_eq!(
            content_modes.state_for_test::<u8>(mode_id, content),
            Some(&2)
        );

        let mut discarded = ModeDraftJournal::default();
        content_modes
            .execute(&registry, &contents, content, &command, &mut discarded)
            .unwrap();
        drop(discarded);
        assert_eq!(
            content_modes.state_for_test::<u8>(mode_id, content),
            Some(&2)
        );
    }

    #[test]
    fn passive_callback_fault_is_published_only_with_its_frame() {
        let name = ModeName::new("faulting-observer-draft");
        let mut registry = ModeRegistry::new();
        registry.register(DraftStateMode {
            name: name.clone(),
            actions: vec![ModeActionName::new("advance")],
            fail_observer: true,
        });
        let mode = registry.instantiate(&name).unwrap();
        let mode_id = mode.registered.id;
        let content = ContentId(1);
        let mut content_modes = ModeContentStore::default();
        content_modes.attach_view(content, &mode);
        let contents = ContentStore::default();
        let change = ContentChange::Text(
            crate::core::transaction::TextChangeSet::from_edits(
                0,
                vec![crate::core::transaction::TextEdit::new(0..0, "x")],
            )
            .unwrap(),
        );
        let mut discarded = ModeDraftJournal::default();

        content_modes.notify_changed(content, &contents, &change, &mut discarded);

        assert!(content_modes.faults_for_test().is_empty());
        assert_eq!(
            content_modes.state_for_test::<u8>(mode_id, content),
            Some(&0)
        );
        drop(discarded);
        assert!(content_modes.faults_for_test().is_empty());

        let mut committed = ModeDraftJournal::default();
        content_modes.notify_changed(content, &contents, &change, &mut committed);
        committed.commit_content(&mut content_modes);

        assert_eq!(
            content_modes.faults_for_test(),
            vec![(name.as_str().to_owned(), content)]
        );
        assert_eq!(
            content_modes.state_for_test::<u8>(mode_id, content),
            Some(&0)
        );
    }
}
