use std::path::PathBuf;

use crate::core::buffer::Buffer;
use crate::core::command::{ContentCommand, TransactionCommand};
use crate::core::content_view_state::ContentViewState;
use crate::core::edit::apply_edit;
use crate::core::mode::ModeName;
use crate::core::status_bar::StatusBar;
use crate::core::transaction::{TextChangeSet, TextStateId, TextTransactionError};
use crate::protocol::status::StatusMessage;

pub enum ContentInput<'a> {
    Command(ContentCommand),
    View {
        command: ContentCommand,
        state: &'a mut ContentViewState,
    },
    Event(ContentEvent),
}

pub enum ContentEvent {
    SaveFinished {
        state: TextStateId,
        result: std::io::Result<()>,
    },
}

#[derive(Debug, PartialEq, Eq)]
pub struct SaveSnapshot {
    pub path: PathBuf,
    pub bytes: String,
    pub revision: u64,
    pub state: TextStateId,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ContentEffect {
    None,
    Save(SaveSnapshot),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContentChange {
    Text(TextChangeSet),
}

#[derive(Debug, PartialEq, Eq)]
pub struct ContentOutcome {
    pub effect: ContentEffect,
    pub content_changed: bool,
    pub view_changed: bool,
    pub change: Option<ContentChange>,
}

impl ContentOutcome {
    fn new(effect: ContentEffect, content_changed: bool, view_changed: bool) -> Self {
        Self {
            effect,
            content_changed,
            view_changed,
            change: None,
        }
    }

    fn with_change(mut self, change: Option<TextChangeSet>) -> Self {
        self.change = change.map(ContentChange::Text);
        self
    }

    fn merge(&mut self, next: Self) {
        debug_assert_eq!(next.effect, ContentEffect::None);
        self.content_changed |= next.content_changed;
        self.view_changed |= next.view_changed;
        self.change = match (self.change.take(), next.change) {
            (None, change) | (change, None) => change,
            (Some(ContentChange::Text(first)), Some(ContentChange::Text(second))) => {
                Some(ContentChange::Text(
                    first
                        .compose(&second)
                        .expect("ordered content changes must compose"),
                ))
            }
        };
    }
}

pub trait TransactionalContent {
    type Transaction;
    type Change;

    fn begin_transaction(&mut self);
    fn commit_transaction(&mut self) -> Result<bool, TextTransactionError>;
    fn rollback_transaction(&mut self) -> Result<bool, TextTransactionError>;
    fn undo(&mut self) -> Result<bool, TextTransactionError>;
    fn redo(&mut self) -> Result<bool, TextTransactionError>;
    fn take_change(&mut self) -> Option<Self::Change>;
}

impl TransactionalContent for Buffer {
    type Transaction = TextChangeSet;
    type Change = TextChangeSet;

    fn begin_transaction(&mut self) {
        Buffer::begin_transaction(self);
    }

    fn commit_transaction(&mut self) -> Result<bool, TextTransactionError> {
        Buffer::commit_transaction(self)
    }

    fn rollback_transaction(&mut self) -> Result<bool, TextTransactionError> {
        Buffer::rollback_transaction(self)
    }

    fn undo(&mut self) -> Result<bool, TextTransactionError> {
        Buffer::undo(self)
    }

    fn redo(&mut self) -> Result<bool, TextTransactionError> {
        Buffer::redo(self)
    }

    fn take_change(&mut self) -> Option<Self::Change> {
        Buffer::take_last_change(self)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum ContentResult {
    Handled(ContentOutcome),
    NotHandled,
}

pub enum Content {
    Buffer(Buffer),
    StatusBar(StatusBar),
}

impl Content {
    pub fn create_view_state(&self) -> ContentViewState {
        match self {
            Self::Buffer(_) => ContentViewState::buffer(),
            Self::StatusBar(_) => ContentViewState::StatusBar,
        }
    }

    pub fn default_mode(&self) -> Option<ModeName> {
        match self {
            Self::Buffer(_) => Some(ModeName::new("vim")),
            Self::StatusBar(_) => None,
        }
    }

    pub fn transform_view_state(
        &self,
        state: &mut ContentViewState,
        change: &ContentChange,
    ) -> bool {
        match (self, state, change) {
            (
                Self::Buffer(buffer),
                ContentViewState::Buffer(state),
                ContentChange::Text(change),
            ) => buffer.transform_selections(state.selections_mut(), change),
            (Self::StatusBar(_), ContentViewState::StatusBar, _) => false,
            _ => panic!("content/view state mismatch"),
        }
    }

    pub fn execute(&mut self, input: ContentInput<'_>) -> ContentResult {
        match (self, input) {
            (
                Self::Buffer(buffer),
                ContentInput::View {
                    command,
                    state: ContentViewState::Buffer(state),
                },
            ) => execute_buffer_view(buffer, state, command),
            (
                Self::Buffer(_),
                ContentInput::View {
                    state: ContentViewState::StatusBar,
                    ..
                },
            )
            | (
                Self::StatusBar(_),
                ContentInput::View {
                    state: ContentViewState::Buffer(_),
                    ..
                },
            ) => {
                panic!("content/view state mismatch")
            }
            (
                Self::StatusBar(_),
                ContentInput::View {
                    state: ContentViewState::StatusBar,
                    ..
                },
            ) => ContentResult::NotHandled,
            (Self::Buffer(buffer), ContentInput::Command(ContentCommand::Save)) => {
                let transaction_was_active = buffer.transaction_active();
                buffer
                    .commit_transaction()
                    .expect("active text transaction must be valid");
                if transaction_was_active {
                    buffer.begin_transaction();
                }
                match buffer.path().cloned() {
                    Some(path) => ContentResult::Handled(
                        ContentEffect::Save(SaveSnapshot {
                            path,
                            bytes: buffer.slice().to_string(),
                            revision: buffer.revision(),
                            state: buffer.state_id(),
                        })
                        .into(),
                    ),
                    None => {
                        let changed = buffer.status() != StatusMessage::SaveFailed;
                        buffer.set_status(StatusMessage::SaveFailed);
                        ContentResult::Handled(ContentOutcome::new(
                            ContentEffect::None,
                            changed,
                            false,
                        ))
                    }
                }
            }
            (
                Self::Buffer(buffer),
                ContentInput::Event(ContentEvent::SaveFinished { state, result }),
            ) => {
                let before_modified = buffer.modified();
                let before_status = buffer.status();
                match result {
                    Ok(()) => {
                        if buffer.mark_saved(state) {
                            buffer.set_status(StatusMessage::Saved);
                        }
                    }
                    Err(_) => buffer.set_status(StatusMessage::SaveFailed),
                }
                ContentResult::Handled(ContentOutcome::new(
                    ContentEffect::None,
                    buffer.modified() != before_modified || buffer.status() != before_status,
                    false,
                ))
            }
            (Self::Buffer(_), ContentInput::Command(_)) | (Self::StatusBar(_), _) => {
                ContentResult::NotHandled
            }
        }
    }
}

fn execute_buffer_view(
    buffer: &mut Buffer,
    state: &mut crate::core::content_view_state::BufferViewState,
    command: ContentCommand,
) -> ContentResult {
    if let ContentCommand::Sequence(commands) = command {
        let mut combined = ContentOutcome::new(ContentEffect::None, false, false);
        for command in commands.into_commands() {
            match execute_buffer_view(buffer, state, command) {
                ContentResult::Handled(outcome) => combined.merge(outcome),
                ContentResult::NotHandled => {
                    unreachable!("validated content sequence contains only view-state commands")
                }
            }
        }
        return ContentResult::Handled(combined);
    }

    let content_revision = buffer.revision();
    let selections = state.selections().clone();
    match command {
        ContentCommand::Edit(command) => apply_edit(command, buffer, state.selections_mut()),
        ContentCommand::Transaction(command) => match command {
            TransactionCommand::Begin => TransactionalContent::begin_transaction(buffer),
            TransactionCommand::Commit => {
                TransactionalContent::commit_transaction(buffer)
                    .expect("active text transaction must be valid");
            }
            TransactionCommand::Rollback => {
                TransactionalContent::rollback_transaction(buffer)
                    .expect("active text transaction must be valid");
                if let Some(change) = TransactionalContent::take_change(buffer) {
                    buffer.transform_selections(state.selections_mut(), &change);
                    return ContentResult::Handled(
                        ContentOutcome::new(
                            ContentEffect::None,
                            buffer.revision() != content_revision,
                            state.selections() != &selections,
                        )
                        .with_change(Some(change)),
                    );
                }
            }
        },
        ContentCommand::Undo | ContentCommand::Redo => {
            let changed = if matches!(command, ContentCommand::Undo) {
                TransactionalContent::undo(buffer)
            } else {
                TransactionalContent::redo(buffer)
            }
            .expect("text history transaction must be valid");
            if changed {
                let change = TransactionalContent::take_change(buffer)
                    .expect("history change accompanies a successful traversal");
                buffer.transform_selections(state.selections_mut(), &change);
                return ContentResult::Handled(
                    ContentOutcome::new(
                        ContentEffect::None,
                        true,
                        state.selections() != &selections,
                    )
                    .with_change(Some(change)),
                );
            }
        }
        ContentCommand::Save | ContentCommand::Sequence(_) => {
            return ContentResult::NotHandled;
        }
    }

    let change = TransactionalContent::take_change(buffer);
    ContentResult::Handled(
        ContentOutcome::new(
            ContentEffect::None,
            buffer.revision() != content_revision,
            state.selections() != &selections,
        )
        .with_change(change),
    )
}

impl From<ContentEffect> for ContentOutcome {
    fn from(effect: ContentEffect) -> Self {
        Self::new(effect, false, false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::command::{ContentCommand, EditCommand, TransactionCommand};
    use crate::protocol::ids::ContentId;

    #[test]
    fn buffer_creates_text_view_state_with_vim_default() {
        let content = Content::Buffer(Buffer::new());
        assert!(matches!(
            content.create_view_state(),
            ContentViewState::Buffer(_)
        ));
        assert_eq!(content.default_mode(), Some(ModeName::new("vim")));
    }

    #[test]
    #[should_panic(expected = "content/view state mismatch")]
    fn mismatched_view_state_is_an_internal_error() {
        let mut content = Content::Buffer(Buffer::new());
        let mut state = ContentViewState::StatusBar;

        content.execute(ContentInput::View {
            command: ContentCommand::Edit(EditCommand::InsertText("x".to_string())),
            state: &mut state,
        });
    }

    #[test]
    fn status_bar_creates_stateless_view_without_mode() {
        let content = Content::StatusBar(StatusBar::new(ContentId(0)));
        assert!(matches!(
            content.create_view_state(),
            ContentViewState::StatusBar
        ));
        assert_eq!(content.default_mode(), None);
    }

    #[test]
    fn contents_explicitly_report_command_support() {
        let command = ContentCommand::Edit(EditCommand::MoveLeftBy(1));
        let mut buffer = Content::Buffer(Buffer::new());
        let mut buffer_state = buffer.create_view_state();
        assert_eq!(
            buffer.execute(ContentInput::View {
                command: command.clone(),
                state: &mut buffer_state,
            }),
            ContentResult::Handled(ContentOutcome::new(ContentEffect::None, false, false))
        );

        let mut status = Content::StatusBar(StatusBar::new(ContentId(0)));
        let mut status_state = status.create_view_state();
        assert_eq!(
            status.execute(ContentInput::View {
                command,
                state: &mut status_state,
            }),
            ContentResult::NotHandled
        );
    }

    #[test]
    fn save_checkpoints_an_insert_transaction_and_reopens_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("checkpoint.txt");
        std::fs::write(&path, "").unwrap();
        let mut buffer = Buffer::new();
        buffer.open_path(path.to_str().unwrap()).unwrap();
        let mut content = Content::Buffer(buffer);
        let mut state = content.create_view_state();

        for command in [
            ContentCommand::Transaction(TransactionCommand::Begin),
            ContentCommand::Edit(EditCommand::InsertText("a".to_string())),
        ] {
            assert!(matches!(
                content.execute(ContentInput::View {
                    command,
                    state: &mut state,
                }),
                ContentResult::Handled(_)
            ));
        }
        assert!(matches!(
            content.execute(ContentInput::Command(ContentCommand::Save)),
            ContentResult::Handled(ContentOutcome {
                effect: ContentEffect::Save(_),
                ..
            })
        ));
        let Content::Buffer(buffer) = &content else {
            unreachable!()
        };
        assert!(buffer.transaction_active());

        for command in [
            ContentCommand::Edit(EditCommand::InsertText("b".to_string())),
            ContentCommand::Transaction(TransactionCommand::Commit),
            ContentCommand::Undo,
        ] {
            content.execute(ContentInput::View {
                command,
                state: &mut state,
            });
        }
        let Content::Buffer(buffer) = &content else {
            unreachable!()
        };
        assert_eq!(buffer.slice().to_string(), "a");
    }
}
