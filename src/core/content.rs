use std::path::PathBuf;

use crate::core::buffer::Buffer;
use crate::core::command::{Command, ContentCommand};
use crate::core::content_runtime::{ContentRuntime, StatusBarRuntime};
use crate::core::edit::apply_edit;
use crate::core::keymap::{KeyBinding, Keymap};
use crate::core::status_bar::StatusBar;
use crate::protocol::key_event::KeyEvent;
use crate::protocol::selection::Selections;
use crate::protocol::status::StatusMessage;

pub enum ContentInput<'a> {
    Command(ContentCommand),
    View {
        command: ContentCommand,
        selections: &'a mut Selections,
        runtime: &'a mut ContentRuntime,
    },
    Event(ContentEvent),
}

pub enum ContentEvent {
    SaveFinished(std::io::Result<()>),
}

#[derive(Debug, PartialEq, Eq)]
pub struct SaveSnapshot {
    pub path: PathBuf,
    pub bytes: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ContentEffect {
    None,
    Save(SaveSnapshot),
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

    pub fn create_runtime(&self) -> ContentRuntime {
        match self {
            Self::Buffer(buffer) => ContentRuntime::Buffer(buffer.create_runtime()),
            Self::StatusBar(_) => ContentRuntime::StatusBar(StatusBarRuntime),
        }
    }

    pub fn resolve_key(&self, runtime: &ContentRuntime, key: KeyEvent) -> Option<Command> {
        match (self, runtime) {
            (Self::Buffer(buffer), ContentRuntime::Buffer(runtime)) => {
                buffer.resolve_key(runtime, key)
            }
            (Self::StatusBar(_), ContentRuntime::StatusBar(_)) => match self.keymap().lookup(key) {
                Some(KeyBinding::Command(command)) => Some(command.clone()),
                Some(KeyBinding::Prefix(_)) | None => None,
            },
            _ => panic!("content/runtime mismatch"),
        }
    }

    pub fn execute(&mut self, input: ContentInput<'_>) -> ContentEffect {
        match (self, input) {
            (
                Self::Buffer(buffer),
                ContentInput::View {
                    command: ContentCommand::Edit(command),
                    selections,
                    runtime: ContentRuntime::Buffer(_),
                },
            ) => {
                apply_edit(command, buffer, selections);
                ContentEffect::None
            }
            (
                Self::Buffer(buffer),
                ContentInput::View {
                    command: ContentCommand::Mode { mode, action },
                    runtime: ContentRuntime::Buffer(runtime),
                    ..
                },
            ) => {
                buffer.execute_mode(runtime, mode, action);
                ContentEffect::None
            }
            (
                Self::Buffer(_),
                ContentInput::View {
                    runtime: ContentRuntime::StatusBar(_),
                    ..
                },
            )
            | (
                Self::StatusBar(_),
                ContentInput::View {
                    runtime: ContentRuntime::Buffer(_),
                    ..
                },
            ) => {
                panic!("content/runtime mismatch")
            }
            (
                Self::StatusBar(_),
                ContentInput::View {
                    runtime: ContentRuntime::StatusBar(_),
                    ..
                },
            ) => ContentEffect::None,
            (Self::Buffer(buffer), ContentInput::Command(ContentCommand::Save)) => {
                match buffer.path().cloned() {
                    Some(path) => ContentEffect::Save(SaveSnapshot {
                        path,
                        bytes: buffer.slice().to_string(),
                    }),
                    None => {
                        buffer.set_status(StatusMessage::SaveFailed);
                        ContentEffect::None
                    }
                }
            }
            (Self::Buffer(buffer), ContentInput::Event(ContentEvent::SaveFinished(result))) => {
                match result {
                    Ok(()) => {
                        buffer.mark_saved();
                        buffer.set_status(StatusMessage::Saved);
                    }
                    Err(_) => buffer.set_status(StatusMessage::SaveFailed),
                }
                ContentEffect::None
            }
            (
                Self::Buffer(_),
                ContentInput::View {
                    runtime: ContentRuntime::Buffer(_),
                    ..
                },
            )
            | (Self::Buffer(_), ContentInput::Command(_))
            | (Self::StatusBar(_), _) => ContentEffect::None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::command::{Command, ContentCommand, EditCommand};
    use crate::core::content_runtime::{ContentRuntime, StatusBarRuntime};
    use crate::core::mode::{ModeActionId, ModeId};
    use crate::protocol::ids::ContentId;
    use crate::protocol::key_event::KeyEvent;
    use crate::protocol::selection::{CursorPos, Selection, Selections};

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
    fn buffer_creates_independent_content_runtimes() {
        let content = Content::Buffer(Buffer::new());
        let mut first = content.create_runtime();
        let second = content.create_runtime();
        let mut selections = Selections::single(Selection::collapsed(CursorPos::origin()));
        let mut content = content;

        content.execute(ContentInput::View {
            command: ContentCommand::Mode {
                mode: ModeId::new("vim"),
                action: ModeActionId::new("enter-insert"),
            },
            selections: &mut selections,
            runtime: &mut first,
        });

        assert!(content.resolve_key(&first, KeyEvent::char('a')).is_some());
        assert!(content.resolve_key(&second, KeyEvent::char('a')).is_none());
    }

    #[test]
    #[should_panic(expected = "content/runtime mismatch")]
    fn mismatched_view_runtime_is_an_internal_error() {
        let mut content = Content::Buffer(Buffer::new());
        let mut runtime = ContentRuntime::StatusBar(StatusBarRuntime);
        let mut selections = Selections::single(Selection::collapsed(CursorPos::origin()));

        content.execute(ContentInput::View {
            command: ContentCommand::Edit(EditCommand::InsertText("x".to_string())),
            selections: &mut selections,
            runtime: &mut runtime,
        });
    }

    #[test]
    fn status_bar_creates_a_status_bar_runtime() {
        let content = Content::StatusBar(StatusBar::new(ContentId(0)));
        assert!(matches!(
            content.create_runtime(),
            ContentRuntime::StatusBar(_)
        ));
    }
}
