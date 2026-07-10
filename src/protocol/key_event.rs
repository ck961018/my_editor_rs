use crossterm::event::{
    KeyCode as CrosstermCode, KeyEvent as CrosstermKey, KeyModifiers as CrosstermModifiers,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct KeyModifiers {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

impl KeyModifiers {
    pub fn none() -> Self {
        Self::default()
    }
    pub fn ctrl() -> Self {
        Self {
            ctrl: true,
            alt: false,
            shift: false,
        }
    }
    #[allow(dead_code)] // 通用修饰键模型 API（spec §10：Ctrl+Alt / Ctrl+Shift 等未来键位预留）
    pub fn alt() -> Self {
        Self {
            ctrl: false,
            alt: true,
            shift: false,
        }
    }
    pub fn shift() -> Self {
        Self {
            ctrl: false,
            alt: false,
            shift: true,
        }
    }
    #[allow(dead_code)] // 通用修饰键模型 API（spec §10：Ctrl+Shift+Arrow 等未来键位预留）
    pub fn ctrl_shift() -> Self {
        Self {
            ctrl: true,
            alt: false,
            shift: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ArrowKey {
    Up,
    Down,
    Left,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyCode {
    Char(char),
    Arrow(ArrowKey),
    Backspace,
    Enter,
    Escape,
    Function(u8),
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeyEvent {
    pub code: KeyCode,
    pub modifiers: KeyModifiers,
}

impl KeyEvent {
    pub fn plain(code: KeyCode) -> Self {
        Self {
            code,
            modifiers: KeyModifiers::none(),
        }
    }
    // 以下两个构造器当前仅测试使用（binary crate 测试用方法仍触发 dead_code），
    // 保留为通用修饰键模型 API surface（spec §8）。
    #[allow(dead_code)]
    pub fn char(c: char) -> Self {
        Self::plain(KeyCode::Char(c))
    }
    pub fn ctrl(c: char) -> Self {
        Self::modified(KeyCode::Char(c.to_ascii_lowercase()), KeyModifiers::ctrl())
    }
    pub fn arrow(arrow: ArrowKey) -> Self {
        Self::plain(KeyCode::Arrow(arrow))
    }
    pub fn shift_arrow(arrow: ArrowKey) -> Self {
        Self::modified(KeyCode::Arrow(arrow), KeyModifiers::shift())
    }
    pub fn modified(code: KeyCode, modifiers: KeyModifiers) -> Self {
        Self { code, modifiers }
    }
    #[allow(dead_code)] // 同上：仅测试使用，保留 API surface
    pub fn unknown() -> Self {
        Self::plain(KeyCode::Unknown)
    }
    pub fn is_plain_char(&self) -> Option<char> {
        if self.modifiers == KeyModifiers::none() {
            if let KeyCode::Char(c) = self.code {
                return Some(c);
            }
        }
        None
    }
}

fn translate_modifiers(mods: CrosstermModifiers) -> KeyModifiers {
    KeyModifiers {
        ctrl: mods.contains(CrosstermModifiers::CONTROL),
        alt: mods.contains(CrosstermModifiers::ALT),
        shift: mods.contains(CrosstermModifiers::SHIFT),
    }
}

pub fn translate_key(k: CrosstermKey) -> KeyEvent {
    let modifiers = translate_modifiers(k.modifiers);
    match k.code {
        CrosstermCode::Char(c) if c.is_ascii_graphic() || c == ' ' => {
            let ch = if modifiers.ctrl {
                c.to_ascii_lowercase()
            } else {
                c
            };
            KeyEvent::modified(KeyCode::Char(ch), modifiers)
        }
        CrosstermCode::Backspace => KeyEvent::modified(KeyCode::Backspace, modifiers),
        CrosstermCode::Enter => KeyEvent::modified(KeyCode::Enter, modifiers),
        CrosstermCode::Esc => KeyEvent::modified(KeyCode::Escape, modifiers),
        CrosstermCode::Left => KeyEvent::modified(KeyCode::Arrow(ArrowKey::Left), modifiers),
        CrosstermCode::Right => KeyEvent::modified(KeyCode::Arrow(ArrowKey::Right), modifiers),
        CrosstermCode::Up => KeyEvent::modified(KeyCode::Arrow(ArrowKey::Up), modifiers),
        CrosstermCode::Down => KeyEvent::modified(KeyCode::Arrow(ArrowKey::Down), modifiers),
        CrosstermCode::F(n) => KeyEvent::modified(KeyCode::Function(n), modifiers),
        _ => KeyEvent::modified(KeyCode::Unknown, modifiers),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: CrosstermCode, mods: CrosstermModifiers) -> CrosstermKey {
        CrosstermKey::new(code, mods)
    }

    #[test]
    fn printable_ascii_becomes_char() {
        assert_eq!(
            translate_key(key(CrosstermCode::Char('a'), CrosstermModifiers::empty())),
            KeyEvent::char('a')
        );
        assert_eq!(
            translate_key(key(CrosstermCode::Char(' '), CrosstermModifiers::empty())),
            KeyEvent::char(' ')
        );
        assert_eq!(
            translate_key(key(CrosstermCode::Char('Z'), CrosstermModifiers::empty())),
            KeyEvent::char('Z')
        );
    }

    #[test]
    fn ctrl_ascii_chars_keep_ctrl_modifier() {
        assert_eq!(
            translate_key(key(CrosstermCode::Char('q'), CrosstermModifiers::CONTROL)),
            KeyEvent::ctrl('q')
        );
        assert_eq!(
            translate_key(key(CrosstermCode::Char('S'), CrosstermModifiers::CONTROL)),
            KeyEvent::ctrl('s')
        );
        assert_eq!(
            translate_key(key(CrosstermCode::Char('x'), CrosstermModifiers::CONTROL)),
            KeyEvent::ctrl('x')
        );
    }

    #[test]
    fn ctrl_arrow_and_function_keep_ctrl_modifier() {
        assert_eq!(
            translate_key(key(CrosstermCode::Left, CrosstermModifiers::CONTROL)),
            KeyEvent::modified(KeyCode::Arrow(ArrowKey::Left), KeyModifiers::ctrl())
        );
        assert_eq!(
            translate_key(key(CrosstermCode::F(1), CrosstermModifiers::CONTROL)),
            KeyEvent::modified(KeyCode::Function(1), KeyModifiers::ctrl())
        );
    }

    #[test]
    fn special_keys_map() {
        assert_eq!(
            translate_key(key(CrosstermCode::Backspace, CrosstermModifiers::empty())),
            KeyEvent::plain(KeyCode::Backspace)
        );
        assert_eq!(
            translate_key(key(CrosstermCode::Enter, CrosstermModifiers::empty())),
            KeyEvent::plain(KeyCode::Enter)
        );
        assert_eq!(
            translate_key(key(CrosstermCode::Esc, CrosstermModifiers::empty())),
            KeyEvent::plain(KeyCode::Escape)
        );
    }

    #[test]
    fn arrows_map() {
        assert_eq!(
            translate_key(key(CrosstermCode::Up, CrosstermModifiers::empty())),
            KeyEvent::arrow(ArrowKey::Up)
        );
        assert_eq!(
            translate_key(key(CrosstermCode::Down, CrosstermModifiers::empty())),
            KeyEvent::arrow(ArrowKey::Down)
        );
        assert_eq!(
            translate_key(key(CrosstermCode::Left, CrosstermModifiers::empty())),
            KeyEvent::arrow(ArrowKey::Left)
        );
        assert_eq!(
            translate_key(key(CrosstermCode::Right, CrosstermModifiers::empty())),
            KeyEvent::arrow(ArrowKey::Right)
        );
    }

    #[test]
    fn function_key_keeps_function_code() {
        assert_eq!(
            translate_key(key(CrosstermCode::F(1), CrosstermModifiers::empty())),
            KeyEvent::modified(KeyCode::Function(1), KeyModifiers::none())
        );
    }

    #[test]
    fn shift_arrow_becomes_shift_variant() {
        assert_eq!(
            translate_key(key(CrosstermCode::Left, CrosstermModifiers::SHIFT)),
            KeyEvent::shift_arrow(ArrowKey::Left)
        );
        assert_eq!(
            translate_key(key(CrosstermCode::Right, CrosstermModifiers::SHIFT)),
            KeyEvent::shift_arrow(ArrowKey::Right)
        );
        assert_eq!(
            translate_key(key(CrosstermCode::Up, CrosstermModifiers::SHIFT)),
            KeyEvent::shift_arrow(ArrowKey::Up)
        );
        assert_eq!(
            translate_key(key(CrosstermCode::Down, CrosstermModifiers::SHIFT)),
            KeyEvent::shift_arrow(ArrowKey::Down)
        );
    }

    #[test]
    fn shift_char_and_enter_keep_shift_modifier() {
        // shift+char 保留 shift（Char arm 不再落 Unknown）
        assert_eq!(
            translate_key(key(CrosstermCode::Char('a'), CrosstermModifiers::SHIFT)),
            KeyEvent::modified(KeyCode::Char('a'), KeyModifiers::shift())
        );
        // shift+enter 保留 shift
        assert_eq!(
            translate_key(key(CrosstermCode::Enter, CrosstermModifiers::SHIFT)),
            KeyEvent::modified(KeyCode::Enter, KeyModifiers::shift())
        );
    }

    #[test]
    fn arrow_without_shift_unchanged() {
        // 回归：无 shift 时方向键仍为 Arrow（不被 shift 分流误伤）
        assert_eq!(
            translate_key(key(CrosstermCode::Left, CrosstermModifiers::empty())),
            KeyEvent::arrow(ArrowKey::Left)
        );
        assert_eq!(
            translate_key(key(CrosstermCode::Down, CrosstermModifiers::empty())),
            KeyEvent::arrow(ArrowKey::Down)
        );
    }
}
