use crate::core::mode::{ModeActionId, ModeId};

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
    Text(EditCommand),
    Save,
    Mode { mode: ModeId, action: ModeActionId },
}

impl ContentCommand {
    #[allow(non_snake_case)] // Temporary constructor bridge while Task 3 migrates pattern matches.
    pub fn Edit(command: EditCommand) -> Self {
        Self::Text(command)
    }
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
    MoveUpBy(usize),
    MoveDownBy(usize),
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
    CollapseSelections,
}

impl From<EditCommand> for Command {
    fn from(command: EditCommand) -> Self {
        Command::Content(ContentCommand::Edit(command))
    }
}

/// Temporary app-layer compatibility alias. Task 3 removes its remaining callers.
pub type TextCommand = EditCommand;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_command_wraps_as_content_command() {
        let command: Command = EditCommand::MoveLeftBy(1).into();
        assert_eq!(
            command,
            Command::Content(ContentCommand::Edit(EditCommand::MoveLeftBy(1)))
        );
    }

    #[test]
    fn mode_command_carries_mode_action_ids() {
        let command = Command::Content(ContentCommand::Mode {
            mode: ModeId::new("vim"),
            action: ModeActionId::new("enter-insert"),
        });
        assert_eq!(
            command,
            Command::Content(ContentCommand::Mode {
                mode: ModeId::new("vim"),
                action: ModeActionId::new("enter-insert"),
            })
        );
    }
}
