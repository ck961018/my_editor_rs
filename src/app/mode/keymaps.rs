use super::{CommandKeymapExt, VimAction};
use crate::app::command::{Command, ContentCommand, ModeCommand};
use crate::app::mode_name::{ModeActionName, ModeName};
use crate::core::command::EditCommand;
use crate::core::keymap::Keymap;
use crate::protocol::key_event::{ArrowKey, KeyCode, KeyEvent};

#[cfg(test)]
pub(super) fn plain_edit_keymap() -> Keymap<Command> {
    default_text_keymap(true)
}

pub(super) fn vim_insert_keymap() -> Keymap<Command> {
    let mut keymap = default_text_keymap(false);
    keymap.bind_edit(KeyEvent::ctrl('b'), EditCommand::MoveLeftBy(1));
    keymap.bind_edit(KeyEvent::ctrl('f'), EditCommand::MoveRightBy(1));
    keymap.bind_edit(KeyEvent::ctrl('h'), EditCommand::Delete(-1));
    keymap.bind_edit(KeyEvent::ctrl('w'), EditCommand::DeleteWordBackward);
    keymap.bind_edit(KeyEvent::ctrl('u'), EditCommand::DeleteToLineStart);
    keymap.bind_edit(KeyEvent::ctrl('k'), EditCommand::DeleteToLineEnd);
    keymap.bind_edit(
        KeyEvent::ctrl('j'),
        EditCommand::InsertText("\n".to_string()),
    );
    keymap.bind_edit(
        KeyEvent::ctrl('m'),
        EditCommand::InsertText("\n".to_string()),
    );
    keymap
}

fn default_text_keymap(bind_escape_to_collapse: bool) -> Keymap<Command> {
    let mut keymap = Keymap::new();
    keymap.bind_edit(
        KeyEvent::plain(KeyCode::Enter),
        EditCommand::InsertText("\n".to_string()),
    );
    keymap.bind_edit(KeyEvent::plain(KeyCode::Backspace), EditCommand::Delete(-1));
    keymap.bind_edit(KeyEvent::arrow(ArrowKey::Left), EditCommand::MoveLeftBy(1));
    keymap.bind_edit(
        KeyEvent::arrow(ArrowKey::Right),
        EditCommand::MoveRightBy(1),
    );
    keymap.bind_edit(KeyEvent::arrow(ArrowKey::Up), EditCommand::MoveUpBy(1));
    keymap.bind_edit(KeyEvent::arrow(ArrowKey::Down), EditCommand::MoveDownBy(1));
    keymap.bind_edit(
        KeyEvent::shift_arrow(ArrowKey::Left),
        EditCommand::ExtendLeftBy(1),
    );
    keymap.bind_edit(
        KeyEvent::shift_arrow(ArrowKey::Right),
        EditCommand::ExtendRightBy(1),
    );
    keymap.bind_edit(
        KeyEvent::shift_arrow(ArrowKey::Up),
        EditCommand::ExtendUpBy(1),
    );
    keymap.bind_edit(
        KeyEvent::shift_arrow(ArrowKey::Down),
        EditCommand::ExtendDownBy(1),
    );
    if bind_escape_to_collapse {
        keymap.bind_edit(
            KeyEvent::plain(KeyCode::Escape),
            EditCommand::CollapseSelections,
        );
    } else {
        keymap.bind(
            KeyEvent::plain(KeyCode::Escape),
            vim_mode_command(VimAction::EnterNormal),
        );
    }
    keymap
}

