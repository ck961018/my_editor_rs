use crate::core::command::Command;
use crate::core::keymap::Keymap;
use crate::protocol::key_event::KeyEvent;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ModeId(&'static str);

impl ModeId {
    pub const fn new(value: &'static str) -> Self {
        Self(value)
    }

    #[allow(dead_code)] // 预留：spec §10 未来 UI 输出（ModeActionId::as_str 已在 vim runtime 用）
    pub const fn as_str(self) -> &'static str {
        self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ModeActionId(&'static str);

impl ModeActionId {
    pub const fn new(value: &'static str) -> Self {
        Self(value)
    }

    pub const fn as_str(self) -> &'static str {
        self.0
    }
}

/// 通用 Mode 契约：自持 keymap + typing 兜底 + 模式命令处理。
/// 第一版仅 Buffer 持 mode runtime（BufferModes），StatusBar 无 mode runtime。
pub trait Mode {
    fn id(&self) -> ModeId;
    #[allow(dead_code)] // 预留：spec §10 状态栏未来显示模式名（NORMAL/INSERT）
    fn label(&self) -> &str;
    fn keymap(&self) -> &Keymap;
    fn typing(&self, key: KeyEvent) -> Option<Command>;
    fn handle_mode_command(&mut self, action: ModeActionId);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_ids_are_copyable_values() {
        let id = ModeId::new("vim");
        assert_eq!(id.as_str(), "vim");
        assert_eq!(id, ModeId::new("vim"));
    }

    #[test]
    fn mode_action_ids_are_copyable_values() {
        let action = ModeActionId::new("enter-insert");
        assert_eq!(action.as_str(), "enter-insert");
        assert_eq!(action, ModeActionId::new("enter-insert"));
    }
}
