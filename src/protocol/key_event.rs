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
    #[expect(
        dead_code,
        reason = "Alt-only bindings are supported by the neutral key protocol"
    )]
    pub fn alt() -> Self {
        Self {
            ctrl: false,
            alt: true,
            shift: false,
        }
    }
    #[cfg(test)]
    pub fn shift() -> Self {
        Self {
            ctrl: false,
            alt: false,
            shift: true,
        }
    }
    #[expect(
        dead_code,
        reason = "combined modifiers are supported by the neutral key protocol"
    )]
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

impl AsRef<[KeyEvent]> for KeyEvent {
    fn as_ref(&self) -> &[KeyEvent] {
        std::slice::from_ref(self)
    }
}

impl KeyEvent {
    pub fn plain(code: KeyCode) -> Self {
        Self {
            code,
            modifiers: KeyModifiers::none(),
        }
    }
    pub fn char(c: char) -> Self {
        Self::plain(KeyCode::Char(c))
    }
    pub fn ctrl(c: char) -> Self {
        Self::modified(KeyCode::Char(c.to_ascii_lowercase()), KeyModifiers::ctrl())
    }
    #[cfg(test)]
    pub fn arrow(arrow: ArrowKey) -> Self {
        Self::plain(KeyCode::Arrow(arrow))
    }
    #[cfg(test)]
    pub fn shift_arrow(arrow: ArrowKey) -> Self {
        Self::modified(KeyCode::Arrow(arrow), KeyModifiers::shift())
    }
    pub fn modified(code: KeyCode, modifiers: KeyModifiers) -> Self {
        Self { code, modifiers }
    }
    #[expect(
        dead_code,
        reason = "unknown keys are representable at the terminal protocol boundary"
    )]
    pub fn unknown() -> Self {
        Self::plain(KeyCode::Unknown)
    }
}
