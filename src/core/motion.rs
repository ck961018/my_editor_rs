use std::ops::Range;

use ropey::Rope;

use crate::core::buffer::{forward_word_start, line_end_insert};

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
    fn line_target_stays_linewise_until_lowered_by_content() {
        let rope = Rope::from_str("one\ntwo\nthree");
        let outcome = resolve_target(&rope, 5, TextTarget::Lines { count: 2 });

        assert_eq!(outcome.destination, 4);
        assert_eq!(outcome.covered, TextRange::Linewise(1..3));
    }
}
