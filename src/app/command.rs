use std::collections::BTreeMap;
use std::fmt;

use crate::app::mode_name::{ModeActionName, ModeName};
use crate::core::command::EditCommand;
use crate::protocol::viewport::ViewportCommand;

#[allow(
    dead_code,
    reason = "the neutral command protocol includes dynamically bound commands"
)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    App(AppCommand),
    Content(ContentCommand),
    Mode(ModeCommand),
    Viewport(ViewportCommand),
    Noop,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModeCommand {
    pub mode: ModeName,
    pub action: ModeActionName,
    pub arguments: ModeValue,
}

#[allow(
    dead_code,
    reason = "non-string values are reserved for script mode arguments"
)]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum ModeValue {
    #[default]
    Null,
    Bool(bool),
    Integer(i64),
    String(String),
    List(Vec<Self>),
    Map(BTreeMap<String, Self>),
}

impl ModeCommand {
    pub fn new(mode: ModeName, action: ModeActionName) -> Self {
        Self {
            mode,
            action,
            arguments: ModeValue::Null,
        }
    }

    #[allow(
        dead_code,
        reason = "script adapters attach arguments to mode commands"
    )]
    pub fn with_arguments(mut self, arguments: ModeValue) -> Self {
        self.arguments = arguments;
        self
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AppCommand {
    Quit,
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "focus commands are not bound by the built-in keymap"
        )
    )]
    FocusNext,
    #[expect(
        dead_code,
        reason = "focus commands are not bound by the built-in keymap"
    )]
    FocusPrev,
}

#[allow(
    dead_code,
    reason = "script effects construct the full content command protocol indirectly"
)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContentCommand {
    Edit(EditCommand),
    Transaction(TransactionCommand),
    Undo,
    Redo,
    Sequence(ContentSequence),
    Save,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContentSequence(Vec<ContentCommand>);

#[allow(
    dead_code,
    reason = "validated command sequences remain part of the extension contract"
)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ContentSequenceError {
    index: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ContentCommandContext {
    ContentOnly,
    WithViewState,
}

impl ContentCommand {
    #[allow(
        dead_code,
        reason = "validated command sequences remain part of the extension contract"
    )]
    pub fn try_sequence(commands: Vec<Self>) -> Result<Self, ContentSequenceError> {
        ContentSequence::try_new(commands).map(Self::Sequence)
    }

    pub(crate) fn context(&self) -> ContentCommandContext {
        match self {
            Self::Save => ContentCommandContext::ContentOnly,
            Self::Edit(_) | Self::Transaction(_) | Self::Undo | Self::Redo | Self::Sequence(_) => {
                ContentCommandContext::WithViewState
            }
        }
    }
}

impl ContentSequence {
    #[allow(
        dead_code,
        reason = "validated command sequences remain part of the extension contract"
    )]
    fn try_new(commands: Vec<ContentCommand>) -> Result<Self, ContentSequenceError> {
        if let Some(index) = commands
            .iter()
            .position(|command| command.context() == ContentCommandContext::ContentOnly)
        {
            return Err(ContentSequenceError { index });
        }
        Ok(Self(commands))
    }

    pub(crate) fn into_commands(self) -> Vec<ContentCommand> {
        self.0
    }
}

impl fmt::Display for ContentSequenceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "content command at sequence index {} requires a different execution context",
            self.index
        )
    }
}

impl std::error::Error for ContentSequenceError {}

#[allow(
    dead_code,
    reason = "script transaction effects map onto the full transaction protocol"
)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransactionCommand {
    Begin,
    Commit,
    #[expect(
        dead_code,
        reason = "transaction rollback is reserved for cancelling compound edits"
    )]
    Rollback,
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
    fn content_sequence_rejects_content_only_commands() {
        let error = ContentCommand::try_sequence(vec![
            ContentCommand::Edit(EditCommand::MoveLeftBy(1)),
            ContentCommand::Save,
        ])
        .unwrap_err();

        assert_eq!(
            error.to_string(),
            "content command at sequence index 1 requires a different execution context"
        );
    }
}
