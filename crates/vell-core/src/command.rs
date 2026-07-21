use crate::core::motion::OperatorCommand;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EditCommand {
    Operate(OperatorCommand),
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
    MoveToLinePreservingColumn {
        line_index: usize,
    },
    MoveToChar {
        target: char,
        direction: CharSearchDirection,
        occurrence: usize,
    },
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
    ExtendToLinePreservingColumn {
        line_index: usize,
    },
    ExtendToChar {
        target: char,
        direction: CharSearchDirection,
        occurrence: usize,
    },
    ExtendWordForwardBy(usize),
    ExtendWordBackwardBy(usize),
    ExtendWordEndBy(usize),
    ExtendToLineStart,
    ExtendToFirstNonBlank,
    ExtendToLineEnd,
    ExtendToLastLine,
    ExtendToPrevParagraphBy(usize),
    ExtendToNextParagraphBy(usize),
    InsertText(String),
    Delete(isize),
    DeleteLines {
        lines: usize,
    },
    DeleteWordBackward,
    CollapseSelections,
    ClampCursorToCharacter,
    // Modal and scripted editing primitives.
    DeleteToLineStart,
    DeleteToLineEnd,
    MoveWordForwardBy(usize),
    MoveWordBackwardBy(usize),
    MoveWordEndBy(usize),
    MoveToLineStart,
    MoveToFirstNonBlank,
    MoveToLineEnd,
    MoveToLastLine,
    MoveToPrevParagraphBy(usize),
    MoveToNextParagraphBy(usize),
    JoinLines,
    ToggleCase,
    InsertNewLineBelow,
    InsertNewLineAbove,
    MoveAfterLineEnd,
    DeleteLineContent,
    ChangeLines {
        lines: usize,
    },
    DeleteInclusiveSelection,
    DeleteSelectedLines,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CharSearchDirection {
    Forward,
    Backward,
}
