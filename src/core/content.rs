use std::path::PathBuf;

use crate::core::buffer::Buffer;
use crate::core::command::{Command, ContentCommand};
use crate::core::edit::apply_edit;
use crate::core::keymap::{KeyBinding, Keymap};
use crate::core::status_bar::StatusBar;
use crate::protocol::key_event::KeyEvent;
use crate::protocol::selection::Selections;
use crate::protocol::status::StatusMessage;

pub enum ContentInput<'a> {
    Command(ContentCommand),
    WithSelections {
        command: ContentCommand,
        selections: &'a mut Selections,
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

    pub fn resolve_key(&self, key: KeyEvent) -> Option<Command> {
        match self {
            Self::Buffer(buffer) => buffer.resolve_key(key),
            Self::StatusBar(_) => match self.keymap().lookup(key) {
                Some(KeyBinding::Command(command)) => Some(command.clone()),
                Some(KeyBinding::Prefix(_)) | None => None,
            },
        }
    }

    pub fn execute(&mut self, input: ContentInput<'_>) -> ContentEffect {
        let Self::Buffer(buffer) = self else {
            return ContentEffect::None;
        };

        match input {
            ContentInput::WithSelections {
                command: ContentCommand::Edit(command),
                selections,
            } => {
                apply_edit(command, buffer, selections);
                ContentEffect::None
            }
            ContentInput::Command(ContentCommand::Mode { mode, action }) => {
                buffer.handle_mode_command(mode, action);
                ContentEffect::None
            }
            ContentInput::Command(ContentCommand::Save) => match buffer.path().cloned() {
                Some(path) => ContentEffect::Save(SaveSnapshot {
                    path,
                    bytes: buffer.slice().to_string(),
                }),
                None => {
                    buffer.set_status(StatusMessage::SaveFailed);
                    ContentEffect::None
                }
            },
            ContentInput::Event(ContentEvent::SaveFinished(result)) => {
                match result {
                    Ok(()) => {
                        buffer.mark_saved();
                        buffer.set_status(StatusMessage::Saved);
                    }
                    Err(_) => buffer.set_status(StatusMessage::SaveFailed),
                }
                ContentEffect::None
            }
            ContentInput::Command(ContentCommand::Edit(_))
            | ContentInput::WithSelections {
                command: ContentCommand::Save | ContentCommand::Mode { .. },
                ..
            } => ContentEffect::None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::command::{Command, ContentCommand, EditCommand};
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

    // Cursors 测试已移至 protocol::selection。本模块无剩余测试。
}
