use ropey::Rope;

use crate::core::grapheme::{boundary_at_or_after, boundary_at_or_before, previous_boundary};

pub(super) fn line_content_len(rope: &Rope, row: usize) -> usize {
    let line = rope.line(row);
    let len = line.len_chars();
    if len >= 2 && line.char(len - 2) == '\r' && line.char(len - 1) == '\n' {
        len - 2
    } else if len >= 1 && line.char(len - 1) == '\n' {
        len - 1
    } else {
        len
    }
}

pub(super) fn line_break_width_before(rope: &Rope, row: usize) -> usize {
    let line_start = rope.line_to_char(row);
    if line_start >= 2 && rope.char(line_start - 2) == '\r' && rope.char(line_start - 1) == '\n' {
        2
    } else {
        usize::from(line_start > 0)
    }
}

pub(super) fn backward_word_start(rope: &Rope, char_index: usize) -> usize {
    let mut start = boundary_at_or_after(rope, char_index);
    while start > 0 {
        let previous = previous_boundary(rope, start);
        if !rope.char(previous).is_whitespace() {
            break;
        }
        start = previous;
    }
    if start == 0 {
        return 0;
    }

    let previous = previous_boundary(rope, start);
    if !is_word_char(rope.char(previous)) {
        return previous;
    }
    start = previous;
    while start > 0 {
        let previous = previous_boundary(rope, start);
        if !is_word_char(rope.char(previous)) {
            break;
        }
        start = previous;
    }
    start
}

pub(super) fn first_non_blank_in_line(rope: &Rope, row: usize) -> usize {
    let line_start = rope.line_to_char(row);
    let line = rope.line(row);
    for (i, ch) in line.chars().enumerate() {
        if ch == '\n' {
            break;
        }
        if !ch.is_whitespace() {
            return boundary_at_or_before(rope, line_start + i);
        }
    }
    line_start
}

pub(super) fn line_end_char(rope: &Rope, row: usize) -> usize {
    let line_start = rope.line_to_char(row);
    let content_end = line_start + line_content_len(rope, row);
    if content_end == line_start {
        line_start
    } else {
        previous_boundary(rope, content_end)
    }
}

pub(super) fn prev_paragraph(rope: &Rope, char_index: usize) -> usize {
    let current_row = rope.char_to_line(char_index.min(rope.len_chars()));
    if current_row == 0 {
        return 0;
    }

    let mut row = current_row - 1;
    loop {
        if is_empty_line(rope, row) {
            return rope.line_to_char(row);
        }
        if row == 0 {
            break;
        }
        row -= 1;
    }
    0
}

pub(super) fn next_paragraph(rope: &Rope, char_index: usize) -> usize {
    let current_row = rope.char_to_line(char_index.min(rope.len_chars()));
    let last_row = rope.len_lines().saturating_sub(1);
    for row in (current_row + 1)..=last_row {
        if is_empty_line(rope, row) {
            return rope.line_to_char(row);
        }
    }
    rope.line_to_char(last_row)
}

fn is_empty_line(rope: &Rope, row: usize) -> bool {
    line_content_len(rope, row) == 0
}

fn is_word_char(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_'
}
