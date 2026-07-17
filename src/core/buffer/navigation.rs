use ropey::Rope;

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
    let mut start = char_index.min(rope.len_chars());
    while start > 0 && rope.char(start - 1).is_whitespace() {
        start -= 1;
    }
    if start == 0 {
        return 0;
    }
    if is_word_char(rope.char(start - 1)) {
        while start > 0 && is_word_char(rope.char(start - 1)) {
            start -= 1;
        }
    } else {
        start -= 1;
    }
    start
}

pub(super) fn forward_word_end(rope: &Rope, char_index: usize) -> usize {
    let len = rope.len_chars();
    let mut pos = char_index.min(len);
    if pos >= len {
        return len;
    }

    if rope.char(pos).is_whitespace() {
        while pos < len && rope.char(pos).is_whitespace() {
            pos += 1;
        }
        if pos >= len {
            return len;
        }
    } else {
        let start_class = char_class(rope.char(pos));
        if pos + 1 < len && char_class(rope.char(pos + 1)) != start_class {
            pos += 1;
            while pos < len && rope.char(pos).is_whitespace() {
                pos += 1;
            }
            if pos >= len {
                return len;
            }
        }
    }

    let end_class = char_class(rope.char(pos));
    while pos + 1 < len && char_class(rope.char(pos + 1)) == end_class {
        pos += 1;
    }
    pos
}

pub(super) fn first_non_blank_in_line(rope: &Rope, row: usize) -> usize {
    let line_start = rope.line_to_char(row);
    let line = rope.line(row);
    for (i, ch) in line.chars().enumerate() {
        if ch == '\n' {
            break;
        }
        if !ch.is_whitespace() {
            return line_start + i;
        }
    }
    line_start
}

pub(super) fn line_end_char(rope: &Rope, row: usize) -> usize {
    let line_start = rope.line_to_char(row);
    let content_len = line_content_len(rope, row);
    if content_len == 0 {
        line_start
    } else {
        line_start + content_len - 1
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

fn char_class(ch: char) -> u8 {
    if ch.is_whitespace() {
        0
    } else if is_word_char(ch) {
        1
    } else {
        2
    }
}

fn is_empty_line(rope: &Rope, row: usize) -> bool {
    line_content_len(rope, row) == 0
}

fn is_word_char(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_'
}
