use std::any::Any;
use std::collections::HashMap;
use std::fmt;
use std::rc::Rc;
use std::sync::LazyLock;

use tokio_util::sync::CancellationToken;

use crate::app::action::{TransactionIntent, ViewAction};
use crate::app::command::{
    AppCommand, Command, ContentCommand, ModeCommand, ModeValue, TransactionCommand,
};
use crate::app::mode_name::{ModeActionName, ModeName};
use crate::app::view::View;
use crate::core::action::ContentAction;
use crate::core::command::{CharSearchDirection, EditCommand};
use crate::core::content::ContentChange;
use crate::core::content_store::ContentStore;
use crate::core::input::{InputDecision, InputStatus, TimeoutPolicy};
use crate::core::keymap::Keymap;
use crate::core::motion::{OperatorCommand, TextMotion, TextOperator, TextTarget};
use crate::protocol::content_query::{
    ContentData, ContentQuery, CursorStyle, Face, FaceName, NamedTextDecoration, RowRange,
    SelectionShape, TextDecoration,
};
use crate::protocol::ids::{ContentId, ViewId};
use crate::protocol::key_event::{KeyCode, KeyEvent};
use crate::protocol::revision::Revision;
use crate::protocol::selection::Selections;
use crate::protocol::viewport::{
    ViewportCommand, ViewportCursorBehavior, ViewportMoveAmount, ViewportMoveDirection,
};

mod keymaps;
mod tree_sitter;

use tree_sitter::TreeSitterMode;

static EMPTY_KEYMAP: LazyLock<Keymap<Command>> = LazyLock::new(Keymap::new);

#[cfg(test)]
use keymaps::{plain_edit_keymap, vim_mode_command};
use keymaps::{vim_insert_keymap, vim_normal_keymap, vim_visual_keymap};

trait CommandKeymapExt {
    fn bind_edit(
        &mut self,
        sequence: impl AsRef<[KeyEvent]>,
        command: EditCommand,
    ) -> Option<Command>;
}

impl CommandKeymapExt for Keymap<Command> {
    fn bind_edit(
        &mut self,
        sequence: impl AsRef<[KeyEvent]>,
        command: EditCommand,
    ) -> Option<Command> {
        self.bind(sequence, command.into())
    }
}

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

#[allow(dead_code, reason = "built-in Vim does not need every read capability")]
pub struct ModeViewContext<'a> {
    view_id: ViewId,
    view: &'a View,
    contents: &'a ContentStore,
}

