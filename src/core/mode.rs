use std::any::Any;
use std::collections::HashMap;
use std::rc::Rc;

use crate::core::command::{
    CharSearchDirection, Command, ContentCommand, EditCommand, TransactionCommand,
};
use crate::core::input::{InputContext, InputDecision, InputStatus, TimeoutPolicy};
use crate::core::keymap::Keymap;
use crate::core::motion::{OperatorCommand, TextMotion, TextOperator, TextTarget};
use crate::protocol::content_query::{CursorStyle, SelectionShape};
use crate::protocol::key_event::{ArrowKey, KeyCode, KeyEvent};
use crate::protocol::viewport::{
    ViewportCommand, ViewportCursorBehavior, ViewportMoveAmount, ViewportMoveDirection,
};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ModeName(String);

impl ModeName {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    #[allow(dead_code)] // Future script/protocol adapters read the owned symbolic name.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ModeActionName(String);

impl ModeActionName {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ModeId(u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ModeActionId(u32);

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
    fn keymap(&self, state: &dyn ModeState) -> &Keymap;
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
    fn execute(&self, state: &mut dyn ModeState, action: &ModeActionName)
    -> Option<ContentCommand>;
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

    pub(crate) fn resolve_command(
        &self,
        mode: &ModeName,
        action: &ModeActionName,
    ) -> Option<(ModeId, ModeActionId)> {
        let mode = self.resolve_mode(mode)?;
        Some((mode, self.resolve_action(mode, action)?))
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
    pub(crate) fn keymap(&self) -> &Keymap {
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

    pub(crate) fn execute(&mut self, mode: ModeId, action: ModeActionId) -> Option<ContentCommand> {
        assert_eq!(self.registered.id, mode, "mode command targets active mode");
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
        let (mode, action) = self.registry.resolve_command(&mode, &action)?;
        match instance.execute(mode, action) {
            Some(ContentCommand::Edit(edit)) => Some(edit),
            Some(ContentCommand::Sequence(commands)) => commands.into_iter().find_map(|command| {
                if let ContentCommand::Edit(edit) = command {
                    Some(edit)
                } else {
                    None
                }
            }),
            None | Some(ContentCommand::Transaction(_)) | Some(ContentCommand::Viewport(_)) => None,
            Some(command) => panic!("test helper expected edit command, got {command:?}"),
        }
    }
}

#[cfg(test)]
struct PlainEditMode {
    name: ModeName,
    actions: Vec<ModeActionName>,
    keymap: Keymap,
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

    fn keymap(&self, _state: &dyn ModeState) -> &Keymap {
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
        _action: &ModeActionName,
    ) -> Option<ContentCommand> {
        None
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
    normal_keymap: Keymap,
    insert_keymap: Keymap,
    visual_keymap: Keymap,
}

const VIM_ACTION_NAMES: [&str; 45] = [
    "enter-insert",
    "enter-normal",
    "toggle-visual",
    "toggle-line-visual",
    "leave-visual",
    "delete-selection",
    "change-selection",
    "append",
    "open-below",
    "open-above",
    "insert-at-first-non-blank",
    "append-at-line-end",
    "substitute-char",
    "change-to-line-end",
    "substitute-line",
    "move-left",
    "move-down",
    "move-up",
    "move-right",
    "move-word-forward",
    "move-word-backward",
    "move-word-end",
    "move-line-start",
    "move-first-non-blank",
    "move-line-end",
    "move-last-line",
    "move-prev-paragraph",
    "move-next-paragraph",
    "goto-line",
    "find-forward",
    "find-backward",
    "delete-operator",
    "viewport-half-up",
    "viewport-half-down",
    "viewport-full-up",
    "viewport-full-down",
    "count-1",
    "count-2",
    "count-3",
    "count-4",
    "count-5",
    "count-6",
    "count-7",
    "count-8",
    "count-9",
];

impl VimMode {
    fn new() -> Self {
        Self {
            name: ModeName::new("vim"),
            actions: VIM_ACTION_NAMES
                .into_iter()
                .map(ModeActionName::new)
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
    ContentCommand::Sequence((0..count).map(|_| command.clone().into()).collect())
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

    fn keymap(&self, state: &dyn ModeState) -> &Keymap {
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
    ) -> Option<ContentCommand> {
        match action.as_str() {
            "enter-insert" => {
                Self::set_editor_state(self.state_mut(state), VimState::Insert);
                Some(ContentCommand::Transaction(TransactionCommand::Begin))
            }
            "enter-normal" => {
                Self::set_editor_state(self.state_mut(state), VimState::Normal);
                Some(ContentCommand::Transaction(TransactionCommand::Commit))
            }
            "toggle-visual" => {
                let state = self.state_mut(state);
                if state.state == VimState::Visual {
                    Self::set_editor_state(state, VimState::Normal);
                    Some(EditCommand::CollapseSelections.into())
                } else {
                    Self::set_editor_state(state, VimState::Visual);
                    None
                }
            }
            "toggle-line-visual" => {
                let state = self.state_mut(state);
                if state.state == VimState::VisualLine {
                    Self::set_editor_state(state, VimState::Normal);
                    Some(EditCommand::CollapseSelections.into())
                } else {
                    Self::set_editor_state(state, VimState::VisualLine);
                    None
                }
            }
            "leave-visual" => {
                Self::set_editor_state(self.state_mut(state), VimState::Normal);
                Some(EditCommand::CollapseSelections.into())
            }
            "delete-selection" => {
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
            "change-selection" => {
                let state = self.state_mut(state);
                let linewise = state.state == VimState::VisualLine;
                Self::set_editor_state(state, VimState::Insert);
                Some(ContentCommand::Sequence(vec![
                    ContentCommand::Transaction(TransactionCommand::Begin),
                    if linewise {
                        EditCommand::DeleteSelectedLines
                    } else {
                        EditCommand::Delete(1)
                    }
                    .into(),
                ]))
            }
            "append" => {
                Self::set_editor_state(self.state_mut(state), VimState::Insert);
                Some(ContentCommand::Sequence(vec![
                    EditCommand::MoveRightBy(1).into(),
                    ContentCommand::Transaction(TransactionCommand::Begin),
                ]))
            }
            "open-below" => {
                Self::set_editor_state(self.state_mut(state), VimState::Insert);
                Some(ContentCommand::Sequence(vec![
                    ContentCommand::Transaction(TransactionCommand::Begin),
                    EditCommand::InsertNewLineBelow.into(),
                ]))
            }
            "open-above" => {
                Self::set_editor_state(self.state_mut(state), VimState::Insert);
                Some(ContentCommand::Sequence(vec![
                    ContentCommand::Transaction(TransactionCommand::Begin),
                    EditCommand::InsertNewLineAbove.into(),
                ]))
            }
            "insert-at-first-non-blank" => {
                Self::set_editor_state(self.state_mut(state), VimState::Insert);
                Some(ContentCommand::Sequence(vec![
                    EditCommand::MoveToFirstNonBlank.into(),
                    ContentCommand::Transaction(TransactionCommand::Begin),
                ]))
            }
            "append-at-line-end" => {
                Self::set_editor_state(self.state_mut(state), VimState::Insert);
                Some(ContentCommand::Sequence(vec![
                    EditCommand::MoveAfterLineEnd.into(),
                    ContentCommand::Transaction(TransactionCommand::Begin),
                ]))
            }
            "substitute-char" => {
                Self::set_editor_state(self.state_mut(state), VimState::Insert);
                Some(ContentCommand::Sequence(vec![
                    ContentCommand::Transaction(TransactionCommand::Begin),
                    EditCommand::Delete(1).into(),
                ]))
            }
            "change-to-line-end" => {
                Self::set_editor_state(self.state_mut(state), VimState::Insert);
                Some(ContentCommand::Sequence(vec![
                    ContentCommand::Transaction(TransactionCommand::Begin),
                    EditCommand::DeleteToLineEnd.into(),
                ]))
            }
            "substitute-line" => {
                Self::set_editor_state(self.state_mut(state), VimState::Insert);
                Some(ContentCommand::Sequence(vec![
                    ContentCommand::Transaction(TransactionCommand::Begin),
                    EditCommand::DeleteLineContent.into(),
                ]))
            }
            "move-left" => {
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
            "move-down" => {
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
            "move-up" => {
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
            "move-right" => {
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
            "move-word-forward" => {
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
            "move-word-backward" => {
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
            "move-word-end" => {
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
            "move-line-start" => {
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
            "move-first-non-blank" => {
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
            "move-line-end" => {
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
            "move-last-line" => {
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
            "move-prev-paragraph" => {
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
            "move-next-paragraph" => {
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
            "goto-line" => {
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
            "find-forward" | "find-backward" => {
                let state = self.state_mut(state);
                let count = Self::take_count(state).unwrap_or(1);
                state.pending = Some(VimPending::Find {
                    direction: if action.as_str() == "find-forward" {
                        CharSearchDirection::Forward
                    } else {
                        CharSearchDirection::Backward
                    },
                    count,
                });
                None
            }
            "delete-operator" => {
                let state = self.state_mut(state);
                let operator_count = Self::take_count(state).unwrap_or(1);
                state.pending = Some(VimPending::Delete {
                    operator_count,
                    motion_count: None,
                });
                None
            }
            "viewport-half-up" => Some(viewport_command(
                self.state_mut(state),
                ViewportMoveDirection::Up,
                ViewportMoveAmount::HalfPage,
            )),
            "viewport-half-down" => Some(viewport_command(
                self.state_mut(state),
                ViewportMoveDirection::Down,
                ViewportMoveAmount::HalfPage,
            )),
            "viewport-full-up" => Some(viewport_command(
                self.state_mut(state),
                ViewportMoveDirection::Up,
                ViewportMoveAmount::FullPage,
            )),
            "viewport-full-down" => Some(viewport_command(
                self.state_mut(state),
                ViewportMoveDirection::Down,
                ViewportMoveAmount::FullPage,
            )),
            name if name
                .strip_prefix("count-")
                .and_then(|digit| digit.parse::<usize>().ok())
                .is_some() =>
            {
                let digit = name
                    .strip_prefix("count-")
                    .and_then(|digit| digit.parse::<usize>().ok())
                    .expect("count action contains a digit");
                self.state_mut(state).pending = Some(VimPending::Count(digit));
                None
            }
            _ => None,
        }
    }
}

fn viewport_command(
    state: &mut VimModeState,
    direction: ViewportMoveDirection,
    amount: ViewportMoveAmount,
) -> ContentCommand {
    VimMode::take_count(state);
    ContentCommand::Viewport(ViewportCommand::new(
        direction,
        amount,
        if matches!(state.state, VimState::Visual | VimState::VisualLine) {
            ViewportCursorBehavior::Extend
        } else {
            ViewportCursorBehavior::Move
        },
    ))
}

#[cfg(test)]
fn plain_edit_keymap() -> Keymap {
    default_text_keymap(true)
}

fn vim_insert_keymap() -> Keymap {
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

fn default_text_keymap(bind_escape_to_collapse: bool) -> Keymap {
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
            vim_mode_command("enter-normal"),
        );
    }
    km
}

fn vim_normal_keymap() -> Keymap {
    let mut km = Keymap::new();
    km.bind(KeyEvent::char('h'), vim_mode_command("move-left"));
    km.bind(KeyEvent::char('j'), vim_mode_command("move-down"));
    km.bind(KeyEvent::char('k'), vim_mode_command("move-up"));
    km.bind(KeyEvent::char('l'), vim_mode_command("move-right"));
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
    km.bind(KeyEvent::char('o'), vim_mode_command("open-below"));
    km.bind(KeyEvent::char('O'), vim_mode_command("open-above"));
    km.bind(
        KeyEvent::char('I'),
        vim_mode_command("insert-at-first-non-blank"),
    );
    km.bind(KeyEvent::char('A'), vim_mode_command("append-at-line-end"));
    km.bind(KeyEvent::char('s'), vim_mode_command("substitute-char"));
    km.bind(KeyEvent::char('C'), vim_mode_command("change-to-line-end"));
    km.bind(KeyEvent::char('S'), vim_mode_command("substitute-line"));
    km.bind(KeyEvent::char('i'), vim_mode_command("enter-insert"));
    km.bind(KeyEvent::char('a'), vim_mode_command("append"));
    km.bind(KeyEvent::char('v'), vim_mode_command("toggle-visual"));
    km.bind(KeyEvent::char('V'), vim_mode_command("toggle-line-visual"));
    bind_vim_viewport_keys(&mut km);
    km.bind(
        [KeyEvent::char('g'), KeyEvent::char('g')],
        vim_mode_command("goto-line"),
    );
    km.bind(KeyEvent::char('f'), vim_mode_command("find-forward"));
    km.bind(KeyEvent::char('F'), vim_mode_command("find-backward"));
    km.bind(KeyEvent::char('d'), vim_mode_command("delete-operator"));
    for digit in '1'..='9' {
        km.bind(
            KeyEvent::char(digit),
            vim_mode_command(&format!("count-{digit}")),
        );
    }
    km.bind(KeyEvent::plain(KeyCode::Escape), Command::Noop);
    km
}

fn vim_visual_keymap() -> Keymap {
    let mut km = Keymap::new();
    km.bind(KeyEvent::char('h'), vim_mode_command("move-left"));
    km.bind(KeyEvent::char('j'), vim_mode_command("move-down"));
    km.bind(KeyEvent::char('k'), vim_mode_command("move-up"));
    km.bind(KeyEvent::char('l'), vim_mode_command("move-right"));
    km.bind(KeyEvent::char('w'), vim_mode_command("move-word-forward"));
    km.bind(KeyEvent::char('b'), vim_mode_command("move-word-backward"));
    km.bind(KeyEvent::char('e'), vim_mode_command("move-word-end"));
    km.bind(KeyEvent::char('0'), vim_mode_command("move-line-start"));
    km.bind(
        KeyEvent::char('^'),
        vim_mode_command("move-first-non-blank"),
    );
    km.bind(KeyEvent::char('$'), vim_mode_command("move-line-end"));
    km.bind(KeyEvent::char('G'), vim_mode_command("move-last-line"));
    km.bind(KeyEvent::char('{'), vim_mode_command("move-prev-paragraph"));
    km.bind(KeyEvent::char('}'), vim_mode_command("move-next-paragraph"));
    km.bind(
        [KeyEvent::char('g'), KeyEvent::char('g')],
        vim_mode_command("goto-line"),
    );
    km.bind(KeyEvent::char('f'), vim_mode_command("find-forward"));
    km.bind(KeyEvent::char('F'), vim_mode_command("find-backward"));
    for key in ['d', 'x', 'D', 'X'] {
        km.bind(KeyEvent::char(key), vim_mode_command("delete-selection"));
    }
    for key in ['c', 's'] {
        km.bind(KeyEvent::char(key), vim_mode_command("change-selection"));
    }
    km.bind(KeyEvent::char('v'), vim_mode_command("toggle-visual"));
    km.bind(KeyEvent::char('V'), vim_mode_command("toggle-line-visual"));
    bind_vim_viewport_keys(&mut km);
    km.bind(
        KeyEvent::plain(KeyCode::Escape),
        vim_mode_command("leave-visual"),
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
    for digit in '1'..='9' {
        km.bind(
            KeyEvent::char(digit),
            vim_mode_command(&format!("count-{digit}")),
        );
    }
    km
}

fn bind_vim_viewport_keys(keymap: &mut Keymap) {
    keymap.bind(KeyEvent::ctrl('u'), vim_mode_command("viewport-half-up"));
    keymap.bind(KeyEvent::ctrl('d'), vim_mode_command("viewport-half-down"));
    keymap.bind(KeyEvent::ctrl('b'), vim_mode_command("viewport-full-up"));
    keymap.bind(KeyEvent::ctrl('f'), vim_mode_command("viewport-full-down"));
}

fn vim_mode_command(action: &str) -> Command {
    Command::Content(ContentCommand::Mode {
        mode: ModeName::new("vim"),
        action: ModeActionName::new(action),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::command::{ContentCommand, EditCommand};
    use crate::core::input::{InputContext, InputDecision, InputStatus, TimeoutPolicy};

    struct DynamicMode {
        name: ModeName,
        actions: Vec<ModeActionName>,
        keymap: Keymap,
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

        fn keymap(&self, _state: &dyn ModeState) -> &Keymap {
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
            _action: &ModeActionName,
        ) -> Option<ContentCommand> {
            None
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
            Some(Command::Content(ContentCommand::Mode {
                mode: ModeName::new("vim"),
                action: ModeActionName::new("append"),
            }))
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
            Some(vim_mode_command("toggle-visual")),
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
            Some(vim_mode_command("move-right")),
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
            Some(vim_mode_command("delete-selection")),
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
            Some(vim_mode_command("toggle-visual")),
        );
    }

    #[test]
    fn vim_line_visual_uses_line_shape_and_deletes_touched_lines() {
        let modes = ModeSet::vim();
        let mut runtime = modes.create_runtime();

        assert_eq!(
            modes.resolve_key(&runtime, KeyEvent::char('V')),
            Some(vim_mode_command("toggle-line-visual")),
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

        for (key, action_name, direction, amount) in [
            (
                'u',
                "viewport-half-up",
                ViewportMoveDirection::Up,
                ViewportMoveAmount::HalfPage,
            ),
            (
                'd',
                "viewport-half-down",
                ViewportMoveDirection::Down,
                ViewportMoveAmount::HalfPage,
            ),
            (
                'b',
                "viewport-full-up",
                ViewportMoveDirection::Up,
                ViewportMoveAmount::FullPage,
            ),
            (
                'f',
                "viewport-full-down",
                ViewportMoveDirection::Down,
                ViewportMoveAmount::FullPage,
            ),
        ] {
            assert_eq!(
                runtime.resolve_key(KeyEvent::ctrl(key)),
                Some(vim_mode_command(action_name)),
            );
            let (mode, action) = registry
                .resolve_command(&mode_name, &ModeActionName::new(action_name))
                .unwrap();
            assert_eq!(
                runtime.execute(mode, action),
                Some(ContentCommand::Viewport(ViewportCommand::new(
                    direction,
                    amount,
                    ViewportCursorBehavior::Move,
                ))),
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
            Some(vim_mode_command("leave-visual")),
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
            Some(vim_mode_command("enter-insert")),
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
        let (mode, action) = registry.resolve_command(&mode_name, &action_name).unwrap();

        assert_eq!(
            instance.execute(mode, action),
            Some(ContentCommand::Sequence(vec![
                ContentCommand::Edit(EditCommand::MoveRightBy(1)),
                ContentCommand::Transaction(TransactionCommand::Begin),
            ]))
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
            Some(Command::Content(ContentCommand::Mode {
                mode: ModeName::new("vim"),
                action: ModeActionName::new("open-below"),
            })),
        );
    }

    #[test]
    fn vim_normal_capital_o_resolves_to_open_above_mode_command() {
        let modes = ModeSet::vim();
        let runtime = modes.create_runtime();
        assert_eq!(
            modes.resolve_key(&runtime, KeyEvent::char('O')),
            Some(Command::Content(ContentCommand::Mode {
                mode: ModeName::new("vim"),
                action: ModeActionName::new("open-above"),
            })),
        );
    }

    #[test]
    fn vim_normal_capital_i_resolves_to_insert_at_first_non_blank() {
        let modes = ModeSet::vim();
        let runtime = modes.create_runtime();
        assert_eq!(
            modes.resolve_key(&runtime, KeyEvent::char('I')),
            Some(Command::Content(ContentCommand::Mode {
                mode: ModeName::new("vim"),
                action: ModeActionName::new("insert-at-first-non-blank"),
            })),
        );
    }

    #[test]
    fn vim_normal_capital_a_resolves_to_append_at_line_end() {
        let modes = ModeSet::vim();
        let runtime = modes.create_runtime();
        assert_eq!(
            modes.resolve_key(&runtime, KeyEvent::char('A')),
            Some(Command::Content(ContentCommand::Mode {
                mode: ModeName::new("vim"),
                action: ModeActionName::new("append-at-line-end"),
            })),
        );
    }

    #[test]
    fn vim_normal_s_resolves_to_substitute_char() {
        let modes = ModeSet::vim();
        let runtime = modes.create_runtime();
        assert_eq!(
            modes.resolve_key(&runtime, KeyEvent::char('s')),
            Some(Command::Content(ContentCommand::Mode {
                mode: ModeName::new("vim"),
                action: ModeActionName::new("substitute-char"),
            })),
        );
    }

    #[test]
    fn vim_normal_capital_c_resolves_to_change_to_line_end() {
        let modes = ModeSet::vim();
        let runtime = modes.create_runtime();
        assert_eq!(
            modes.resolve_key(&runtime, KeyEvent::char('C')),
            Some(Command::Content(ContentCommand::Mode {
                mode: ModeName::new("vim"),
                action: ModeActionName::new("change-to-line-end"),
            })),
        );
    }

    #[test]
    fn vim_normal_capital_s_resolves_to_substitute_line() {
        let modes = ModeSet::vim();
        let runtime = modes.create_runtime();
        assert_eq!(
            modes.resolve_key(&runtime, KeyEvent::char('S')),
            Some(Command::Content(ContentCommand::Mode {
                mode: ModeName::new("vim"),
                action: ModeActionName::new("substitute-line"),
            })),
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
