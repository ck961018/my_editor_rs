use std::any::Any;
use std::collections::HashMap;
use std::fmt;
use std::rc::Rc;
use std::sync::LazyLock;

use tokio_util::sync::CancellationToken;

use crate::app::action::{TransactionIntent, ViewAction};
use crate::app::command::{AppCommand, Command, ModeCommand, ModeValue};
use crate::app::mode_name::{ModeActionName, ModeName};
use crate::app::view::View;
use crate::core::action::ContentAction;
use crate::core::command::EditCommand;
use crate::core::content::ContentChange;
use crate::core::content_store::ContentStore;
use crate::core::input::{InputDecision, InputStatus};
use crate::core::keymap::Keymap;
use crate::protocol::content_query::{
    ContentData, ContentQuery, CursorStyle, Face, FaceName, NamedTextDecoration, RowRange,
    SelectionShape, TextDecoration,
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

pub(crate) struct ModeStateSnapshot(Box<dyn ModeState>);

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
    fn merge_missing(&mut self, next: Self) {
        self.cursor_style = self.cursor_style.or(next.cursor_style);
        self.cursor_domain = self.cursor_domain.or(next.cursor_domain);
        self.selection_shape = self.selection_shape.or(next.selection_shape);
        self.selection_face = self.selection_face.take().or(next.selection_face);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModeActionScope {
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
}

impl ModeContentInstance {
    fn snapshot(&self) -> ModeStateSnapshot {
        ModeStateSnapshot(self.state.clone_box())
    }

    fn restore(&mut self, snapshot: ModeStateSnapshot) {
        self.state = snapshot.0;
    }

    fn execute(
        &mut self,
        mode: ModeId,
        action: ModeActionId,
        arguments: &ModeValue,
        contents: &ContentStore,
    ) -> Result<ModeResult, ModeError> {
        if self.faulted {
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
        self.registered.mode().execute_content_with_arguments(
            self.state.as_mut(),
            &context,
            action,
            arguments,
        )
    }

    fn notify_changed(&mut self, contents: &ContentStore, change: &ContentChange) {
        if self.faulted {
            return;
        }
        let snapshot = self.snapshot();
        let context = ModeContentContext::new(self.content, contents);
        if self
            .registered
            .mode()
            .on_content_changed(self.state.as_mut(), &context, change)
            .is_err()
        {
            self.restore(snapshot);
            self.faulted = true;
        }
    }

    fn take_background_job(&mut self, contents: &ContentStore) -> Option<ModeJobRequest> {
        if self.faulted {
            return None;
        }
        let context = ModeContentContext::new(self.content, contents);
        self.registered
            .mode()
            .take_background_job(self.state.as_mut(), &context)
    }

    fn apply_background_job(
        &mut self,
        contents: &ContentStore,
        version: u64,
        result: ModeJobResult,
    ) -> bool {
        if self.faulted {
            return false;
        }
        let snapshot = self.snapshot();
        let context = ModeContentContext::new(self.content, contents);
        match self.registered.mode().apply_background_job(
            self.state.as_mut(),
            &context,
            version,
            result,
        ) {
            Ok(changed) => changed,
            Err(_) => {
                self.restore(snapshot);
                self.faulted = true;
                false
            }
        }
    }
}

#[derive(Default)]
pub(crate) struct ModeContentStore {
    instances: HashMap<(ModeId, ContentId), ModeContentInstance>,
}

impl ModeContentStore {
    pub(crate) fn take_background_jobs(
        &mut self,
        contents: &ContentStore,
    ) -> Vec<(ModeId, ContentId, ModeJobRequest)> {
        self.instances
            .iter_mut()
            .filter_map(|(&(mode, content), instance)| {
                instance
                    .take_background_job(contents)
                    .map(|job| (mode, content, job))
            })
            .collect()
    }

    pub(crate) fn apply_background_job(
        &mut self,
        mode: ModeId,
        content: ContentId,
        contents: &ContentStore,
        version: u64,
        result: ModeJobResult,
    ) -> bool {
        self.instance_mut(mode, content)
            .is_some_and(|instance| instance.apply_background_job(contents, version, result))
    }

    pub(crate) fn snapshots(
        &self,
        content: ContentId,
        modes: &[ModeId],
    ) -> Vec<(ModeId, ModeStateSnapshot)> {
        modes
            .iter()
            .filter_map(|mode| {
                self.instance(*mode, content)
                    .map(|instance| (*mode, instance.snapshot()))
            })
            .collect()
    }

    pub(crate) fn snapshot_for(
        &self,
        mode: ModeId,
        content: ContentId,
    ) -> Option<ModeStateSnapshot> {
        self.instance(mode, content)
            .map(ModeContentInstance::snapshot)
    }

    pub(crate) fn restore_for(
        &mut self,
        mode: ModeId,
        content: ContentId,
        snapshot: ModeStateSnapshot,
    ) {
        self.instance_mut(mode, content)
            .expect("mode content state still exists")
            .restore(snapshot);
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
        &mut self,
        content: ContentId,
        contents: &ContentStore,
        change: &ContentChange,
    ) {
        let modes: Vec<_> = self
            .instances
            .keys()
            .filter_map(|(mode, candidate)| (*candidate == content).then_some(*mode))
            .collect();
        for mode in modes {
            self.instances
                .get_mut(&(mode, content))
                .expect("collected mode exists")
                .notify_changed(contents, change);
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

    fn instance_mut(
        &mut self,
        mode: ModeId,
        content: ContentId,
    ) -> Option<&mut ModeContentInstance> {
        self.instances.get_mut(&(mode, content))
    }

    pub(crate) fn execute(
        &mut self,
        registry: &ModeRegistry,
        contents: &ContentStore,
        content: ContentId,
        command: &crate::app::command::ModeCommand,
    ) -> Result<ModeResult, ModeError> {
        let (mode, action) = registry.resolve_command_checked(&command.mode, &command.action)?;
        let Some(instance) = self.instances.get_mut(&(mode, content)) else {
            return Err(ModeError::InactiveMode {
                requested: command.mode.clone(),
                active: self
                    .active_instance(content)
                    .map(|instance| instance.registered.mode().name().clone()),
            });
        };
        instance.execute(mode, action, &command.arguments, contents)
    }
}

impl ModeViewInstance {
    fn snapshot(&self) -> ModeStateSnapshot {
        ModeStateSnapshot(self.state.clone_box())
    }

    fn restore(&mut self, snapshot: ModeStateSnapshot) {
        self.state = snapshot.0;
    }

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

    fn input_keymap(
        &self,
        content_state: &dyn ModeState,
        context: &ModeViewContext<'_>,
    ) -> &Keymap<Command> {
        if self.faulted {
            return &EMPTY_KEYMAP;
        }
        self.registered
            .mode()
            .input_keymap(content_state, self.state.as_ref(), context)
    }

    fn input_fallback(
        &self,
        content_state: &dyn ModeState,
        context: &ModeViewContext<'_>,
        key: KeyEvent,
    ) -> Option<Command> {
        if self.faulted {
            return None;
        }
        self.registered
            .mode()
            .input_typing(content_state, self.state.as_ref(), context, key)
    }

    fn view_policy(
        &self,
        content_state: &dyn ModeState,
        context: &ModeViewContext<'_>,
    ) -> ModeViewPolicy {
        if self.faulted {
            return ModeViewPolicy::default();
        }
        self.registered
            .mode()
            .view_policy(content_state, self.state.as_ref(), context)
    }

    pub(crate) fn execute_with_context(
        &mut self,
        content_state: &mut dyn ModeState,
        mode: ModeId,
        action: ModeActionId,
        arguments: &ModeValue,
        context: &ModeViewContext<'_>,
    ) -> Result<ModeResult, ModeError> {
        if self.faulted {
            return Err(ModeError::InactiveMode {
                requested: self.name().clone(),
                active: None,
            });
        }
        assert_eq!(self.registered.id, mode, "resolved mode must be active");
        let action = self
            .registered
            .action_names
            .get(usize::try_from(action.0).expect("mode action index overflow"))
            .expect("mode action id belongs to registered mode");
        self.registered.mode().execute_view_with_arguments(
            content_state,
            self.state.as_mut(),
            context,
            action,
            arguments,
        )
    }
}

impl ModeViewInstance {
    fn input_status_with_content(
        &self,
        content_state: &dyn ModeState,
        context: &ModeViewContext<'_>,
    ) -> InputStatus {
        if self.faulted {
            return InputStatus::Ready;
        }
        self.registered
            .mode()
            .mode_input_status(content_state, self.state.as_ref(), context)
    }

    fn input_capture_with_content(
        &mut self,
        content_state: &mut dyn ModeState,
        context: &ModeViewContext<'_>,
        key: KeyEvent,
    ) -> InputDecision<Command> {
        if self.faulted {
            return InputDecision::Pass;
        }
        self.registered
            .mode()
            .input_capture(content_state, self.state.as_mut(), context, key)
    }

    fn input_timeout_with_content(
        &mut self,
        content_state: &mut dyn ModeState,
        context: &ModeViewContext<'_>,
    ) -> Vec<ModeEffect> {
        if self.faulted {
            return Vec::new();
        }
        self.registered
            .mode()
            .input_timeout(content_state, self.state.as_mut(), context)
            .into_operations()
    }

    fn input_cancel_with_content(
        &mut self,
        content_state: &mut dyn ModeState,
        context: &ModeViewContext<'_>,
    ) {
        if self.faulted {
            return;
        }
        self.registered
            .mode()
            .input_cancel(content_state, self.state.as_mut(), context);
    }

    fn notify_changed(
        &mut self,
        content: &mut ModeContentInstance,
        context: &ModeViewContext<'_>,
        change: &ContentChange,
    ) {
        if self.faulted || content.faulted {
            return;
        }
        let content_snapshot = content.snapshot();
        let view_snapshot = self.snapshot();
        if self
            .registered
            .mode()
            .on_view_content_changed(content.state.as_mut(), self.state.as_mut(), context, change)
            .is_err()
        {
            content.restore(content_snapshot);
            self.restore(view_snapshot);
            self.faulted = true;
        }
    }
}

#[derive(Default)]
pub(crate) struct ModeViewStore {
    chains: HashMap<ViewId, Vec<ModeId>>,
    instances: HashMap<(ModeId, ViewId), ModeViewInstance>,
}

impl ModeViewStore {
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
        &mut self,
        views: &HashMap<ViewId, View>,
        content: ContentId,
        mode_contents: &mut ModeContentStore,
        contents: &ContentStore,
        change: &ContentChange,
    ) {
        let targets: Vec<_> = views
            .iter()
            .filter_map(|(view, data)| (data.content() == content).then_some(*view))
            .collect();
        for view in targets {
            let context = ModeViewContext::new(view, &views[&view], contents);
            let modes = self.mode_ids(view).to_vec();
            for mode in modes {
                let Some(content_instance) = mode_contents.instance_mut(mode, content) else {
                    continue;
                };
                if let Some(view_instance) = self.instances.get_mut(&(mode, view)) {
                    view_instance.notify_changed(content_instance, &context, change);
                }
            }
        }
    }

    pub(crate) fn decorations(
        &self,
        view: ViewId,
        context: &ModeViewContext<'_>,
        mode_contents: &ModeContentStore,
        faces: &FaceRegistry,
        visible_rows: RowRange,
    ) -> Vec<TextDecoration> {
        let mut decorations = Vec::new();
        for mode in self.mode_ids(view).iter().rev() {
            let Some(content_instance) = mode_contents.instance(*mode, context.content_id()) else {
                continue;
            };
            let Some(view_instance) = self.instances.get(&(*mode, view)) else {
                continue;
            };
            if content_instance.faulted || view_instance.faulted {
                continue;
            }
            decorations.extend(
                view_instance
                    .registered
                    .mode()
                    .decorations(
                        content_instance.state.as_ref(),
                        view_instance.state.as_ref(),
                        context,
                        visible_rows,
                    )
                    .into_iter()
                    .map(|decoration| TextDecoration {
                        start: decoration.start,
                        end: decoration.end,
                        face: faces.resolve(&decoration.face),
                    }),
            );
        }
        decorations
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
    ) -> Option<&'a Keymap<Command>> {
        let mode = *self.mode_ids(view).get(index)?;
        let content_state = mode_contents.instance(mode, context.content_id())?;
        let instance = self.instances.get(&(mode, view))?;
        (!content_state.faulted && !instance.faulted)
            .then(|| instance.input_keymap(content_state.state.as_ref(), context))
    }

    pub(crate) fn fallback_at(
        &self,
        view: ViewId,
        index: usize,
        context: &ModeViewContext<'_>,
        mode_contents: &ModeContentStore,
        key: KeyEvent,
    ) -> Option<Command> {
        let mode = *self.mode_ids(view).get(index)?;
        let content_state = mode_contents.instance(mode, context.content_id())?;
        let instance = self.instances.get(&(mode, view))?;
        if content_state.faulted || instance.faulted {
            return None;
        }
        instance.input_fallback(content_state.state.as_ref(), context, key)
    }

    pub(crate) fn status_at(
        &self,
        view: ViewId,
        index: usize,
        context: &ModeViewContext<'_>,
        mode_contents: &ModeContentStore,
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
        if content_state.faulted || instance.faulted {
            return InputStatus::Ready;
        }
        instance.input_status_with_content(content_state.state.as_ref(), context)
    }

    pub(crate) fn capture_at(
        &mut self,
        view: ViewId,
        index: usize,
        context: &ModeViewContext<'_>,
        mode_contents: &mut ModeContentStore,
        key: KeyEvent,
    ) -> InputDecision<Command> {
        let Some(mode) = self.mode_ids(view).get(index).copied() else {
            return InputDecision::Pass;
        };
        let Some(content_state) = mode_contents.instance_mut(mode, context.content_id()) else {
            return InputDecision::Pass;
        };
        let Some(instance) = self.instances.get_mut(&(mode, view)) else {
            return InputDecision::Pass;
        };
        if content_state.faulted || instance.faulted {
            return InputDecision::Pass;
        }
        instance.input_capture_with_content(content_state.state.as_mut(), context, key)
    }

    pub(crate) fn timeout_at(
        &mut self,
        view: ViewId,
        index: usize,
        context: &ModeViewContext<'_>,
        mode_contents: &mut ModeContentStore,
    ) -> Option<Vec<ModeEffect>> {
        let mode = self.mode_ids(view).get(index).copied()?;
        let content_state = mode_contents.instance_mut(mode, context.content_id())?;
        let instance = self.instances.get_mut(&(mode, view))?;
        if content_state.faulted || instance.faulted {
            return None;
        }
        Some(instance.input_timeout_with_content(content_state.state.as_mut(), context))
    }

    pub(crate) fn fallback_in_chain(
        &self,
        view: ViewId,
        start_mode: usize,
        context: &ModeViewContext<'_>,
        mode_contents: &ModeContentStore,
        _contents: &ContentStore,
        key: KeyEvent,
    ) -> Option<(usize, Command)> {
        self.mode_ids(view)
            .iter()
            .enumerate()
            .skip(start_mode)
            .find_map(|(index, mode)| {
                let content_state = mode_contents.instance(*mode, context.content_id())?;
                self.instances
                    .get(&(*mode, view))?
                    .input_fallback(content_state.state.as_ref(), context, key)
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
        for mode in modes {
            if let Some(content_state) = mode_contents.instance_mut(mode, context.content_id())
                && let Some(instance) = self.instances.get_mut(&(mode, view))
            {
                instance.input_cancel_with_content(content_state.state.as_mut(), context);
            }
        }
    }

    pub(crate) fn snapshots(&self, view: ViewId) -> Vec<(ModeId, ModeStateSnapshot)> {
        self.mode_ids(view)
            .iter()
            .filter_map(|mode| {
                self.instances
                    .get(&(*mode, view))
                    .map(|instance| (*mode, instance.snapshot()))
            })
            .collect()
    }

    pub(crate) fn snapshot_for(&self, mode: ModeId, view: ViewId) -> Option<ModeStateSnapshot> {
        self.instances
            .get(&(mode, view))
            .map(ModeViewInstance::snapshot)
    }

    pub(crate) fn restore_for(&mut self, mode: ModeId, view: ViewId, snapshot: ModeStateSnapshot) {
        self.instances
            .get_mut(&(mode, view))
            .expect("mode view state still exists")
            .restore(snapshot);
    }

    pub(crate) fn view_policy(
        &self,
        view: ViewId,
        context: &ModeViewContext<'_>,
        mode_contents: &ModeContentStore,
    ) -> ModeViewPolicy {
        let mut policy = ModeViewPolicy::default();
        for mode in self.mode_ids(view) {
            let Some(content_state) = mode_contents.instance(*mode, context.content_id()) else {
                continue;
            };
            let Some(view_state) = self.instances.get(&(*mode, view)) else {
                continue;
            };
            if content_state.faulted || view_state.faulted {
                continue;
            }
            policy.merge_missing(view_state.view_policy(content_state.state.as_ref(), context));
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
    ) -> Result<ModeResult, ModeError> {
        let (mode, action) = registry.resolve_command_checked(&command.mode, &command.action)?;
        let Some(instance) = self.instances.get_mut(&(mode, view)) else {
            return Err(ModeError::InactiveMode {
                requested: command.mode.clone(),
                active: self.first(view).map(|instance| instance.name().clone()),
            });
        };
        let content_state = mode_contents
            .instance_mut(mode, context.content_id())
            .expect("attached mode has content state");
        instance.execute_with_context(
            content_state.state.as_mut(),
            mode,
            action,
            &command.arguments,
            context,
        )
    }
}
