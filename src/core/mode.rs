use std::any::Any;
use std::collections::HashMap;
use std::fmt;
use std::rc::Rc;

use crate::core::command::{
    CharSearchDirection, Command, ContentCommand, EditCommand, ModeCommand, TransactionCommand,
};
use crate::core::input::{InputContext, InputDecision, InputStatus, TimeoutPolicy};
use crate::core::keymap::Keymap;
use crate::core::mode_name::{ModeActionName, ModeName};
use crate::core::motion::{OperatorCommand, TextMotion, TextOperator, TextTarget};
use crate::protocol::content_query::{CursorStyle, SelectionShape};
use crate::protocol::key_event::{ArrowKey, KeyCode, KeyEvent};
use crate::protocol::viewport::{
    ViewportCommand, ViewportCursorBehavior, ViewportMoveAmount, ViewportMoveDirection,
};

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
}

impl<T: Any> ModeState for T {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

pub trait Mode {
    fn name(&self) -> &ModeName;
    fn actions(&self) -> &[ModeActionName];
    fn new_state(&self) -> Box<dyn ModeState>;
    fn keymap(&self, state: &dyn ModeState) -> &Keymap<Command>;
    fn typing(&self, state: &dyn ModeState, key: KeyEvent) -> Option<Command>;
    fn input_status(&self, _state: &dyn ModeState) -> InputStatus {
        InputStatus::Ready
    }
    fn capture(&self, _state: &mut dyn ModeState, _key: KeyEvent) -> InputDecision<Command> {
        InputDecision::Pass
    }
    fn on_timeout(&self, _state: &mut dyn ModeState) {}
    fn cancel(&self, _state: &mut dyn ModeState) {}
    fn cursor_style(&self, state: &dyn ModeState) -> CursorStyle;
    fn selection_shape(&self, _state: &dyn ModeState) -> SelectionShape {
        SelectionShape::Character
    }
    fn execute(
        &self,
        state: &mut dyn ModeState,
        action: &ModeActionName,
    ) -> Result<Option<Command>, ModeError>;
}

pub(crate) struct ModeRegistry {
    definitions: HashMap<ModeId, Rc<RegisteredMode>>,
    ids_by_name: HashMap<ModeName, ModeId>,
    next_id: u32,
}

struct RegisteredMode {
    id: ModeId,
    definition: Box<dyn Mode>,
    action_names: Vec<ModeActionName>,
    actions: HashMap<ModeActionName, ModeActionId>,
}

pub(crate) struct ModeInstance {
    registered: Rc<RegisteredMode>,
    state: Box<dyn ModeState>,
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
        assert!(
            !self.ids_by_name.contains_key(&name),
            "mode name must be unique"
        );
        let action_names = mode.actions().to_vec();
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
        let registered = Rc::new(RegisteredMode {
            id,
            definition: Box::new(mode),
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

    pub(crate) fn instantiate(&self, name: &ModeName) -> Option<ModeInstance> {
        let id = self.resolve_mode(name)?;
        let registered = self.definitions.get(&id)?.clone();
        Some(ModeInstance {
            state: registered.definition.new_state(),
            registered,
        })
    }
}

impl ModeInstance {
    pub(crate) fn name(&self) -> &ModeName {
        self.registered.definition.name()
    }

    pub(crate) fn keymap(&self) -> &Keymap<Command> {
        self.registered.definition.keymap(self.state.as_ref())
    }

    #[cfg(test)]
    pub(crate) fn resolve_key(&self, key: KeyEvent) -> Option<Command> {
        self.keymap()
            .node(&[key])
            .and_then(|node| node.action().cloned())
            .or_else(|| self.registered.definition.typing(self.state.as_ref(), key))
    }

    pub(crate) fn fallback(&self, key: KeyEvent) -> Option<Command> {
        self.registered.definition.typing(self.state.as_ref(), key)
    }

    pub(crate) fn cursor_style(&self) -> CursorStyle {
        self.registered.definition.cursor_style(self.state.as_ref())
    }

    pub(crate) fn selection_shape(&self) -> SelectionShape {
        self.registered
            .definition
            .selection_shape(self.state.as_ref())
    }

    pub(crate) fn execute(
        &mut self,
        mode: ModeId,
        action: ModeActionId,
    ) -> Result<Option<Command>, ModeError> {
        assert_eq!(self.registered.id, mode, "resolved mode must be active");
        let action = self
            .registered
            .action_names
            .get(usize::try_from(action.0).expect("mode action index overflow"))
            .expect("mode action id belongs to registered mode");
        self.registered
            .definition
            .execute(self.state.as_mut(), action)
    }
}

impl InputContext<Command> for ModeInstance {
    fn status(&self) -> InputStatus {
        self.registered.definition.input_status(self.state.as_ref())
    }

    fn capture(&mut self, key: KeyEvent) -> InputDecision<Command> {
        self.registered.definition.capture(self.state.as_mut(), key)
    }

    fn on_timeout(&mut self) {
        self.registered.definition.on_timeout(self.state.as_mut());
    }

    fn cancel(&mut self) {
        self.registered.definition.cancel(self.state.as_mut());
    }
}

#[cfg(test)]
pub(crate) struct ModeSet {
    registry: ModeRegistry,
    mode: ModeName,
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

    pub(crate) fn create_runtime(&self) -> ModeInstance {
        self.registry
            .instantiate(&self.mode)
            .expect("test mode exists")
    }

    pub(crate) fn resolve_key(&self, instance: &ModeInstance, key: KeyEvent) -> Option<Command> {
        instance.resolve_key(key)
    }

    pub(crate) fn cursor_style(&self, instance: &ModeInstance) -> CursorStyle {
        instance.cursor_style()
    }

    pub(crate) fn execute(
        &self,
        instance: &mut ModeInstance,
        mode: ModeName,
        action: ModeActionName,
    ) -> Option<EditCommand> {
        let (mode, action) = self.registry.resolve_command_checked(&mode, &action).ok()?;
        match instance.execute(mode, action).ok().flatten() {
            Some(Command::Content(ContentCommand::Edit(edit))) => Some(edit),
            Some(Command::Content(ContentCommand::Sequence(commands))) => {
                commands.into_commands().into_iter().find_map(|command| {
                    if let ContentCommand::Edit(edit) = command {
                        Some(edit)
                    } else {
                        None
                    }
                })
            }
            None
            | Some(Command::Content(ContentCommand::Transaction(_)))
            | Some(Command::Viewport(_))
            | Some(Command::App(_))
            | Some(Command::Mode(_))
            | Some(Command::Noop) => None,
            Some(Command::Content(command)) => {
                panic!("test helper expected edit command, got {command:?}")
            }
        }
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

    fn new_state(&self) -> Box<dyn ModeState> {
        Box::new(())
    }

    fn keymap(&self, _state: &dyn ModeState) -> &Keymap<Command> {
        &self.keymap
    }

    fn typing(&self, _state: &dyn ModeState, key: KeyEvent) -> Option<Command> {
        key.is_plain_char()
            .map(|ch| EditCommand::InsertText(ch.to_string()).into())
    }

    fn cursor_style(&self, _state: &dyn ModeState) -> CursorStyle {
        CursorStyle::Default
    }

    fn execute(
        &self,
        _state: &mut dyn ModeState,
        action: &ModeActionName,
    ) -> Result<Option<Command>, ModeError> {
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

    fn new_state(&self) -> Box<dyn ModeState> {
        Box::new(VimModeState {
            state: VimState::Normal,
            pending: None,
        })
    }

    fn keymap(&self, state: &dyn ModeState) -> &Keymap<Command> {
        match self.state(state).state {
            VimState::Normal => &self.normal_keymap,
            VimState::Insert => &self.insert_keymap,
            VimState::Visual | VimState::VisualLine => &self.visual_keymap,
        }
    }

    fn typing(&self, state: &dyn ModeState, key: KeyEvent) -> Option<Command> {
        match self.state(state).state {
            VimState::Normal => None,
            VimState::Insert => key
                .is_plain_char()
                .map(|ch| EditCommand::InsertText(ch.to_string()).into()),
            VimState::Visual | VimState::VisualLine => None,
        }
    }

    fn input_status(&self, state: &dyn ModeState) -> InputStatus {
        if self.state(state).pending.is_some() {
            InputStatus::Awaiting(TimeoutPolicy::Never)
        } else {
            InputStatus::Ready
        }
    }

    fn capture(&self, state: &mut dyn ModeState, key: KeyEvent) -> InputDecision<Command> {
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
                Some('h' | 'j' | 'k' | 'l' | 'w' | 'f' | 'F' | 'g' | 'd') => {
                    state.pending = Some(VimPending::Count(count));
                    InputDecision::Pass
                }
                Some('b' | 'e' | '^' | '$' | 'G' | '{' | '}')
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

    fn on_timeout(&self, state: &mut dyn ModeState) {
        self.state_mut(state).pending = None;
    }

    fn cancel(&self, state: &mut dyn ModeState) {
        self.state_mut(state).pending = None;
    }

    fn cursor_style(&self, state: &dyn ModeState) -> CursorStyle {
        match self.state(state).state {
            VimState::Normal | VimState::Visual | VimState::VisualLine => CursorStyle::Block,
            VimState::Insert => CursorStyle::Bar,
        }
    }

    fn selection_shape(&self, state: &dyn ModeState) -> SelectionShape {
        if self.state(state).state == VimState::VisualLine {
            SelectionShape::Line
        } else {
            SelectionShape::Character
        }
    }

    fn execute(
        &self,
        state: &mut dyn ModeState,
        action: &ModeActionName,
    ) -> Result<Option<Command>, ModeError> {
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
                Some(ContentCommand::Transaction(TransactionCommand::Commit))
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
                let state = self.state_mut(state);
                let linewise = state.state == VimState::VisualLine;
                Self::set_editor_state(state, VimState::Normal);
                Some(
                    if linewise {
                        EditCommand::DeleteSelectedLines
                    } else {
                        EditCommand::Delete(1)
                    }
                    .into(),
                )
            }
            VimAction::ChangeSelection => {
                let state = self.state_mut(state);
                let linewise = state.state == VimState::VisualLine;
                Self::set_editor_state(state, VimState::Insert);
                Some(content_sequence(vec![
                    ContentCommand::Transaction(TransactionCommand::Begin),
                    if linewise {
                        EditCommand::DeleteSelectedLines
                    } else {
                        EditCommand::Delete(1)
                    }
                    .into(),
                ]))
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
                return Ok(Some(Command::Viewport(viewport_command(
                    self.state_mut(state),
                    ViewportMoveDirection::Up,
                    ViewportMoveAmount::HalfPage,
                ))));
            }
            VimAction::ViewportHalfDown => {
                return Ok(Some(Command::Viewport(viewport_command(
                    self.state_mut(state),
                    ViewportMoveDirection::Down,
                    ViewportMoveAmount::HalfPage,
                ))));
            }
            VimAction::ViewportFullUp => {
                return Ok(Some(Command::Viewport(viewport_command(
                    self.state_mut(state),
                    ViewportMoveDirection::Up,
                    ViewportMoveAmount::FullPage,
                ))));
            }
            VimAction::ViewportFullDown => {
                return Ok(Some(Command::Viewport(viewport_command(
                    self.state_mut(state),
                    ViewportMoveDirection::Down,
                    ViewportMoveAmount::FullPage,
                ))));
            }
            VimAction::Count(digit) => {
                self.state_mut(state).pending = Some(VimPending::Count(usize::from(digit)));
                None
            }
        };
        Ok(command.map(Command::Content))
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
fn plain_edit_keymap() -> Keymap<Command> {
    default_text_keymap(true)
}

fn vim_insert_keymap() -> Keymap<Command> {
    let mut km = default_text_keymap(false);
    km.bind_edit(KeyEvent::ctrl('b'), EditCommand::MoveLeftBy(1));
    km.bind_edit(KeyEvent::ctrl('f'), EditCommand::MoveRightBy(1));
    km.bind_edit(KeyEvent::ctrl('h'), EditCommand::Delete(-1));
    km.bind_edit(KeyEvent::ctrl('w'), EditCommand::DeleteWordBackward);
    km.bind_edit(KeyEvent::ctrl('u'), EditCommand::DeleteToLineStart);
    km.bind_edit(KeyEvent::ctrl('k'), EditCommand::DeleteToLineEnd);
    km.bind_edit(
        KeyEvent::ctrl('j'),
        EditCommand::InsertText("\n".to_string()),
    );
    km.bind_edit(
        KeyEvent::ctrl('m'),
        EditCommand::InsertText("\n".to_string()),
    );
    km
}

fn default_text_keymap(bind_escape_to_collapse: bool) -> Keymap<Command> {
    let mut km = Keymap::new();
    km.bind_edit(
        KeyEvent::plain(KeyCode::Enter),
        EditCommand::InsertText("\n".to_string()),
    );
    km.bind_edit(KeyEvent::plain(KeyCode::Backspace), EditCommand::Delete(-1));
    km.bind_edit(KeyEvent::arrow(ArrowKey::Left), EditCommand::MoveLeftBy(1));
    km.bind_edit(
        KeyEvent::arrow(ArrowKey::Right),
        EditCommand::MoveRightBy(1),
    );
    km.bind_edit(KeyEvent::arrow(ArrowKey::Up), EditCommand::MoveUpBy(1));
    km.bind_edit(KeyEvent::arrow(ArrowKey::Down), EditCommand::MoveDownBy(1));
    km.bind_edit(
        KeyEvent::shift_arrow(ArrowKey::Left),
        EditCommand::ExtendLeftBy(1),
    );
    km.bind_edit(
        KeyEvent::shift_arrow(ArrowKey::Right),
        EditCommand::ExtendRightBy(1),
    );
    km.bind_edit(
        KeyEvent::shift_arrow(ArrowKey::Up),
        EditCommand::ExtendUpBy(1),
    );
    km.bind_edit(
        KeyEvent::shift_arrow(ArrowKey::Down),
        EditCommand::ExtendDownBy(1),
    );
    if bind_escape_to_collapse {
        km.bind_edit(
            KeyEvent::plain(KeyCode::Escape),
            EditCommand::CollapseSelections,
        );
    } else {
        km.bind(
            KeyEvent::plain(KeyCode::Escape),
            vim_mode_command(VimAction::EnterNormal),
        );
    }
    km
}

fn vim_normal_keymap() -> Keymap<Command> {
    let mut km = Keymap::new();
    km.bind(KeyEvent::char('h'), vim_mode_command(VimAction::MoveLeft));
    km.bind(KeyEvent::char('j'), vim_mode_command(VimAction::MoveDown));
    km.bind(KeyEvent::char('k'), vim_mode_command(VimAction::MoveUp));
    km.bind(KeyEvent::char('l'), vim_mode_command(VimAction::MoveRight));
    km.bind_edit(KeyEvent::char('w'), EditCommand::MoveWordForward);
    km.bind_edit(KeyEvent::char('b'), EditCommand::MoveWordBackward);
    km.bind_edit(KeyEvent::char('e'), EditCommand::MoveWordEnd);
    km.bind_edit(KeyEvent::char('0'), EditCommand::MoveToLineStart);
    km.bind_edit(KeyEvent::char('^'), EditCommand::MoveToFirstNonBlank);
    km.bind_edit(KeyEvent::char('$'), EditCommand::MoveToLineEnd);
    km.bind_edit(KeyEvent::char('G'), EditCommand::MoveToLastLine);
    km.bind_edit(KeyEvent::char('{'), EditCommand::MoveToPrevParagraph);
    km.bind_edit(KeyEvent::char('}'), EditCommand::MoveToNextParagraph);
    km.bind_edit(KeyEvent::char('x'), EditCommand::Delete(1));
    km.bind_edit(KeyEvent::char('X'), EditCommand::Delete(-1));
    km.bind_edit(KeyEvent::char('J'), EditCommand::JoinLines);
    km.bind_edit(KeyEvent::char('D'), EditCommand::DeleteToLineEnd);
    km.bind_edit(KeyEvent::char('~'), EditCommand::ToggleCase);
    km.bind(KeyEvent::char('u'), Command::Content(ContentCommand::Undo));
    km.bind(KeyEvent::ctrl('r'), Command::Content(ContentCommand::Redo));
    km.bind(KeyEvent::char('o'), vim_mode_command(VimAction::OpenBelow));
    km.bind(KeyEvent::char('O'), vim_mode_command(VimAction::OpenAbove));
    km.bind(
        KeyEvent::char('I'),
        vim_mode_command(VimAction::InsertAtFirstNonBlank),
    );
    km.bind(
        KeyEvent::char('A'),
        vim_mode_command(VimAction::AppendAtLineEnd),
    );
    km.bind(
        KeyEvent::char('s'),
        vim_mode_command(VimAction::SubstituteChar),
    );
    km.bind(
        KeyEvent::char('C'),
        vim_mode_command(VimAction::ChangeToLineEnd),
    );
    km.bind(
        KeyEvent::char('S'),
        vim_mode_command(VimAction::SubstituteLine),
    );
    km.bind(
        KeyEvent::char('i'),
        vim_mode_command(VimAction::EnterInsert),
    );
    km.bind(KeyEvent::char('a'), vim_mode_command(VimAction::Append));
    km.bind(
        KeyEvent::char('v'),
        vim_mode_command(VimAction::ToggleVisual),
    );
    km.bind(
        KeyEvent::char('V'),
        vim_mode_command(VimAction::ToggleLineVisual),
    );
    bind_vim_viewport_keys(&mut km);
    km.bind(
        [KeyEvent::char('g'), KeyEvent::char('g')],
        vim_mode_command(VimAction::GotoLine),
    );
    km.bind(
        KeyEvent::char('f'),
        vim_mode_command(VimAction::FindForward),
    );
    km.bind(
        KeyEvent::char('F'),
        vim_mode_command(VimAction::FindBackward),
    );
    km.bind(
        KeyEvent::char('d'),
        vim_mode_command(VimAction::DeleteOperator),
    );
    for digit in 1..=9 {
        km.bind(
            KeyEvent::char(char::from(b'0' + digit)),
            vim_mode_command(VimAction::Count(digit)),
        );
    }
    km.bind(KeyEvent::plain(KeyCode::Escape), Command::Noop);
    km
}

fn vim_visual_keymap() -> Keymap<Command> {
    let mut km = Keymap::new();
    km.bind(KeyEvent::char('h'), vim_mode_command(VimAction::MoveLeft));
    km.bind(KeyEvent::char('j'), vim_mode_command(VimAction::MoveDown));
    km.bind(KeyEvent::char('k'), vim_mode_command(VimAction::MoveUp));
    km.bind(KeyEvent::char('l'), vim_mode_command(VimAction::MoveRight));
    km.bind(
        KeyEvent::char('w'),
        vim_mode_command(VimAction::MoveWordForward),
    );
    km.bind(
        KeyEvent::char('b'),
        vim_mode_command(VimAction::MoveWordBackward),
    );
    km.bind(
        KeyEvent::char('e'),
        vim_mode_command(VimAction::MoveWordEnd),
    );
    km.bind(
        KeyEvent::char('0'),
        vim_mode_command(VimAction::MoveLineStart),
    );
    km.bind(
        KeyEvent::char('^'),
        vim_mode_command(VimAction::MoveFirstNonBlank),
    );
    km.bind(
        KeyEvent::char('$'),
        vim_mode_command(VimAction::MoveLineEnd),
    );
    km.bind(
        KeyEvent::char('G'),
        vim_mode_command(VimAction::MoveLastLine),
    );
    km.bind(
        KeyEvent::char('{'),
        vim_mode_command(VimAction::MovePrevParagraph),
    );
    km.bind(
        KeyEvent::char('}'),
        vim_mode_command(VimAction::MoveNextParagraph),
    );
    km.bind(
        [KeyEvent::char('g'), KeyEvent::char('g')],
        vim_mode_command(VimAction::GotoLine),
    );
    km.bind(
        KeyEvent::char('f'),
        vim_mode_command(VimAction::FindForward),
    );
    km.bind(
        KeyEvent::char('F'),
        vim_mode_command(VimAction::FindBackward),
    );
    for key in ['d', 'x', 'D', 'X'] {
        km.bind(
            KeyEvent::char(key),
            vim_mode_command(VimAction::DeleteSelection),
        );
    }
    for key in ['c', 's'] {
        km.bind(
            KeyEvent::char(key),
            vim_mode_command(VimAction::ChangeSelection),
        );
    }
    km.bind(
        KeyEvent::char('v'),
        vim_mode_command(VimAction::ToggleVisual),
    );
    km.bind(
        KeyEvent::char('V'),
        vim_mode_command(VimAction::ToggleLineVisual),
    );
    bind_vim_viewport_keys(&mut km);
    km.bind(
        KeyEvent::plain(KeyCode::Escape),
        vim_mode_command(VimAction::LeaveVisual),
    );
    km.bind_edit(
        KeyEvent::arrow(ArrowKey::Left),
        EditCommand::ExtendLeftBy(1),
    );
    km.bind_edit(
        KeyEvent::arrow(ArrowKey::Right),
        EditCommand::ExtendRightBy(1),
    );
    km.bind_edit(KeyEvent::arrow(ArrowKey::Up), EditCommand::ExtendUpBy(1));
    km.bind_edit(
        KeyEvent::arrow(ArrowKey::Down),
        EditCommand::ExtendDownBy(1),
    );
    for digit in 1..=9 {
        km.bind(
            KeyEvent::char(char::from(b'0' + digit)),
            vim_mode_command(VimAction::Count(digit)),
        );
    }
    km
}

fn bind_vim_viewport_keys(keymap: &mut Keymap<Command>) {
    keymap.bind(
        KeyEvent::ctrl('u'),
        vim_mode_command(VimAction::ViewportHalfUp),
    );
    keymap.bind(
        KeyEvent::ctrl('d'),
        vim_mode_command(VimAction::ViewportHalfDown),
    );
    keymap.bind(
        KeyEvent::ctrl('b'),
        vim_mode_command(VimAction::ViewportFullUp),
    );
    keymap.bind(
        KeyEvent::ctrl('f'),
        vim_mode_command(VimAction::ViewportFullDown),
    );
}

fn vim_mode_command(action: VimAction) -> Command {
    Command::Mode(ModeCommand {
        mode: ModeName::new("vim"),
        action: ModeActionName::new(action.name()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    use crate::core::command::{AppCommand, ContentCommand, EditCommand};
    use crate::core::input::{InputContext, InputDecision, InputStatus, TimeoutPolicy};

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

        fn new_state(&self) -> Box<dyn ModeState> {
            Box::new(())
        }

        fn keymap(&self, _state: &dyn ModeState) -> &Keymap<Command> {
            &self.keymap
        }

        fn typing(&self, _state: &dyn ModeState, _key: KeyEvent) -> Option<Command> {
            None
        }

        fn cursor_style(&self, _state: &dyn ModeState) -> CursorStyle {
            CursorStyle::Default
        }

        fn execute(
            &self,
            _state: &mut dyn ModeState,
            action: &ModeActionName,
        ) -> Result<Option<Command>, ModeError> {
            if action.as_str() == "focus-next" {
                return Ok(Some(Command::App(AppCommand::FocusNext)));
            }
            Err(ModeError::UnknownAction {
                mode: self.name.clone(),
                action: action.clone(),
            })
        }
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

        assert_eq!(
            instance.execute(mode, action),
            Ok(Some(Command::App(AppCommand::FocusNext)))
        );
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
            Some(EditCommand::Delete(1)),
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
        assert_eq!(runtime.selection_shape(), SelectionShape::Line);
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
        assert_eq!(runtime.selection_shape(), SelectionShape::Character);
    }

    #[test]
    fn vim_viewport_keys_emit_frontend_sized_commands() {
        let registry = ModeRegistry::builtin();
        let mode_name = ModeName::new("vim");
        let mut runtime = registry.instantiate(&mode_name).unwrap();

        for (key, action, direction, amount) in [
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
                runtime.resolve_key(KeyEvent::ctrl(key)),
                Some(vim_mode_command(action)),
            );
            let (mode, action) = registry
                .resolve_command_checked(&mode_name, &ModeActionName::new(action.name()))
                .unwrap();
            assert_eq!(
                runtime.execute(mode, action),
                Ok(Some(Command::Viewport(ViewportCommand::new(
                    direction,
                    amount,
                    ViewportCursorBehavior::Move,
                )))),
            );
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
            Ok(Some(Command::Content(content_sequence(vec![
                ContentCommand::Edit(EditCommand::MoveRightBy(1)),
                ContentCommand::Transaction(TransactionCommand::Begin),
            ]))))
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
            Some(EditCommand::MoveWordForward.into()),
        );
    }

    #[test]
    fn vim_normal_b_resolves_to_move_word_backward() {
        let modes = ModeSet::vim();
        let runtime = modes.create_runtime();
        assert_eq!(
            modes.resolve_key(&runtime, KeyEvent::char('b')),
            Some(EditCommand::MoveWordBackward.into()),
        );
    }

    #[test]
    fn vim_normal_e_resolves_to_move_word_end() {
        let modes = ModeSet::vim();
        let runtime = modes.create_runtime();
        assert_eq!(
            modes.resolve_key(&runtime, KeyEvent::char('e')),
            Some(EditCommand::MoveWordEnd.into()),
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
            runtime.status(),
            InputStatus::Awaiting(TimeoutPolicy::Never)
        );
        assert_eq!(runtime.capture(KeyEvent::char('d')), InputDecision::Pass);
        assert_eq!(
            modes.execute(
                &mut runtime,
                ModeName::new("vim"),
                ModeActionName::new("delete-operator"),
            ),
            None
        );
        assert_eq!(
            runtime.capture(KeyEvent::char('3')),
            InputDecision::Consumed
        );
        assert_eq!(
            runtime.capture(KeyEvent::char('0')),
            InputDecision::Consumed
        );
        assert_eq!(
            runtime.capture(KeyEvent::char('d')),
            InputDecision::Emit(Command::Content(ContentCommand::Edit(
                EditCommand::Operate(OperatorCommand {
                    operator: TextOperator::Delete,
                    target: TextTarget::Lines { count: 60 },
                })
            )))
        );
        assert_eq!(runtime.status(), InputStatus::Ready);
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
            runtime.capture(KeyEvent::char('0')),
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
        assert_eq!(runtime.status(), InputStatus::Ready);
    }
}
