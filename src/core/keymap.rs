use std::collections::HashMap;

use crate::core::command::{Command, ContentCommand, TextCommand};
use crate::protocol::key_event::KeyEvent;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeyBinding {
    Command(Command),
    #[allow(dead_code)] // 仅测试构造前缀链；Dispatcher 读但生产 keymap 不绑前缀
    Prefix(Keymap),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Keymap {
    bindings: HashMap<KeyEvent, KeyBinding>,
}

impl Keymap {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn lookup(&self, key: KeyEvent) -> Option<&KeyBinding> {
        self.bindings.get(&key)
    }
    pub fn bind(&mut self, key: KeyEvent, command: Command) {
        self.bindings.insert(key, KeyBinding::Command(command));
    }
    pub fn bind_text(&mut self, key: KeyEvent, command: TextCommand) {
        self.bind(
            key,
            Command::Content(ContentCommand::Text(command)),
        );
    }
    #[allow(dead_code)] // 测试用：生产 keymap 不绑前缀，前缀链仅 dispatcher 单测构造
    pub fn bind_prefix(&mut self, key: KeyEvent, sub: Keymap) {
        self.bindings.insert(key, KeyBinding::Prefix(sub));
    }
    #[allow(dead_code)] // 预留：v0.2 无 unbind 键路径
    pub fn unbind(&mut self, key: KeyEvent) {
        self.bindings.remove(&key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::command::{Command, ContentCommand, TextCommand};
    use crate::protocol::key_event::{ArrowKey, KeyCode};

    #[test]
    fn bind_and_lookup_command() {
        let mut km = Keymap::new();
        km.bind_text(
            KeyEvent::plain(KeyCode::Enter),
            TextCommand::InsertText("\n".to_string()),
        );
        let binding = km.lookup(KeyEvent::plain(KeyCode::Enter)).unwrap();
        assert_eq!(
            binding,
            &KeyBinding::Command(Command::Content(ContentCommand::Text(
                TextCommand::InsertText("\n".to_string())
            )))
        );
    }

    #[test]
    fn lookup_missing_is_none() {
        let km = Keymap::new();
        assert!(km.lookup(KeyEvent::plain(KeyCode::Enter)).is_none());
    }

    #[test]
    fn unbind_removes() {
        let mut km = Keymap::new();
        km.bind_text(KeyEvent::plain(KeyCode::Backspace), TextCommand::Delete(-1));
        km.unbind(KeyEvent::plain(KeyCode::Backspace));
        assert!(km.lookup(KeyEvent::plain(KeyCode::Backspace)).is_none());
    }

    #[test]
    fn bind_prefix_nested() {
        let mut sub = Keymap::new();
        sub.bind(KeyEvent::char('s'), Command::Content(ContentCommand::Save));
        let mut km = Keymap::new();
        km.bind_prefix(KeyEvent::char('x'), sub);
        match km.lookup(KeyEvent::char('x')).unwrap() {
            KeyBinding::Prefix(sub_km) => {
                assert!(matches!(
                    sub_km.lookup(KeyEvent::char('s')),
                    Some(KeyBinding::Command(Command::Content(ContentCommand::Save)))
                ));
            }
            _ => panic!("expected Prefix"),
        }
    }

    #[test]
    fn keymap_clone_eq() {
        let mut km = Keymap::new();
        km.bind_text(KeyEvent::arrow(ArrowKey::Left), TextCommand::MoveLeftBy(1));
        let cloned = km.clone();
        assert_eq!(km, cloned);
    }
}
