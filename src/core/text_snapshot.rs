use ropey::Rope;

#[cfg(test)]
use crate::core::transaction::{TextChangeSet, TextTransactionError};

/// A cheaply cloned, immutable text snapshot for background analyzers.
#[derive(Clone)]
pub struct TextSnapshot {
    rope: Rope,
}

impl TextSnapshot {
    pub(crate) fn new(rope: &Rope) -> Self {
        Self { rope: rope.clone() }
    }

    pub fn len_chars(&self) -> usize {
        self.rope.len_chars()
    }

    /// Converts a zero-based UTF-16 line/character position to a character offset.
    pub fn utf16_position_to_char(&self, line: usize, character: usize) -> Option<usize> {
        if line >= self.rope.len_lines() {
            return None;
        }
        let line_start = self.rope.line_to_char(line);
        let mut utf16_offset = 0;
        let mut char_offset = line_start;
        let mut characters = self.rope.line(line).chars().peekable();
        while let Some(value) = characters.next() {
            if value == '\n' || value == '\r' && characters.peek() == Some(&'\n') {
                break;
            }
            if utf16_offset == character {
                return Some(char_offset);
            }
            let width = value.len_utf16();
            if character < utf16_offset + width {
                return None;
            }
            utf16_offset += width;
            char_offset += 1;
        }
        (utf16_offset == character).then_some(char_offset)
    }

    #[cfg(test)]
    pub fn apply(&self, change: &TextChangeSet) -> Result<Self, TextTransactionError> {
        let mut rope = self.rope.clone();
        change.apply(&mut rope)?;
        Ok(Self { rope })
    }

    pub fn to_owned_string(&self) -> String {
        self.rope.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_utf16_positions_without_splitting_surrogate_pairs() {
        let snapshot = TextSnapshot::new(&Rope::from_str("a😀b\n中"));

        assert_eq!(snapshot.utf16_position_to_char(0, 0), Some(0));
        assert_eq!(snapshot.utf16_position_to_char(0, 1), Some(1));
        assert_eq!(snapshot.utf16_position_to_char(0, 2), None);
        assert_eq!(snapshot.utf16_position_to_char(0, 3), Some(2));
        assert_eq!(snapshot.utf16_position_to_char(1, 1), Some(5));
        assert_eq!(snapshot.utf16_position_to_char(2, 0), None);
    }
}
