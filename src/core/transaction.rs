use std::collections::VecDeque;
use std::ops::Range;

use ropey::Rope;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TextStateId(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Affinity {
    Before,
    After,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TextChange {
    Retain(usize),
    Delete(usize),
    Insert(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextEdit {
    pub range: Range<usize>,
    pub insert: String,
}

impl TextEdit {
    pub fn new(range: Range<usize>, insert: impl Into<String>) -> Self {
        Self {
            range,
            insert: insert.into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TextTransactionError {
    LengthMismatch { expected: usize, actual: usize },
    InvalidRange { start: usize, end: usize },
    ConflictingEdits { previous_end: usize, start: usize },
    DuplicateInsert { offset: usize },
    InvalidChangeSet,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextChangeSet {
    before_len: usize,
    after_len: usize,
    changes: Vec<TextChange>,
}

impl TextChangeSet {
    pub fn empty(len: usize) -> Self {
        Self {
            before_len: len,
            after_len: len,
            changes: if len == 0 {
                Vec::new()
            } else {
                vec![TextChange::Retain(len)]
            },
        }
    }

    fn builder(before_len: usize) -> Self {
        Self {
            before_len,
            after_len: before_len,
            changes: Vec::new(),
        }
    }

    pub fn from_edits(
        before_len: usize,
        mut edits: Vec<TextEdit>,
    ) -> Result<Self, TextTransactionError> {
        edits.retain(|edit| !edit.range.is_empty() || !edit.insert.is_empty());
        edits.sort_by_key(|edit| (edit.range.start, edit.range.end));

        let mut result = Self::builder(before_len);
        let mut cursor = 0;
        let mut last_start = None;
        for edit in edits {
            if edit.range.start > edit.range.end || edit.range.end > before_len {
                return Err(TextTransactionError::InvalidRange {
                    start: edit.range.start,
                    end: edit.range.end,
                });
            }
            if edit.range.start < cursor {
                return Err(TextTransactionError::ConflictingEdits {
                    previous_end: cursor,
                    start: edit.range.start,
                });
            }
            if last_start == Some(edit.range.start) {
                return Err(TextTransactionError::DuplicateInsert {
                    offset: edit.range.start,
                });
            }
            result.push(TextChange::Retain(edit.range.start - cursor));
            result.push(TextChange::Delete(edit.range.end - edit.range.start));
            result.push(TextChange::Insert(edit.insert));
            cursor = edit.range.end;
            last_start = Some(edit.range.start);
        }
        result.push(TextChange::Retain(before_len - cursor));
        result.after_len = result
            .changes
            .iter()
            .map(|change| match change {
                TextChange::Retain(len) => *len,
                TextChange::Delete(_) => 0,
                TextChange::Insert(text) => text.chars().count(),
            })
            .sum();
        result.validate()?;
        Ok(result)
    }

    pub fn is_empty(&self) -> bool {
        self.to_edits().is_ok_and(|edits| edits.is_empty())
    }

    pub fn validate(&self) -> Result<(), TextTransactionError> {
        let consumed: usize = self
            .changes
            .iter()
            .map(|change| match change {
                TextChange::Retain(len) | TextChange::Delete(len) => *len,
                TextChange::Insert(_) => 0,
            })
            .sum();
        let produced: usize = self
            .changes
            .iter()
            .map(|change| match change {
                TextChange::Retain(len) => *len,
                TextChange::Delete(_) => 0,
                TextChange::Insert(text) => text.chars().count(),
            })
            .sum();
        if consumed != self.before_len || produced != self.after_len {
            return Err(TextTransactionError::InvalidChangeSet);
        }
        Ok(())
    }

    pub fn apply(&self, rope: &mut Rope) -> Result<(), TextTransactionError> {
        if rope.len_chars() != self.before_len {
            return Err(TextTransactionError::LengthMismatch {
                expected: self.before_len,
                actual: rope.len_chars(),
            });
        }
        let edits = self.to_edits()?;
        for edit in edits.into_iter().rev() {
            if !edit.range.is_empty() {
                rope.remove(edit.range.clone());
            }
            if !edit.insert.is_empty() {
                rope.insert(edit.range.start, &edit.insert);
            }
        }
        debug_assert_eq!(rope.len_chars(), self.after_len);
        Ok(())
    }

    pub fn invert(&self, original: &Rope) -> Result<Self, TextTransactionError> {
        if original.len_chars() != self.before_len {
            return Err(TextTransactionError::LengthMismatch {
                expected: self.before_len,
                actual: original.len_chars(),
            });
        }
        let mut delta = 0isize;
        let inverse = self
            .to_edits()?
            .into_iter()
            .map(|edit| {
                let start = edit
                    .range
                    .start
                    .checked_add_signed(delta)
                    .expect("valid edit delta");
                let inserted_len = edit.insert.chars().count();
                let removed_len = edit.range.end - edit.range.start;
                delta += inserted_len as isize - removed_len as isize;
                TextEdit::new(
                    start..start + inserted_len,
                    original.slice(edit.range).to_string(),
                )
            })
            .collect();
        Self::from_edits(self.after_len, inverse)
    }

    pub fn compose(&self, next: &Self) -> Result<Self, TextTransactionError> {
        self.validate()?;
        next.validate()?;
        if self.after_len != next.before_len {
            return Err(TextTransactionError::LengthMismatch {
                expected: self.after_len,
                actual: next.before_len,
            });
        }

        let mut left = segments(&self.changes);
        let mut right = segments(&next.changes);
        let mut result = Self::builder(self.before_len);

        while !left.is_empty() || !right.is_empty() {
            if matches!(right.front(), Some(Segment::Insert(_))) {
                let Segment::Insert(chars) = right.pop_front().expect("front exists") else {
                    unreachable!()
                };
                result.push(TextChange::Insert(chars.into_iter().collect()));
                continue;
            }
            if matches!(left.front(), Some(Segment::Delete(_))) {
                let Segment::Delete(len) = left.pop_front().expect("front exists") else {
                    unreachable!()
                };
                result.push(TextChange::Delete(len));
                continue;
            }

            let Some(left_front) = left.front() else {
                if right.is_empty() {
                    break;
                }
                return Err(TextTransactionError::InvalidChangeSet);
            };
            let Some(right_front) = right.front() else {
                return Err(TextTransactionError::InvalidChangeSet);
            };
            let count = left_front.len().min(right_front.len());
            let left_part = take_front(&mut left, count);
            let right_part = take_front(&mut right, count);
            match (left_part, right_part) {
                (Segment::Retain(_), Segment::Retain(len)) => {
                    result.push(TextChange::Retain(len));
                }
                (Segment::Retain(_), Segment::Delete(len)) => {
                    result.push(TextChange::Delete(len));
                }
                (Segment::Insert(chars), Segment::Retain(_)) => {
                    result.push(TextChange::Insert(chars.into_iter().collect()));
                }
                (Segment::Insert(_), Segment::Delete(_)) => {}
                _ => return Err(TextTransactionError::InvalidChangeSet),
            }
        }

        result.after_len = next.after_len;
        result.validate()?;
        Ok(result)
    }

    pub fn map_position(&self, offset: usize, affinity: Affinity) -> usize {
        let offset = offset.min(self.before_len);
        let mut delta = 0isize;
        for edit in self.to_edits().unwrap_or_default() {
            if offset < edit.range.start {
                break;
            }
            let mapped_start = edit
                .range
                .start
                .checked_add_signed(delta)
                .expect("valid edit delta");
            let inserted_len = edit.insert.chars().count();
            if offset <= edit.range.end {
                return match affinity {
                    Affinity::Before => mapped_start,
                    Affinity::After => mapped_start + inserted_len,
                };
            }
            delta += inserted_len as isize - (edit.range.end - edit.range.start) as isize;
        }
        offset.checked_add_signed(delta).expect("valid edit delta")
    }

    pub fn to_edits(&self) -> Result<Vec<TextEdit>, TextTransactionError> {
        self.validate()?;
        let mut edits = Vec::new();
        let mut current: Option<TextEdit> = None;
        let mut old_offset = 0;
        let flush = |current: &mut Option<TextEdit>, edits: &mut Vec<TextEdit>| {
            if let Some(edit) = current.take()
                && (!edit.range.is_empty() || !edit.insert.is_empty())
            {
                edits.push(edit);
            }
        };
        for change in &self.changes {
            match change {
                TextChange::Retain(len) => {
                    flush(&mut current, &mut edits);
                    old_offset += len;
                }
                TextChange::Delete(len) => {
                    let edit = current.get_or_insert_with(|| {
                        TextEdit::new(old_offset..old_offset, String::new())
                    });
                    edit.range.end += len;
                    old_offset += len;
                }
                TextChange::Insert(text) => {
                    let edit = current.get_or_insert_with(|| {
                        TextEdit::new(old_offset..old_offset, String::new())
                    });
                    edit.insert.push_str(text);
                }
            }
        }
        flush(&mut current, &mut edits);
        Ok(edits)
    }

    fn push(&mut self, change: TextChange) {
        match change {
            TextChange::Retain(0) | TextChange::Delete(0) => return,
            TextChange::Insert(ref text) if text.is_empty() => return,
            _ => {}
        }
        match (self.changes.last_mut(), change) {
            (Some(TextChange::Retain(existing)), TextChange::Retain(len))
            | (Some(TextChange::Delete(existing)), TextChange::Delete(len)) => *existing += len,
            (Some(TextChange::Insert(existing)), TextChange::Insert(text)) => {
                existing.push_str(&text)
            }
            (_, change) => self.changes.push(change),
        }
    }
}

#[derive(Clone, Debug)]
enum Segment {
    Retain(usize),
    Delete(usize),
    Insert(Vec<char>),
}

impl Segment {
    fn len(&self) -> usize {
        match self {
            Self::Retain(len) | Self::Delete(len) => *len,
            Self::Insert(chars) => chars.len(),
        }
    }
}

fn segments(changes: &[TextChange]) -> VecDeque<Segment> {
    changes
        .iter()
        .map(|change| match change {
            TextChange::Retain(len) => Segment::Retain(*len),
            TextChange::Delete(len) => Segment::Delete(*len),
            TextChange::Insert(text) => Segment::Insert(text.chars().collect()),
        })
        .collect()
}

fn take_front(queue: &mut VecDeque<Segment>, count: usize) -> Segment {
    let front = queue.pop_front().expect("front exists");
    match front {
        Segment::Retain(len) => {
            if len > count {
                queue.push_front(Segment::Retain(len - count));
            }
            Segment::Retain(count)
        }
        Segment::Delete(len) => {
            if len > count {
                queue.push_front(Segment::Delete(len - count));
            }
            Segment::Delete(count)
        }
        Segment::Insert(mut chars) => {
            let remainder = chars.split_off(count);
            if !remainder.is_empty() {
                queue.push_front(Segment::Insert(remainder));
            }
            Segment::Insert(chars)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn change_set_applies_disjoint_unicode_edits() {
        let mut rope = Rope::from_str("aβcd");
        let changes = TextChangeSet::from_edits(
            rope.len_chars(),
            vec![TextEdit::new(1..2, "Ω"), TextEdit::new(4..4, "!")],
        )
        .unwrap();

        changes.apply(&mut rope).unwrap();

        assert_eq!(rope.to_string(), "aΩcd!");
    }

    #[test]
    fn invert_restores_original_text() {
        let original = Rope::from_str("hello world");
        let changes = TextChangeSet::from_edits(
            original.len_chars(),
            vec![TextEdit::new(0..5, "hi"), TextEdit::new(11..11, "!")],
        )
        .unwrap();
        let inverse = changes.invert(&original).unwrap();
        let mut changed = original.clone();

        changes.apply(&mut changed).unwrap();
        inverse.apply(&mut changed).unwrap();

        assert_eq!(changed, original);
    }

    #[test]
    fn compose_coalesces_insert_session() {
        let first = TextChangeSet::from_edits(0, vec![TextEdit::new(0..0, "a")]).unwrap();
        let second = TextChangeSet::from_edits(1, vec![TextEdit::new(1..1, "β")]).unwrap();
        let composed = first.compose(&second).unwrap();
        let mut rope = Rope::new();

        composed.apply(&mut rope).unwrap();

        assert_eq!(rope.to_string(), "aβ");
        assert_eq!(composed.changes, [TextChange::Insert("aβ".to_string())]);
    }

    #[test]
    fn compose_cancels_insert_then_delete() {
        let first = TextChangeSet::from_edits(0, vec![TextEdit::new(0..0, "x")]).unwrap();
        let second = TextChangeSet::from_edits(1, vec![TextEdit::new(0..1, "")]).unwrap();

        let composed = first.compose(&second).unwrap();

        assert!(composed.is_empty());
        assert_eq!(composed.before_len, 0);
        assert_eq!(composed.after_len, 0);
    }

    #[test]
    fn affinity_controls_insertion_boundary() {
        let changes = TextChangeSet::from_edits(5, vec![TextEdit::new(2..2, "abc")]).unwrap();

        assert_eq!(changes.map_position(2, Affinity::Before), 2);
        assert_eq!(changes.map_position(2, Affinity::After), 5);
        assert_eq!(changes.map_position(4, Affinity::Before), 7);
    }

    #[test]
    fn conflicting_edits_are_rejected() {
        let result =
            TextChangeSet::from_edits(5, vec![TextEdit::new(1..3, ""), TextEdit::new(2..4, "")]);

        assert!(matches!(
            result,
            Err(TextTransactionError::ConflictingEdits { .. })
        ));
    }

    #[test]
    fn compose_matches_sequential_application_for_replacements() {
        let original = Rope::from_str("abcd");
        for first_start in 0..=original.len_chars() {
            for first_end in first_start..=original.len_chars() {
                for first_insert in ["", "X", "YZ"] {
                    let first = TextChangeSet::from_edits(
                        original.len_chars(),
                        vec![TextEdit::new(first_start..first_end, first_insert)],
                    )
                    .unwrap();
                    let mut middle = original.clone();
                    first.apply(&mut middle).unwrap();
                    for second_start in 0..=middle.len_chars() {
                        for second_end in second_start..=middle.len_chars() {
                            for second_insert in ["", "Q", "RS"] {
                                let second = TextChangeSet::from_edits(
                                    middle.len_chars(),
                                    vec![TextEdit::new(second_start..second_end, second_insert)],
                                )
                                .unwrap();
                                let mut sequential = middle.clone();
                                second.apply(&mut sequential).unwrap();

                                let composed = first.compose(&second).unwrap();
                                let mut once = original.clone();
                                composed.apply(&mut once).unwrap();
                                assert_eq!(once, sequential);
                            }
                        }
                    }
                }
            }
        }
    }
}
