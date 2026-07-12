use std::any::Any;

use crate::core::command::{Command, ContentCommand, EditCommand};
use crate::core::keymap::{KeyBinding, Keymap};
use crate::protocol::key_event::{ArrowKey, KeyCode, KeyEvent};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ModeId(&'static str);

impl ModeId {
    pub const fn new(value: &'static str) -> Self {
        Self(value)
    }

    #[allow(dead_code)]
    pub const fn as_str(self) -> &'static str {
        self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ModeActionId(&'static str);

impl ModeActionId {
    pub const fn new(value: &'static str) -> Self {
        Self(value)
    }

    pub const fn as_str(self) -> &'static str {
        self.0
    }
}

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
    fn id(&self) -> ModeId;
    fn new_state(&self) -> Box<dyn ModeState>;
    fn keymap(&self, state: &dyn ModeState) -> &Keymap;
    fn typing(&self, state: &dyn ModeState, key: KeyEvent) -> Option<Command>;
    fn execute(&self, state: &mut dyn ModeState, action: ModeActionId) -> Option<EditCommand>;
}

pub(crate) struct ModeRuntime {
    base: Box<dyn ModeState>,
}

pub(crate) struct ModeSet {
    base: Box<dyn Mode>,
}

impl ModeSet {
    pub(crate) fn vim() -> Self {
        Self {
            base: Box::new(VimMode::new()),
        }
    }

    #[cfg(test)]
    pub(crate) fn plain_edit() -> Self {
        Self {
            base: Box::new(PlainEditMode::new()),
        }
    }

    pub(crate) fn create_runtime(&self) -> ModeRuntime {
        ModeRuntime {
            base: self.base.new_state(),
        }
    }

    // Mode keymaps cannot use prefixes because the dispatcher tracks only the
    // static Content keymap; a mode prefix would otherwise fall through typing.
    pub(crate) fn resolve_key(&self, runtime: &ModeRuntime, key: KeyEvent) -> Option<Command> {
        match self.base.keymap(runtime.base.as_ref()).lookup(key) {
            Some(KeyBinding::Command(command)) => Some(command.clone()),
            Some(KeyBinding::Prefix(_)) | None => self.base.typing(runtime.base.as_ref(), key),
        }
    }

    pub(crate) fn execute(
        &self,
        runtime: &mut ModeRuntime,
        mode: ModeId,
        action: ModeActionId,
    ) -> Option<EditCommand> {
        (self.base.id() == mode)
            .then(|| self.base.execute(runtime.base.as_mut(), action))
            .flatten()
    }
}

#[cfg(test)]
struct PlainEditMode {
    keymap: Keymap,
}

#[cfg(test)]
impl PlainEditMode {
    fn new() -> Self {
        Self {
            keymap: plain_edit_keymap(),
        }
    }
}

#[cfg(test)]
impl Mode for PlainEditMode {
    fn id(&self) -> ModeId {
        ModeId::new("plain-edit")
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

    fn execute(&self, _state: &mut dyn ModeState, _action: ModeActionId) -> Option<EditCommand> {
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
}

struct VimMode {
    normal_keymap: Keymap,
    insert_keymap: Keymap,
}

impl VimMode {
    fn new() -> Self {
        Self {
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
}

impl Mode for VimMode {
    fn id(&self) -> ModeId {
        ModeId::new("vim")
    }

    fn new_state(&self) -> Box<dyn ModeState> {
        Box::new(VimModeState {
            state: VimState::Normal,
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

    fn execute(&self, state: &mut dyn ModeState, action: ModeActionId) -> Option<EditCommand> {
        match action.as_str() {
            "enter-insert" => {
                self.state_mut(state).state = VimState::Insert;
                None
            }
            "enter-normal" => {
                self.state_mut(state).state = VimState::Normal;
                None
            }
            "append" => {
                self.state_mut(state).state = VimState::Insert;
                Some(EditCommand::MoveRightBy(1))
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
            Command::Content(ContentCommand::Mode {
                mode: ModeId::new("vim"),
                action: ModeActionId::new("enter-normal"),
            }),
        );
    }
    km
}

fn vim_normal_keymap() -> Keymap {
    let mut km = Keymap::new();
    km.bind_edit(KeyEvent::char('h'), EditCommand::MoveLeftBy(1));
    km.bind_edit(KeyEvent::char('j'), EditCommand::MoveDownBy(1));
    km.bind_edit(KeyEvent::char('k'), EditCommand::MoveUpBy(1));
    km.bind_edit(KeyEvent::char('l'), EditCommand::MoveRightBy(1));
    km.bind(
        KeyEvent::char('i'),
        Command::Content(ContentCommand::Mode {
            mode: ModeId::new("vim"),
            action: ModeActionId::new("enter-insert"),
        }),
    );
    km.bind(
        KeyEvent::char('a'),
        Command::Content(ContentCommand::Mode {
            mode: ModeId::new("vim"),
            action: ModeActionId::new("append"),
        }),
    );
    km.bind(KeyEvent::plain(KeyCode::Escape), Command::Noop);
    km
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::command::{ContentCommand, EditCommand};

    #[test]
    fn mode_ids_are_copyable_values() {
        let id = ModeId::new("vim");
        assert_eq!(id.as_str(), "vim");
        assert_eq!(id, ModeId::new("vim"));
    }

    #[test]
    fn mode_action_ids_are_copyable_values() {
        let action = ModeActionId::new("enter-insert");
        assert_eq!(action.as_str(), "enter-insert");
        assert_eq!(action, ModeActionId::new("enter-insert"));
    }

    #[test]
    fn vim_mode_runtime_is_independent() {
        let modes = ModeSet::vim();
        let mut first = modes.create_runtime();
        let second = modes.create_runtime();
        assert_eq!(
            modes.execute(
                &mut first,
                ModeId::new("vim"),
                ModeActionId::new("enter-insert"),
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
                mode: ModeId::new("vim"),
                action: ModeActionId::new("append"),
            }))
        );
    }

    #[test]
    fn vim_insert_resolves_emacs_motion_and_delete_keys() {
        let modes = ModeSet::vim();
        let mut runtime = modes.create_runtime();
        assert_eq!(
            modes.execute(
                &mut runtime,
                ModeId::new("vim"),
                ModeActionId::new("enter-insert"),
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
                ModeId::new("vim"),
                ModeActionId::new("enter-insert"),
            ),
            None,
        );

        assert_eq!(
            modes.resolve_key(&runtime, KeyEvent::ctrl('w')),
            Some(EditCommand::DeleteWordBackward.into()),
        );
    }

    #[test]
    fn vim_append_enters_insert_and_returns_right_move() {
        let modes = ModeSet::vim();
        let mut runtime = modes.create_runtime();

        assert_eq!(
            modes.execute(
                &mut runtime,
                ModeId::new("vim"),
                ModeActionId::new("append"),
            ),
            Some(EditCommand::MoveRightBy(1)),
        );
        assert_eq!(
            modes.resolve_key(&runtime, KeyEvent::char('x')),
            Some(EditCommand::InsertText("x".to_string()).into()),
        );
    }

    #[test]
    fn vim_normal_c_remains_unbound() {
        let modes = ModeSet::vim();
        let runtime = modes.create_runtime();

        assert_eq!(modes.resolve_key(&runtime, KeyEvent::char('c')), None);
    }
}
