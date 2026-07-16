use std::any::Any;
use std::collections::HashMap;
use std::rc::Rc;

use crate::core::command::{CharSearchDirection, Command, ContentCommand, EditCommand};
use crate::core::input::{InputContext, InputDecision, InputStatus, TimeoutPolicy};
use crate::core::keymap::Keymap;
use crate::protocol::content_query::CursorStyle;
use crate::protocol::key_event::{ArrowKey, KeyCode, KeyEvent};

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
            None => None,
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
}

const VIM_ACTION_NAMES: [&str; 27] = [
    "enter-insert",
    "enter-normal",
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
    "goto-line",
    "find-forward",
    "find-backward",
    "delete-operator",
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
        }
    }

    fn typing(&self, state: &dyn ModeState, key: KeyEvent) -> Option<Command> {
        match self.state(state).state {
            VimState::Normal => None,
            VimState::Insert => key
                .is_plain_char()
                .map(|ch| EditCommand::InsertText(ch.to_string()).into()),
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
                Some('h' | 'j' | 'k' | 'l' | 'f' | 'F' | 'g' | 'd') => {
                    state.pending = Some(VimPending::Count(count));
                    InputDecision::Pass
                }
                _ => InputDecision::Consumed,
            },
            VimPending::Find { direction, count } => match key.is_plain_char() {
                Some(target) => InputDecision::Emit(
                    EditCommand::MoveToChar {
                        target,
                        direction,
                        occurrence: count,
                    }
                    .into(),
                ),
                None => InputDecision::Consumed,
            },
            VimPending::Delete {
                operator_count,
                motion_count,
            } => match key.is_plain_char() {
                Some('0') if motion_count.is_none() => InputDecision::Consumed,
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
                    EditCommand::DeleteLines {
                        lines: operator_count.saturating_mul(motion_count.unwrap_or(1)),
                    }
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
            VimState::Normal => CursorStyle::Block,
            VimState::Insert => CursorStyle::Bar,
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
                None
            }
            "enter-normal" => {
                Self::set_editor_state(self.state_mut(state), VimState::Normal);
                None
            }
            "append" => {
                Self::set_editor_state(self.state_mut(state), VimState::Insert);
                Some(EditCommand::MoveRightBy(1).into())
            }
            "open-below" => {
                Self::set_editor_state(self.state_mut(state), VimState::Insert);
                Some(EditCommand::InsertNewLineBelow.into())
            }
            "open-above" => {
                Self::set_editor_state(self.state_mut(state), VimState::Insert);
                Some(EditCommand::InsertNewLineAbove.into())
            }
            "insert-at-first-non-blank" => {
                Self::set_editor_state(self.state_mut(state), VimState::Insert);
                Some(EditCommand::MoveToFirstNonBlank.into())
            }
            "append-at-line-end" => {
                Self::set_editor_state(self.state_mut(state), VimState::Insert);
                Some(EditCommand::MoveAfterLineEnd.into())
            }
            "substitute-char" => {
                Self::set_editor_state(self.state_mut(state), VimState::Insert);
                Some(EditCommand::Delete(1).into())
            }
            "change-to-line-end" => {
                Self::set_editor_state(self.state_mut(state), VimState::Insert);
                Some(EditCommand::DeleteToLineEnd.into())
            }
            "substitute-line" => {
                Self::set_editor_state(self.state_mut(state), VimState::Insert);
                Some(EditCommand::DeleteLineContent.into())
            }
            "move-left" => Some(
                EditCommand::MoveWithinLineLeftBy(
                    Self::take_count(self.state_mut(state)).unwrap_or(1),
                )
                .into(),
            ),
            "move-down" => Some(
                EditCommand::MoveDownBy(Self::take_count(self.state_mut(state)).unwrap_or(1))
                    .into(),
            ),
            "move-up" => Some(
                EditCommand::MoveUpBy(Self::take_count(self.state_mut(state)).unwrap_or(1)).into(),
            ),
            "move-right" => Some(
                EditCommand::MoveWithinLineRightBy(
                    Self::take_count(self.state_mut(state)).unwrap_or(1),
                )
                .into(),
            ),
            "goto-line" => {
                let line_index = Self::take_count(self.state_mut(state))
                    .unwrap_or(1)
                    .saturating_sub(1);
                Some(EditCommand::MoveToLine { line_index }.into())
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
            Some(ContentCommand::Edit(EditCommand::MoveRightBy(1)))
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
                EditCommand::DeleteLines { lines: 60 }
            )))
        );
        assert_eq!(runtime.status(), InputStatus::Ready);
    }

    #[test]
    fn vim_zero_cannot_start_a_motion_count_after_delete_operator() {
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
            InputDecision::Consumed
        );
        assert_eq!(runtime.status(), InputStatus::Ready);
        assert_eq!(runtime.capture(KeyEvent::char('d')), InputDecision::Pass);
    }
}
