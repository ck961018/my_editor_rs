use crossterm::event::{
    KeyCode as CrosstermCode, KeyEvent as CrosstermKey, KeyModifiers as CrosstermModifiers,
};

use crate::protocol::key_event::{ArrowKey, KeyCode, KeyEvent, KeyModifiers};

fn translate_modifiers(modifiers: CrosstermModifiers) -> KeyModifiers {
    KeyModifiers {
        ctrl: modifiers.contains(CrosstermModifiers::CONTROL),
        alt: modifiers.contains(CrosstermModifiers::ALT),
        shift: modifiers.contains(CrosstermModifiers::SHIFT),
    }
}

pub(crate) fn translate_key(key: CrosstermKey) -> KeyEvent {
    let modifiers = translate_modifiers(key.modifiers);
    let code = match key.code {
        CrosstermCode::Char(character) if character.is_ascii_graphic() || character == ' ' => {
            let character = if modifiers.ctrl {
                character.to_ascii_lowercase()
            } else {
                character
            };
            KeyCode::Char(character)
        }
        CrosstermCode::Backspace => KeyCode::Backspace,
        CrosstermCode::Enter => KeyCode::Enter,
        CrosstermCode::Esc => KeyCode::Escape,
        CrosstermCode::Left => KeyCode::Arrow(ArrowKey::Left),
        CrosstermCode::Right => KeyCode::Arrow(ArrowKey::Right),
        CrosstermCode::Up => KeyCode::Arrow(ArrowKey::Up),
        CrosstermCode::Down => KeyCode::Arrow(ArrowKey::Down),
        CrosstermCode::F(number) => KeyCode::Function(number),
        _ => KeyCode::Unknown,
    };

    KeyEvent::modified(code, modifiers)
}

#[cfg(test)]
mod tests {
    use crossterm::event::{
        KeyCode as CrosstermCode, KeyEvent as CrosstermKey, KeyModifiers as CrosstermModifiers,
    };

    use crate::protocol::key_event::{ArrowKey, KeyCode, KeyEvent, KeyModifiers};

    fn key(code: CrosstermCode, modifiers: CrosstermModifiers) -> CrosstermKey {
        CrosstermKey::new(code, modifiers)
    }

    #[test]
    fn ctrl_uppercase_char_is_normalized_and_keeps_modifier() {
        assert_eq!(
            super::translate_key(key(CrosstermCode::Char('S'), CrosstermModifiers::CONTROL)),
            KeyEvent::ctrl('s')
        );
    }

    #[test]
    fn printable_ascii_becomes_char() {
        assert_eq!(
            super::translate_key(key(CrosstermCode::Char('a'), CrosstermModifiers::empty())),
            KeyEvent::char('a')
        );
        assert_eq!(
            super::translate_key(key(CrosstermCode::Char(' '), CrosstermModifiers::empty())),
            KeyEvent::char(' ')
        );
        assert_eq!(
            super::translate_key(key(CrosstermCode::Char('Z'), CrosstermModifiers::empty())),
            KeyEvent::char('Z')
        );
    }

    #[test]
    fn ctrl_ascii_chars_keep_ctrl_modifier() {
        assert_eq!(
            super::translate_key(key(CrosstermCode::Char('q'), CrosstermModifiers::CONTROL)),
            KeyEvent::ctrl('q')
        );
        assert_eq!(
            super::translate_key(key(CrosstermCode::Char('x'), CrosstermModifiers::CONTROL)),
            KeyEvent::ctrl('x')
        );
    }

    #[test]
    fn ctrl_arrow_and_function_keep_ctrl_modifier() {
        assert_eq!(
            super::translate_key(key(CrosstermCode::Left, CrosstermModifiers::CONTROL)),
            KeyEvent::modified(KeyCode::Arrow(ArrowKey::Left), KeyModifiers::ctrl())
        );
        assert_eq!(
            super::translate_key(key(CrosstermCode::F(1), CrosstermModifiers::CONTROL)),
            KeyEvent::modified(KeyCode::Function(1), KeyModifiers::ctrl())
        );
    }

