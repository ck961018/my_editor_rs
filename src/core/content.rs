use std::path::PathBuf;

use crate::core::buffer::Buffer;
use crate::core::command::ContentCommand;
use crate::core::content_view_state::ContentViewState;
use crate::core::edit::apply_edit;
use crate::core::mode::ModeName;
use crate::core::status_bar::StatusBar;
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
        revision: u64,
        result: std::io::Result<()>,
    },
}

#[derive(Debug, PartialEq, Eq)]
pub struct SaveSnapshot {
    pub path: PathBuf,
    pub bytes: String,
    pub revision: u64,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ContentEffect {
    None,
    Save(SaveSnapshot),
}

#[derive(Debug, PartialEq, Eq)]
pub struct ContentOutcome {
    pub effect: ContentEffect,
    pub content_changed: bool,
    pub view_changed: bool,
}

impl ContentOutcome {
    fn new(effect: ContentEffect, content_changed: bool, view_changed: bool) -> Self {
        Self {
            effect,
            content_changed,
            view_changed,
        }
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

    pub fn reconcile_view_state(&self, state: &mut ContentViewState) -> bool {
        match (self, state) {
            (Self::Buffer(buffer), ContentViewState::Buffer(state)) => {
                buffer.reconcile_selections(state.selections_mut())
            }
            (Self::StatusBar(_), ContentViewState::StatusBar) => false,
            _ => panic!("content/view state mismatch"),
        }
    }

    pub fn execute(&mut self, input: ContentInput<'_>) -> ContentResult {
        match (self, input) {
            (
                Self::Buffer(buffer),
                ContentInput::View {
                    command: ContentCommand::Edit(command),
                    state: ContentViewState::Buffer(state),
                },
            ) => {
                let content_revision = buffer.revision();
                let selections = state.selections().clone();
                apply_edit(command, buffer, state.selections_mut());
                ContentResult::Handled(ContentOutcome::new(
                    ContentEffect::None,
                    buffer.revision() != content_revision,
                    state.selections() != &selections,
                ))
            }
            (
                Self::Buffer(_),
                ContentInput::View {
                    command: ContentCommand::Mode { .. },
                    state: ContentViewState::Buffer(_),
                },
            ) => panic!("mode commands must be executed by the view mode instance"),
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
                match buffer.path().cloned() {
                    Some(path) => ContentResult::Handled(
                        ContentEffect::Save(SaveSnapshot {
                            path,
                            bytes: buffer.slice().to_string(),
                            revision: buffer.revision(),
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
                ContentInput::Event(ContentEvent::SaveFinished { revision, result }),
            ) => {
                let before_modified = buffer.modified();
                let before_status = buffer.status();
                match result {
                    Ok(()) => {
                        if buffer.mark_saved(revision) {
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
            (
                Self::Buffer(_),
                ContentInput::View {
                    state: ContentViewState::Buffer(_),
                    ..
                },
            )
            | (Self::Buffer(_), ContentInput::Command(_))
            | (Self::StatusBar(_), _) => ContentResult::NotHandled,
        }
    }
}

impl From<ContentEffect> for ContentOutcome {
    fn from(effect: ContentEffect) -> Self {
        Self::new(effect, false, false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::command::{ContentCommand, EditCommand};
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
}
