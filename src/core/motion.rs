use std::ops::Range;

use ropey::Rope;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextMotion {
    WordForward,
    LineStart,
    LineEnd,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TextRange {
    Charwise(Range<usize>),
    Linewise(Range<usize>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MotionOutcome {
    pub destination: usize,
    pub covered: TextRange,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextTarget {
    Motion { motion: TextMotion, count: usize },
    Lines { count: usize },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextOperator {
    Delete,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OperatorCommand {
    pub operator: TextOperator,
    pub target: TextTarget,
}

pub(crate) fn forward_word_start(rope: &Rope, char_index: usize) -> usize {
    let len = rope.len_chars();
    let mut pos = char_index.min(len);
    if pos >= len {
        return len;
    }
    let start_class = char_class(rope.char(pos));
    while pos < len && char_class(rope.char(pos)) == start_class {
        pos += 1;
    }
    while pos < len && rope.char(pos).is_whitespace() {
        pos += 1;
    }
    pos
}

pub(crate) fn line_end_insert(rope: &Rope, row: usize) -> usize {
    let line_start = rope.line_to_char(row);
    line_start + line_content_len(rope, row)
}

fn line_content_len(rope: &Rope, row: usize) -> usize {
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

fn char_class(ch: char) -> u8 {
    if ch.is_whitespace() {
        0
    } else if ch.is_alphanumeric() || ch == '_' {
        1
    } else {
        2
    }
}

pub fn resolve_motion(
    rope: &Rope,
    origin: usize,
    motion: TextMotion,
    count: usize,
) -> MotionOutcome {
    let origin = origin.min(rope.len_chars());
    let count = count.max(1);
    let destination = match motion {
        TextMotion::WordForward => {
            (0..count).fold(origin, |position, _| forward_word_start(rope, position))
        }
        TextMotion::LineStart => rope.line_to_char(rope.char_to_line(origin)),
        TextMotion::LineEnd => {
            let row = rope
                .char_to_line(origin)
                .saturating_add(count.saturating_sub(1))
                .min(rope.len_lines().saturating_sub(1));
            line_end_insert(rope, row)
        }
    };
    MotionOutcome {
        destination,
        covered: TextRange::Charwise(origin.min(destination)..origin.max(destination)),
    }
}

pub fn resolve_target(rope: &Rope, origin: usize, target: TextTarget) -> MotionOutcome {
    match target {
        TextTarget::Motion { motion, count } => resolve_motion(rope, origin, motion, count),
        TextTarget::Lines { count } => {
            let origin = origin.min(rope.len_chars());
            let start_line = rope.char_to_line(origin);
            let end_line = start_line
                .saturating_add(count.max(1))
                .min(rope.len_lines());
            MotionOutcome {
                destination: rope.line_to_char(start_line),
                covered: TextRange::Linewise(start_line..end_line),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn word_motion_reports_destination_and_half_open_coverage() {
        let rope = Rope::from_str("one two");
        let outcome = resolve_motion(&rope, 0, TextMotion::WordForward, 1);

        assert_eq!(outcome.destination, 4);
        assert_eq!(outcome.covered, TextRange::Charwise(0..4));
    }

    #[test]
    fn word_start_skips_the_current_unit_and_whitespace() {
        let rope = Rope::from_str("foo.bar baz");

        assert_eq!(forward_word_start(&rope, 0), 3);
        assert_eq!(forward_word_start(&rope, 3), 4);
        assert_eq!(forward_word_start(&rope, 4), 8);
        assert_eq!(
            forward_word_start(&rope, rope.len_chars()),
            rope.len_chars()
        );
    }

    #[test]
    fn line_end_insert_excludes_lf_and_crlf() {
        let rope = Rope::from_str("foo\r\nbar\n");

        assert_eq!(line_end_insert(&rope, 0), 3);
        assert_eq!(line_end_insert(&rope, 1), 8);
    }

    #[test]
    fn line_target_stays_linewise_until_lowered_by_content() {
        let rope = Rope::from_str("one\ntwo\nthree");
        let outcome = resolve_target(&rope, 5, TextTarget::Lines { count: 2 });

        assert_eq!(outcome.destination, 4);
        assert_eq!(outcome.covered, TextRange::Linewise(1..3));
    }
}
