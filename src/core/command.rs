use crate::core::motion::OperatorCommand;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EditCommand {
    Operate(OperatorCommand),
    #[expect(
        dead_code,
        reason = "generic relative motion is an executor-level extension seam"
    )]
    MoveBy {
        chars: isize,
        lines: isize,
    },
    MoveLeftBy(usize),
    MoveRightBy(usize),
    MoveWithinLineLeftBy(usize),
    MoveWithinLineRightBy(usize),
    MoveUpBy(usize),
    MoveDownBy(usize),
    MoveToLine {
        line_index: usize,
    },
    MoveToChar {
        target: char,
        direction: CharSearchDirection,
        occurrence: usize,
    },
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "absolute motion is an executor-level extension seam"
        )
    )]
    MoveTo {
        char_idx: usize,
        line_idx: usize,
    },
    ExtendLeftBy(usize),
    ExtendRightBy(usize),
    ExtendWithinLineLeftBy(usize),
    ExtendWithinLineRightBy(usize),
    ExtendUpBy(usize),
    ExtendDownBy(usize),
    ExtendToLine {
        line_index: usize,
    },
    ExtendToChar {
        target: char,
        direction: CharSearchDirection,
        occurrence: usize,
    },
    ExtendWordForward,
    ExtendWordBackward,
    ExtendWordEnd,
    ExtendToLineStart,
    ExtendToFirstNonBlank,
    ExtendToLineEnd,
    ExtendToLastLine,
    ExtendToPrevParagraph,
    ExtendToNextParagraph,
    InsertText(String),
    Delete(isize),
    #[expect(
        dead_code,
        reason = "direct line deletion remains part of the content editing command contract"
    )]
    DeleteLines {
        lines: usize,
    },
    DeleteWordBackward,
    CollapseSelections,
    ClampCursorToCharacter,
    // Modal and scripted editing primitives.
    DeleteToLineStart,
    DeleteToLineEnd,
    MoveWordForward,
    MoveWordBackward,
    MoveWordEnd,
    MoveToLineStart,
    MoveToFirstNonBlank,
    MoveToLineEnd,
    MoveToLastLine,
    MoveToPrevParagraph,
    MoveToNextParagraph,
    JoinLines,
    ToggleCase,
    InsertNewLineBelow,
    InsertNewLineAbove,
    MoveAfterLineEnd,
    DeleteLineContent,
    DeleteInclusiveSelection,
    DeleteSelectedLines,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CharSearchDirection {
    Forward,
    Backward,
}
