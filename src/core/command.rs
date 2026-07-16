use crate::core::mode::{ModeActionName, ModeName};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    App(AppCommand),
    Content(ContentCommand),
    Noop,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AppCommand {
    Quit,
    #[allow(dead_code)] // 预留：v0.2 焦点切换（仅 dispatcher 单测构造，生产 keymap 未绑）
    FocusNext,
    #[allow(dead_code)] // 预留：v0.2 焦点切换（仅 dispatcher 单测构造，生产 keymap 未绑）
    FocusPrev,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContentCommand {
    Edit(EditCommand),
    Save,
    Mode {
        mode: ModeName,
        action: ModeActionName,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EditCommand {
    #[allow(dead_code)]
    // 预留：仅 executor 单测构造，生产 keymap 用 MoveLeftBy/RightBy/UpBy/DownBy
    MoveBy {
        chars: isize,
        lines: isize,
    },
    MoveLeftBy(usize),
    MoveRightBy(usize),
    MoveWithinLineLeftBy(usize),
    MoveWithinLineRightBy(usize),
    MoveUpBy(usize),
    MoveDownBy(usize),
    MoveToLine {
        line_index: usize,
    },
    MoveToChar {
        target: char,
        direction: CharSearchDirection,
        occurrence: usize,
    },
    #[allow(dead_code)]
    // 预留：仅 executor 单测构造，生产 keymap 用 MoveLeftBy/RightBy/UpBy/DownBy
    MoveTo {
        char_idx: usize,
        line_idx: usize,
    },
    ExtendLeftBy(usize),
    ExtendRightBy(usize),
    ExtendUpBy(usize),
    ExtendDownBy(usize),
    InsertText(String),
    Delete(isize),
    DeleteLines {
        lines: usize,
    },
    DeleteWordBackward,
    CollapseSelections,
    // Vim 基础编辑与移动操作。
    DeleteToLineStart,
    DeleteToLineEnd,
    MoveWordForward,
    MoveWordBackward,
    MoveWordEnd,
    MoveToLineStart,
    MoveToFirstNonBlank,
    MoveToLineEnd,
    MoveToLastLine,
    MoveToPrevParagraph,
    MoveToNextParagraph,
    JoinLines,
    ToggleCase,
    InsertNewLineBelow,
    InsertNewLineAbove,
    MoveAfterLineEnd,
    DeleteLineContent,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CharSearchDirection {
    Forward,
    Backward,
}

impl From<EditCommand> for Command {
    fn from(command: EditCommand) -> Self {
        Command::Content(command.into())
    }
}

impl From<EditCommand> for ContentCommand {
    fn from(command: EditCommand) -> Self {
        Self::Edit(command)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edit_command_wraps_as_content_command() {
        let command: Command = EditCommand::MoveLeftBy(1).into();
        assert_eq!(
            command,
            Command::Content(ContentCommand::Edit(EditCommand::MoveLeftBy(1)))
        );
    }

    #[test]
    fn edit_command_uses_the_edit_variant() {
        let command = ContentCommand::Edit(EditCommand::MoveLeftBy(1));

        assert!(matches!(command, ContentCommand::Edit(_)));
    }

    #[test]
    fn mode_command_carries_owned_mode_action_names() {
        let command = Command::Content(ContentCommand::Mode {
            mode: ModeName::new("vim"),
            action: ModeActionName::new("enter-insert"),
        });
        assert_eq!(
            command,
            Command::Content(ContentCommand::Mode {
                mode: ModeName::new("vim"),
                action: ModeActionName::new("enter-insert"),
            })
        );
    }
}
