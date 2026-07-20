use std::ops::Range;

use ropey::Rope;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextMotion {
    WordForward,
    WordEnd,
    ChangeWordForward,
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
    if start_class == 0 {
        pos = next_char(rope, pos);
    } else {
        while pos < len && char_class(rope.char(pos)) == start_class {
            pos += 1;
        }
    }
    while pos < len && rope.char(pos).is_whitespace() {
        if is_empty_line_start(rope, pos) {
            break;
        }
        pos = next_char(rope, pos);
    }
    pos
}

pub(crate) fn forward_word_end(rope: &Rope, char_index: usize) -> usize {
    let len = rope.len_chars();
    let mut pos = char_index.min(len);
    if pos >= len {
        return len;
    }

    if rope.char(pos).is_whitespace() {
        while pos < len && rope.char(pos).is_whitespace() {
            pos = next_char(rope, pos);
        }
        if pos >= len {
            return len;
        }
    } else {
        let start_class = char_class(rope.char(pos));
        if pos + 1 < len && char_class(rope.char(pos + 1)) != start_class {
            pos = next_char(rope, pos);
            while pos < len && rope.char(pos).is_whitespace() {
                pos = next_char(rope, pos);
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

fn next_char(rope: &Rope, pos: usize) -> usize {
    if pos + 1 < rope.len_chars() && rope.char(pos) == '\r' && rope.char(pos + 1) == '\n' {
        pos + 2
    } else {
        pos + 1
    }
}

fn is_empty_line_start(rope: &Rope, pos: usize) -> bool {
    let row = rope.char_to_line(pos);
    pos == rope.line_to_char(row) && line_content_len(rope, row) == 0
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
    let change_from_blank_line = motion == TextMotion::ChangeWordForward
        && origin < rope.len_chars()
        && rope.char(origin).is_whitespace()
        && is_blank_line(rope, rope.char_to_line(origin));
    let destination = match motion {
        TextMotion::WordForward => {
            (0..count).fold(origin, |position, _| forward_word_start(rope, position))
        }
        TextMotion::WordEnd => {
            (0..count).fold(origin, |position, _| forward_word_end(rope, position))
        }
        TextMotion::ChangeWordForward => {
            if origin < rope.len_chars() && rope.char(origin).is_whitespace() {
                if change_from_blank_line {
                    change_word_from_blank_line(rope, origin, count)
                } else {
                    let boundary =
                        (1..count).fold(origin, |position, _| forward_word_start(rope, position));
                    let destination = forward_word_start(rope, boundary);
                    exclusive_word_range_end(rope, boundary, destination)
                }
            } else {
                let first = if is_unit_end(rope, origin) {
                    origin
                } else {
                    forward_word_end(rope, origin)
                };
                (1..count).fold(first, |position, _| forward_word_end(rope, position))
            }
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
    let inclusive = motion == TextMotion::WordEnd
        || motion == TextMotion::ChangeWordForward
            && origin < rope.len_chars()
            && !rope.char(origin).is_whitespace();
    let covered_end = usize::from(inclusive && destination < rope.len_chars()) + destination;
    MotionOutcome {
        destination,
        covered: TextRange::Charwise(origin.min(destination)..origin.max(covered_end)),
    }
}

pub fn resolve_operator(rope: &Rope, origin: usize, command: OperatorCommand) -> MotionOutcome {
    let mut outcome = resolve_target(rope, origin, command.target);
    if let OperatorCommand {
        operator: TextOperator::Delete,
        target:
            TextTarget::Motion {
                motion: TextMotion::WordForward,
                count,
            },
    } = command
    {
        let origin = origin.min(rope.len_chars());
        let count = count.max(1);
        let boundary = (1..count).fold(origin, |position, _| forward_word_start(rope, position));
        let destination = forward_word_start(rope, boundary);
        let range_end = exclusive_word_range_end(rope, boundary, destination);
        outcome.destination = range_end;
        outcome.covered = TextRange::Charwise(origin.min(range_end)..origin.max(range_end));
    }
    outcome
}

fn change_word_from_blank_line(rope: &Rope, origin: usize, count: usize) -> usize {
    let first = line_end_insert(rope, rope.char_to_line(origin));
    if count == 1 {
        return first;
    }

    let boundary = (1..count).fold(first, |position, _| forward_word_start(rope, position));
    if boundary >= rope.len_chars() || rope.char(boundary).is_whitespace() {
        return boundary;
    }

    let destination = forward_word_start(rope, boundary);
    exclusive_word_range_end(rope, boundary, destination)
}

fn exclusive_word_range_end(rope: &Rope, boundary: usize, destination: usize) -> usize {
    let boundary_row = rope.char_to_line(boundary);
    if rope.char_to_line(destination) <= boundary_row {
        return destination;
    }

    // A true empty line is itself a `w` boundary, so consuming it includes its
    // break. Other boundaries stop at their line's content end.
    if line_content_len(rope, boundary_row) == 0 && boundary_row + 1 < rope.len_lines() {
        rope.line_to_char(boundary_row + 1)
    } else {
        line_end_insert(rope, boundary_row)
    }
}

fn is_unit_end(rope: &Rope, position: usize) -> bool {
    position < rope.len_chars()
        && (position + 1 >= rope.len_chars()
            || char_class(rope.char(position + 1)) != char_class(rope.char(position)))
}

fn is_blank_line(rope: &Rope, row: usize) -> bool {
    let start = rope.line_to_char(row);
    let end = start + line_content_len(rope, row);
    (start..end).all(|position| rope.char(position).is_whitespace())
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
    fn word_end_motion_includes_the_destination_character() {
        let rope = Rope::from_str("one two");

        let first = resolve_motion(&rope, 0, TextMotion::WordEnd, 1);
        let second = resolve_motion(&rope, 0, TextMotion::WordEnd, 2);

        assert_eq!(first.destination, 2);
        assert_eq!(first.covered, TextRange::Charwise(0..3));
        assert_eq!(second.destination, 6);
        assert_eq!(second.covered, TextRange::Charwise(0..7));
    }

    #[test]
    fn change_word_uses_word_start_motion_when_on_whitespace() {
        let rope = Rope::from_str("one   two");
        let outcome = resolve_motion(&rope, 3, TextMotion::ChangeWordForward, 1);

        assert_eq!(outcome.destination, 6);
        assert_eq!(outcome.covered, TextRange::Charwise(3..6));
    }

    #[test]
    fn change_word_from_inline_whitespace_preserves_a_crossed_line_break() {
        let single = Rope::from_str("one   \ntwo");
        let counted = Rope::from_str("one   two\nthree");

        let single_outcome = resolve_motion(&single, 3, TextMotion::ChangeWordForward, 1);
        let counted_outcome = resolve_motion(&counted, 3, TextMotion::ChangeWordForward, 2);

        assert_eq!(single_outcome.covered, TextRange::Charwise(3..6));
        assert_eq!(counted_outcome.covered, TextRange::Charwise(3..9));
    }

    #[test]
    fn change_word_stops_before_the_break_of_a_blank_line() {
        let empty = Rope::from_str("one\n\ntwo");
        let spaces = Rope::from_str("one\n   \ntwo");

        let empty_outcome = resolve_motion(&empty, 4, TextMotion::ChangeWordForward, 1);
        let spaces_outcome = resolve_motion(&spaces, 4, TextMotion::ChangeWordForward, 1);

        assert_eq!(empty_outcome.destination, 4);
        assert_eq!(empty_outcome.covered, TextRange::Charwise(4..4));
        assert_eq!(spaces_outcome.destination, 7);
        assert_eq!(spaces_outcome.covered, TextRange::Charwise(4..7));
    }

    #[test]
    fn counted_change_word_from_a_blank_line_covers_the_next_word() {
        let final_word = Rope::from_str("one\n\ntwo");
        let consecutive_blanks = Rope::from_str("one\n\n\ntwo");
        let following_word = Rope::from_str("one\n\ntwo three");
        let following_line = Rope::from_str("one\n\ntwo\nthree");
        let whitespace_line = Rope::from_str("one\n\ntwo\n   \nthree");
        let indented_word = Rope::from_str("one\n\ntwo\n   three");

        let final_outcome = resolve_motion(&final_word, 4, TextMotion::ChangeWordForward, 2);
        let blank_outcome =
            resolve_motion(&consecutive_blanks, 4, TextMotion::ChangeWordForward, 2);
        let word_outcome = resolve_motion(&following_word, 4, TextMotion::ChangeWordForward, 2);
        let line_outcome = resolve_motion(&following_line, 4, TextMotion::ChangeWordForward, 2);
        let whitespace_outcome =
            resolve_motion(&whitespace_line, 4, TextMotion::ChangeWordForward, 2);
        let indented_outcome = resolve_motion(&indented_word, 4, TextMotion::ChangeWordForward, 2);

        assert_eq!(final_outcome.covered, TextRange::Charwise(4..8));
        assert_eq!(blank_outcome.covered, TextRange::Charwise(4..5));
        assert_eq!(word_outcome.covered, TextRange::Charwise(4..9));
        assert_eq!(line_outcome.covered, TextRange::Charwise(4..8));
        assert_eq!(whitespace_outcome.covered, TextRange::Charwise(4..8));
        assert_eq!(indented_outcome.covered, TextRange::Charwise(4..8));
    }

    #[test]
    fn single_delete_word_crossing_a_line_stops_before_the_break() {
        let word_rope = Rope::from_str("one\ntwo");
        let suffix_rope = Rope::from_str("one! \ntwo");
        let command = OperatorCommand {
            operator: TextOperator::Delete,
            target: TextTarget::Motion {
                motion: TextMotion::WordForward,
                count: 1,
            },
        };

        let word = resolve_operator(&word_rope, 0, command);
        let punctuation = resolve_operator(&suffix_rope, 3, command);
        let whitespace = resolve_operator(&suffix_rope, 4, command);
        let blank_rope = Rope::from_str("one\n\ntwo");
        let blank = resolve_operator(&blank_rope, 4, command);
        let blank_before_spaces_rope = Rope::from_str("one\n\n   \ntwo");
        let blank_before_spaces = resolve_operator(&blank_before_spaces_rope, 4, command);
        let movement = resolve_motion(&word_rope, 0, TextMotion::WordForward, 1);

        assert_eq!(word.covered, TextRange::Charwise(0..3));
        assert_eq!(punctuation.covered, TextRange::Charwise(3..5));
        assert_eq!(whitespace.covered, TextRange::Charwise(4..5));
        assert_eq!(blank.covered, TextRange::Charwise(4..5));
        assert_eq!(blank_before_spaces.covered, TextRange::Charwise(4..5));
        assert_eq!(movement.destination, 4);
        assert_eq!(movement.covered, TextRange::Charwise(0..4));
    }

    #[test]
    fn counted_delete_word_uses_the_last_word_boundary_to_preserve_breaks() {
        let direct = Rope::from_str("one two\nthree");
        let whitespace_line = Rope::from_str("one two\n   \nthree");
        let command = OperatorCommand {
            operator: TextOperator::Delete,
            target: TextTarget::Motion {
                motion: TextMotion::WordForward,
                count: 2,
            },
        };

        let direct_outcome = resolve_operator(&direct, 0, command);
        let whitespace_outcome = resolve_operator(&whitespace_line, 0, command);

        assert_eq!(direct_outcome.covered, TextRange::Charwise(0..7));
        assert_eq!(whitespace_outcome.covered, TextRange::Charwise(0..7));
    }

    #[test]
    fn change_word_counts_the_current_word_end_as_the_first_target() {
        let rope = Rope::from_str("one two three");

        let first = resolve_motion(&rope, 2, TextMotion::ChangeWordForward, 1);
        let second = resolve_motion(&rope, 2, TextMotion::ChangeWordForward, 2);

        assert_eq!(first.destination, 2);
        assert_eq!(first.covered, TextRange::Charwise(2..3));
        assert_eq!(second.destination, 6);
        assert_eq!(second.covered, TextRange::Charwise(2..7));
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
    fn word_start_treats_each_empty_line_as_a_word() {
        let rope = Rope::from_str("foo\n\n\nbar");

        assert_eq!(forward_word_start(&rope, 0), 4);
        assert_eq!(forward_word_start(&rope, 4), 5);
        assert_eq!(forward_word_start(&rope, 5), 6);

        let whitespace_only = Rope::from_str("foo\n   \nbar");
        assert_eq!(forward_word_start(&whitespace_only, 0), 8);

        let crlf = Rope::from_str("foo\r\n\r\nbar");
        assert_eq!(forward_word_start(&crlf, 0), 5);
        assert_eq!(forward_word_start(&crlf, 5), 7);
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
