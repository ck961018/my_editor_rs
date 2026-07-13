use std::any::Any;

use crate::core::command::{Command, ContentCommand, EditCommand};
use crate::core::keymap::{KeyBinding, Keymap};
use crate::protocol::content_query::CursorStyle;
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
    fn cursor_style(&self, state: &dyn ModeState) -> CursorStyle;
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

    pub(crate) fn cursor_style(&self, runtime: &ModeRuntime) -> CursorStyle {
        self.base.cursor_style(runtime.base.as_ref())
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

    fn cursor_style(&self, _state: &dyn ModeState) -> CursorStyle {
        CursorStyle::Default
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

    fn cursor_style(&self, state: &dyn ModeState) -> CursorStyle {
        match self.state(state).state {
            VimState::Normal => CursorStyle::Block,
            VimState::Insert => CursorStyle::Bar,
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
            "open-below" => {
                self.state_mut(state).state = VimState::Insert;
                Some(EditCommand::InsertNewLineBelow)
            }
            "open-above" => {
                self.state_mut(state).state = VimState::Insert;
                Some(EditCommand::InsertNewLineAbove)
            }
            "insert-at-first-non-blank" => {
                self.state_mut(state).state = VimState::Insert;
                Some(EditCommand::MoveToFirstNonBlank)
            }
            "append-at-line-end" => {
                self.state_mut(state).state = VimState::Insert;
                Some(EditCommand::MoveAfterLineEnd)
            }
            "substitute-char" => {
                self.state_mut(state).state = VimState::Insert;
                Some(EditCommand::Delete(1))
            }
            "change-to-line-end" => {
                self.state_mut(state).state = VimState::Insert;
                Some(EditCommand::DeleteToLineEnd)
            }
            "substitute-line" => {
                self.state_mut(state).state = VimState::Insert;
                Some(EditCommand::DeleteLineContent)
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
    km.bind(
        KeyEvent::char('o'),
        Command::Content(ContentCommand::Mode {
            mode: ModeId::new("vim"),
            action: ModeActionId::new("open-below"),
        }),
    );
    km.bind(
        KeyEvent::char('O'),
        Command::Content(ContentCommand::Mode {
            mode: ModeId::new("vim"),
            action: ModeActionId::new("open-above"),
        }),
    );
    km.bind(
        KeyEvent::char('I'),
        Command::Content(ContentCommand::Mode {
            mode: ModeId::new("vim"),
            action: ModeActionId::new("insert-at-first-non-blank"),
        }),
    );
    km.bind(
        KeyEvent::char('A'),
        Command::Content(ContentCommand::Mode {
            mode: ModeId::new("vim"),
            action: ModeActionId::new("append-at-line-end"),
        }),
    );
    km.bind(
        KeyEvent::char('s'),
        Command::Content(ContentCommand::Mode {
            mode: ModeId::new("vim"),
            action: ModeActionId::new("substitute-char"),
        }),
    );
    km.bind(
        KeyEvent::char('C'),
        Command::Content(ContentCommand::Mode {
            mode: ModeId::new("vim"),
            action: ModeActionId::new("change-to-line-end"),
        }),
    );
    km.bind(
        KeyEvent::char('S'),
        Command::Content(ContentCommand::Mode {
            mode: ModeId::new("vim"),
            action: ModeActionId::new("substitute-line"),
        }),
    );
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
    fn vim_cursor_style_tracks_runtime_mode() {
        let modes = ModeSet::vim();
        let mut runtime = modes.create_runtime();

        assert_eq!(modes.cursor_style(&runtime), CursorStyle::Block);
        modes.execute(
            &mut runtime,
            ModeId::new("vim"),
            ModeActionId::new("enter-insert"),
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
    fn vim_insert_ctrl_u_resolves_to_delete_to_line_start() {
        let modes = ModeSet::vim();
        let mut runtime = modes.create_runtime();
        modes.execute(
            &mut runtime,
            ModeId::new("vim"),
            ModeActionId::new("enter-insert"),
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
            ModeId::new("vim"),
            ModeActionId::new("enter-insert"),
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
            ModeId::new("vim"),
            ModeActionId::new("enter-insert"),
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
            ModeId::new("vim"),
            ModeActionId::new("enter-insert"),
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
                ModeId::new("vim"),
                ModeActionId::new("open-below"),
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
                ModeId::new("vim"),
                ModeActionId::new("open-above"),
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
                ModeId::new("vim"),
                ModeActionId::new("insert-at-first-non-blank"),
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
                ModeId::new("vim"),
                ModeActionId::new("append-at-line-end"),
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
                ModeId::new("vim"),
                ModeActionId::new("substitute-char"),
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
                ModeId::new("vim"),
                ModeActionId::new("change-to-line-end"),
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
                ModeId::new("vim"),
                ModeActionId::new("substitute-line"),
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
                mode: ModeId::new("vim"),
                action: ModeActionId::new("open-below"),
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
                mode: ModeId::new("vim"),
                action: ModeActionId::new("open-above"),
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
                mode: ModeId::new("vim"),
                action: ModeActionId::new("insert-at-first-non-blank"),
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
                mode: ModeId::new("vim"),
                action: ModeActionId::new("append-at-line-end"),
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
                mode: ModeId::new("vim"),
                action: ModeActionId::new("substitute-char"),
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
                mode: ModeId::new("vim"),
                action: ModeActionId::new("change-to-line-end"),
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
                mode: ModeId::new("vim"),
                action: ModeActionId::new("substitute-line"),
            })),
        );
    }
}