pub(super) fn vim_normal_keymap() -> Keymap<Command> {
    let mut keymap = Keymap::new();
    for (key, action) in [
        ('h', VimAction::MoveLeft),
        ('j', VimAction::MoveDown),
        ('k', VimAction::MoveUp),
        ('l', VimAction::MoveRight),
    ] {
        keymap.bind(KeyEvent::char(key), vim_mode_command(action));
    }
    for (key, edit) in [
        ('0', EditCommand::MoveToLineStart),
        ('^', EditCommand::MoveToFirstNonBlank),
        ('$', EditCommand::MoveToLineEnd),
        ('G', EditCommand::MoveToLastLine),
        ('{', EditCommand::MoveToPrevParagraph),
        ('}', EditCommand::MoveToNextParagraph),
        ('x', EditCommand::Delete(1)),
        ('X', EditCommand::Delete(-1)),
        ('J', EditCommand::JoinLines),
        ('D', EditCommand::DeleteToLineEnd),
        ('~', EditCommand::ToggleCase),
    ] {
        keymap.bind_edit(KeyEvent::char(key), edit);
    }
    keymap.bind(KeyEvent::char('u'), Command::Content(ContentCommand::Undo));
    keymap.bind(KeyEvent::ctrl('r'), Command::Content(ContentCommand::Redo));
    for (key, action) in [
        ('o', VimAction::OpenBelow),
        ('O', VimAction::OpenAbove),
        ('I', VimAction::InsertAtFirstNonBlank),
        ('A', VimAction::AppendAtLineEnd),
        ('s', VimAction::SubstituteChar),
        ('C', VimAction::ChangeToLineEnd),
        ('S', VimAction::SubstituteLine),
        ('i', VimAction::EnterInsert),
        ('a', VimAction::Append),
        ('v', VimAction::ToggleVisual),
        ('V', VimAction::ToggleLineVisual),
        ('w', VimAction::MoveWordForward),
        ('b', VimAction::MoveWordBackward),
        ('e', VimAction::MoveWordEnd),
        ('f', VimAction::FindForward),
        ('F', VimAction::FindBackward),
        ('d', VimAction::DeleteOperator),
    ] {
        keymap.bind(KeyEvent::char(key), vim_mode_command(action));
    }
    bind_vim_viewport_keys(&mut keymap);
    keymap.bind(
        [KeyEvent::char('g'), KeyEvent::char('g')],
        vim_mode_command(VimAction::GotoLine),
    );
    bind_counts(&mut keymap);
    keymap.bind(KeyEvent::plain(KeyCode::Escape), Command::Noop);
    keymap
}

pub(super) fn vim_visual_keymap() -> Keymap<Command> {
    let mut keymap = Keymap::new();
    for (key, action) in [
        ('h', VimAction::MoveLeft),
        ('j', VimAction::MoveDown),
        ('k', VimAction::MoveUp),
        ('l', VimAction::MoveRight),
        ('w', VimAction::MoveWordForward),
        ('b', VimAction::MoveWordBackward),
        ('e', VimAction::MoveWordEnd),
        ('0', VimAction::MoveLineStart),
        ('^', VimAction::MoveFirstNonBlank),
        ('$', VimAction::MoveLineEnd),
        ('G', VimAction::MoveLastLine),
        ('{', VimAction::MovePrevParagraph),
        ('}', VimAction::MoveNextParagraph),
        ('f', VimAction::FindForward),
        ('F', VimAction::FindBackward),
        ('v', VimAction::ToggleVisual),
        ('V', VimAction::ToggleLineVisual),
    ] {
        keymap.bind(KeyEvent::char(key), vim_mode_command(action));
    }
    keymap.bind(
        [KeyEvent::char('g'), KeyEvent::char('g')],
        vim_mode_command(VimAction::GotoLine),
    );
    for key in ['d', 'x', 'D', 'X'] {
        keymap.bind(
            KeyEvent::char(key),
            vim_mode_command(VimAction::DeleteSelection),
        );
    }
    for key in ['c', 's'] {
        keymap.bind(
            KeyEvent::char(key),
            vim_mode_command(VimAction::ChangeSelection),
        );
    }
    bind_vim_viewport_keys(&mut keymap);
    keymap.bind(
        KeyEvent::plain(KeyCode::Escape),
        vim_mode_command(VimAction::LeaveVisual),
    );
    keymap.bind_edit(
        KeyEvent::arrow(ArrowKey::Left),
        EditCommand::ExtendLeftBy(1),
    );
    keymap.bind_edit(
        KeyEvent::arrow(ArrowKey::Right),
        EditCommand::ExtendRightBy(1),
    );
    keymap.bind_edit(KeyEvent::arrow(ArrowKey::Up), EditCommand::ExtendUpBy(1));
    keymap.bind_edit(
        KeyEvent::arrow(ArrowKey::Down),
        EditCommand::ExtendDownBy(1),
    );
    bind_counts(&mut keymap);
    keymap
}

fn bind_vim_viewport_keys(keymap: &mut Keymap<Command>) {
    for (key, action) in [
        ('u', VimAction::ViewportHalfUp),
        ('d', VimAction::ViewportHalfDown),
        ('b', VimAction::ViewportFullUp),
        ('f', VimAction::ViewportFullDown),
    ] {
        keymap.bind(KeyEvent::ctrl(key), vim_mode_command(action));
    }
}

fn bind_counts(keymap: &mut Keymap<Command>) {
    for digit in 1..=9 {
        keymap.bind(
            KeyEvent::char(char::from(b'0' + digit)),
            vim_mode_command(VimAction::Count(digit)),
        );
    }
}

pub(super) fn vim_mode_command(action: VimAction) -> Command {
    Command::Mode(ModeCommand::new(
        ModeName::new("vim"),
        ModeActionName::new(action.name()),
    ))
}
