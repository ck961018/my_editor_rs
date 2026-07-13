use std::path::PathBuf;

use crate::core::buffer::Buffer;
use crate::core::command::ContentCommand;
use crate::core::content_view_state::ContentViewState;
use crate::core::edit::apply_edit;
use crate::core::keymap::Keymap;
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
pub enum ContentResult {
    Handled(ContentEffect),
    NotHandled,
}

pub enum Content {
    Buffer(Buffer),
    StatusBar(StatusBar),
}

impl Content {
    pub fn keymap(&self) -> &Keymap {
        match self {
            Self::Buffer(buffer) => buffer.keymap(),
            Self::StatusBar(status_bar) => status_bar.keymap(),
        }
    }

    #[allow(dead_code)] // Static Content API reserves keymap mutation for future bindings.
    pub fn keymap_mut(&mut self) -> &mut Keymap {
        match self {
            Self::Buffer(buffer) => buffer.keymap_mut(),
            Self::StatusBar(status_bar) => status_bar.keymap_mut(),
        }
    }

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

    pub fn execute(&mut self, input: ContentInput<'_>) -> ContentResult {
        match (self, input) {
            (
                Self::Buffer(buffer),
                ContentInput::View {
                    command: ContentCommand::Edit(command),
                    state: ContentViewState::Buffer(state),
                },
            ) => {
                apply_edit(command, buffer, state.selections_mut());
                ContentResult::Handled(ContentEffect::None)
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
                    Some(path) => ContentResult::Handled(ContentEffect::Save(SaveSnapshot {
                        path,
                        bytes: buffer.slice().to_string(),
                        revision: buffer.revision(),
                    })),
                    None => {
                        buffer.set_status(StatusMessage::SaveFailed);
                        ContentResult::Handled(ContentEffect::None)
                    }
                }
            }
            (
                Self::Buffer(buffer),
                ContentInput::Event(ContentEvent::SaveFinished { revision, result }),
            ) => {
                match result {
                    Ok(()) => {
                        if buffer.mark_saved(revision) {
                            buffer.set_status(StatusMessage::Saved);
                        }
                    }
                    Err(_) => buffer.set_status(StatusMessage::SaveFailed),
                }
                ContentResult::Handled(ContentEffect::None)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::command::{Command, ContentCommand, EditCommand};
    use crate::core::keymap::KeyBinding;
    use crate::protocol::ids::ContentId;
    use crate::protocol::key_event::KeyEvent;

    #[test]
    fn keymap_mut_updates_static_buffer_content_keymap() {
        let mut content = Content::Buffer(Buffer::new());
        let command = Command::Content(ContentCommand::Edit(EditCommand::CollapseSelections));

        content
            .keymap_mut()
            .bind(KeyEvent::char('x'), command.clone());

        assert_eq!(
            content.keymap().lookup(KeyEvent::char('x')),
            Some(&KeyBinding::Command(command))
        );
    }

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
            ContentResult::Handled(ContentEffect::None)
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
