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
