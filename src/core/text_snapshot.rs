use ropey::Rope;

use crate::core::transaction::{TextChangeSet, TextTransactionError};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextBytePoint {
    pub row: usize,
    pub byte_col: usize,
}

/// A cheaply cloned, immutable text snapshot for background analyzers.
#[derive(Clone)]
pub struct TextSnapshot {
    rope: Rope,
}

impl TextSnapshot {
    pub(crate) fn new(rope: &Rope) -> Self {
        Self { rope: rope.clone() }
    }

    pub fn char_to_byte(&self, char_index: usize) -> usize {
        self.rope
            .char_to_byte(char_index.min(self.rope.len_chars()))
    }

    pub fn byte_to_char(&self, byte_index: usize) -> usize {
        self.rope
            .byte_to_char(byte_index.min(self.rope.len_bytes()))
    }

    pub fn byte_point_at_char(&self, char_index: usize) -> TextBytePoint {
        let char_index = char_index.min(self.rope.len_chars());
        let row = self.rope.char_to_line(char_index);
        let line_start = self.rope.line_to_char(row);
        TextBytePoint {
            row,
            byte_col: self.rope.char_to_byte(char_index) - self.rope.char_to_byte(line_start),
        }
    }

    pub fn row_to_byte(&self, row: usize) -> usize {
        if row >= self.rope.len_lines() {
            self.rope.len_bytes()
        } else {
            self.rope.line_to_byte(row)
        }
    }

    pub fn apply(&self, change: &TextChangeSet) -> Result<Self, TextTransactionError> {
        let mut rope = self.rope.clone();
        change.apply(&mut rope)?;
        Ok(Self { rope })
    }

    pub fn to_owned_string(&self) -> String {
        self.rope.to_string()
    }
}