#[allow(dead_code, reason = "built-in Vim does not need every read capability")]
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

    fn resolve_edit(&self, command: EditCommand) -> Option<ResolvedViewEdit> {
        let before = self.selections()?.clone();
        let plan = self
            .contents
            .plan_edit(self.content_id(), command, &before)?;
        Some(ResolvedViewEdit {
            content: plan.action,
            view: Some(ViewAction::SetSelections(plan.selections)),
            before,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModeResult {
    flow: InputFlow,
    operations: Vec<ModeEffect>,
    #[cfg(test)]
    edit_command: Option<EditCommand>,
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
            #[cfg(test)]
            edit_command: None,
        }
    }

    #[allow(dead_code, reason = "Mode results are an extension-facing API")]
    pub fn operations(operations: Vec<ModeEffect>) -> Self {
        Self {
            flow: InputFlow::Stop,
            operations,
            #[cfg(test)]
            edit_command: None,
        }
    }

    #[allow(dead_code, reason = "dynamic modes can pass input to the next mode")]
    pub fn continue_with(operations: Vec<ModeEffect>) -> Self {
        Self {
            flow: InputFlow::Continue,
            operations,
            #[cfg(test)]
            edit_command: None,
        }
    }

    fn from_command(context: &ModeViewContext<'_>, command: Option<Command>) -> Self {
        #[cfg(test)]
        let edit_command = command.as_ref().and_then(first_edit_command);
        let mut operations = Vec::new();
        if let Some(command) = command {
            append_command_operations(context, command, &mut operations, false);
        }
        Self {
            flow: InputFlow::Stop,
            operations,
            #[cfg(test)]
            edit_command,
        }
    }

    fn into_operations(self) -> Vec<ModeEffect> {
        self.operations
    }

    pub(crate) fn into_parts(self) -> (InputFlow, Vec<ModeEffect>) {
        (self.flow, self.operations)
    }
}

#[cfg(test)]
fn first_edit_command(command: &Command) -> Option<EditCommand> {
    match command {
        Command::Content(ContentCommand::Edit(command)) => Some(command.clone()),
        Command::Content(ContentCommand::Sequence(commands)) => commands
            .clone()
            .into_commands()
            .iter()
            .find_map(|command| first_edit_command(&Command::Content(command.clone()))),
        _ => None,
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

fn append_command_operations(
    context: &ModeViewContext<'_>,
    command: Command,
    operations: &mut Vec<ModeEffect>,
    defer_edits: bool,
) {
    match command {
        Command::Content(ContentCommand::Edit(command)) => {
            if defer_edits {
                operations.push(ModeEffect::DeferredEdit(command));
            } else if let Some(edit) = context.resolve_edit(command) {
                operations.push(ModeEffect::Edit(edit));
            }
        }
        Command::Content(ContentCommand::Transaction(command)) => {
            operations.push(ModeEffect::Transaction(match command {
                TransactionCommand::Begin => TransactionIntent::Begin,
                TransactionCommand::Commit => TransactionIntent::Commit,
                TransactionCommand::Rollback => TransactionIntent::Rollback,
            }));
        }
        Command::Content(ContentCommand::Undo) => {
            operations.push(ModeEffect::Transaction(TransactionIntent::Undo));
        }
        Command::Content(ContentCommand::Redo) => {
            operations.push(ModeEffect::Transaction(TransactionIntent::Redo));
        }
        Command::Content(ContentCommand::Sequence(commands)) => {
            for command in commands.into_commands() {
                append_command_operations(context, Command::Content(command), operations, true);
            }
        }
        Command::Content(ContentCommand::Save) => operations.push(ModeEffect::Save),
        Command::App(command) => operations.push(ModeEffect::App(command)),
        Command::Mode(command) => operations.push(ModeEffect::Mode(command)),
        Command::Viewport(command) => operations.push(ModeEffect::Viewport(command)),
        Command::Noop => {}
    }
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

    pub(crate) fn builtin() -> Self {
        let mut registry = Self::new();
        registry.register(VimMode::new());
        registry.register(TreeSitterMode::new());
        registry
    }

    #[cfg(test)]
    pub(crate) fn plain_edit() -> Self {
        let mut registry = Self::new();
        registry.register(PlainEditMode::new());
        registry
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

    #[cfg(test)]
    pub(crate) fn instantiate_content(
        &self,
        name: &ModeName,
        content: ContentId,
    ) -> Option<ModeContentInstance> {
        let id = self.resolve_mode(name)?;
        let registered = self.definitions.get(&id)?.clone();
        let state = registered.definition.new_content_state();
        Some(ModeContentInstance {
            content,
            state,
            registered,
            attachments: 0,
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
    pub(crate) fn bind(
        &mut self,
        registry: &ModeRegistry,
        content: ContentId,
        name: &ModeName,
        _contents: &ContentStore,
    ) -> bool {
        let Some(mut instance) = registry.instantiate_content(name, content) else {
            return false;
        };
        let mode = instance.registered.id;
        if let Some(existing) = self.instances.get_mut(&(mode, content)) {
            existing.attachments += 1;
            return true;
        }
        instance.attachments = 1;
        self.instances.insert((mode, content), instance);
        true
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

    #[cfg(test)]
    pub(crate) fn keymap(&self, context: &ModeViewContext<'_>) -> &Keymap<Command> {
        if self.faulted {
            return &EMPTY_KEYMAP;
        }
        self.registered.mode().keymap(self.state.as_ref(), context)
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

    #[cfg(test)]
    pub(crate) fn resolve_key(
        &self,
        context: &ModeViewContext<'_>,
        key: KeyEvent,
    ) -> Option<Command> {
        self.keymap(context)
            .node(&[key])
            .and_then(|node| node.action().cloned())
            .or_else(|| {
                self.registered
                    .mode()
                    .typing(self.state.as_ref(), context, key)
            })
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

    #[cfg(test)]
    fn timeout(&mut self, context: &ModeViewContext<'_>) -> Vec<ModeEffect> {
        if self.faulted {
            return Vec::new();
        }
        self.registered
            .mode()
            .on_timeout(self.state.as_mut(), context)
            .into_operations()
    }

    #[cfg(test)]
    pub(crate) fn execute(
        &mut self,
        mode: ModeId,
        action: ModeActionId,
    ) -> Result<Option<EditCommand>, ModeError> {
        use crate::core::buffer::Buffer;
        use crate::core::content::Content;

        let content = ContentId(0);
        let mut contents = ContentStore::default();
        contents
            .insert(content, Content::Buffer(Buffer::new()))
            .expect("detached test content is unique");
        let view = View::new(
            content,
            contents
                .create_view_state(content)
                .expect("detached test content creates view state"),
        );
        let context = ModeViewContext::new(ViewId(0), &view, &contents);
        assert_eq!(self.registered.id, mode, "resolved mode must be active");
        let action = self
            .registered
            .action_names
            .get(usize::try_from(action.0).expect("mode action index overflow"))
            .expect("mode action id belongs to registered mode");
        self.registered
            .mode()
            .execute_view(self.state.as_mut(), &context, action)
            .map(|result| result.edit_command)
    }
}

impl ModeViewInstance {
    #[cfg(test)]
    fn status(&self, context: &ModeViewContext<'_>) -> InputStatus {
        if self.faulted {
            return InputStatus::Ready;
        }
        self.registered
            .mode()
            .input_status(self.state.as_ref(), context)
    }

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

    #[cfg(test)]
    fn capture(&mut self, context: &ModeViewContext<'_>, key: KeyEvent) -> InputDecision<Command> {
        if self.faulted {
            return InputDecision::Pass;
        }
        self.registered
            .mode()
            .capture(self.state.as_mut(), context, key)
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

    #[cfg(test)]
    fn status_for_test(&self) -> InputStatus {
        with_detached_view_context(|context| self.status(context))
    }

    #[cfg(test)]
    fn capture_for_test(&mut self, key: KeyEvent) -> InputDecision<Command> {
        with_detached_view_context(|context| self.capture(context, key))
    }

    #[cfg(test)]
    fn selection_shape_for_test(&self) -> SelectionShape {
        let content_state: Box<dyn ModeState> = Box::new(());
        with_detached_view_context(|context| {
            self.view_policy(content_state.as_ref(), context)
                .selection_shape
                .unwrap_or(SelectionShape::Character)
        })
    }

    #[cfg(test)]
    fn resolve_key_for_test(&self, key: KeyEvent) -> Option<Command> {
        with_detached_view_context(|context| self.resolve_key(context, key))
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

    #[cfg(test)]
    pub(crate) fn execute(
        &mut self,
        view: ViewId,
        registry: &ModeRegistry,
        command: &crate::app::command::ModeCommand,
    ) -> Result<Option<EditCommand>, ModeError> {
        let (mode, action) = registry.resolve_command_checked(&command.mode, &command.action)?;
        let Some(instance) = self.instances.get_mut(&(mode, view)) else {
            return Err(ModeError::InactiveMode {
                requested: command.mode.clone(),
                active: None,
            });
        };
        instance.execute(mode, action)
    }
}

#[cfg(test)]
pub(crate) struct ModeSet {
    registry: ModeRegistry,
    mode: ModeName,
}

#[cfg(test)]
fn with_detached_view_context<R>(query: impl FnOnce(&ModeViewContext<'_>) -> R) -> R {
    use crate::core::buffer::Buffer;
    use crate::core::content::Content;

    let content = ContentId(0);
    let mut contents = ContentStore::default();
    contents
        .insert(content, Content::Buffer(Buffer::new()))
        .expect("detached test content is unique");
    let view = View::new(
        content,
        contents
            .create_view_state(content)
            .expect("detached test content creates view state"),
    );
    let context = ModeViewContext::new(ViewId(0), &view, &contents);
    query(&context)
}

#[cfg(test)]
impl ModeSet {
    pub(crate) fn vim() -> Self {
        Self {
            registry: ModeRegistry::builtin(),
            mode: ModeName::new("vim"),
        }
    }

    pub(crate) fn plain_edit() -> Self {
        Self {
            registry: ModeRegistry::plain_edit(),
            mode: ModeName::new("plain-edit"),
        }
    }

    pub(crate) fn create_runtime(&self) -> ModeViewInstance {
        self.registry
            .instantiate(&self.mode)
            .expect("test mode exists")
    }

    pub(crate) fn resolve_key(
        &self,
        instance: &ModeViewInstance,
        key: KeyEvent,
    ) -> Option<Command> {
        with_detached_view_context(|context| instance.resolve_key(context, key))
    }

    pub(crate) fn cursor_style(&self, instance: &ModeViewInstance) -> CursorStyle {
        let content_state: Box<dyn ModeState> = Box::new(());
        with_detached_view_context(|context| {
            instance
                .view_policy(content_state.as_ref(), context)
                .cursor_style
                .unwrap_or(CursorStyle::Default)
        })
    }

    pub(crate) fn execute(
        &self,
        instance: &mut ModeViewInstance,
        mode: ModeName,
        action: ModeActionName,
    ) -> Option<EditCommand> {
        let (mode, action) = self.registry.resolve_command_checked(&mode, &action).ok()?;
        instance.execute(mode, action).ok().flatten()
    }
}

#[cfg(test)]
struct PlainEditMode {
    name: ModeName,
    actions: Vec<ModeActionName>,
    keymap: Keymap<Command>,
}

#[cfg(test)]
impl PlainEditMode {
    fn new() -> Self {
        Self {
            name: ModeName::new("plain-edit"),
            actions: Vec::new(),
            keymap: plain_edit_keymap(),
        }
    }
}

#[cfg(test)]
impl Mode for PlainEditMode {
    fn name(&self) -> &ModeName {
        &self.name
    }

    fn actions(&self) -> &[ModeActionName] {
        &self.actions
    }

    fn new_view_state(&self) -> Box<dyn ModeState> {
        Box::new(())
    }

    fn keymap(&self, _state: &dyn ModeState, _context: &ModeViewContext<'_>) -> &Keymap<Command> {
        &self.keymap
    }

    fn typing(
        &self,
        _state: &dyn ModeState,
        _context: &ModeViewContext<'_>,
        key: KeyEvent,
    ) -> Option<Command> {
        key.is_plain_char()
            .map(|ch| EditCommand::InsertText(ch.to_string()).into())
    }

    fn execute_view(
        &self,
        _state: &mut dyn ModeState,
        _context: &ModeViewContext<'_>,
        action: &ModeActionName,
    ) -> Result<ModeResult, ModeError> {
        Err(ModeError::UnknownAction {
            mode: self.name.clone(),
            action: action.clone(),
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum VimState {
    Normal,
    Insert,
    // Charwise Visual 复用全局半开 selection；不在 mode state 复制 anchor/head。
    Visual,
    // Linewise Visual 仍只保存 anchor/head；行形态由 presentation 与专用编辑命令表达。
    VisualLine,
}

#[derive(Clone)]
struct VimModeState {
    state: VimState,
    pending: Option<VimPending>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum VimPending {
    Count(usize),
    Find {
        direction: CharSearchDirection,
        count: usize,
    },
    Delete {
        operator_count: usize,
        motion_count: Option<usize>,
    },
}

struct VimMode {
    name: ModeName,
    actions: Vec<ModeActionName>,
    normal_keymap: Keymap<Command>,
    insert_keymap: Keymap<Command>,
    visual_keymap: Keymap<Command>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum VimAction {
    EnterInsert,
    EnterNormal,
    ToggleVisual,
    ToggleLineVisual,
    LeaveVisual,
    DeleteSelection,
    ChangeSelection,
    Append,
    OpenBelow,
    OpenAbove,
    InsertAtFirstNonBlank,
    AppendAtLineEnd,
    SubstituteChar,
    ChangeToLineEnd,
    SubstituteLine,
    MoveLeft,
    MoveDown,
    MoveUp,
    MoveRight,
    MoveWordForward,
    MoveWordBackward,
    MoveWordEnd,
    MoveLineStart,
    MoveFirstNonBlank,
    MoveLineEnd,
    MoveLastLine,
    MovePrevParagraph,
    MoveNextParagraph,
    GotoLine,
    FindForward,
    FindBackward,
    DeleteOperator,
    ViewportHalfUp,
    ViewportHalfDown,
    ViewportFullUp,
    ViewportFullDown,
    Count(u8),
}

const VIM_ACTIONS: [VimAction; 45] = [
    VimAction::EnterInsert,
    VimAction::EnterNormal,
    VimAction::ToggleVisual,
    VimAction::ToggleLineVisual,
    VimAction::LeaveVisual,
    VimAction::DeleteSelection,
    VimAction::ChangeSelection,
    VimAction::Append,
    VimAction::OpenBelow,
    VimAction::OpenAbove,
    VimAction::InsertAtFirstNonBlank,
    VimAction::AppendAtLineEnd,
    VimAction::SubstituteChar,
    VimAction::ChangeToLineEnd,
    VimAction::SubstituteLine,
    VimAction::MoveLeft,
    VimAction::MoveDown,
    VimAction::MoveUp,
    VimAction::MoveRight,
    VimAction::MoveWordForward,
    VimAction::MoveWordBackward,
    VimAction::MoveWordEnd,
    VimAction::MoveLineStart,
    VimAction::MoveFirstNonBlank,
    VimAction::MoveLineEnd,
    VimAction::MoveLastLine,
    VimAction::MovePrevParagraph,
    VimAction::MoveNextParagraph,
    VimAction::GotoLine,
    VimAction::FindForward,
    VimAction::FindBackward,
    VimAction::DeleteOperator,
    VimAction::ViewportHalfUp,
    VimAction::ViewportHalfDown,
    VimAction::ViewportFullUp,
    VimAction::ViewportFullDown,
    VimAction::Count(1),
    VimAction::Count(2),
    VimAction::Count(3),
    VimAction::Count(4),
    VimAction::Count(5),
    VimAction::Count(6),
    VimAction::Count(7),
    VimAction::Count(8),
    VimAction::Count(9),
];

impl VimAction {
    fn name(self) -> &'static str {
        match self {
            Self::EnterInsert => "enter-insert",
            Self::EnterNormal => "enter-normal",
            Self::ToggleVisual => "toggle-visual",
            Self::ToggleLineVisual => "toggle-line-visual",
            Self::LeaveVisual => "leave-visual",
            Self::DeleteSelection => "delete-selection",
            Self::ChangeSelection => "change-selection",
            Self::Append => "append",
            Self::OpenBelow => "open-below",
            Self::OpenAbove => "open-above",
            Self::InsertAtFirstNonBlank => "insert-at-first-non-blank",
            Self::AppendAtLineEnd => "append-at-line-end",
            Self::SubstituteChar => "substitute-char",
            Self::ChangeToLineEnd => "change-to-line-end",
            Self::SubstituteLine => "substitute-line",
            Self::MoveLeft => "move-left",
            Self::MoveDown => "move-down",
            Self::MoveUp => "move-up",
            Self::MoveRight => "move-right",
            Self::MoveWordForward => "move-word-forward",
            Self::MoveWordBackward => "move-word-backward",
            Self::MoveWordEnd => "move-word-end",
            Self::MoveLineStart => "move-line-start",
            Self::MoveFirstNonBlank => "move-first-non-blank",
            Self::MoveLineEnd => "move-line-end",
            Self::MoveLastLine => "move-last-line",
            Self::MovePrevParagraph => "move-prev-paragraph",
            Self::MoveNextParagraph => "move-next-paragraph",
            Self::GotoLine => "goto-line",
            Self::FindForward => "find-forward",
            Self::FindBackward => "find-backward",
            Self::DeleteOperator => "delete-operator",
            Self::ViewportHalfUp => "viewport-half-up",
            Self::ViewportHalfDown => "viewport-half-down",
            Self::ViewportFullUp => "viewport-full-up",
            Self::ViewportFullDown => "viewport-full-down",
            Self::Count(1) => "count-1",
            Self::Count(2) => "count-2",
            Self::Count(3) => "count-3",
            Self::Count(4) => "count-4",
            Self::Count(5) => "count-5",
            Self::Count(6) => "count-6",
            Self::Count(7) => "count-7",
            Self::Count(8) => "count-8",
            Self::Count(9) => "count-9",
            Self::Count(_) => unreachable!("Vim count actions are limited to digits 1..=9"),
        }
    }

    fn from_name(name: &ModeActionName) -> Option<Self> {
        VIM_ACTIONS
            .iter()
            .copied()
            .find(|action| action.name() == name.as_str())
    }
}

impl VimMode {
    fn new() -> Self {
        Self {
            name: ModeName::new("vim"),
            actions: VIM_ACTIONS
                .into_iter()
                .map(|action| ModeActionName::new(action.name()))
                .collect(),
            normal_keymap: vim_normal_keymap(),
            insert_keymap: vim_insert_keymap(),
            visual_keymap: vim_visual_keymap(),
        }
    }

    fn state<'a>(&self, state: &'a dyn ModeState) -> &'a VimModeState {
        state
            .as_any()
            .downcast_ref()
            .expect("vim runtime must use VimModeState")
    }

    fn state_mut<'a>(&self, state: &'a mut dyn ModeState) -> &'a mut VimModeState {
        state
            .as_any_mut()
            .downcast_mut()
            .expect("vim runtime must use VimModeState")
    }

    fn take_count(state: &mut VimModeState) -> Option<usize> {
        match state.pending.take() {
            Some(VimPending::Count(count)) => Some(count),
            pending => {
                state.pending = pending;
                None
            }
        }
    }

    fn set_editor_state(state: &mut VimModeState, editor_state: VimState) {
        state.state = editor_state;
        state.pending = None;
    }
}

fn append_count(count: Option<usize>, digit: usize) -> usize {
    count.unwrap_or(0).saturating_mul(10).saturating_add(digit)
}

fn repeat_edit_command(command: EditCommand, count: usize) -> ContentCommand {
    let count = count.max(1);
    if count == 1 {
        return command.into();
    }
    content_sequence((0..count).map(|_| command.clone().into()).collect())
}

fn content_sequence(commands: Vec<ContentCommand>) -> ContentCommand {
    ContentCommand::try_sequence(commands).expect("built-in mode sequence must be valid")
}

impl Mode for VimMode {
    fn name(&self) -> &ModeName {
        &self.name
    }

    fn actions(&self) -> &[ModeActionName] {
        &self.actions
    }

    fn new_view_state(&self) -> Box<dyn ModeState> {
        Box::new(VimModeState {
            state: VimState::Normal,
            pending: None,
        })
    }

    fn keymap(&self, state: &dyn ModeState, _context: &ModeViewContext<'_>) -> &Keymap<Command> {
        match self.state(state).state {
            VimState::Normal => &self.normal_keymap,
            VimState::Insert => &self.insert_keymap,
            VimState::Visual | VimState::VisualLine => &self.visual_keymap,
        }
    }

    fn typing(
        &self,
        state: &dyn ModeState,
        _context: &ModeViewContext<'_>,
        key: KeyEvent,
    ) -> Option<Command> {
        match self.state(state).state {
            VimState::Normal => None,
            VimState::Insert => key
                .is_plain_char()
                .map(|ch| EditCommand::InsertText(ch.to_string()).into()),
            VimState::Visual | VimState::VisualLine => None,
        }
    }

    fn input_status(&self, state: &dyn ModeState, _context: &ModeViewContext<'_>) -> InputStatus {
        if self.state(state).pending.is_some() {
            InputStatus::Awaiting(TimeoutPolicy::Never)
        } else {
            InputStatus::Ready
        }
    }

    fn capture(
        &self,
        state: &mut dyn ModeState,
        _context: &ModeViewContext<'_>,
        key: KeyEvent,
    ) -> InputDecision<Command> {
        let state = self.state_mut(state);
        if state.pending.is_none() {
            return InputDecision::Pass;
        }
        if key == KeyEvent::plain(KeyCode::Escape) {
            state.pending = None;
            return InputDecision::Consumed;
        }
        let pending = state.pending.take().expect("pending state was checked");
        match pending {
            VimPending::Count(count) => match key.is_plain_char() {
                Some(ch @ '0'..='9') => {
                    state.pending = Some(VimPending::Count(append_count(
                        Some(count),
                        ch.to_digit(10).expect("digit") as usize,
                    )));
                    InputDecision::Consumed
                }
                Some('h' | 'j' | 'k' | 'l' | 'w' | 'b' | 'e' | 'f' | 'F' | 'g' | 'd') => {
                    state.pending = Some(VimPending::Count(count));
                    InputDecision::Pass
                }
                Some('^' | '$' | 'G' | '{' | '}')
                    if matches!(state.state, VimState::Visual | VimState::VisualLine) =>
                {
                    state.pending = Some(VimPending::Count(count));
                    InputDecision::Pass
                }
                _ => InputDecision::Consumed,
            },
            VimPending::Find { direction, count } => match key.is_plain_char() {
                Some(target) => InputDecision::Emit(
                    if matches!(state.state, VimState::Visual | VimState::VisualLine) {
                        EditCommand::ExtendToChar {
                            target,
                            direction,
                            occurrence: count,
                        }
                    } else {
                        EditCommand::MoveToChar {
                            target,
                            direction,
                            occurrence: count,
                        }
                    }
                    .into(),
                ),
                None => InputDecision::Consumed,
            },
            VimPending::Delete {
                operator_count,
                motion_count,
            } => match key.is_plain_char() {
                Some('0') if motion_count.is_none() => InputDecision::Emit(
                    EditCommand::Operate(OperatorCommand {
                        operator: TextOperator::Delete,
                        target: TextTarget::Motion {
                            motion: TextMotion::LineStart,
                            count: operator_count,
                        },
                    })
                    .into(),
                ),
                Some(ch @ '0'..='9') => {
                    state.pending = Some(VimPending::Delete {
                        operator_count,
                        motion_count: Some(append_count(
                            motion_count,
                            ch.to_digit(10).expect("digit") as usize,
                        )),
                    });
                    InputDecision::Consumed
                }
                Some('d') => InputDecision::Emit(
                    EditCommand::Operate(OperatorCommand {
                        operator: TextOperator::Delete,
                        target: TextTarget::Lines {
                            count: operator_count.saturating_mul(motion_count.unwrap_or(1)),
                        },
                    })
                    .into(),
                ),
                Some('w') => InputDecision::Emit(
                    EditCommand::Operate(OperatorCommand {
                        operator: TextOperator::Delete,
                        target: TextTarget::Motion {
                            motion: TextMotion::WordForward,
                            count: operator_count.saturating_mul(motion_count.unwrap_or(1)),
                        },
                    })
                    .into(),
                ),
                Some('$') => InputDecision::Emit(
                    EditCommand::Operate(OperatorCommand {
                        operator: TextOperator::Delete,
                        target: TextTarget::Motion {
                            motion: TextMotion::LineEnd,
                            count: operator_count.saturating_mul(motion_count.unwrap_or(1)),
                        },
                    })
                    .into(),
                ),
                _ => InputDecision::Consumed,
            },
        }
    }

    fn on_timeout(&self, state: &mut dyn ModeState, _context: &ModeViewContext<'_>) -> ModeResult {
        self.state_mut(state).pending = None;
        ModeResult::none()
    }

    fn cancel(&self, state: &mut dyn ModeState, _context: &ModeViewContext<'_>) {
        self.state_mut(state).pending = None;
    }

    fn view_policy(
        &self,
        _content_state: &dyn ModeState,
        view_state: &dyn ModeState,
        _context: &ModeViewContext<'_>,
    ) -> ModeViewPolicy {
        let state = self.state(view_state).state;
        ModeViewPolicy {
            cursor_style: Some(match state {
                VimState::Normal | VimState::Visual | VimState::VisualLine => CursorStyle::Block,
                VimState::Insert => CursorStyle::Bar,
            }),
            cursor_domain: Some(match state {
                VimState::Insert => CursorDomain::InsertionPoint,
                VimState::Normal | VimState::Visual | VimState::VisualLine => {
                    CursorDomain::Character
                }
            }),
            selection_shape: Some(match state {
                VimState::Visual => SelectionShape::CharacterInclusive,
                VimState::VisualLine => SelectionShape::Line,
                VimState::Normal | VimState::Insert => SelectionShape::Character,
            }),
            selection_face: None,
        }
    }

    fn execute_view(
        &self,
        state: &mut dyn ModeState,
        context: &ModeViewContext<'_>,
        action: &ModeActionName,
    ) -> Result<ModeResult, ModeError> {
        let Some(vim_action) = VimAction::from_name(action) else {
            return Err(ModeError::UnknownAction {
                mode: self.name.clone(),
                action: action.clone(),
            });
        };
        let command = match vim_action {
            VimAction::EnterInsert => {
                Self::set_editor_state(self.state_mut(state), VimState::Insert);
                Some(ContentCommand::Transaction(TransactionCommand::Begin))
            }
            VimAction::EnterNormal => {
                Self::set_editor_state(self.state_mut(state), VimState::Normal);
                Some(content_sequence(vec![
                    ContentCommand::Transaction(TransactionCommand::Commit),
                    EditCommand::CollapseSelections.into(),
                ]))
            }
            VimAction::ToggleVisual => {
                let state = self.state_mut(state);
                if state.state == VimState::Visual {
                    Self::set_editor_state(state, VimState::Normal);
                    Some(EditCommand::CollapseSelections.into())
                } else {
                    Self::set_editor_state(state, VimState::Visual);
                    None
                }
            }
            VimAction::ToggleLineVisual => {
                let state = self.state_mut(state);
                if state.state == VimState::VisualLine {
                    Self::set_editor_state(state, VimState::Normal);
                    Some(EditCommand::CollapseSelections.into())
                } else {
                    Self::set_editor_state(state, VimState::VisualLine);
                    None
                }
            }
            VimAction::LeaveVisual => {
                Self::set_editor_state(self.state_mut(state), VimState::Normal);
                Some(EditCommand::CollapseSelections.into())
            }
            VimAction::DeleteSelection => {
                let linewise = self.state(state).state == VimState::VisualLine;
                let command = if linewise {
                    EditCommand::DeleteSelectedLines
                } else {
                    EditCommand::DeleteInclusiveSelection
                };
                let result =
                    ModeResult::from_command(context, Some(Command::Content(command.into())));
                Self::set_editor_state(self.state_mut(state), VimState::Normal);
                return Ok(result);
            }
            VimAction::ChangeSelection => {
                let linewise = self.state(state).state == VimState::VisualLine;
                let command = content_sequence(vec![
                    ContentCommand::Transaction(TransactionCommand::Begin),
                    if linewise {
                        EditCommand::DeleteSelectedLines
                    } else {
                        EditCommand::DeleteInclusiveSelection
                    }
                    .into(),
                ]);
                let result = ModeResult::from_command(context, Some(Command::Content(command)));
                Self::set_editor_state(self.state_mut(state), VimState::Insert);
                return Ok(result);
            }
            VimAction::Append => {
                Self::set_editor_state(self.state_mut(state), VimState::Insert);
                Some(content_sequence(vec![
                    EditCommand::MoveRightBy(1).into(),
                    ContentCommand::Transaction(TransactionCommand::Begin),
                ]))
            }
            VimAction::OpenBelow => {
                Self::set_editor_state(self.state_mut(state), VimState::Insert);
                Some(content_sequence(vec![
                    ContentCommand::Transaction(TransactionCommand::Begin),
                    EditCommand::InsertNewLineBelow.into(),
                ]))
            }
            VimAction::OpenAbove => {
                Self::set_editor_state(self.state_mut(state), VimState::Insert);
                Some(content_sequence(vec![
                    ContentCommand::Transaction(TransactionCommand::Begin),
                    EditCommand::InsertNewLineAbove.into(),
                ]))
            }
            VimAction::InsertAtFirstNonBlank => {
                Self::set_editor_state(self.state_mut(state), VimState::Insert);
                Some(content_sequence(vec![
                    EditCommand::MoveToFirstNonBlank.into(),
                    ContentCommand::Transaction(TransactionCommand::Begin),
                ]))
            }
            VimAction::AppendAtLineEnd => {
                Self::set_editor_state(self.state_mut(state), VimState::Insert);
                Some(content_sequence(vec![
                    EditCommand::MoveAfterLineEnd.into(),
                    ContentCommand::Transaction(TransactionCommand::Begin),
                ]))
            }
            VimAction::SubstituteChar => {
                Self::set_editor_state(self.state_mut(state), VimState::Insert);
                Some(content_sequence(vec![
                    ContentCommand::Transaction(TransactionCommand::Begin),
                    EditCommand::Delete(1).into(),
                ]))
            }
            VimAction::ChangeToLineEnd => {
                Self::set_editor_state(self.state_mut(state), VimState::Insert);
                Some(content_sequence(vec![
                    ContentCommand::Transaction(TransactionCommand::Begin),
                    EditCommand::DeleteToLineEnd.into(),
                ]))
            }
            VimAction::SubstituteLine => {
                Self::set_editor_state(self.state_mut(state), VimState::Insert);
                Some(content_sequence(vec![
                    ContentCommand::Transaction(TransactionCommand::Begin),
                    EditCommand::DeleteLineContent.into(),
                ]))
            }
            VimAction::MoveLeft => {
                let state = self.state_mut(state);
                let count = Self::take_count(state).unwrap_or(1);
                Some(
                    if matches!(state.state, VimState::Visual | VimState::VisualLine) {
                        EditCommand::ExtendWithinLineLeftBy(count)
                    } else {
                        EditCommand::MoveWithinLineLeftBy(count)
                    }
                    .into(),
                )
            }
            VimAction::MoveDown => {
                let state = self.state_mut(state);
                let count = Self::take_count(state).unwrap_or(1);
                Some(
                    if matches!(state.state, VimState::Visual | VimState::VisualLine) {
                        EditCommand::ExtendDownBy(count)
                    } else {
                        EditCommand::MoveDownBy(count)
                    }
                    .into(),
                )
            }
            VimAction::MoveUp => {
                let state = self.state_mut(state);
                let count = Self::take_count(state).unwrap_or(1);
                Some(
                    if matches!(state.state, VimState::Visual | VimState::VisualLine) {
                        EditCommand::ExtendUpBy(count)
                    } else {
                        EditCommand::MoveUpBy(count)
                    }
                    .into(),
                )
            }
            VimAction::MoveRight => {
                let state = self.state_mut(state);
                let count = Self::take_count(state).unwrap_or(1);
                Some(
                    if matches!(state.state, VimState::Visual | VimState::VisualLine) {
                        EditCommand::ExtendWithinLineRightBy(count)
                    } else {
                        EditCommand::MoveWithinLineRightBy(count)
                    }
                    .into(),
                )
            }
            VimAction::MoveWordForward => {
                let state = self.state_mut(state);
                let count = Self::take_count(state).unwrap_or(1);
                Some(repeat_edit_command(
                    if matches!(state.state, VimState::Visual | VimState::VisualLine) {
                        EditCommand::ExtendWordForward
                    } else {
                        EditCommand::MoveWordForward
                    },
                    count,
                ))
            }
            VimAction::MoveWordBackward => {
                let state = self.state_mut(state);
                let count = Self::take_count(state).unwrap_or(1);
                Some(repeat_edit_command(
                    if matches!(state.state, VimState::Visual | VimState::VisualLine) {
                        EditCommand::ExtendWordBackward
                    } else {
                        EditCommand::MoveWordBackward
                    },
                    count,
                ))
            }
            VimAction::MoveWordEnd => {
                let state = self.state_mut(state);
                let count = Self::take_count(state).unwrap_or(1);
                Some(repeat_edit_command(
                    if matches!(state.state, VimState::Visual | VimState::VisualLine) {
                        EditCommand::ExtendWordEnd
                    } else {
                        EditCommand::MoveWordEnd
                    },
                    count,
                ))
            }
            VimAction::MoveLineStart => {
                let state = self.state_mut(state);
                Self::take_count(state);
                Some(
                    if matches!(state.state, VimState::Visual | VimState::VisualLine) {
                        EditCommand::ExtendToLineStart
                    } else {
                        EditCommand::MoveToLineStart
                    }
                    .into(),
                )
            }
            VimAction::MoveFirstNonBlank => {
                let state = self.state_mut(state);
                Self::take_count(state);
                Some(
                    if matches!(state.state, VimState::Visual | VimState::VisualLine) {
                        EditCommand::ExtendToFirstNonBlank
                    } else {
                        EditCommand::MoveToFirstNonBlank
                    }
                    .into(),
                )
            }
            VimAction::MoveLineEnd => {
                let state = self.state_mut(state);
                Self::take_count(state);
                Some(
                    if matches!(state.state, VimState::Visual | VimState::VisualLine) {
                        EditCommand::ExtendToLineEnd
                    } else {
                        EditCommand::MoveToLineEnd
                    }
                    .into(),
                )
            }
            VimAction::MoveLastLine => {
                let state = self.state_mut(state);
                Self::take_count(state);
                Some(
                    if matches!(state.state, VimState::Visual | VimState::VisualLine) {
                        EditCommand::ExtendToLastLine
                    } else {
                        EditCommand::MoveToLastLine
                    }
                    .into(),
                )
            }
            VimAction::MovePrevParagraph => {
                let state = self.state_mut(state);
                Self::take_count(state);
                Some(
                    if matches!(state.state, VimState::Visual | VimState::VisualLine) {
                        EditCommand::ExtendToPrevParagraph
                    } else {
                        EditCommand::MoveToPrevParagraph
                    }
                    .into(),
                )
            }
            VimAction::MoveNextParagraph => {
                let state = self.state_mut(state);
                Self::take_count(state);
                Some(
                    if matches!(state.state, VimState::Visual | VimState::VisualLine) {
                        EditCommand::ExtendToNextParagraph
                    } else {
                        EditCommand::MoveToNextParagraph
                    }
                    .into(),
                )
            }
            VimAction::GotoLine => {
                let state = self.state_mut(state);
                let line_index = Self::take_count(state).unwrap_or(1).saturating_sub(1);
                Some(
                    if matches!(state.state, VimState::Visual | VimState::VisualLine) {
                        EditCommand::ExtendToLine { line_index }
                    } else {
                        EditCommand::MoveToLine { line_index }
                    }
                    .into(),
                )
            }
            VimAction::FindForward | VimAction::FindBackward => {
                let state = self.state_mut(state);
                let count = Self::take_count(state).unwrap_or(1);
                state.pending = Some(VimPending::Find {
                    direction: if vim_action == VimAction::FindForward {
                        CharSearchDirection::Forward
                    } else {
                        CharSearchDirection::Backward
                    },
                    count,
                });
                None
            }
            VimAction::DeleteOperator => {
                let state = self.state_mut(state);
                let operator_count = Self::take_count(state).unwrap_or(1);
                state.pending = Some(VimPending::Delete {
                    operator_count,
                    motion_count: None,
                });
                None
            }
            VimAction::ViewportHalfUp => {
                return Ok(ModeResult::from_command(
                    context,
                    Some(Command::Viewport(viewport_command(
                        self.state_mut(state),
                        ViewportMoveDirection::Up,
                        ViewportMoveAmount::HalfPage,
                    ))),
                ));
            }
            VimAction::ViewportHalfDown => {
                return Ok(ModeResult::from_command(
                    context,
                    Some(Command::Viewport(viewport_command(
                        self.state_mut(state),
                        ViewportMoveDirection::Down,
                        ViewportMoveAmount::HalfPage,
                    ))),
                ));
            }
            VimAction::ViewportFullUp => {
                return Ok(ModeResult::from_command(
                    context,
                    Some(Command::Viewport(viewport_command(
                        self.state_mut(state),
                        ViewportMoveDirection::Up,
                        ViewportMoveAmount::FullPage,
                    ))),
                ));
            }
            VimAction::ViewportFullDown => {
                return Ok(ModeResult::from_command(
                    context,
                    Some(Command::Viewport(viewport_command(
                        self.state_mut(state),
                        ViewportMoveDirection::Down,
                        ViewportMoveAmount::FullPage,
                    ))),
                ));
            }
            VimAction::Count(digit) => {
                self.state_mut(state).pending = Some(VimPending::Count(usize::from(digit)));
                None
            }
        };
        Ok(ModeResult::from_command(
            context,
            command.map(Command::Content),
        ))
    }
}

fn viewport_command(
    state: &mut VimModeState,
    direction: ViewportMoveDirection,
    amount: ViewportMoveAmount,
) -> ViewportCommand {
    VimMode::take_count(state);
    ViewportCommand::new(
        direction,
        amount,
        if matches!(state.state, VimState::Visual | VimState::VisualLine) {
            ViewportCursorBehavior::Extend
        } else {
            ViewportCursorBehavior::Move
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    use crate::app::command::{AppCommand, ContentCommand};
    use crate::core::command::EditCommand;
    use crate::core::input::{InputDecision, InputStatus, TimeoutPolicy};
    use crate::protocol::key_event::ArrowKey;

    struct DynamicMode {
        name: ModeName,
        actions: Vec<ModeActionName>,
        keymap: Keymap<Command>,
    }

    impl DynamicMode {
        fn new<N, I, A>(name: N, actions: I) -> Self
        where
            N: Into<String>,
            I: IntoIterator<Item = A>,
            A: Into<String>,
        {
            Self {
                name: ModeName::new(name),
                actions: actions.into_iter().map(ModeActionName::new).collect(),
                keymap: Keymap::new(),
            }
        }
    }

    impl Mode for DynamicMode {
        fn name(&self) -> &ModeName {
            &self.name
        }

        fn actions(&self) -> &[ModeActionName] {
            &self.actions
        }

        fn new_view_state(&self) -> Box<dyn ModeState> {
            Box::new(())
        }

        fn keymap(
            &self,
            _state: &dyn ModeState,
            _context: &ModeViewContext<'_>,
        ) -> &Keymap<Command> {
            &self.keymap
        }

        fn typing(
            &self,
            _state: &dyn ModeState,
            _context: &ModeViewContext<'_>,
            _key: KeyEvent,
        ) -> Option<Command> {
            None
        }

        fn execute_view(
            &self,
            _state: &mut dyn ModeState,
            context: &ModeViewContext<'_>,
            action: &ModeActionName,
        ) -> Result<ModeResult, ModeError> {
            if action.as_str() == "focus-next" {
                return Ok(ModeResult::from_command(
                    context,
                    Some(Command::App(AppCommand::FocusNext)),
                ));
            }
            Err(ModeError::UnknownAction {
                mode: self.name.clone(),
                action: action.clone(),
            })
        }
    }

    struct DynamicContentMode {
        name: ModeName,
        actions: Vec<ModeActionName>,
    }

    struct TimeoutViewMode {
        name: ModeName,
        keymap: Keymap<Command>,
    }

    impl Mode for TimeoutViewMode {
        fn name(&self) -> &ModeName {
            &self.name
        }

        fn actions(&self) -> &[ModeActionName] {
            &[]
        }

        fn new_view_state(&self) -> Box<dyn ModeState> {
            Box::new(())
        }

        fn keymap(
            &self,
            _state: &dyn ModeState,
            _context: &ModeViewContext<'_>,
        ) -> &Keymap<Command> {
            &self.keymap
        }

        fn typing(
            &self,
            _state: &dyn ModeState,
            _context: &ModeViewContext<'_>,
            _key: KeyEvent,
        ) -> Option<Command> {
            None
        }

        fn input_status(
            &self,
            _state: &dyn ModeState,
            _context: &ModeViewContext<'_>,
        ) -> InputStatus {
            InputStatus::Awaiting(TimeoutPolicy::Never)
        }

        fn on_timeout(
            &self,
            _state: &mut dyn ModeState,
            _context: &ModeViewContext<'_>,
        ) -> ModeResult {
            ModeResult::operations(vec![
                ModeEffect::Transaction(TransactionIntent::Commit),
                ModeEffect::View(ViewAction::SetSelections(Selections::single(
                    crate::protocol::selection::Selection::collapsed(
                        crate::protocol::selection::TextOffset { char_index: 1 },
                    ),
                ))),
            ])
        }

        fn execute_view(
            &self,
            _state: &mut dyn ModeState,
            _context: &ModeViewContext<'_>,
            action: &ModeActionName,
        ) -> Result<ModeResult, ModeError> {
            Err(ModeError::UnknownAction {
                mode: self.name.clone(),
                action: action.clone(),
            })
        }
    }

    impl DynamicContentMode {
        fn new() -> Self {
            Self {
                name: ModeName::new("content-probe"),
                actions: vec![ModeActionName::new("advance")],
            }
        }
    }

    impl Mode for DynamicContentMode {
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
            context: &ModeContentContext<'_>,
            action: &ModeActionName,
        ) -> Result<ModeResult, ModeError> {
            if action != &self.actions[0] {
                return Err(ModeError::UnknownAction {
                    mode: self.name.clone(),
                    action: action.clone(),
                });
            }
            assert_eq!(context.content_id(), ContentId(7));
            let _ = context.query_content(ContentQuery::DocumentStatus);
            let count = state
                .as_any_mut()
                .downcast_mut::<u8>()
                .expect("content mode owns its state");
            *count += 1;
            Ok(ModeResult::none())
        }
    }

    #[test]
    fn content_mode_is_instantiated_once_for_its_content() {
        use crate::core::buffer::Buffer;
        use crate::core::content::Content;

        let content = ContentId(7);
        let name = ModeName::new("content-probe");
        let command = crate::app::command::ModeCommand {
            mode: name.clone(),
            action: ModeActionName::new("advance"),
            arguments: Default::default(),
        };
        let mut contents = ContentStore::default();
        contents
            .insert(content, Content::Buffer(Buffer::new()))
            .unwrap();
        let mut registry = ModeRegistry::new();
        registry.register(DynamicContentMode::new());
        let mut instances = ModeContentStore::default();

        assert!(instances.bind(&registry, content, &name, &contents));
        assert_eq!(
            instances.execute(&registry, &contents, content, &command),
            Ok(ModeResult::none())
        );
        assert_eq!(
            instances.execute(&registry, &contents, content, &command),
            Ok(ModeResult::none())
        );
        assert!(registry.instantiate(&name).is_some());
    }

    #[test]
    fn view_mode_timeout_uses_the_same_typed_view_result_path() {
        use crate::core::buffer::Buffer;
        use crate::core::content::Content;

        let content = ContentId(7);
        let mut contents = ContentStore::default();
        contents
            .insert(content, Content::Buffer(Buffer::new()))
            .unwrap();
        let view = View::new(content, contents.create_view_state(content).unwrap());
        let mut registry = ModeRegistry::new();
        registry.register(TimeoutViewMode {
            name: ModeName::new("timeout-view"),
            keymap: Keymap::new(),
        });
        let mut instance = registry
            .instantiate(&ModeName::new("timeout-view"))
            .unwrap();
        let context = ModeViewContext::new(ViewId(9), &view, &contents);

        assert!(matches!(
            instance.timeout(&context).as_slice(),
            [
                ModeEffect::Transaction(TransactionIntent::Commit),
                ModeEffect::View(ViewAction::SetSelections(selections)),
            ]
                if selections.primary().head().char_index == 1
        ));
    }

    #[test]
    fn ordered_command_conversion_defers_edits_until_execution() {
        use crate::core::buffer::Buffer;
        use crate::core::content::Content;

        let content = ContentId(7);
        let mut contents = ContentStore::default();
        contents
            .insert(content, Content::Buffer(Buffer::new()))
            .unwrap();
        let view = View::new(content, contents.create_view_state(content).unwrap());
        let context = ModeViewContext::new(ViewId(9), &view, &contents);
        let command = ContentCommand::try_sequence(vec![
            ContentCommand::Undo,
            EditCommand::InsertText("x".to_string()).into(),
        ])
        .unwrap();

        let operations =
            ModeResult::from_command(&context, Some(Command::Content(command))).into_operations();

        assert!(matches!(
            operations.as_slice(),
            [
                ModeEffect::Transaction(TransactionIntent::Undo),
                ModeEffect::DeferredEdit(EditCommand::InsertText(text)),
            ] if text == "x"
        ));
    }

    #[test]
    fn mode_names_own_runtime_strings() {
        let source = String::from("script-mode");
        let name = ModeName::new(source.clone());
        drop(source);

        assert_eq!(name.as_str(), "script-mode");
        assert_eq!(
            ModeActionName::new("script-action").as_str(),
            "script-action"
        );
    }

    #[test]
    fn vim_action_catalog_is_unique_and_round_trips_through_dynamic_names() {
        let mode = VimMode::new();
        let names: Vec<_> = VIM_ACTIONS
            .iter()
            .copied()
            .map(|action| ModeActionName::new(action.name()))
            .collect();
        let unique: HashSet<_> = names.iter().collect();

        assert_eq!(mode.actions, names);
        assert_eq!(unique.len(), VIM_ACTIONS.len());
        for action in VIM_ACTIONS {
            assert_eq!(
                VimAction::from_name(&ModeActionName::new(action.name())),
                Some(action)
            );
        }
    }

    #[test]
    fn registry_maps_owned_names_to_stable_runtime_ids() {
        let mode_name = String::from("script-mode");
        let action_name = String::from("script-action");
        let mut registry = ModeRegistry::new();
        let id = registry.register(DynamicMode::new(mode_name.clone(), [action_name.clone()]));

        assert_eq!(registry.resolve_mode(&ModeName::new(mode_name)), Some(id));
        let action = registry
            .resolve_action(id, &ModeActionName::new(action_name.clone()))
            .expect("registered action");
        assert_eq!(
            registry.resolve_action(id, &ModeActionName::new(action_name)),
            Some(action)
        );
        assert_eq!(
            registry.resolve_action(id, &ModeActionName::new("missing")),
            None
        );
    }

    #[test]
    fn registry_reports_unknown_mode_and_action() {
        let mut registry = ModeRegistry::new();
        registry.register(DynamicMode::new("script-mode", ["known"]));

        assert_eq!(
            registry
                .resolve_command_checked(&ModeName::new("missing"), &ModeActionName::new("known")),
            Err(ModeError::UnknownMode {
                mode: ModeName::new("missing")
            })
        );
        assert_eq!(
            registry.resolve_command_checked(
                &ModeName::new("script-mode"),
                &ModeActionName::new("missing")
            ),
            Err(ModeError::UnknownAction {
                mode: ModeName::new("script-mode"),
                action: ModeActionName::new("missing")
            })
        );
    }

    #[test]
    fn registered_but_unimplemented_action_is_an_error() {
        let mut registry = ModeRegistry::new();
        registry.register(DynamicMode::new("script-mode", ["declared"]));
        let mode_name = ModeName::new("script-mode");
        let action_name = ModeActionName::new("declared");
        let mut instance = registry.instantiate(&mode_name).unwrap();
        let (mode, action) = registry
            .resolve_command_checked(&mode_name, &action_name)
            .unwrap();

        assert_eq!(
            instance.execute(mode, action),
            Err(ModeError::UnknownAction {
                mode: mode_name,
                action: action_name,
            })
        );
    }

    #[test]
    fn mode_action_can_return_an_app_command() {
        let mut registry = ModeRegistry::new();
        registry.register(DynamicMode::new("script-mode", ["focus-next"]));
        let mode_name = ModeName::new("script-mode");
        let action_name = ModeActionName::new("focus-next");
        let mut instance = registry.instantiate(&mode_name).unwrap();
        let (mode, action) = registry
            .resolve_command_checked(&mode_name, &action_name)
            .unwrap();

        assert_eq!(instance.execute(mode, action), Ok(None));
    }

    #[test]
    #[should_panic(expected = "mode name must be unique")]
    fn registry_rejects_duplicate_mode_names() {
        let mut registry = ModeRegistry::new();
        registry.register(DynamicMode::new("script-mode", ["first"]));
        registry.register(DynamicMode::new("script-mode", ["second"]));
    }

    #[test]
    #[should_panic(expected = "mode action name must be unique")]
    fn registry_rejects_duplicate_action_names() {
        let mut registry = ModeRegistry::new();
        registry.register(DynamicMode::new("script-mode", ["same", "same"]));
    }

    #[test]
    fn vim_mode_runtime_is_independent() {
        let modes = ModeSet::vim();
        let mut first = modes.create_runtime();
        let second = modes.create_runtime();
        assert_eq!(
            modes.execute(
                &mut first,
                ModeName::new("vim"),
                ModeActionName::new("enter-insert"),
            ),
            None,
        );
        assert_eq!(
            modes.resolve_key(&first, KeyEvent::char('a')),
            Some(Command::Content(ContentCommand::Edit(
                EditCommand::InsertText("a".to_string())
            )))
        );
        assert_eq!(
            modes.resolve_key(&second, KeyEvent::char('a')),
            Some(vim_mode_command(VimAction::Append))
        );
    }

    #[test]
    fn vim_cursor_style_tracks_runtime_mode() {
        let modes = ModeSet::vim();
        let mut runtime = modes.create_runtime();

        assert_eq!(modes.cursor_style(&runtime), CursorStyle::Block);
        modes.execute(
            &mut runtime,
            ModeName::new("vim"),
            ModeActionName::new("enter-insert"),
        );
        assert_eq!(modes.cursor_style(&runtime), CursorStyle::Bar);
    }

    #[test]
    fn vim_visual_uses_extend_motions_and_block_cursor() {
        let modes = ModeSet::vim();
        let mut runtime = modes.create_runtime();

        assert_eq!(
            modes.resolve_key(&runtime, KeyEvent::char('v')),
            Some(vim_mode_command(VimAction::ToggleVisual)),
        );
        assert_eq!(
            modes.execute(
                &mut runtime,
                ModeName::new("vim"),
                ModeActionName::new("toggle-visual"),
            ),
            None,
        );
        assert_eq!(modes.cursor_style(&runtime), CursorStyle::Block);
        assert_eq!(
            runtime.selection_shape_for_test(),
            SelectionShape::CharacterInclusive
        );
        assert_eq!(
            modes.resolve_key(&runtime, KeyEvent::char('l')),
            Some(vim_mode_command(VimAction::MoveRight)),
        );
        assert_eq!(
            modes.execute(
                &mut runtime,
                ModeName::new("vim"),
                ModeActionName::new("move-right"),
            ),
            Some(EditCommand::ExtendWithinLineRightBy(1)),
        );
        assert_eq!(
            modes.execute(
                &mut runtime,
                ModeName::new("vim"),
                ModeActionName::new("move-word-forward"),
            ),
            Some(EditCommand::ExtendWordForward),
        );
    }

    #[test]
    fn vim_visual_delete_returns_to_normal() {
        let modes = ModeSet::vim();
        let mut runtime = modes.create_runtime();
        modes.execute(
            &mut runtime,
            ModeName::new("vim"),
            ModeActionName::new("toggle-visual"),
        );

        assert_eq!(
            modes.resolve_key(&runtime, KeyEvent::char('d')),
            Some(vim_mode_command(VimAction::DeleteSelection)),
        );
        assert_eq!(
            modes.execute(
                &mut runtime,
                ModeName::new("vim"),
                ModeActionName::new("delete-selection"),
            ),
            Some(EditCommand::DeleteInclusiveSelection),
        );
        assert_eq!(
            modes.resolve_key(&runtime, KeyEvent::char('v')),
            Some(vim_mode_command(VimAction::ToggleVisual)),
        );
    }

    #[test]
    fn vim_line_visual_uses_line_shape_and_deletes_touched_lines() {
        let modes = ModeSet::vim();
        let mut runtime = modes.create_runtime();

        assert_eq!(
            modes.resolve_key(&runtime, KeyEvent::char('V')),
            Some(vim_mode_command(VimAction::ToggleLineVisual)),
        );
        assert_eq!(
            modes.execute(
                &mut runtime,
                ModeName::new("vim"),
                ModeActionName::new("toggle-line-visual"),
            ),
            None,
        );
        assert_eq!(runtime.selection_shape_for_test(), SelectionShape::Line);
        assert_eq!(
            modes.execute(
                &mut runtime,
                ModeName::new("vim"),
                ModeActionName::new("move-down"),
            ),
            Some(EditCommand::ExtendDownBy(1)),
        );
        assert_eq!(
            modes.execute(
                &mut runtime,
                ModeName::new("vim"),
                ModeActionName::new("delete-selection"),
            ),
            Some(EditCommand::DeleteSelectedLines),
        );
        assert_eq!(
            runtime.selection_shape_for_test(),
            SelectionShape::Character
        );
    }

    #[test]
    fn vim_viewport_keys_emit_frontend_sized_commands() {
        let registry = ModeRegistry::builtin();
        let mode_name = ModeName::new("vim");
        let mut runtime = registry.instantiate(&mode_name).unwrap();

        for (key, action, _direction, _amount) in [
            (
                'u',
                VimAction::ViewportHalfUp,
                ViewportMoveDirection::Up,
                ViewportMoveAmount::HalfPage,
            ),
            (
                'd',
                VimAction::ViewportHalfDown,
                ViewportMoveDirection::Down,
                ViewportMoveAmount::HalfPage,
            ),
            (
                'b',
                VimAction::ViewportFullUp,
                ViewportMoveDirection::Up,
                ViewportMoveAmount::FullPage,
            ),
            (
                'f',
                VimAction::ViewportFullDown,
                ViewportMoveDirection::Down,
                ViewportMoveAmount::FullPage,
            ),
        ] {
            assert_eq!(
                runtime.resolve_key_for_test(KeyEvent::ctrl(key)),
                Some(vim_mode_command(action)),
            );
            let (mode, action) = registry
                .resolve_command_checked(&mode_name, &ModeActionName::new(action.name()))
                .unwrap();
            assert_eq!(runtime.execute(mode, action), Ok(None),);
        }
    }

    #[test]
    fn vim_visual_escape_collapses_and_returns_to_normal() {
        let modes = ModeSet::vim();
        let mut runtime = modes.create_runtime();
        modes.execute(
            &mut runtime,
            ModeName::new("vim"),
            ModeActionName::new("toggle-visual"),
        );

        assert_eq!(
            modes.resolve_key(&runtime, KeyEvent::plain(KeyCode::Escape)),
            Some(vim_mode_command(VimAction::LeaveVisual)),
        );
        assert_eq!(
            modes.execute(
                &mut runtime,
                ModeName::new("vim"),
                ModeActionName::new("leave-visual"),
            ),
            Some(EditCommand::CollapseSelections),
        );
        assert_eq!(
            modes.resolve_key(&runtime, KeyEvent::char('i')),
            Some(vim_mode_command(VimAction::EnterInsert)),
        );
    }

    #[test]
    fn vim_insert_resolves_emacs_motion_and_delete_keys() {
        let modes = ModeSet::vim();
        let mut runtime = modes.create_runtime();
        assert_eq!(
            modes.execute(
                &mut runtime,
                ModeName::new("vim"),
                ModeActionName::new("enter-insert"),
            ),
            None,
        );

        assert_eq!(
            modes.resolve_key(&runtime, KeyEvent::ctrl('b')),
            Some(EditCommand::MoveLeftBy(1).into()),
        );
        assert_eq!(
            modes.resolve_key(&runtime, KeyEvent::ctrl('f')),
            Some(EditCommand::MoveRightBy(1).into()),
        );
        assert_eq!(
            modes.resolve_key(&runtime, KeyEvent::ctrl('h')),
            Some(EditCommand::Delete(-1).into()),
        );
    }

    #[test]
    fn vim_insert_shift_arrows_extend_selections() {
        let modes = ModeSet::vim();
        let mut runtime = modes.create_runtime();
        modes.execute(
            &mut runtime,
            ModeName::new("vim"),
            ModeActionName::new("enter-insert"),
        );
        for (arrow, command) in [
            (ArrowKey::Left, EditCommand::ExtendLeftBy(1)),
            (ArrowKey::Right, EditCommand::ExtendRightBy(1)),
            (ArrowKey::Up, EditCommand::ExtendUpBy(1)),
            (ArrowKey::Down, EditCommand::ExtendDownBy(1)),
        ] {
            assert_eq!(
                modes.resolve_key(&runtime, KeyEvent::shift_arrow(arrow)),
                Some(Command::Content(ContentCommand::Edit(command)))
            );
        }
    }

    #[test]
    fn plain_edit_escape_collapses_selections() {
        let modes = ModeSet::plain_edit();
        let runtime = modes.create_runtime();
        assert_eq!(
            modes.resolve_key(&runtime, KeyEvent::plain(KeyCode::Escape)),
            Some(Command::Content(ContentCommand::Edit(
                EditCommand::CollapseSelections
            )))
        );
    }

    #[test]
    fn vim_insert_ctrl_w_resolves_to_delete_word_backward() {
        let modes = ModeSet::vim();
        let mut runtime = modes.create_runtime();
        assert_eq!(
            modes.execute(
                &mut runtime,
                ModeName::new("vim"),
                ModeActionName::new("enter-insert"),
            ),
            None,
        );

        assert_eq!(
            modes.resolve_key(&runtime, KeyEvent::ctrl('w')),
            Some(EditCommand::DeleteWordBackward.into()),
        );
    }

    #[test]
    fn vim_insert_ctrl_u_resolves_to_delete_to_line_start() {
        let modes = ModeSet::vim();
        let mut runtime = modes.create_runtime();
        modes.execute(
            &mut runtime,
            ModeName::new("vim"),
            ModeActionName::new("enter-insert"),
        );
        assert_eq!(
            modes.resolve_key(&runtime, KeyEvent::ctrl('u')),
            Some(EditCommand::DeleteToLineStart.into()),
        );
    }

    #[test]
    fn vim_insert_ctrl_k_resolves_to_delete_to_line_end() {
        let modes = ModeSet::vim();
        let mut runtime = modes.create_runtime();
        modes.execute(
            &mut runtime,
            ModeName::new("vim"),
            ModeActionName::new("enter-insert"),
        );
        assert_eq!(
            modes.resolve_key(&runtime, KeyEvent::ctrl('k')),
            Some(EditCommand::DeleteToLineEnd.into()),
        );
    }

    #[test]
    fn vim_insert_ctrl_j_resolves_to_insert_newline() {
        let modes = ModeSet::vim();
        let mut runtime = modes.create_runtime();
        modes.execute(
            &mut runtime,
            ModeName::new("vim"),
            ModeActionName::new("enter-insert"),
        );
        assert_eq!(
            modes.resolve_key(&runtime, KeyEvent::ctrl('j')),
            Some(EditCommand::InsertText("\n".to_string()).into()),
        );
    }

    #[test]
    fn vim_insert_ctrl_m_resolves_to_insert_newline() {
        let modes = ModeSet::vim();
        let mut runtime = modes.create_runtime();
        modes.execute(
            &mut runtime,
            ModeName::new("vim"),
            ModeActionName::new("enter-insert"),
        );
        assert_eq!(
            modes.resolve_key(&runtime, KeyEvent::ctrl('m')),
            Some(EditCommand::InsertText("\n".to_string()).into()),
        );
    }

    #[test]
    fn vim_append_enters_insert_and_returns_right_move() {
        let modes = ModeSet::vim();
        let mut runtime = modes.create_runtime();

        assert_eq!(
            modes.execute(
                &mut runtime,
                ModeName::new("vim"),
                ModeActionName::new("append"),
            ),
            Some(EditCommand::MoveRightBy(1)),
        );
        assert_eq!(
            modes.resolve_key(&runtime, KeyEvent::char('x')),
            Some(EditCommand::InsertText("x".to_string()).into()),
        );
    }

    #[test]
    fn mode_action_returns_a_content_command() {
        let registry = ModeRegistry::builtin();
        let mode_name = ModeName::new("vim");
        let action_name = ModeActionName::new("append");
        let mut instance = registry.instantiate(&mode_name).unwrap();
        let (mode, action) = registry
            .resolve_command_checked(&mode_name, &action_name)
            .unwrap();

        assert_eq!(
            instance.execute(mode, action),
            Ok(Some(EditCommand::MoveRightBy(1)))
        );
    }

    #[test]
    fn vim_normal_c_remains_unbound() {
        let modes = ModeSet::vim();
        let runtime = modes.create_runtime();

        assert_eq!(modes.resolve_key(&runtime, KeyEvent::char('c')), None);
    }

    #[test]
    fn vim_normal_w_resolves_to_move_word_forward() {
        let modes = ModeSet::vim();
        let runtime = modes.create_runtime();
        assert_eq!(
            modes.resolve_key(&runtime, KeyEvent::char('w')),
            Some(vim_mode_command(VimAction::MoveWordForward)),
        );
    }

    #[test]
    fn vim_normal_b_resolves_to_move_word_backward() {
        let modes = ModeSet::vim();
        let runtime = modes.create_runtime();
        assert_eq!(
            modes.resolve_key(&runtime, KeyEvent::char('b')),
            Some(vim_mode_command(VimAction::MoveWordBackward)),
        );
    }

    #[test]
    fn vim_normal_e_resolves_to_move_word_end() {
        let modes = ModeSet::vim();
        let runtime = modes.create_runtime();
        assert_eq!(
            modes.resolve_key(&runtime, KeyEvent::char('e')),
            Some(vim_mode_command(VimAction::MoveWordEnd)),
        );
    }

    #[test]
    fn vim_normal_zero_resolves_to_move_to_line_start() {
        let modes = ModeSet::vim();
        let runtime = modes.create_runtime();
        assert_eq!(
            modes.resolve_key(&runtime, KeyEvent::char('0')),
            Some(EditCommand::MoveToLineStart.into()),
        );
    }

    #[test]
    fn vim_normal_caret_resolves_to_move_to_first_non_blank() {
        let modes = ModeSet::vim();
        let runtime = modes.create_runtime();
        assert_eq!(
            modes.resolve_key(&runtime, KeyEvent::char('^')),
            Some(EditCommand::MoveToFirstNonBlank.into()),
        );
    }

    #[test]
    fn vim_normal_dollar_resolves_to_move_to_line_end() {
        let modes = ModeSet::vim();
        let runtime = modes.create_runtime();
        assert_eq!(
            modes.resolve_key(&runtime, KeyEvent::char('$')),
            Some(EditCommand::MoveToLineEnd.into()),
        );
    }

    #[test]
    fn vim_normal_capital_g_resolves_to_move_to_last_line() {
        let modes = ModeSet::vim();
        let runtime = modes.create_runtime();
        assert_eq!(
            modes.resolve_key(&runtime, KeyEvent::char('G')),
            Some(EditCommand::MoveToLastLine.into()),
        );
    }

    #[test]
    fn vim_normal_open_brace_resolves_to_prev_paragraph() {
        let modes = ModeSet::vim();
        let runtime = modes.create_runtime();
        assert_eq!(
            modes.resolve_key(&runtime, KeyEvent::char('{')),
            Some(EditCommand::MoveToPrevParagraph.into()),
        );
    }

    #[test]
    fn vim_normal_close_brace_resolves_to_next_paragraph() {
        let modes = ModeSet::vim();
        let runtime = modes.create_runtime();
        assert_eq!(
            modes.resolve_key(&runtime, KeyEvent::char('}')),
            Some(EditCommand::MoveToNextParagraph.into()),
        );
    }

    #[test]
    fn vim_normal_x_resolves_to_delete_forward() {
        let modes = ModeSet::vim();
        let runtime = modes.create_runtime();
        assert_eq!(
            modes.resolve_key(&runtime, KeyEvent::char('x')),
            Some(EditCommand::Delete(1).into()),
        );
    }

    #[test]
    fn vim_normal_capital_x_resolves_to_delete_backward() {
        let modes = ModeSet::vim();
        let runtime = modes.create_runtime();
        assert_eq!(
            modes.resolve_key(&runtime, KeyEvent::char('X')),
            Some(EditCommand::Delete(-1).into()),
        );
    }

    #[test]
    fn vim_normal_capital_j_resolves_to_join_lines() {
        let modes = ModeSet::vim();
        let runtime = modes.create_runtime();
        assert_eq!(
            modes.resolve_key(&runtime, KeyEvent::char('J')),
            Some(EditCommand::JoinLines.into()),
        );
    }

    #[test]
    fn vim_normal_capital_d_resolves_to_delete_to_line_end() {
        let modes = ModeSet::vim();
        let runtime = modes.create_runtime();
        assert_eq!(
            modes.resolve_key(&runtime, KeyEvent::char('D')),
            Some(EditCommand::DeleteToLineEnd.into()),
        );
    }

    #[test]
    fn vim_normal_tilde_resolves_to_toggle_case() {
        let modes = ModeSet::vim();
        let runtime = modes.create_runtime();
        assert_eq!(
            modes.resolve_key(&runtime, KeyEvent::char('~')),
            Some(EditCommand::ToggleCase.into()),
        );
    }

    #[test]
    fn vim_open_below_enters_insert_and_returns_new_line_below() {
        let modes = ModeSet::vim();
        let mut runtime = modes.create_runtime();
        assert_eq!(
            modes.execute(
                &mut runtime,
                ModeName::new("vim"),
                ModeActionName::new("open-below"),
            ),
            Some(EditCommand::InsertNewLineBelow),
        );
        assert_eq!(
            modes.resolve_key(&runtime, KeyEvent::char('x')),
            Some(EditCommand::InsertText("x".to_string()).into()),
        );
    }

    #[test]
    fn vim_open_above_enters_insert_and_returns_new_line_above() {
        let modes = ModeSet::vim();
        let mut runtime = modes.create_runtime();
        assert_eq!(
            modes.execute(
                &mut runtime,
                ModeName::new("vim"),
                ModeActionName::new("open-above"),
            ),
            Some(EditCommand::InsertNewLineAbove),
        );
    }

    #[test]
    fn vim_insert_at_first_non_blank_enters_insert_and_returns_move() {
        let modes = ModeSet::vim();
        let mut runtime = modes.create_runtime();
        assert_eq!(
            modes.execute(
                &mut runtime,
                ModeName::new("vim"),
                ModeActionName::new("insert-at-first-non-blank"),
            ),
            Some(EditCommand::MoveToFirstNonBlank),
        );
    }

    #[test]
    fn vim_append_at_line_end_enters_insert_and_returns_move_after_line_end() {
        let modes = ModeSet::vim();
        let mut runtime = modes.create_runtime();
        assert_eq!(
            modes.execute(
                &mut runtime,
                ModeName::new("vim"),
                ModeActionName::new("append-at-line-end"),
            ),
            Some(EditCommand::MoveAfterLineEnd),
        );
    }

    #[test]
    fn vim_substitute_char_enters_insert_and_returns_delete_forward() {
        let modes = ModeSet::vim();
        let mut runtime = modes.create_runtime();
        assert_eq!(
            modes.execute(
                &mut runtime,
                ModeName::new("vim"),
                ModeActionName::new("substitute-char"),
            ),
            Some(EditCommand::Delete(1)),
        );
    }

    #[test]
    fn vim_change_to_line_end_enters_insert_and_returns_delete_to_line_end() {
        let modes = ModeSet::vim();
        let mut runtime = modes.create_runtime();
        assert_eq!(
            modes.execute(
                &mut runtime,
                ModeName::new("vim"),
                ModeActionName::new("change-to-line-end"),
            ),
            Some(EditCommand::DeleteToLineEnd),
        );
    }

    #[test]
    fn vim_substitute_line_enters_insert_and_returns_delete_line_content() {
        let modes = ModeSet::vim();
        let mut runtime = modes.create_runtime();
        assert_eq!(
            modes.execute(
                &mut runtime,
                ModeName::new("vim"),
                ModeActionName::new("substitute-line"),
            ),
            Some(EditCommand::DeleteLineContent),
        );
    }

    #[test]
    fn vim_normal_o_resolves_to_open_below_mode_command() {
        let modes = ModeSet::vim();
        let runtime = modes.create_runtime();
        assert_eq!(
            modes.resolve_key(&runtime, KeyEvent::char('o')),
            Some(vim_mode_command(VimAction::OpenBelow)),
        );
    }

    #[test]
    fn vim_normal_capital_o_resolves_to_open_above_mode_command() {
        let modes = ModeSet::vim();
        let runtime = modes.create_runtime();
        assert_eq!(
            modes.resolve_key(&runtime, KeyEvent::char('O')),
            Some(vim_mode_command(VimAction::OpenAbove)),
        );
    }

    #[test]
    fn vim_normal_capital_i_resolves_to_insert_at_first_non_blank() {
        let modes = ModeSet::vim();
        let runtime = modes.create_runtime();
        assert_eq!(
            modes.resolve_key(&runtime, KeyEvent::char('I')),
            Some(vim_mode_command(VimAction::InsertAtFirstNonBlank)),
        );
    }

    #[test]
    fn vim_normal_capital_a_resolves_to_append_at_line_end() {
        let modes = ModeSet::vim();
        let runtime = modes.create_runtime();
        assert_eq!(
            modes.resolve_key(&runtime, KeyEvent::char('A')),
            Some(vim_mode_command(VimAction::AppendAtLineEnd)),
        );
    }

    #[test]
    fn vim_normal_s_resolves_to_substitute_char() {
        let modes = ModeSet::vim();
        let runtime = modes.create_runtime();
        assert_eq!(
            modes.resolve_key(&runtime, KeyEvent::char('s')),
            Some(vim_mode_command(VimAction::SubstituteChar)),
        );
    }

    #[test]
    fn vim_normal_capital_c_resolves_to_change_to_line_end() {
        let modes = ModeSet::vim();
        let runtime = modes.create_runtime();
        assert_eq!(
            modes.resolve_key(&runtime, KeyEvent::char('C')),
            Some(vim_mode_command(VimAction::ChangeToLineEnd)),
        );
    }

    #[test]
    fn vim_normal_capital_s_resolves_to_substitute_line() {
        let modes = ModeSet::vim();
        let runtime = modes.create_runtime();
        assert_eq!(
            modes.resolve_key(&runtime, KeyEvent::char('S')),
            Some(vim_mode_command(VimAction::SubstituteLine)),
        );
    }

    #[test]
    fn vim_operator_counts_before_and_after_d_are_multiplied_privately() {
        let modes = ModeSet::vim();
        let mut runtime = modes.create_runtime();
        assert_eq!(
            modes.execute(
                &mut runtime,
                ModeName::new("vim"),
                ModeActionName::new("count-2"),
            ),
            None
        );
        assert_eq!(
            runtime.status_for_test(),
            InputStatus::Awaiting(TimeoutPolicy::Never)
        );
        assert_eq!(
            runtime.capture_for_test(KeyEvent::char('d')),
            InputDecision::Pass
        );
        assert_eq!(
            modes.execute(
                &mut runtime,
                ModeName::new("vim"),
                ModeActionName::new("delete-operator"),
            ),
            None
        );
        assert_eq!(
            runtime.capture_for_test(KeyEvent::char('3')),
            InputDecision::Consumed
        );
        assert_eq!(
            runtime.capture_for_test(KeyEvent::char('0')),
            InputDecision::Consumed
        );
        assert_eq!(
            runtime.capture_for_test(KeyEvent::char('d')),
            InputDecision::Emit(Command::Content(ContentCommand::Edit(
                EditCommand::Operate(OperatorCommand {
                    operator: TextOperator::Delete,
                    target: TextTarget::Lines { count: 60 },
                })
            )))
        );
        assert_eq!(runtime.status_for_test(), InputStatus::Ready);
    }

    #[test]
    fn vim_zero_after_delete_operator_is_line_start_motion() {
        let modes = ModeSet::vim();
        let mut runtime = modes.create_runtime();
        assert_eq!(
            modes.execute(
                &mut runtime,
                ModeName::new("vim"),
                ModeActionName::new("delete-operator"),
            ),
            None
        );

        assert_eq!(
            runtime.capture_for_test(KeyEvent::char('0')),
            InputDecision::Emit(Command::Content(ContentCommand::Edit(
                EditCommand::Operate(OperatorCommand {
                    operator: TextOperator::Delete,
                    target: TextTarget::Motion {
                        motion: TextMotion::LineStart,
                        count: 1,
                    },
                })
            )))
        );
        assert_eq!(runtime.status_for_test(), InputStatus::Ready);
    }
}
