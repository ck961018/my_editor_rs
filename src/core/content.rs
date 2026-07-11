use std::path::PathBuf;

use crate::core::buffer::Buffer;
use crate::core::command::{Command, ContentCommand};
use crate::core::edit::apply_edit;
use crate::core::keymap::{KeyBinding, Keymap};
use crate::core::mode::{ModeActionId, ModeId};
use crate::core::status_bar::StatusBar;
use crate::protocol::ids::ContentId;
use crate::protocol::key_event::KeyEvent;
use crate::protocol::selection::Selections;
use crate::protocol::status::StatusMessage;

#[allow(dead_code)] // The app-layer migration consumes these inputs in Task 3.
pub enum ContentInput<'a> {
    Command(ContentCommand),
    WithSelections {
        command: ContentCommand,
        selections: &'a mut Selections,
    },
    Event(ContentEvent),
}

#[allow(dead_code)] // Save completion is delivered by the app layer in Task 3.
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

#[allow(dead_code)] // ContentStore becomes the app-layer source of content access in Task 3.
impl Content {
    pub fn keymap(&self) -> &Keymap {
        match self {
            Self::Buffer(buffer) => buffer.keymap(),
            Self::StatusBar(status_bar) => status_bar.keymap(),
        }
    }

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
                command: ContentCommand::Text(command),
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
            ContentInput::Command(ContentCommand::Text(_))
            | ContentInput::WithSelections {
                command: ContentCommand::Save | ContentCommand::Mode { .. },
                ..
            } => ContentEffect::None,
        }
    }
}

pub trait ContentLookup {
    fn get(&self, id: ContentId) -> Option<&dyn ContentHandler>;
}

/// content 多态契约：自持 keymap + 类型查询。仅分发契约（查表返回 Command），
/// 不含渲染——渲染由前端 pull ContentQuery 自治。
pub trait ContentHandler {
    fn keymap(&self) -> &Keymap;
    #[allow(dead_code)] // 测试用：生产路径只读 keymap
    fn keymap_mut(&mut self) -> &mut Keymap;
    /// 模式化按键解析：查 content 自持 keymap，命中 Command 返回之；
    /// 命中 Prefix 或未命中返回 None。Buffer 覆写为走 mode runtime。
    fn resolve_key(&self, key: KeyEvent) -> Option<Command> {
        match self.keymap().lookup(key) {
            Some(crate::core::keymap::KeyBinding::Command(command)) => Some(command.clone()),
            Some(crate::core::keymap::KeyBinding::Prefix(_)) | None => None,
        }
    }
    /// 模式命令分发：默认空操作。Buffer 覆写为转发到 mode runtime。
    fn handle_mode_command(&mut self, _mode: ModeId, _action: ModeActionId) {}
    fn buffer_mut(&mut self) -> Option<&mut Buffer> {
        None
    }
    /// 只读 Buffer 查询（ContentQuery impl 用）。
    fn as_buffer(&self) -> Option<&Buffer> {
        None
    }
    /// 只读 StatusBar 查询（ContentQuery impl 用）。
    fn as_status_bar(&self) -> Option<&StatusBar> {
        None
    }
}

#[cfg(test)]
mod tests {
    // Cursors 测试已移至 protocol::selection。本模块无剩余测试。
}
