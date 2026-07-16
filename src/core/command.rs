use crate::core::mode::{ModeActionName, ModeName};
use crate::core::motion::OperatorCommand;

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
    Transaction(TransactionCommand),
    Undo,
    Redo,
    Sequence(Vec<ContentCommand>),
    Save,
    Mode {
        mode: ModeName,
        action: ModeActionName,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransactionCommand {
    Begin,
    Commit,
    #[allow(dead_code)] // 为取消复合编辑预留；当前 Vim Escape 提交而非回滚。
    Rollback,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EditCommand {
    Operate(OperatorCommand),
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
    ExtendWithinLineLeftBy(usize),
    ExtendWithinLineRightBy(usize),
    ExtendUpBy(usize),
    ExtendDownBy(usize),
    ExtendToLine {
        line_index: usize,
    },
    ExtendToChar {
        target: char,
        direction: CharSearchDirection,
        occurrence: usize,
    },
    ExtendWordForward,
    ExtendWordBackward,
    ExtendWordEnd,
    ExtendToLineStart,
    ExtendToFirstNonBlank,
    ExtendToLineEnd,
    ExtendToLastLine,
    ExtendToPrevParagraph,
    ExtendToNextParagraph,
    InsertText(String),
    Delete(isize),
    #[allow(dead_code)] // 兼容直接编辑命令；Vim dd 已走 operator + linewise target。
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
