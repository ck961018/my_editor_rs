use crate::core::buffer::Buffer;
use crate::core::command::Command;
use crate::core::keymap::Keymap;
use crate::core::mode::{ModeActionId, ModeId};
use crate::core::status_bar::StatusBar;
use crate::protocol::ids::ContentId;
use crate::protocol::key_event::KeyEvent;

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