    #[test]
    fn special_keys_map() {
        assert_eq!(
            super::translate_key(key(CrosstermCode::Backspace, CrosstermModifiers::empty())),
            KeyEvent::plain(KeyCode::Backspace)
        );
        assert_eq!(
            super::translate_key(key(CrosstermCode::Enter, CrosstermModifiers::empty())),
            KeyEvent::plain(KeyCode::Enter)
        );
        assert_eq!(
            super::translate_key(key(CrosstermCode::Esc, CrosstermModifiers::empty())),
            KeyEvent::plain(KeyCode::Escape)
        );
    }

    #[test]
    fn arrows_map() {
        assert_eq!(
            super::translate_key(key(CrosstermCode::Up, CrosstermModifiers::empty())),
            KeyEvent::arrow(ArrowKey::Up)
        );
        assert_eq!(
            super::translate_key(key(CrosstermCode::Down, CrosstermModifiers::empty())),
            KeyEvent::arrow(ArrowKey::Down)
        );
        assert_eq!(
            super::translate_key(key(CrosstermCode::Left, CrosstermModifiers::empty())),
            KeyEvent::arrow(ArrowKey::Left)
        );
        assert_eq!(
            super::translate_key(key(CrosstermCode::Right, CrosstermModifiers::empty())),
            KeyEvent::arrow(ArrowKey::Right)
        );
    }

    #[test]
    fn function_key_keeps_function_code() {
        assert_eq!(
            super::translate_key(key(CrosstermCode::F(1), CrosstermModifiers::empty())),
            KeyEvent::modified(KeyCode::Function(1), KeyModifiers::none())
        );
    }

    #[test]
    fn unknown_key_keeps_all_modifiers() {
        assert_eq!(
            super::translate_key(key(
                CrosstermCode::Tab,
                CrosstermModifiers::CONTROL | CrosstermModifiers::ALT | CrosstermModifiers::SHIFT,
            )),
            KeyEvent::modified(
                KeyCode::Unknown,
                KeyModifiers {
                    ctrl: true,
                    alt: true,
                    shift: true,
                },
            )
        );
    }

    #[test]
    fn shift_arrow_becomes_shift_variant() {
        assert_eq!(
            super::translate_key(key(CrosstermCode::Left, CrosstermModifiers::SHIFT)),
            KeyEvent::shift_arrow(ArrowKey::Left)
        );
        assert_eq!(
            super::translate_key(key(CrosstermCode::Right, CrosstermModifiers::SHIFT)),
            KeyEvent::shift_arrow(ArrowKey::Right)
        );
        assert_eq!(
            super::translate_key(key(CrosstermCode::Up, CrosstermModifiers::SHIFT)),
            KeyEvent::shift_arrow(ArrowKey::Up)
        );
        assert_eq!(
            super::translate_key(key(CrosstermCode::Down, CrosstermModifiers::SHIFT)),
            KeyEvent::shift_arrow(ArrowKey::Down)
        );
    }

    #[test]
    fn shift_char_and_enter_keep_shift_modifier() {
        assert_eq!(
            super::translate_key(key(CrosstermCode::Char('a'), CrosstermModifiers::SHIFT)),
            KeyEvent::modified(KeyCode::Char('a'), KeyModifiers::shift())
        );
        assert_eq!(
            super::translate_key(key(CrosstermCode::Enter, CrosstermModifiers::SHIFT)),
            KeyEvent::modified(KeyCode::Enter, KeyModifiers::shift())
        );
    }

    #[test]
    fn arrow_without_shift_unchanged() {
        assert_eq!(
            super::translate_key(key(CrosstermCode::Left, CrosstermModifiers::empty())),
            KeyEvent::arrow(ArrowKey::Left)
        );
        assert_eq!(
            super::translate_key(key(CrosstermCode::Down, CrosstermModifiers::empty())),
            KeyEvent::arrow(ArrowKey::Down)
        );
    }
}
