use ropey::Rope;
use std::borrow::Cow;
use std::io;
use std::path::PathBuf;

use crate::core::action::{ContentAction, ContentEditPlan};
use crate::core::clipboard::{ClipboardKind, ClipboardPayload, PastePlacement};
use crate::core::command::{CharSearchDirection, EditCommand, IndentationConfig};
use crate::core::grapheme::{
    at_column, boundary_at_or_after, boundary_at_or_before, column, next_boundary,
    previous_boundary,
};
use crate::core::motion::{
    OperatorCommand, TextOperator, TextRange, TextTarget, forward_word_end, forward_word_start,
    line_end_insert, resolve_operator,
};
use crate::core::transaction::{
    Affinity, TextChangeSet, TextEdit, TextStateId, TextTransactionData, TextTransactionError,
    TransactionDirection,
};
use crate::protocol::content_query::{BufferBackingState, SaveState};
use crate::protocol::selection::{Selection, Selections, TextOffset, TextPoint};

mod navigation;
mod ranges;

use navigation::{
    backward_word_start, first_non_blank_in_line, line_break_width_before, line_content_len,
    line_end_char, next_paragraph, prev_paragraph,
};
use ranges::merge_ranges;

#[derive(Clone)]
pub struct Buffer {
    rope: Rope,
    path: Option<PathBuf>,
    revision: u64,
    current_state: TextStateId,
    saved_state: TextStateId,
    next_state: u64,
    last_change: Option<TextChangeSet>,
    backing_state: BufferBackingState,
    save_state: SaveState,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BufferTransactionData {
    pub text: TextTransactionData,
}

#[derive(Clone, Copy, Debug)]
struct LineBlock {
    start_row: usize,
    end_row: usize,
    start: usize,
    end: usize,
}

fn merge_line_blocks(mut blocks: Vec<LineBlock>) -> Vec<LineBlock> {
    blocks.sort_unstable_by_key(|block| (block.start_row, block.end_row));
    let mut merged: Vec<LineBlock> = Vec::with_capacity(blocks.len());
    for block in blocks {
        if let Some(previous) = merged.last_mut()
            && block.start_row <= previous.end_row.saturating_add(1)
        {
            previous.end_row = previous.end_row.max(block.end_row);
            previous.end = previous.end.max(block.end);
        } else {
            merged.push(block);
        }
    }
    merged
}

fn ordered_selection_range(selection: &Selection) -> (usize, usize) {
    let anchor = selection.anchor.char_index;
    let head = selection.head.char_index;
    (anchor.min(head), anchor.max(head))
}

fn containing_range(ranges: &[(usize, usize)], start: usize, end: usize) -> &(usize, usize) {
    ranges
        .iter()
        .find(|&&(range_start, range_end)| range_start <= start && range_end >= end)
        .expect("each selection belongs to one normalized range")
}

fn valid_pair(open: &str, close: &str) -> bool {
    !open.is_empty()
        && !close.is_empty()
        && !open.contains(['\r', '\n'])
        && !close.contains(['\r', '\n'])
}

fn distributed_fragments(fragments: &[String], count: usize) -> Vec<String> {
    if fragments.len() == count {
        return fragments.to_vec();
    }
    vec![fragments.concat(); count]
}

fn with_trailing_line_ending(mut text: String, line_ending: &str) -> String {
    if !text.ends_with('\n') {
        text.push_str(line_ending);
    }
    text
}

fn without_trailing_line_ending(mut text: String) -> String {
    if text.ends_with("\r\n") {
        text.truncate(text.len() - 2);
    } else if text.ends_with('\n') {
        text.pop();
    }
    text
}

impl Buffer {
    pub fn new() -> Self {
        Self {
            rope: Rope::new(),
            path: None,
            revision: 0,
            current_state: TextStateId(0),
            saved_state: TextStateId(0),
            next_state: 1,
            last_change: None,
            backing_state: BufferBackingState::Untitled,
            save_state: SaveState::Idle,
        }
    }

    pub fn from_file(path: PathBuf, text: String) -> Self {
        Self::from_path(
            path,
            Rope::from_str(&text),
            BufferBackingState::Materialized,
        )
    }

    pub fn for_new_file(path: PathBuf) -> Self {
        Self::from_path(path, Rope::new(), BufferBackingState::Unmaterialized)
    }

    fn from_path(path: PathBuf, rope: Rope, backing_state: BufferBackingState) -> Self {
        Self {
            rope,
            path: Some(path),
            revision: 1,
            current_state: TextStateId(1),
            saved_state: TextStateId(1),
            next_state: 2,
            last_change: None,
            backing_state,
            save_state: SaveState::Idle,
        }
    }

    pub fn plan_edit(&self, command: EditCommand, selections: &Selections) -> ContentEditPlan {
        let mut scratch = Self {
            rope: self.rope.clone(),
            path: self.path.clone(),
            revision: self.revision,
            current_state: self.current_state,
            saved_state: self.saved_state,
            next_state: self.next_state,
            last_change: None,
            backing_state: self.backing_state,
            save_state: self.save_state,
        };
        let mut selections = selections.clone();
        crate::core::edit::apply_edit(command, &mut scratch, &mut selections);
        ContentEditPlan {
            action: scratch.take_last_change().map(ContentAction::Text),
            selections,
        }
    }

    pub fn copy_selections(
        &self,
        selections: &Selections,
        kind: ClipboardKind,
    ) -> ClipboardPayload {
        let mut selections = selections.clone();
        self.reconcile_selections(&mut selections);
        let fragments = match kind {
            ClipboardKind::CharacterWise => selections
                .all()
                .map(|selection| {
                    let (start, end) = ordered_selection_range(selection);
                    self.rope.slice(start..end).to_string()
                })
                .collect(),
            ClipboardKind::LineWise => self
                .selected_line_blocks(&selections)
                .into_iter()
                .map(|block| self.rope.slice(block.start..block.end).to_string())
                .collect(),
        };
        ClipboardPayload { kind, fragments }
    }

    pub fn plan_cut(
        &self,
        selections: &Selections,
        kind: ClipboardKind,
    ) -> (ClipboardPayload, ContentEditPlan) {
        let payload = self.copy_selections(selections, kind);
        let mut scratch = self.clone();
        scratch.last_change = None;
        let mut selections = selections.clone();
        match kind {
            ClipboardKind::CharacterWise => {
                scratch.delete_at_selections(&mut selections, 0);
            }
            ClipboardKind::LineWise => {
                scratch.delete_selected_lines_at_selections(&mut selections);
            }
        }
        (
            payload,
            ContentEditPlan {
                action: scratch.take_last_change().map(ContentAction::Text),
                selections,
            },
        )
    }

    pub fn plan_paste(
        &self,
        selections: &Selections,
        payload: &ClipboardPayload,
        placement: PastePlacement,
    ) -> ContentEditPlan {
        let mut scratch = self.clone();
        scratch.last_change = None;
        let mut selections = selections.clone();
        match payload.kind {
            ClipboardKind::CharacterWise => {
                scratch.paste_charwise(&mut selections, payload, placement);
            }
            ClipboardKind::LineWise => {
                scratch.paste_linewise(&mut selections, payload, placement);
            }
        }
        ContentEditPlan {
            action: scratch.take_last_change().map(ContentAction::Text),
            selections,
        }
    }

    pub fn load_from_file(&mut self, path: &str) -> io::Result<()> {
        self.path = Some(PathBuf::from(path));
        match std::fs::read_to_string(path) {
            Ok(text) => {
                self.rope = Rope::from_str(&text);
                self.advance_revision();
                self.reset_to_saved_state();
                self.backing_state = BufferBackingState::Materialized;
                self.save_state = SaveState::Idle;
                Ok(())
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                self.rope = Rope::new();
                self.advance_revision();
                self.reset_to_saved_state();
                self.backing_state = BufferBackingState::Unmaterialized;
                self.save_state = SaveState::Idle;
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    pub fn open_path(&mut self, path: &str) -> io::Result<()> {
        self.load_from_file(path)
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn state_id(&self) -> TextStateId {
        self.current_state
    }

    pub fn mark_saved(&mut self, state: TextStateId) -> bool {
        self.saved_state = state;
        self.backing_state = BufferBackingState::Materialized;
        self.current_state == state
    }

    pub(crate) fn set_path(&mut self, path: PathBuf) {
        self.path = Some(path);
    }

    pub(crate) fn reload(
        &mut self,
        path: PathBuf,
        text: String,
        backing_state: BufferBackingState,
    ) -> Option<TextChangeSet> {
        let change = (self.rope != text).then(|| {
            TextChangeSet::from_edits(
                self.rope.len_chars(),
                vec![TextEdit::new(0..self.rope.len_chars(), text)],
            )
            .expect("full-buffer replacement is always valid")
        });
        if let Some(change) = &change {
            change
                .apply(&mut self.rope)
                .expect("validated full-buffer replacement must apply");
        }
        self.path = Some(path);
        self.current_state = self.allocate_state();
        self.saved_state = self.current_state;
        self.last_change = None;
        self.backing_state = backing_state;
        self.save_state = SaveState::Idle;
        self.advance_revision();
        change
    }

    fn advance_revision(&mut self) {
        self.revision = self
            .revision
            .checked_add(1)
            .expect("buffer revision overflow");
    }

    fn allocate_state(&mut self) -> TextStateId {
        let state = TextStateId(self.next_state);
        self.next_state = self
            .next_state
            .checked_add(1)
            .expect("text state id overflow");
        state
    }

    fn reset_to_saved_state(&mut self) {
        let state = self.allocate_state();
        self.current_state = state;
        self.saved_state = state;
        self.last_change = None;
    }

    pub fn take_last_change(&mut self) -> Option<TextChangeSet> {
        self.last_change.take()
    }

    pub fn transform_selections(
        &self,
        selections: &mut Selections,
        changes: &TextChangeSet,
    ) -> bool {
        let before = selections.clone();
        for selection in selections.all_mut() {
            let anchor = selection.anchor.char_index;
            let head = selection.head.char_index;
            if anchor == head {
                let mapped = changes.map_position(head, Affinity::After);
                selection.anchor.char_index = mapped;
                selection.head.char_index = mapped;
                continue;
            }

            let (start, end, forward) = if anchor < head {
                (anchor, head, true)
            } else {
                (head, anchor, false)
            };
            let mapped_start = changes.map_position(start, Affinity::After);
            let mapped_end = changes.map_position(end, Affinity::Before);
            let (mapped_start, mapped_end) =
                (mapped_start.min(mapped_end), mapped_start.max(mapped_end));
            if forward {
                selection.anchor.char_index = mapped_start;
                selection.head.char_index = mapped_end;
            } else {
                selection.anchor.char_index = mapped_end;
                selection.head.char_index = mapped_start;
            }
        }
        self.reconcile_selections(selections);
        selections != &before
    }

    fn apply_text_edits(&mut self, edits: Vec<TextEdit>) -> Result<bool, TextTransactionError> {
        let changes = TextChangeSet::from_edits(self.rope.len_chars(), edits)?;
        self.apply_resolved_change(changes)
    }

    pub(crate) fn apply_resolved_change(
        &mut self,
        changes: TextChangeSet,
    ) -> Result<bool, TextTransactionError> {
        if changes.is_empty() {
            self.last_change = None;
            return Ok(false);
        }
        self.validate_edit_boundaries(&changes)?;
        changes.apply(&mut self.rope)?;
        self.current_state = self.allocate_state();
        self.advance_revision();
        self.last_change = Some(changes);
        Ok(true)
    }

    pub fn apply_content_change(
        &mut self,
        change: TextChangeSet,
    ) -> Result<Option<BufferTransactionData>, TextTransactionError> {
        if change.is_empty() {
            self.last_change = None;
            return Ok(None);
        }
        self.validate_edit_boundaries(&change)?;
        let before_state = self.current_state;
        let inverse = change.invert(&self.rope)?;
        change.apply(&mut self.rope)?;
        let after_state = self.allocate_state();
        self.current_state = after_state;
        self.advance_revision();
        self.last_change = None;
        Ok(Some(BufferTransactionData {
            text: TextTransactionData {
                forward: change,
                inverse,
                before_state,
                after_state,
            },
        }))
    }

    pub fn apply_transaction_data(
        &mut self,
        data: &BufferTransactionData,
        direction: TransactionDirection,
    ) -> Result<TextChangeSet, TextTransactionError> {
        let (expected, next, change) = match direction {
            TransactionDirection::Forward => (
                data.text.before_state,
                data.text.after_state,
                &data.text.forward,
            ),
            TransactionDirection::Inverse => (
                data.text.after_state,
                data.text.before_state,
                &data.text.inverse,
            ),
        };
        if self.current_state != expected {
            return Err(TextTransactionError::StateMismatch {
                expected,
                actual: self.current_state,
            });
        }
        change.apply(&mut self.rope)?;
        self.current_state = next;
        self.advance_revision();
        self.last_change = None;
        Ok(change.clone())
    }

    fn validate_edit_boundaries(
        &self,
        changes: &TextChangeSet,
    ) -> Result<(), TextTransactionError> {
        for edit in changes.to_edits()? {
            for offset in [edit.range.start, edit.range.end] {
                let splits_crlf = offset > 0
                    && offset < self.rope.len_chars()
                    && self.rope.char(offset - 1) == '\r'
                    && self.rope.char(offset) == '\n';
                if splits_crlf {
                    return Err(TextTransactionError::InvalidRange {
                        start: edit.range.start,
                        end: edit.range.end,
                    });
                }
            }
        }
        Ok(())
    }

    pub fn backing_state(&self) -> BufferBackingState {
        self.backing_state
    }

    pub fn save_state(&self) -> SaveState {
        self.save_state
    }

    pub(crate) fn set_save_state(&mut self, state: SaveState) {
        self.save_state = state;
    }

    #[cfg(test)]
    pub(crate) fn insert_char(&mut self, char_idx: usize, ch: char) {
        self.apply_text_edits(vec![TextEdit::new(char_idx..char_idx, ch.to_string())])
            .expect("valid character insertion");
    }

    #[cfg(test)]
    #[expect(
        dead_code,
        reason = "direct backward deletion is retained as a buffer test primitive"
    )]
    pub(crate) fn delete_backward(&mut self, char_idx: usize) -> bool {
        let char_idx = boundary_at_or_after(&self.rope, char_idx);
        if char_idx == 0 {
            return false;
        }
        let start = previous_boundary(&self.rope, char_idx);
        self.apply_text_edits(vec![TextEdit::new(start..char_idx, "")])
            .expect("valid backward deletion");
        true
    }

    pub fn len_lines(&self) -> usize {
        self.rope.len_lines()
    }

    pub fn slice(&self) -> &Rope {
        &self.rope
    }

    pub fn path(&self) -> Option<&PathBuf> {
        self.path.as_ref()
    }

    /// 取第 idx 行（含尾部换行），供文本行查询使用。
    pub fn line(&self, idx: usize) -> Cow<'_, str> {
        Cow::Owned(self.slice().line(idx).to_string())
    }

    /// 文件名（path 末段），供资源名查询使用。
    pub fn file_name(&self) -> Option<&str> {
        self.path()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
    }

    pub fn modified(&self) -> bool {
        self.current_state != self.saved_state
    }

    // ——编辑原语：底层点操作（pub(crate)，操作 head）——

    pub fn clamp_offset(&self, cur: &mut TextOffset) {
        cur.char_index = boundary_at_or_after(&self.rope, cur.char_index);
    }

    fn grapheme_column(&self, offset: TextOffset) -> (usize, usize) {
        let mut offset = offset;
        self.clamp_offset(&mut offset);
        let row = self.rope.char_to_line(offset.char_index);
        let line_start = self.rope.line_to_char(row);
        (row, column(&self.rope, line_start, offset.char_index))
    }

    fn at_grapheme_column(&self, row: usize, target_column: usize) -> usize {
        let line_start = self.rope.line_to_char(row);
        let line_end = line_start + line_content_len(&self.rope, row);
        at_column(&self.rope, line_start, line_end, target_column)
    }

    pub fn text_point(&self, offset: TextOffset) -> TextPoint {
        let mut offset = offset;
        self.clamp_offset(&mut offset);
        let clamped = offset.char_index;
        let row = self.rope.char_to_line(clamped);
        TextPoint {
            row,
            col: clamped - self.rope.line_to_char(row),
        }
    }

    pub(crate) fn move_cursor_by(&self, cur: &mut TextOffset, chars: isize, lines: isize) {
        if chars != 0 {
            if chars < 0 {
                self.move_cursor_left(cur, chars.unsigned_abs());
            } else {
                self.move_cursor_right(cur, chars as usize);
            }
        }
        if lines != 0 {
            let (row, column) = self.grapheme_column(*cur);
            let max_row = self.rope.len_lines().saturating_sub(1);
            let target_row = (row as isize + lines).clamp(0, max_row as isize) as usize;
            cur.char_index = self.at_grapheme_column(target_row, column);
        }
        self.clamp_offset(cur);
    }

    pub(crate) fn move_cursor_left(&self, cur: &mut TextOffset, n: usize) {
        self.clamp_offset(cur);
        for _ in 0..n {
            cur.char_index = previous_boundary(&self.rope, cur.char_index);
        }
    }

    pub(crate) fn move_cursor_right(&self, cur: &mut TextOffset, n: usize) {
        self.clamp_offset(cur);
        for _ in 0..n {
            cur.char_index = next_boundary(&self.rope, cur.char_index);
        }
    }

    pub(crate) fn move_cursor_up(&self, cur: &mut TextOffset, n: usize) {
        let (row, column) = self.grapheme_column(*cur);
        cur.char_index = self.at_grapheme_column(row.saturating_sub(n), column);
    }

    pub(crate) fn move_cursor_down(&self, cur: &mut TextOffset, n: usize) {
        let (row, column) = self.grapheme_column(*cur);
        let max_row = self.rope.len_lines().saturating_sub(1);
        cur.char_index = self.at_grapheme_column(row.saturating_add(n).min(max_row), column);
    }

    pub(crate) fn set_cursor(&self, cur: &mut TextOffset, char_idx: usize, _line_idx: usize) {
        cur.char_index = char_idx.min(self.rope.len_chars());
        self.clamp_offset(cur);
    }

    // ——编辑原语：selection 层（pub，head/anchor 独立，守恒由调用方决定）——

    /// 将 head 与 anchor 钳制到当前文档范围，不缓存逻辑行列。
    pub fn clamp_selection(&self, sel: &mut Selection) {
        self.clamp_offset(&mut sel.head);
        self.clamp_offset(&mut sel.anchor);
    }

    pub fn reconcile_selections(&self, selections: &mut Selections) -> bool {
        let before = selections.clone();
        for selection in selections.all_mut() {
            self.clamp_selection(selection);
        }
        *selections != before
    }

    pub fn clamp_cursor_to_character(&self, selections: &mut Selections) {
        self.reconcile_selections(selections);
        for selection in selections.all_mut() {
            let collapsed = selection.anchor == selection.head;
            let row = self.rope.char_to_line(selection.head.char_index);
            selection.head.char_index = selection
                .head
                .char_index
                .min(line_end_char(&self.rope, row));
            if collapsed {
                selection.anchor = selection.head;
            }
        }
    }

    /// 移动 head，不碰 anchor（extend 语义：selection 变非空）。
    pub fn move_head_by(&self, sel: &mut Selection, chars: isize, lines: isize) {
        self.move_cursor_by(&mut sel.head, chars, lines);
    }

    pub fn move_head_left(&self, sel: &mut Selection, n: usize) {
        self.move_cursor_left(&mut sel.head, n);
    }

    pub fn move_head_right(&self, sel: &mut Selection, n: usize) {
        self.move_cursor_right(&mut sel.head, n);
    }

    pub fn move_head_within_line_left(&self, sel: &mut Selection, n: usize) {
        self.clamp_offset(&mut sel.head);
        let row = self.rope.char_to_line(sel.head.char_index);
        let line_start = self.rope.line_to_char(row);
        for _ in 0..n {
            sel.head.char_index =
                previous_boundary(&self.rope, sel.head.char_index).max(line_start);
        }
    }

    pub fn move_head_within_line_right(&self, sel: &mut Selection, n: usize) {
        self.clamp_offset(&mut sel.head);
        let row = self.rope.char_to_line(sel.head.char_index);
        let line_end = line_end_char(&self.rope, row);
        for _ in 0..n {
            sel.head.char_index = next_boundary(&self.rope, sel.head.char_index).min(line_end);
        }
    }

    pub fn move_head_up(&self, sel: &mut Selection, n: usize) {
        self.move_cursor_up(&mut sel.head, n);
    }

    pub fn move_head_down(&self, sel: &mut Selection, n: usize) {
        self.move_cursor_down(&mut sel.head, n);
    }

    pub fn move_head_to_line(&self, sel: &mut Selection, line_index: usize) {
        let row = line_index.min(self.rope.len_lines().saturating_sub(1));
        sel.head.char_index = self.rope.line_to_char(row);
        self.clamp_offset(&mut sel.head);
    }

    pub fn move_head_to_line_preserving_column(&self, sel: &mut Selection, line_index: usize) {
        let (_, column) = self.grapheme_column(sel.head);
        let row = line_index.min(self.rope.len_lines().saturating_sub(1));
        sel.head.char_index = self.at_grapheme_column(row, column);
    }

    pub fn move_head_to_char(
        &self,
        sel: &mut Selection,
        target: char,
        direction: CharSearchDirection,
        occurrence: usize,
    ) -> bool {
        let occurrence = occurrence.max(1);
        let head = sel.head.char_index.min(self.rope.len_chars());
        let row = self.rope.char_to_line(head);
        let line_start = self.rope.line_to_char(row);
        let line_end = line_start + line_content_len(&self.rope, row);
        let found = match direction {
            CharSearchDirection::Forward => {
                let start = next_boundary(&self.rope, head).min(line_end);
                (start..line_end)
                    .filter(|index| self.rope.char(*index) == target)
                    .nth(occurrence - 1)
            }
            CharSearchDirection::Backward => (line_start..head)
                .rev()
                .filter(|index| self.rope.char(*index) == target)
                .nth(occurrence - 1),
        };
        let Some(found) = found else {
            return false;
        };
        sel.head.char_index = boundary_at_or_before(&self.rope, found);
        true
    }

    pub fn move_head_word_forward(&self, sel: &mut Selection) {
        let target = forward_word_start(&self.rope, sel.head.char_index);
        sel.head.char_index = target;
        self.clamp_offset(&mut sel.head);
    }

    pub fn move_head_word_backward(&self, sel: &mut Selection) {
        let target = backward_word_start(&self.rope, sel.head.char_index);
        sel.head.char_index = target;
        self.clamp_offset(&mut sel.head);
    }

    pub fn move_head_word_end(&self, sel: &mut Selection) {
        let target = forward_word_end(&self.rope, sel.head.char_index);
        sel.head.char_index = target;
        self.clamp_offset(&mut sel.head);
    }

    pub fn move_head_to_line_start(&self, sel: &mut Selection) {
        let row = self
            .rope
            .char_to_line(sel.head.char_index.min(self.rope.len_chars()));
        sel.head.char_index = self.rope.line_to_char(row);
        self.clamp_offset(&mut sel.head);
    }

    pub fn move_head_to_first_non_blank(&self, sel: &mut Selection) {
        let row = self
            .rope
            .char_to_line(sel.head.char_index.min(self.rope.len_chars()));
        sel.head.char_index = first_non_blank_in_line(&self.rope, row);
        self.clamp_offset(&mut sel.head);
    }

    pub fn move_head_to_line_end(&self, sel: &mut Selection) {
        let row = self
            .rope
            .char_to_line(sel.head.char_index.min(self.rope.len_chars()));
        sel.head.char_index = line_end_char(&self.rope, row);
        self.clamp_offset(&mut sel.head);
    }

    pub fn move_head_after_line_end(&self, sel: &mut Selection) {
        let row = self
            .rope
            .char_to_line(sel.head.char_index.min(self.rope.len_chars()));
        sel.head.char_index = line_end_insert(&self.rope, row);
        self.clamp_offset(&mut sel.head);
    }

    pub fn move_head_to_last_line(&self, sel: &mut Selection) {
        let max_row = self.rope.len_lines().saturating_sub(1);
        sel.head.char_index = self.rope.line_to_char(max_row);
        self.clamp_offset(&mut sel.head);
    }

    pub fn move_head_to_prev_paragraph(&self, sel: &mut Selection) {
        sel.head.char_index = prev_paragraph(&self.rope, sel.head.char_index);
        self.clamp_offset(&mut sel.head);
    }

    pub fn move_head_to_next_paragraph(&self, sel: &mut Selection) {
        sel.head.char_index = next_paragraph(&self.rope, sel.head.char_index);
        self.clamp_offset(&mut sel.head);
    }

    /// 设 head，不碰 anchor。
    pub fn set_head(&self, sel: &mut Selection, char_idx: usize, line_idx: usize) {
        self.set_cursor(&mut sel.head, char_idx, line_idx);
    }

    /// anchor = head（collapsed 守恒，由调用方决定时机）。
    pub fn collapse_to_head(sel: &mut Selection) {
        sel.anchor = sel.head;
    }

    /// 在每个 selection 插入文本：非空时先删 `[min,max]` 再插入，head 到插入末尾，collapse。
    /// 空时在 head 点插入，head 前移 text_len，collapse。
    pub fn insert_at_selections(&mut self, selections: &mut Selections, text: &str) {
        self.reconcile_selections(selections);
        let text = self.normalize_insert_text(text);
        if text.is_empty() {
            return;
        }
        let text_len = text.chars().count();
        let ranges: Vec<(usize, usize)> = selections
            .all()
            .map(|s| {
                if s.anchor != s.head {
                    let (a, b) = (s.anchor.char_index, s.head.char_index);
                    (a.min(b), a.max(b))
                } else {
                    (s.head.char_index, s.head.char_index) // 空：不删
                }
            })
            .collect();
        let normalized = merge_ranges(ranges.clone());
        self.apply_text_edits(
            normalized
                .into_iter()
                .map(|(start, end)| TextEdit::new(start..end, text.clone()))
                .collect(),
        )
        .expect("valid selection insertion");
        let change = self.last_change.as_ref().cloned();
        for sel in selections.all_mut() {
            let insert_at = sel.anchor.char_index.min(sel.head.char_index);
            sel.head.char_index = change.as_ref().map_or(insert_at + text_len, |change| {
                change.map_position(insert_at, crate::core::transaction::Affinity::After)
            });
            self.clamp_offset(&mut sel.head);
            Self::collapse_to_head(sel);
        }
    }

    /// 在每个 selection 删除：非空时删 `[min,max]`，head=min，collapse。
    /// 空时按方向删 n，head 回退（backward）或不动（forward），collapse。
    pub fn delete_at_selections(&mut self, selections: &mut Selections, n: isize) {
        self.reconcile_selections(selections);
        let len = self.rope.len_chars();
        // 1) 计算每个 selection 的删除区间
        let selection_ranges: Vec<(usize, usize)> = selections
            .all()
            .map(|s| {
                if s.anchor != s.head {
                    let (a, b) = (s.anchor.char_index, s.head.char_index);
                    (a.min(b), a.max(b))
                } else {
                    // 空：按方向删 n
                    let ci = s.head.char_index.min(len);
                    if n < 0 {
                        let mut start = TextOffset { char_index: ci };
                        self.move_cursor_left(&mut start, n.unsigned_abs());
                        (start.char_index, ci)
                    } else {
                        let mut end = TextOffset { char_index: ci };
                        self.move_cursor_right(&mut end, n as usize);
                        (ci, end.char_index)
                    }
                }
            })
            .collect();
        let normalized = merge_ranges(selection_ranges.clone());
        self.apply_text_edits(
            normalized
                .iter()
                .map(|&(start, end)| TextEdit::new(start..end, ""))
                .collect(),
        )
        .expect("valid selection deletion");
        // 2) 更新每个 selection
        for (sel, (target, _)) in selections.all_mut().zip(selection_ranges) {
            let mut deleted_before = 0;
            sel.head.char_index = target;
            for &(start, end) in &normalized {
                if target < start {
                    break;
                }
                if target <= end {
                    sel.head.char_index = start - deleted_before;
                    break;
                }
                deleted_before += end - start;
                sel.head.char_index = target - deleted_before;
            }
            self.clamp_offset(&mut sel.head);
            Self::collapse_to_head(sel);
        }
    }

    pub fn delete_inclusive_selection_at_selections(&mut self, selections: &mut Selections) {
        self.reconcile_selections(selections);
        for selection in selections.all_mut() {
            let end = if selection.anchor.char_index > selection.head.char_index {
                &mut selection.anchor
            } else {
                &mut selection.head
            };
            self.move_cursor_right(end, 1);
        }
        self.delete_at_selections(selections, 1);
    }

    pub fn delete_word_backward_at_selections(&mut self, selections: &mut Selections) {
        self.reconcile_selections(selections);
        let starts: Vec<usize> = selections
            .all()
            .map(|selection| {
                if selection.anchor != selection.head {
                    selection.anchor.char_index.min(selection.head.char_index)
                } else {
                    backward_word_start(&self.rope, selection.head.char_index)
                }
            })
            .collect();
        let ranges: Vec<(usize, usize)> = selections
            .all()
            .zip(starts.iter().copied())
            .map(|(selection, start)| {
                let end = selection.anchor.char_index.max(selection.head.char_index);
                (start, end)
            })
            .collect();

        let normalized_ranges = merge_ranges(ranges);
        self.apply_text_edits(
            normalized_ranges
                .iter()
                .map(|&(start, end)| TextEdit::new(start..end, ""))
                .collect(),
        )
        .expect("valid word deletion");
        for (selection, start) in selections.all_mut().zip(starts) {
            let mut deleted_before = 0;
            selection.head.char_index = start;
            for &(range_start, range_end) in &normalized_ranges {
                if range_start <= start && start < range_end {
                    selection.head.char_index = range_start - deleted_before;
                    break;
                }
                if range_end <= start {
                    deleted_before += range_end - range_start;
                    selection.head.char_index = start - deleted_before;
                }
            }
            self.clamp_offset(&mut selection.head);
            Self::collapse_to_head(selection);
        }
    }

    pub fn delete_lines_at_selections(&mut self, selections: &mut Selections, lines: usize) {
        self.reconcile_selections(selections);
        let lines = lines.max(1);
        let max_row = self.rope.len_lines().saturating_sub(1);
        let rows: Vec<usize> = selections
            .all()
            .map(|selection| {
                self.rope
                    .char_to_line(selection.head.char_index.min(self.rope.len_chars()))
            })
            .collect();
        let ranges: Vec<(usize, usize)> = rows
            .iter()
            .map(|row| {
                let end_row = row.saturating_add(lines.saturating_sub(1)).min(max_row);
                let mut start = self.rope.line_to_char(*row);
                let end = if end_row < max_row {
                    self.rope.line_to_char(end_row + 1)
                } else {
                    if *row > 0 {
                        start = start.saturating_sub(line_break_width_before(&self.rope, *row));
                    }
                    self.rope.len_chars()
                };
                (start, end)
            })
            .collect();
        let normalized = merge_ranges(ranges);
        self.apply_text_edits(
            normalized
                .iter()
                .map(|&(start, end)| TextEdit::new(start..end, ""))
                .collect(),
        )
        .expect("valid line deletion");
        let new_max_row = self.rope.len_lines().saturating_sub(1);
        for (selection, row) in selections.all_mut().zip(rows) {
            selection.head.char_index = self.rope.line_to_char(row.min(new_max_row));
            self.clamp_offset(&mut selection.head);
            Self::collapse_to_head(selection);
        }
    }

    /// 删除每个 selection 的 anchor/head 所触及的完整逻辑行（两端行都包含）。
    pub fn delete_selected_lines_at_selections(&mut self, selections: &mut Selections) {
        self.reconcile_selections(selections);
        let max_row = self.rope.len_lines().saturating_sub(1);
        let row_ranges: Vec<(usize, usize)> = selections
            .all()
            .map(|selection| {
                let anchor_row = self
                    .rope
                    .char_to_line(selection.anchor.char_index.min(self.rope.len_chars()));
                let head_row = self
                    .rope
                    .char_to_line(selection.head.char_index.min(self.rope.len_chars()));
                (anchor_row.min(head_row), anchor_row.max(head_row))
            })
            .collect();
        let ranges: Vec<(usize, usize)> = row_ranges
            .iter()
            .map(|(start_row, end_row)| {
                let mut start = self.rope.line_to_char(*start_row);
                let end = if *end_row < max_row {
                    self.rope.line_to_char(end_row + 1)
                } else {
                    if *start_row > 0 {
                        start =
                            start.saturating_sub(line_break_width_before(&self.rope, *start_row));
                    }
                    self.rope.len_chars()
                };
                (start, end)
            })
            .collect();
        let normalized = merge_ranges(ranges);
        self.apply_text_edits(
            normalized
                .iter()
                .map(|&(start, end)| TextEdit::new(start..end, ""))
                .collect(),
        )
        .expect("valid selected-line deletion");
        let new_max_row = self.rope.len_lines().saturating_sub(1);
        for (selection, (start_row, _)) in selections.all_mut().zip(row_ranges) {
            selection.head.char_index = self.rope.line_to_char(start_row.min(new_max_row));
            self.clamp_offset(&mut selection.head);
            Self::collapse_to_head(selection);
        }
    }

    pub fn delete_to_line_start_at_selections(&mut self, selections: &mut Selections) {
        self.reconcile_selections(selections);
        let ranges: Vec<(usize, usize)> = selections
            .all()
            .map(|s| {
                if s.anchor != s.head {
                    let (a, b) = (s.anchor.char_index, s.head.char_index);
                    (a.min(b), a.max(b))
                } else {
                    let row = self
                        .rope
                        .char_to_line(s.head.char_index.min(self.rope.len_chars()));
                    let line_start = self.rope.line_to_char(row);
                    (line_start, s.head.char_index)
                }
            })
            .collect();
        let sorted = merge_ranges(ranges.clone());
        self.apply_text_edits(
            sorted
                .iter()
                .map(|&(start, end)| TextEdit::new(start..end, ""))
                .collect(),
        )
        .expect("valid deletion to line start");
        for (sel, (start, _)) in selections.all_mut().zip(ranges.iter()) {
            let mut deleted_before = 0;
            for &(r_start, r_end) in &sorted {
                if r_end <= *start {
                    deleted_before += r_end - r_start;
                }
            }
            sel.head.char_index = start - deleted_before;
            self.clamp_offset(&mut sel.head);
            Self::collapse_to_head(sel);
        }
    }

    pub fn delete_to_line_end_at_selections(&mut self, selections: &mut Selections) {
        self.reconcile_selections(selections);
        let ranges: Vec<(usize, usize)> = selections
            .all()
            .map(|s| {
                if s.anchor != s.head {
                    let (a, b) = (s.anchor.char_index, s.head.char_index);
                    (a.min(b), a.max(b))
                } else {
                    let row = self
                        .rope
                        .char_to_line(s.head.char_index.min(self.rope.len_chars()));
                    let end = line_end_insert(&self.rope, row);
                    (s.head.char_index.min(end), end)
                }
            })
            .collect();
        let sorted = merge_ranges(ranges.clone());
        self.apply_text_edits(
            sorted
                .iter()
                .map(|&(start, end)| TextEdit::new(start..end, ""))
                .collect(),
        )
        .expect("valid deletion to line end");
        for (sel, (start, _end)) in selections.all_mut().zip(ranges.iter()) {
            let mut deleted_before = 0;
            for &(r_start, r_end) in &sorted {
                if r_end <= *start {
                    deleted_before += r_end - r_start;
                }
            }
            sel.head.char_index = start - deleted_before;
            self.clamp_offset(&mut sel.head);
            Self::collapse_to_head(sel);
        }
    }

    pub fn join_lines_at_selections(&mut self, selections: &mut Selections) {
        self.reconcile_selections(selections);
        let max_row = self.rope.len_lines().saturating_sub(1);
        let mut joins: Vec<Option<(usize, usize, usize)>> = selections
            .all()
            .map(|s| {
                let row = self
                    .rope
                    .char_to_line(s.head.char_index.min(self.rope.len_chars()));
                if row >= max_row {
                    return None;
                }
                let newline_pos = self.rope.line_to_char(row) + line_content_len(&self.rope, row);
                let next_line_start = self.rope.line_to_char(row + 1);
                let next_row = row + 1;
                let next_content_end = next_line_start + line_content_len(&self.rope, next_row);
                let mut strip_end = next_line_start;
                while strip_end < next_content_end && self.rope.char(strip_end).is_whitespace() {
                    strip_end = next_boundary(&self.rope, strip_end);
                }
                Some((newline_pos, strip_end, next_line_start))
            })
            .collect::<Vec<_>>();
        joins.retain(|j| j.is_some());
        let joins: Vec<(usize, usize, usize)> = joins.into_iter().map(|j| j.unwrap()).collect();
        // Remove in reverse: delete [next_content_start, next_line_start) (leading ws) then remove newline
        // Simpler: remove range [newline_pos, next_content_start + ws_len) and insert " " at newline_pos
        // Actually: remove range [newline_pos, next_line_start + ws_len) then insert " " at newline_pos
        let mut sorted_joins = joins.clone();
        sorted_joins.sort_unstable_by_key(|join| join.0);
        sorted_joins.dedup_by_key(|join| join.0);
        self.apply_text_edits(
            sorted_joins
                .iter()
                .map(|&(newline_pos, strip_end, _)| TextEdit::new(newline_pos..strip_end, " "))
                .collect(),
        )
        .expect("valid line joins");
        for (sel, (newline_pos, _, _)) in selections.all_mut().zip(joins.iter()) {
            sel.head.char_index = *newline_pos;
            self.clamp_offset(&mut sel.head);
            Self::collapse_to_head(sel);
        }
    }

    pub fn toggle_case_at_selections(&mut self, selections: &mut Selections) {
        self.reconcile_selections(selections);
        let len = self.rope.len_chars();
        let ranges: Vec<(usize, usize, bool, bool)> = selections
            .all()
            .map(|s| {
                if s.anchor != s.head {
                    let (a, b) = (s.anchor.char_index, s.head.char_index);
                    (a.min(b), a.max(b), false, false)
                } else {
                    let ci = s.head.char_index.min(len);
                    let row = self.rope.char_to_line(ci);
                    let at_line_end = ci >= line_end_char(&self.rope, row);
                    if ci < len {
                        (ci, next_boundary(&self.rope, ci), true, at_line_end)
                    } else {
                        (ci, ci, true, true)
                    }
                }
            })
            .collect();
        let mut replacements = Vec::new();
        let mut targeted_graphemes = Vec::new();
        for (start, end, _, _) in &ranges {
            let mut index = *start;
            while index < *end {
                targeted_graphemes.push(index);
                index = next_boundary(&self.rope, index);
            }
        }
        targeted_graphemes.sort_unstable();
        targeted_graphemes.dedup();
        for index in targeted_graphemes {
            let end = next_boundary(&self.rope, index);
            let original = self.rope.slice(index..end).to_string();
            let flipped: String = original
                .chars()
                .flat_map(|character| {
                    if character.is_uppercase() {
                        character.to_lowercase().collect::<Vec<_>>()
                    } else if character.is_lowercase() {
                        character.to_uppercase().collect()
                    } else {
                        vec![character]
                    }
                })
                .collect();
            if flipped != original {
                replacements.push((index, end, flipped));
            }
        }
        let rebase = |offset: usize| {
            replacements
                .iter()
                .filter(|(_, end, _)| *end <= offset)
                .fold(offset as isize, |value, (start, end, text)| {
                    value + text.chars().count() as isize - (*end - *start) as isize
                }) as usize
        };
        let new_heads: Vec<usize> = ranges
            .iter()
            .map(|(start, end, collapsed, at_line_end)| {
                let new_start = rebase(*start);
                if *collapsed {
                    if *at_line_end {
                        new_start
                    } else {
                        rebase(*end)
                    }
                } else {
                    rebase(*end)
                }
            })
            .collect();
        self.apply_text_edits(
            replacements
                .iter()
                .map(|(start, end, flipped)| TextEdit::new(*start..*end, flipped.clone()))
                .collect(),
        )
        .expect("valid case replacements");
        for (sel, new_head) in selections.all_mut().zip(new_heads) {
            sel.head.char_index = new_head;
            self.clamp_offset(&mut sel.head);
            Self::collapse_to_head(sel);
        }
    }

    pub fn insert_new_line_below_at_selections(&mut self, selections: &mut Selections) {
        self.reconcile_selections(selections);
        let newline = self.preferred_line_ending();
        let newline_len = newline.chars().count();
        let insert_points: Vec<usize> = selections
            .all()
            .map(|s| {
                let row = self
                    .rope
                    .char_to_line(s.head.char_index.min(self.rope.len_chars()));
                self.rope.line_to_char(row) + line_content_len(&self.rope, row)
            })
            .collect();
        let mut sorted = insert_points.clone();
        sorted.sort_unstable();
        sorted.dedup();
        self.apply_text_edits(
            sorted
                .iter()
                .map(|&pos| TextEdit::new(pos..pos, newline))
                .collect(),
        )
        .expect("valid new-line insertion");
        for (sel, pos) in selections.all_mut().zip(insert_points.iter()) {
            sel.head.char_index = *pos + newline_len;
            self.clamp_offset(&mut sel.head);
            Self::collapse_to_head(sel);
        }
    }

    pub fn insert_new_line_above_at_selections(&mut self, selections: &mut Selections) {
        self.reconcile_selections(selections);
        let newline = self.preferred_line_ending();
        let insert_points: Vec<usize> = selections
            .all()
            .map(|s| {
                let row = self
                    .rope
                    .char_to_line(s.head.char_index.min(self.rope.len_chars()));
                self.rope.line_to_char(row)
            })
            .collect();
        let mut sorted = insert_points.clone();
        sorted.sort_unstable();
        sorted.dedup();
        self.apply_text_edits(
            sorted
                .iter()
                .map(|&pos| TextEdit::new(pos..pos, newline))
                .collect(),
        )
        .expect("valid new-line insertion");
        for (sel, pos) in selections.all_mut().zip(insert_points.iter()) {
            sel.head.char_index = *pos;
            self.clamp_offset(&mut sel.head);
            Self::collapse_to_head(sel);
        }
    }

    pub fn delete_line_content_at_selections(&mut self, selections: &mut Selections) {
        self.reconcile_selections(selections);
        let ranges: Vec<(usize, usize)> = selections
            .all()
            .map(|s| {
                let row = self
                    .rope
                    .char_to_line(s.head.char_index.min(self.rope.len_chars()));
                let line_start = self.rope.line_to_char(row);
                let content_end = line_start + line_content_len(&self.rope, row);
                (line_start, content_end)
            })
            .collect();
        let sorted = merge_ranges(ranges.clone());
        self.apply_text_edits(
            sorted
                .iter()
                .map(|&(start, end)| TextEdit::new(start..end, ""))
                .collect(),
        )
        .expect("valid line-content deletion");
        for (sel, (start, _)) in selections.all_mut().zip(ranges.iter()) {
            sel.head.char_index = *start;
            self.clamp_offset(&mut sel.head);
            Self::collapse_to_head(sel);
        }
    }

    pub fn change_lines_at_selections(&mut self, selections: &mut Selections, lines: usize) {
        self.reconcile_selections(selections);
        let max_row = self.rope.len_lines().saturating_sub(1);
        let lines = lines.max(1);
        let rows: Vec<usize> = selections
            .all()
            .map(|selection| {
                self.rope
                    .char_to_line(selection.head.char_index.min(self.rope.len_chars()))
            })
            .collect();
        let destinations: Vec<usize> = rows
            .iter()
            .map(|row| self.rope.line_to_char(*row))
            .collect();
        let ranges: Vec<(usize, usize)> = rows
            .iter()
            .map(|row| {
                let end_row = row.saturating_add(lines - 1).min(max_row);
                (
                    self.rope.line_to_char(*row),
                    line_end_insert(&self.rope, end_row),
                )
            })
            .collect();
        let normalized = merge_ranges(ranges);
        self.apply_text_edits(
            normalized
                .iter()
                .map(|&(start, end)| TextEdit::new(start..end, ""))
                .collect(),
        )
        .expect("valid line change");
        let change = self.last_change.clone();
        for (selection, destination) in selections.all_mut().zip(destinations) {
            selection.head.char_index = change.as_ref().map_or(destination, |change| {
                change.map_position(destination, Affinity::Before)
            });
            self.clamp_offset(&mut selection.head);
            Self::collapse_to_head(selection);
        }
    }

    pub fn indent_lines_at_selections(
        &mut self,
        selections: &mut Selections,
        config: IndentationConfig,
    ) {
        self.reconcile_selections(selections);
        let Some(config) = config.validated() else {
            self.last_change = None;
            return;
        };
        let indent = if config.insert_spaces {
            " ".repeat(config.indent_width)
        } else {
            "\t".to_owned()
        };
        let edits = self
            .selected_line_blocks(selections)
            .into_iter()
            .flat_map(|block| block.start_row..=block.end_row)
            .map(|row| {
                let start = self.rope.line_to_char(row);
                TextEdit::new(start..start, indent.clone())
            })
            .collect();
        self.apply_line_edits_and_map_selections(selections, edits);
    }

    pub fn outdent_lines_at_selections(
        &mut self,
        selections: &mut Selections,
        config: IndentationConfig,
    ) {
        self.reconcile_selections(selections);
        let Some(config) = config.validated() else {
            self.last_change = None;
            return;
        };
        let width = config.indent_width;
        let edits = self
            .selected_line_blocks(selections)
            .into_iter()
            .flat_map(|block| block.start_row..=block.end_row)
            .filter_map(|row| {
                let start = self.rope.line_to_char(row);
                let end = start + line_content_len(&self.rope, row);
                if start < end && self.rope.char(start) == '\t' {
                    return Some(TextEdit::new(start..start + 1, ""));
                }
                let spaces = (start..end)
                    .take(width)
                    .take_while(|offset| self.rope.char(*offset) == ' ')
                    .count();
                (spaces > 0).then(|| TextEdit::new(start..start + spaces, ""))
            })
            .collect();
        self.apply_line_edits_and_map_selections(selections, edits);
    }

    pub fn duplicate_lines_at_selections(&mut self, selections: &mut Selections) {
        self.reconcile_selections(selections);
        let selection_blocks = self.selection_line_blocks(selections);
        let blocks = merge_line_blocks(selection_blocks.clone());
        let max_row = self.rope.len_lines().saturating_sub(1);
        let line_ending = self.preferred_line_ending();
        let mut inserted_before = 0;
        let mut targets = Vec::with_capacity(blocks.len());
        let mut edits = Vec::with_capacity(blocks.len());
        for block in blocks {
            let source = self.rope.slice(block.start..block.end).to_string();
            let (text, prefix) = if block.end_row == max_row {
                (
                    format!("{line_ending}{source}"),
                    line_ending.chars().count(),
                )
            } else {
                (source, 0)
            };
            let target = block.end + inserted_before + prefix;
            inserted_before += text.chars().count();
            edits.push(TextEdit::new(block.end..block.end, text));
            targets.push((block, target));
        }
        self.apply_text_edits(edits)
            .expect("valid line duplication");
        self.retarget_line_selections(selections, &selection_blocks, &targets);
    }

    pub fn move_lines_up_at_selections(&mut self, selections: &mut Selections) {
        self.reconcile_selections(selections);
        let selection_blocks = self.selection_line_blocks(selections);
        let blocks = merge_line_blocks(selection_blocks.clone());
        let max_row = self.rope.len_lines().saturating_sub(1);
        let mut targets = Vec::with_capacity(blocks.len());
        let mut edits = Vec::new();
        for block in blocks {
            if block.start_row == 0 {
                targets.push((block, block.start));
                continue;
            }
            let previous_row = block.start_row - 1;
            let previous_start = self.rope.line_to_char(previous_row);
            let previous = self.rope.slice(previous_start..block.start).to_string();
            let selected = self.rope.slice(block.start..block.end).to_string();
            let replacement = if block.end_row == max_row {
                let previous_content_end =
                    previous_start + line_content_len(&self.rope, previous_row);
                let previous_content = self
                    .rope
                    .slice(previous_start..previous_content_end)
                    .to_string();
                let separator = self
                    .rope
                    .slice(previous_content_end..block.start)
                    .to_string();
                format!("{selected}{separator}{previous_content}")
            } else {
                format!("{selected}{previous}")
            };
            edits.push(TextEdit::new(previous_start..block.end, replacement));
            targets.push((block, previous_start));
        }
        self.apply_text_edits(edits)
            .expect("valid upward line move");
        self.retarget_line_selections(selections, &selection_blocks, &targets);
    }

    pub fn move_lines_down_at_selections(&mut self, selections: &mut Selections) {
        self.reconcile_selections(selections);
        let selection_blocks = self.selection_line_blocks(selections);
        let blocks = merge_line_blocks(selection_blocks.clone());
        let max_row = self.rope.len_lines().saturating_sub(1);
        let mut targets = Vec::with_capacity(blocks.len());
        let mut edits = Vec::new();
        for block in blocks {
            if block.end_row == max_row {
                targets.push((block, block.start));
                continue;
            }
            let next_row = block.end_row + 1;
            let next_end = if next_row < max_row {
                self.rope.line_to_char(next_row + 1)
            } else {
                self.rope.len_chars()
            };
            let selected = self.rope.slice(block.start..block.end).to_string();
            let next = self.rope.slice(block.end..next_end).to_string();
            let (replacement, target) = if next_row == max_row {
                let selected_content_end = self.rope.line_to_char(block.end_row)
                    + line_content_len(&self.rope, block.end_row);
                let selected_content = self
                    .rope
                    .slice(block.start..selected_content_end)
                    .to_string();
                let separator = self.rope.slice(selected_content_end..block.end).to_string();
                let target = block.start + next.chars().count() + separator.chars().count();
                (format!("{next}{separator}{selected_content}"), target)
            } else {
                let target = block.start + next.chars().count();
                (format!("{next}{selected}"), target)
            };
            edits.push(TextEdit::new(block.start..next_end, replacement));
            targets.push((block, target));
        }
        self.apply_text_edits(edits)
            .expect("valid downward line move");
        self.retarget_line_selections(selections, &selection_blocks, &targets);
    }

    pub fn insert_newline_at_selections(
        &mut self,
        selections: &mut Selections,
        indent: &str,
        closing_indent: Option<&str>,
    ) {
        self.reconcile_selections(selections);
        let ranges = self.selection_ranges(selections);
        let newline = self.preferred_line_ending();
        let mut insert = format!("{newline}{indent}");
        if let Some(closing_indent) = closing_indent {
            insert.push_str(newline);
            insert.push_str(closing_indent);
        }
        let cursor = newline.chars().count() + indent.chars().count();
        self.apply_text_edits(
            ranges
                .iter()
                .map(|&(start, end)| TextEdit::new(start..end, insert.clone()))
                .collect(),
        )
        .expect("valid newline edits");
        let Some(change) = self.last_change.clone() else {
            return;
        };
        for selection in selections.all_mut() {
            let (start, end) = ordered_selection_range(selection);
            let &(range_start, _) = containing_range(&ranges, start, end);
            let target = change.map_position(range_start, Affinity::Before) + cursor;
            selection.anchor.char_index = target;
            selection.head.char_index = target;
        }
        self.reconcile_selections(selections);
    }

    pub fn toggle_line_comment_at_selections(
        &mut self,
        selections: &mut Selections,
        delimiter: &str,
    ) {
        if delimiter.is_empty() || delimiter.contains(['\r', '\n']) {
            return;
        }
        self.reconcile_selections(selections);
        let blocks = self.selected_line_blocks(selections);
        let delimiter_len = delimiter.chars().count();
        let rows = blocks
            .iter()
            .flat_map(|block| block.start_row..=block.end_row)
            .collect::<Vec<_>>();
        let positions = rows
            .iter()
            .map(|&row| self.first_nonblank_offset(row))
            .collect::<Vec<_>>();
        let uncomment = positions
            .iter()
            .all(|&position| self.slice_equals(position, position + delimiter_len, delimiter));
        let edits = positions
            .into_iter()
            .map(|position| {
                if uncomment {
                    let mut end = position + delimiter_len;
                    if self.slice_equals(end, end + 1, " ") {
                        end += 1;
                    }
                    TextEdit::new(position..end, "")
                } else {
                    TextEdit::new(position..position, format!("{delimiter} "))
                }
            })
            .collect();
        self.apply_line_edits_and_map_selections(selections, edits);
    }

    pub fn toggle_block_comment_at_selections(
        &mut self,
        selections: &mut Selections,
        open: &str,
        close: &str,
    ) {
        if !valid_pair(open, close) {
            return;
        }
        self.reconcile_selections(selections);
        let ranges = self.selection_ranges(selections);
        let open_len = open.chars().count();
        let close_len = close.chars().count();
        let replacements = ranges
            .iter()
            .map(|&(start, end)| {
                let unwrap = end.saturating_sub(start) >= open_len + close_len
                    && self.slice_equals(start, start + open_len, open)
                    && self.slice_equals(end - close_len, end, close);
                if unwrap {
                    let text = self
                        .rope
                        .slice(start + open_len..end - close_len)
                        .to_string();
                    (text, 0, end - start - open_len - close_len)
                } else {
                    let content = self.rope.slice(start..end).to_string();
                    (
                        format!("{open}{content}{close}"),
                        open_len,
                        open_len + end - start,
                    )
                }
            })
            .collect::<Vec<_>>();
        self.apply_replacements(selections, &ranges, replacements);
    }

    pub fn insert_pair_at_selections(
        &mut self,
        selections: &mut Selections,
        open: &str,
        close: &str,
    ) {
        if !valid_pair(open, close) {
            return;
        }
        self.reconcile_selections(selections);
        let ranges = self.selection_ranges(selections);
        let open_len = open.chars().count();
        let replacements = ranges
            .iter()
            .map(|&(start, end)| {
                let content = self.rope.slice(start..end).to_string();
                (
                    format!("{open}{content}{close}"),
                    open_len,
                    open_len + end - start,
                )
            })
            .collect();
        self.apply_replacements(selections, &ranges, replacements);
    }

    pub fn insert_closing_pair_at_selections(&mut self, selections: &mut Selections, close: &str) {
        if close.is_empty() || close.contains(['\r', '\n']) {
            return;
        }
        self.reconcile_selections(selections);
        let ranges = self.selection_ranges(selections);
        let close_len = close.chars().count();
        let actions = ranges
            .iter()
            .map(|&(start, end)| {
                let skip = start == end
                    && self.slice_equals(start, start + close_len, close)
                    && boundary_at_or_after(&self.rope, start + close_len) == start + close_len;
                (skip, start, end)
            })
            .collect::<Vec<_>>();
        let edits = actions
            .iter()
            .filter(|(skip, _, _)| !skip)
            .map(|&(_, start, end)| TextEdit::new(start..end, close))
            .collect::<Vec<_>>();
        if !edits.is_empty() {
            self.apply_text_edits(edits)
                .expect("valid closing-pair edits");
        } else {
            self.last_change = None;
        }
        let change = self.last_change.clone();
        for selection in selections.all_mut() {
            let (start, end) = ordered_selection_range(selection);
            let &(skip, range_start, _) = actions
                .iter()
                .find(|&&(_, candidate_start, candidate_end)| {
                    candidate_start <= start && candidate_end >= end
                })
                .expect("each selection belongs to one normalized range");
            let target = if skip {
                change.as_ref().map_or(range_start + close_len, |change| {
                    change.map_position(range_start + close_len, Affinity::After)
                })
            } else {
                change.as_ref().map_or(range_start + close_len, |change| {
                    change.map_position(range_start, Affinity::Before) + close_len
                })
            };
            selection.anchor.char_index = target;
            selection.head.char_index = target;
        }
        self.reconcile_selections(selections);
    }

    pub fn delete_pair_backward_at_selections(
        &mut self,
        selections: &mut Selections,
        open: &str,
        close: &str,
    ) {
        if !valid_pair(open, close) {
            self.delete_at_selections(selections, -1);
            return;
        }
        self.reconcile_selections(selections);
        let open_len = open.chars().count();
        let close_len = close.chars().count();
        let deletion_ranges = selections
            .all()
            .map(|selection| {
                let (start, end) = ordered_selection_range(selection);
                if start != end {
                    return (start, end);
                }
                let paired = start >= open_len
                    && self.slice_equals(start - open_len, start, open)
                    && self.slice_equals(start, start + close_len, close)
                    && boundary_at_or_before(&self.rope, start - open_len) == start - open_len
                    && boundary_at_or_after(&self.rope, start + close_len) == start + close_len;
                if paired {
                    (start - open_len, start + close_len)
                } else {
                    (previous_boundary(&self.rope, start), start)
                }
            })
            .collect::<Vec<_>>();
        let ranges = merge_ranges(deletion_ranges.clone());
        self.apply_text_edits(
            ranges
                .iter()
                .map(|&(start, end)| TextEdit::new(start..end, ""))
                .collect(),
        )
        .expect("valid paired-backspace edits");
        let change = self.last_change.clone();
        for (selection, (start, end)) in selections.all_mut().zip(deletion_ranges) {
            let &(range_start, _) = containing_range(&ranges, start, end);
            let target = change.as_ref().map_or(range_start, |change| {
                change.map_position(range_start, Affinity::Before)
            });
            selection.anchor.char_index = target;
            selection.head.char_index = target;
        }
        self.reconcile_selections(selections);
    }

    fn paste_charwise(
        &mut self,
        selections: &mut Selections,
        payload: &ClipboardPayload,
        placement: PastePlacement,
    ) {
        if payload.fragments.iter().all(String::is_empty) {
            self.last_change = None;
            return;
        }
        self.reconcile_selections(selections);
        let selection_ranges = selections
            .all()
            .map(|selection| {
                let (start, end) = ordered_selection_range(selection);
                if start != end || placement == PastePlacement::Before {
                    (start, end)
                } else {
                    let after = next_boundary(&self.rope, end);
                    (after, after)
                }
            })
            .collect::<Vec<_>>();
        let ranges = merge_ranges(selection_ranges.clone());
        let fragments = distributed_fragments(&payload.fragments, ranges.len())
            .into_iter()
            .map(|fragment| self.normalize_insert_text(&fragment))
            .collect::<Vec<_>>();
        self.apply_text_edits(
            ranges
                .iter()
                .zip(&fragments)
                .map(|(&(start, end), text)| TextEdit::new(start..end, text.clone()))
                .collect(),
        )
        .expect("valid character-wise paste edits");
        let Some(change) = self.last_change.clone() else {
            return;
        };
        for (selection, (start, end)) in selections.all_mut().zip(selection_ranges) {
            let index = ranges
                .iter()
                .position(|&(range_start, range_end)| range_start <= start && range_end >= end)
                .expect("each selection belongs to one paste range");
            let target = change.map_position(ranges[index].0, Affinity::Before)
                + fragments[index].chars().count();
            selection.anchor.char_index = target;
            selection.head.char_index = target;
        }
        self.reconcile_selections(selections);
    }

    fn paste_linewise(
        &mut self,
        selections: &mut Selections,
        payload: &ClipboardPayload,
        placement: PastePlacement,
    ) {
        self.reconcile_selections(selections);
        let selected = self.selection_line_blocks(selections);
        let blocks = merge_line_blocks(selected.clone());
        let fragments = distributed_fragments(&payload.fragments, blocks.len());
        let max_row = self.rope.len_lines().saturating_sub(1);
        let newline = self.preferred_line_ending();
        let insertions = blocks
            .iter()
            .zip(fragments)
            .map(|(block, fragment)| {
                let fragment = self.normalize_insert_text(&fragment);
                match placement {
                    PastePlacement::Before => {
                        let text = with_trailing_line_ending(fragment, newline);
                        (block.start, text, 0)
                    }
                    PastePlacement::After if block.end_row < max_row => {
                        let text = with_trailing_line_ending(fragment, newline);
                        (block.end, text, 0)
                    }
                    PastePlacement::After if self.rope.len_chars() == 0 => {
                        let text = without_trailing_line_ending(fragment);
                        (0, text, 0)
                    }
                    PastePlacement::After => {
                        let fragment = without_trailing_line_ending(fragment);
                        let prefix = newline.chars().count();
                        (block.end, format!("{newline}{fragment}"), prefix)
                    }
                }
            })
            .collect::<Vec<_>>();
        self.apply_text_edits(
            insertions
                .iter()
                .map(|(at, text, _)| TextEdit::new(*at..*at, text.clone()))
                .collect(),
        )
        .expect("valid line-wise paste edits");
        let Some(change) = self.last_change.clone() else {
            return;
        };
        for (selection, selected_block) in selections.all_mut().zip(selected) {
            let index = blocks
                .iter()
                .position(|block| {
                    block.start_row <= selected_block.start_row
                        && block.end_row >= selected_block.end_row
                })
                .expect("each selection belongs to one paste line block");
            let (at, _, relative) = &insertions[index];
            let target = change.map_position(*at, Affinity::Before) + relative;
            selection.anchor.char_index = target;
            selection.head.char_index = target;
        }
        self.reconcile_selections(selections);
    }

    fn selection_line_blocks(&self, selections: &Selections) -> Vec<LineBlock> {
        selections
            .all()
            .map(|selection| {
                let anchor = selection.anchor.char_index.min(self.rope.len_chars());
                let head = selection.head.char_index.min(self.rope.len_chars());
                let anchor_row = self.rope.char_to_line(anchor);
                let head_row = self.rope.char_to_line(head);
                self.line_block(anchor_row.min(head_row), anchor_row.max(head_row))
            })
            .collect()
    }

    fn selected_line_blocks(&self, selections: &Selections) -> Vec<LineBlock> {
        merge_line_blocks(self.selection_line_blocks(selections))
    }

    fn selection_ranges(&self, selections: &Selections) -> Vec<(usize, usize)> {
        merge_ranges(selections.all().map(ordered_selection_range).collect())
    }

    fn apply_replacements(
        &mut self,
        selections: &mut Selections,
        ranges: &[(usize, usize)],
        replacements: Vec<(String, usize, usize)>,
    ) {
        self.apply_text_edits(
            ranges
                .iter()
                .zip(&replacements)
                .map(|(&(start, end), (text, _, _))| TextEdit::new(start..end, text.clone()))
                .collect(),
        )
        .expect("valid selection replacement edits");
        let Some(change) = self.last_change.clone() else {
            return;
        };
        for selection in selections.all_mut() {
            let forward = selection.anchor.char_index <= selection.head.char_index;
            let (start, end) = ordered_selection_range(selection);
            let index = ranges
                .iter()
                .position(|&(range_start, range_end)| range_start <= start && range_end >= end)
                .expect("each selection belongs to one normalized range");
            let range_start = ranges[index].0;
            let (_, relative_start, relative_end) = &replacements[index];
            let mapped = change.map_position(range_start, Affinity::Before);
            let (anchor, head) = if forward {
                (mapped + relative_start, mapped + relative_end)
            } else {
                (mapped + relative_end, mapped + relative_start)
            };
            selection.anchor.char_index = anchor;
            selection.head.char_index = head;
        }
        self.reconcile_selections(selections);
    }

    fn first_nonblank_offset(&self, row: usize) -> usize {
        let start = self.rope.line_to_char(row);
        let end = line_end_insert(&self.rope, row);
        (start..end)
            .find(|&offset| !matches!(self.rope.char(offset), ' ' | '\t'))
            .unwrap_or(end)
    }

    fn slice_equals(&self, start: usize, end: usize, expected: &str) -> bool {
        end <= self.rope.len_chars()
            && end.saturating_sub(start) == expected.chars().count()
            && self.rope.slice(start..end).chars().eq(expected.chars())
    }

    fn line_block(&self, start_row: usize, end_row: usize) -> LineBlock {
        let max_row = self.rope.len_lines().saturating_sub(1);
        LineBlock {
            start_row,
            end_row,
            start: self.rope.line_to_char(start_row),
            end: if end_row < max_row {
                self.rope.line_to_char(end_row + 1)
            } else {
                self.rope.len_chars()
            },
        }
    }

    fn apply_line_edits_and_map_selections(
        &mut self,
        selections: &mut Selections,
        edits: Vec<TextEdit>,
    ) {
        self.apply_text_edits(edits)
            .expect("valid indentation edits");
        let Some(change) = self.last_change.clone() else {
            return;
        };
        for selection in selections.all_mut() {
            selection.anchor.char_index =
                change.map_position(selection.anchor.char_index, Affinity::After);
            selection.head.char_index =
                change.map_position(selection.head.char_index, Affinity::After);
        }
        self.reconcile_selections(selections);
    }

    fn retarget_line_selections(
        &self,
        selections: &mut Selections,
        selection_blocks: &[LineBlock],
        targets: &[(LineBlock, usize)],
    ) {
        for (selection, selected) in selections.all_mut().zip(selection_blocks) {
            let (block, target) = targets
                .iter()
                .find(|(block, _)| {
                    block.start_row <= selected.start_row && block.end_row >= selected.end_row
                })
                .expect("each selection belongs to one merged line block");
            selection.anchor.char_index =
                target + selection.anchor.char_index.saturating_sub(block.start);
            selection.head.char_index =
                target + selection.head.char_index.saturating_sub(block.start);
        }
        self.reconcile_selections(selections);
    }

    fn preferred_line_ending(&self) -> &'static str {
        for row in 0..self.rope.len_lines().saturating_sub(1) {
            let line = self.rope.line(row);
            let len = line.len_chars();
            if len >= 2 && line.char(len - 2) == '\r' && line.char(len - 1) == '\n' {
                return "\r\n";
            }
            if len >= 1 && line.char(len - 1) == '\n' {
                return "\n";
            }
        }
        "\n"
    }

    fn normalize_insert_text(&self, text: &str) -> String {
        if self.preferred_line_ending() == "\n" || !text.contains('\n') {
            return text.to_string();
        }
        let mut normalized = String::with_capacity(text.len());
        let mut previous = None;
        for ch in text.chars() {
            if ch == '\n' && previous != Some('\r') {
                normalized.push('\r');
            }
            normalized.push(ch);
            previous = Some(ch);
        }
        normalized
    }

    pub fn delete_target_at_selections(&mut self, selections: &mut Selections, target: TextTarget) {
        if let TextTarget::Lines { count } = target {
            self.delete_lines_at_selections(selections, count);
            return;
        }

        self.reconcile_selections(selections);
        let destinations_and_ranges: Vec<(usize, (usize, usize))> = selections
            .all()
            .map(|selection| {
                let outcome = resolve_operator(
                    &self.rope,
                    selection.head.char_index,
                    OperatorCommand {
                        operator: TextOperator::Delete,
                        target,
                    },
                );
                let TextRange::Charwise(range) = outcome.covered else {
                    unreachable!("motion target resolves to a charwise range")
                };
                (outcome.destination, (range.start, range.end))
            })
            .collect();
        let normalized = merge_ranges(
            destinations_and_ranges
                .iter()
                .map(|(_, range)| *range)
                .collect(),
        );
        self.apply_text_edits(
            normalized
                .iter()
                .map(|&(start, end)| TextEdit::new(start..end, ""))
                .collect(),
        )
        .expect("valid operator ranges");
        let change = self.last_change.clone();
        for (selection, (destination, _)) in selections.all_mut().zip(destinations_and_ranges) {
            let mapped = change.as_ref().map_or(destination, |change| {
                change.map_position(destination, Affinity::Before)
            });
            selection.anchor.char_index = mapped;
            selection.head.char_index = mapped;
            self.clamp_offset(&mut selection.head);
            selection.anchor = selection.head;
        }
    }
}

impl Default for Buffer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::selection::{Selection, Selections};
    use tempfile::tempdir;

    fn cur(idx: usize) -> TextOffset {
        TextOffset { char_index: idx }
    }

    fn single_sel(at: TextOffset) -> Selections {
        Selections::single(Selection::collapsed(at))
    }

    fn selection_at(buffer: &Buffer, char_index: usize) -> Selections {
        let mut cursor = TextOffset::origin();
        cursor.char_index = char_index;
        buffer.clamp_offset(&mut cursor);
        Selections::single(Selection::collapsed(cursor))
    }

    #[test]
    fn new_buffer_is_empty() {
        let b = Buffer::new();
        assert_eq!(b.len_lines(), 1);
        assert!(!b.modified());
        assert!(b.path().is_none());
        assert_eq!(b.backing_state(), BufferBackingState::Untitled);
        assert_eq!(b.save_state(), SaveState::Idle);
    }

    #[test]
    fn text_point_is_derived_and_clamps_out_of_range_offsets() {
        let mut buffer = Buffer::new();
        buffer.insert_at_selections(&mut single_sel(TextOffset::origin()), "hello\nab");

        assert_eq!(
            buffer.text_point(TextOffset { char_index: 8 }),
            TextPoint { row: 1, col: 2 }
        );
        assert_eq!(
            buffer.text_point(TextOffset { char_index: 999 }),
            TextPoint { row: 1, col: 2 }
        );
    }

    #[test]
    fn mark_saved_clears_modified() {
        let mut b = Buffer::new();
        b.insert_char(0, 'x');
        assert!(b.modified());
        b.mark_saved(b.state_id());
        assert!(!b.modified());
    }

    #[test]
    fn stale_revision_does_not_clear_modified() {
        let mut b = Buffer::new();
        b.insert_char(0, 'x');
        let saved_state = b.state_id();
        b.insert_char(1, 'y');

        assert!(!b.mark_saved(saved_state));
        assert!(b.modified());
    }

    #[test]
    fn text_change_mapping_preserves_backward_selection_direction() {
        let mut buffer = Buffer::new();
        buffer.insert_at_selections(&mut single_sel(TextOffset::origin()), "abc");
        let mut other = Selections::single(Selection {
            anchor: cur(3),
            head: cur(1),
        });
        let mut editing = single_sel(cur(1));
        buffer.insert_at_selections(&mut editing, "X");
        let change = buffer.take_last_change().unwrap();

        assert!(buffer.transform_selections(&mut other, &change));
        assert_eq!(other.primary().anchor, cur(4));
        assert_eq!(other.primary().head, cur(2));
    }

    #[test]
    fn insert_at_selections_single() {
        let mut b = Buffer::new();
        let mut s = single_sel(TextOffset::origin());
        b.insert_at_selections(&mut s, "hi");
        assert_eq!(b.slice().to_string(), "hi");
        assert_eq!(s.primary().head().char_index, 2);
        assert_eq!(
            b.text_point(s.primary().head()),
            TextPoint { row: 0, col: 2 }
        );
        assert_eq!(s.primary().anchor, s.primary().head()); // collapsed 守恒
    }

    #[test]
    fn delete_at_selections_left() {
        let mut b = Buffer::new();
        let mut s = single_sel(cur(3));
        b.delete_at_selections(&mut s, -1);
        assert_eq!(b.slice().to_string(), "");
        b.insert_char(0, 'a');
        b.insert_char(1, 'b');
        let mut s2 = single_sel(cur(2));
        b.delete_at_selections(&mut s2, -1);
        assert_eq!(b.slice().to_string(), "a");
        assert_eq!(s2.primary().anchor, s2.primary().head());
    }

    #[test]
    fn delete_word_backward_removes_unicode_word() {
        let mut buffer = Buffer::new();
        for (index, ch) in "caf\u{00e9}_42".chars().enumerate() {
            buffer.insert_char(index, ch);
        }
        let mut selections = selection_at(&buffer, 7);

        buffer.delete_word_backward_at_selections(&mut selections);

        assert_eq!(buffer.slice().to_string(), "");
        assert_eq!(selections.primary().head().char_index, 0);
    }

    #[test]
    fn delete_word_backward_removes_one_punctuation_unit() {
        let mut buffer = Buffer::new();
        for (index, ch) in "alpha!!".chars().enumerate() {
            buffer.insert_char(index, ch);
        }
        let mut selections = selection_at(&buffer, 7);

        buffer.delete_word_backward_at_selections(&mut selections);

        assert_eq!(buffer.slice().to_string(), "alpha!");
        assert_eq!(selections.primary().head().char_index, 6);
    }

    #[test]
    fn delete_word_backward_skips_whitespace_and_crosses_newline() {
        let mut buffer = Buffer::new();
        for (index, ch) in "alpha \n beta".chars().enumerate() {
            buffer.insert_char(index, ch);
        }
        let mut selections = selection_at(&buffer, 8);

        buffer.delete_word_backward_at_selections(&mut selections);

        assert_eq!(buffer.slice().to_string(), "beta");
        assert_eq!(selections.primary().head().char_index, 0);
    }

    #[test]
    fn delete_word_backward_deletes_non_empty_selection() {
        let mut buffer = Buffer::new();
        for (index, ch) in "alpha beta".chars().enumerate() {
            buffer.insert_char(index, ch);
        }
        let mut selections = selection_at(&buffer, 6);
        selections.primary_mut().head = selection_at(&buffer, 10).primary().head;

        buffer.delete_word_backward_at_selections(&mut selections);

        assert_eq!(buffer.slice().to_string(), "alpha ");
        assert_eq!(selections.primary().head().char_index, 6);
        assert_eq!(selections.primary().anchor, selections.primary().head());
    }

    #[test]
    fn delete_word_backward_deletes_backward_selection() {
        let mut buffer = Buffer::new();
        for (index, ch) in "alpha beta".chars().enumerate() {
            buffer.insert_char(index, ch);
        }
        let mut selections = selection_at(&buffer, 10);
        selections.primary_mut().head = selection_at(&buffer, 6).primary().head;

        buffer.delete_word_backward_at_selections(&mut selections);

        assert_eq!(buffer.slice().to_string(), "alpha ");
        assert_eq!(selections.primary().head().char_index, 6);
        assert_eq!(selections.primary().anchor, selections.primary().head());
    }

    #[test]
    fn delete_word_backward_rebases_disjoint_non_empty_selection_starts() {
        let mut buffer = Buffer::new();
        for (index, ch) in "alpha beta gamma".chars().enumerate() {
            buffer.insert_char(index, ch);
        }
        let mut selections = Selections::from_parts(
            vec![
                Selection {
                    anchor: selection_at(&buffer, 0).primary().head(),
                    head: selection_at(&buffer, 5).primary().head(),
                },
                Selection {
                    anchor: selection_at(&buffer, 11).primary().head(),
                    head: selection_at(&buffer, 16).primary().head(),
                },
            ],
            0,
        );

        buffer.delete_word_backward_at_selections(&mut selections);

        assert_eq!(buffer.slice().to_string(), " beta ");
        assert_eq!(
            selections
                .all()
                .map(|selection| selection.head.char_index)
                .collect::<Vec<_>>(),
            vec![0, 6]
        );
        assert!(
            selections
                .all()
                .all(|selection| selection.anchor == selection.head)
        );
    }

    #[test]
    fn delete_word_backward_merges_overlapping_non_empty_selections() {
        let mut buffer = Buffer::new();
        for (index, ch) in "alpha beta".chars().enumerate() {
            buffer.insert_char(index, ch);
        }
        let mut selections = Selections::from_parts(
            vec![
                Selection {
                    anchor: selection_at(&buffer, 0).primary().head(),
                    head: selection_at(&buffer, 7).primary().head(),
                },
                Selection {
                    anchor: selection_at(&buffer, 6).primary().head(),
                    head: selection_at(&buffer, 10).primary().head(),
                },
            ],
            0,
        );

        buffer.delete_word_backward_at_selections(&mut selections);

        assert_eq!(buffer.slice().to_string(), "");
        assert_eq!(
            selections
                .all()
                .map(|selection| selection.head.char_index)
                .collect::<Vec<_>>(),
            vec![0, 0]
        );
        assert!(
            selections
                .all()
                .all(|selection| selection.anchor == selection.head)
        );
    }

    #[test]
    fn delete_to_line_start_removes_from_line_start_to_cursor() {
        let mut buffer = Buffer::new();
        for (i, ch) in "foo\nbar".chars().enumerate() {
            buffer.insert_char(i, ch);
        }
        let mut s = selection_at(&buffer, 5); // on 'a' of line 2
        buffer.delete_to_line_start_at_selections(&mut s);
        assert_eq!(buffer.slice().to_string(), "foo\nar");
        assert_eq!(s.primary().head().char_index, 4); // line 2 start
    }

    #[test]
    fn delete_to_line_start_at_line_start_is_noop() {
        let mut buffer = Buffer::new();
        for (i, ch) in "foo".chars().enumerate() {
            buffer.insert_char(i, ch);
        }
        let mut s = selection_at(&buffer, 0);
        buffer.delete_to_line_start_at_selections(&mut s);
        assert_eq!(buffer.slice().to_string(), "foo");
        assert_eq!(s.primary().head().char_index, 0);
    }

    #[test]
    fn delete_to_line_start_non_empty_selection_deletes_range() {
        let mut buffer = Buffer::new();
        for (i, ch) in "abcdef".chars().enumerate() {
            buffer.insert_char(i, ch);
        }
        let mut s = selection_at(&buffer, 2);
        s.primary_mut().head = selection_at(&buffer, 5).primary().head;
        buffer.delete_to_line_start_at_selections(&mut s);
        assert_eq!(buffer.slice().to_string(), "abf");
        assert_eq!(s.primary().head().char_index, 2);
    }

    #[test]
    fn delete_to_line_end_removes_from_cursor_to_line_end() {
        let mut buffer = Buffer::new();
        for (i, ch) in "foo\nbar".chars().enumerate() {
            buffer.insert_char(i, ch);
        }
        let mut s = selection_at(&buffer, 1); // on first 'o'
        buffer.delete_to_line_end_at_selections(&mut s);
        assert_eq!(buffer.slice().to_string(), "f\nbar");
        assert_eq!(s.primary().head().char_index, 1);
    }

    #[test]
    fn delete_to_line_end_at_line_end_is_noop() {
        let mut buffer = Buffer::new();
        for (i, ch) in "foo".chars().enumerate() {
            buffer.insert_char(i, ch);
        }
        let mut s = selection_at(&buffer, 3); // past end
        buffer.delete_to_line_end_at_selections(&mut s);
        assert_eq!(buffer.slice().to_string(), "foo");
        assert_eq!(s.primary().head().char_index, 3);
    }

    #[test]
    fn delete_to_line_end_non_empty_selection_deletes_range() {
        let mut buffer = Buffer::new();
        for (i, ch) in "abcdef".chars().enumerate() {
            buffer.insert_char(i, ch);
        }
        let mut s = selection_at(&buffer, 2);
        s.primary_mut().head = selection_at(&buffer, 4).primary().head;
        buffer.delete_to_line_end_at_selections(&mut s);
        assert_eq!(buffer.slice().to_string(), "abef");
        assert_eq!(s.primary().head().char_index, 2);
    }

    #[test]
    fn join_lines_merges_two_lines_with_space() {
        let mut buffer = Buffer::new();
        for (i, ch) in "foo\nbar".chars().enumerate() {
            buffer.insert_char(i, ch);
        }
        let mut s = selection_at(&buffer, 0);
        buffer.join_lines_at_selections(&mut s);
        assert_eq!(buffer.slice().to_string(), "foo bar");
        assert_eq!(s.primary().head().char_index, 3); // at the space
    }

    #[test]
    fn join_lines_strips_next_line_leading_whitespace() {
        let mut buffer = Buffer::new();
        for (i, ch) in "foo\n  bar".chars().enumerate() {
            buffer.insert_char(i, ch);
        }
        let mut s = selection_at(&buffer, 0);
        buffer.join_lines_at_selections(&mut s);
        assert_eq!(buffer.slice().to_string(), "foo bar");
        assert_eq!(s.primary().head().char_index, 3);
    }

    #[test]
    fn join_lines_on_last_line_is_noop() {
        let mut buffer = Buffer::new();
        for (i, ch) in "foo\nbar".chars().enumerate() {
            buffer.insert_char(i, ch);
        }
        let mut s = selection_at(&buffer, 4); // on 'b' of last line
        buffer.join_lines_at_selections(&mut s);
        assert_eq!(buffer.slice().to_string(), "foo\nbar");
    }

    #[test]
    fn toggle_case_flips_char_and_advances() {
        let mut buffer = Buffer::new();
        for (i, ch) in "aBc".chars().enumerate() {
            buffer.insert_char(i, ch);
        }
        let mut s = selection_at(&buffer, 0);
        buffer.toggle_case_at_selections(&mut s);
        assert_eq!(buffer.slice().to_string(), "ABc");
        assert_eq!(s.primary().head().char_index, 1);
    }

    #[test]
    fn toggle_case_at_line_end_does_not_advance() {
        let mut buffer = Buffer::new();
        for (i, ch) in "ab".chars().enumerate() {
            buffer.insert_char(i, ch);
        }
        let mut s = selection_at(&buffer, 1);
        buffer.toggle_case_at_selections(&mut s);
        assert_eq!(buffer.slice().to_string(), "aB");
        assert_eq!(s.primary().head().char_index, 1);
    }

    #[test]
    fn toggle_case_non_empty_selection_flips_all_in_range() {
        let mut buffer = Buffer::new();
        for (i, ch) in "abc".chars().enumerate() {
            buffer.insert_char(i, ch);
        }
        let mut s = selection_at(&buffer, 0);
        s.primary_mut().head = selection_at(&buffer, 3).primary().head;
        buffer.toggle_case_at_selections(&mut s);
        assert_eq!(buffer.slice().to_string(), "ABC");
        assert_eq!(s.primary().head().char_index, 3);
    }

    #[test]
    fn insert_new_line_below_adds_line_and_moves_cursor() {
        let mut buffer = Buffer::new();
        for (i, ch) in "foo".chars().enumerate() {
            buffer.insert_char(i, ch);
        }
        let mut s = selection_at(&buffer, 1);
        buffer.insert_new_line_below_at_selections(&mut s);
        assert_eq!(buffer.slice().to_string(), "foo\n");
        assert_eq!(s.primary().head().char_index, 4); // start of new line
    }

    #[test]
    fn insert_new_line_below_multiline() {
        let mut buffer = Buffer::new();
        for (i, ch) in "foo\nbar".chars().enumerate() {
            buffer.insert_char(i, ch);
        }
        let mut s = selection_at(&buffer, 1); // on 'o' of line 1
        buffer.insert_new_line_below_at_selections(&mut s);
        assert_eq!(buffer.slice().to_string(), "foo\n\nbar");
        assert_eq!(s.primary().head().char_index, 4); // new empty line
    }

    #[test]
    fn insert_new_line_above_adds_line_and_keeps_cursor() {
        let mut buffer = Buffer::new();
        for (i, ch) in "foo".chars().enumerate() {
            buffer.insert_char(i, ch);
        }
        let mut s = selection_at(&buffer, 1);
        buffer.insert_new_line_above_at_selections(&mut s);
        assert_eq!(buffer.slice().to_string(), "\nfoo");
        assert_eq!(s.primary().head().char_index, 0); // start of new line
    }

    #[test]
    fn insert_new_line_above_multiline() {
        let mut buffer = Buffer::new();
        for (i, ch) in "foo\nbar".chars().enumerate() {
            buffer.insert_char(i, ch);
        }
        let mut s = selection_at(&buffer, 5); // on 'a' of line 2
        buffer.insert_new_line_above_at_selections(&mut s);
        assert_eq!(buffer.slice().to_string(), "foo\n\nbar");
        assert_eq!(s.primary().head().char_index, 4); // new empty line start
    }

    #[test]
    fn delete_line_content_clears_line_keeps_newline() {
        let mut buffer = Buffer::new();
        for (i, ch) in "foo\nbar".chars().enumerate() {
            buffer.insert_char(i, ch);
        }
        let mut s = selection_at(&buffer, 1); // on 'o' of line 1
        buffer.delete_line_content_at_selections(&mut s);
        assert_eq!(buffer.slice().to_string(), "\nbar");
        assert_eq!(s.primary().head().char_index, 0);
    }

    #[test]
    fn delete_line_content_last_line_no_newline() {
        let mut buffer = Buffer::new();
        for (i, ch) in "foo\nbar".chars().enumerate() {
            buffer.insert_char(i, ch);
        }
        let mut s = selection_at(&buffer, 5); // on 'a' of line 2
        buffer.delete_line_content_at_selections(&mut s);
        assert_eq!(buffer.slice().to_string(), "foo\n");
        assert_eq!(s.primary().head().char_index, 4);
    }

    #[test]
    fn delete_last_line_without_trailing_newline_removes_full_crlf() {
        let mut buffer = Buffer::new();
        for (i, ch) in "a\r\nb".chars().enumerate() {
            buffer.insert_char(i, ch);
        }
        let mut s = selection_at(&buffer, 3);

        buffer.delete_lines_at_selections(&mut s, 1);

        assert_eq!(buffer.slice().to_string(), "a");
        assert_eq!(s.primary().head().char_index, 0);
    }

    #[test]
    fn forward_word_start_skips_word_then_whitespace() {
        let mut buffer = Buffer::new();
        for (i, ch) in "foo bar".chars().enumerate() {
            buffer.insert_char(i, ch);
        }
        let rope = buffer.slice();
        assert_eq!(forward_word_start(rope, 0), 4); // f -> b
        assert_eq!(forward_word_start(rope, 4), 7); // b -> end
    }

    #[test]
    fn forward_word_start_treats_punctuation_as_unit() {
        let mut buffer = Buffer::new();
        for (i, ch) in "foo.bar".chars().enumerate() {
            buffer.insert_char(i, ch);
        }
        let rope = buffer.slice();
        assert_eq!(forward_word_start(rope, 0), 3); // f -> .
        assert_eq!(forward_word_start(rope, 3), 4); // . -> b
        assert_eq!(forward_word_start(rope, 4), 7); // b -> end
    }

    #[test]
    fn forward_word_end_lands_on_last_char_of_word() {
        let mut buffer = Buffer::new();
        for (i, ch) in "foo.bar".chars().enumerate() {
            buffer.insert_char(i, ch);
        }
        let rope = buffer.slice();
        assert_eq!(forward_word_end(rope, 0), 2); // f -> o (foo end)
        assert_eq!(forward_word_end(rope, 2), 3); // o -> . (punct end)
        assert_eq!(forward_word_end(rope, 3), 6); // . -> r (bar end)
    }

    #[test]
    fn forward_word_end_skips_whitespace_to_next_word() {
        let mut buffer = Buffer::new();
        for (i, ch) in "foo  bar".chars().enumerate() {
            buffer.insert_char(i, ch);
        }
        let rope = buffer.slice();
        assert_eq!(forward_word_end(rope, 0), 2); // f -> o
        assert_eq!(forward_word_end(rope, 2), 7); // o -> r (skips spaces)
    }

    #[test]
    fn forward_word_start_at_end_stays_at_end() {
        let mut buffer = Buffer::new();
        for (i, ch) in "foo".chars().enumerate() {
            buffer.insert_char(i, ch);
        }
        let rope = buffer.slice();
        assert_eq!(forward_word_start(rope, 3), 3);
    }

    #[test]
    fn forward_word_end_at_end_stays_at_end() {
        let mut buffer = Buffer::new();
        for (i, ch) in "foo".chars().enumerate() {
            buffer.insert_char(i, ch);
        }
        let rope = buffer.slice();
        assert_eq!(forward_word_end(rope, 3), 3);
    }

    #[test]
    fn move_head_word_forward_advances_to_next_word() {
        let mut buffer = Buffer::new();
        for (i, ch) in "foo bar".chars().enumerate() {
            buffer.insert_char(i, ch);
        }
        let mut s = selection_at(&buffer, 0);
        buffer.move_head_word_forward(s.primary_mut());
        assert_eq!(s.primary().head().char_index, 4);
    }

    #[test]
    fn move_head_word_backward_advances_to_prev_word() {
        let mut buffer = Buffer::new();
        for (i, ch) in "foo bar".chars().enumerate() {
            buffer.insert_char(i, ch);
        }
        let mut s = selection_at(&buffer, 7);
        buffer.move_head_word_backward(s.primary_mut());
        assert_eq!(s.primary().head().char_index, 4);
    }

    #[test]
    fn move_head_word_end_advances_to_word_end() {
        let mut buffer = Buffer::new();
        for (i, ch) in "foo.bar".chars().enumerate() {
            buffer.insert_char(i, ch);
        }
        let mut s = selection_at(&buffer, 0);
        buffer.move_head_word_end(s.primary_mut());
        assert_eq!(s.primary().head().char_index, 2);
    }

    #[test]
    fn first_non_blank_finds_first_non_whitespace() {
        let mut buffer = Buffer::new();
        for (i, ch) in "  foo".chars().enumerate() {
            buffer.insert_char(i, ch);
        }
        let rope = buffer.slice();
        assert_eq!(first_non_blank_in_line(rope, 0), 2);
    }

    #[test]
    fn first_non_blank_all_blank_returns_line_start() {
        let mut buffer = Buffer::new();
        for (i, ch) in "   \n".chars().enumerate() {
            buffer.insert_char(i, ch);
        }
        let rope = buffer.slice();
        assert_eq!(first_non_blank_in_line(rope, 0), 0);
    }

    #[test]
    fn line_end_char_returns_last_non_newline_index() {
        let mut buffer = Buffer::new();
        for (i, ch) in "foo\nbar".chars().enumerate() {
            buffer.insert_char(i, ch);
        }
        let rope = buffer.slice();
        assert_eq!(line_end_char(rope, 0), 2); // 'o' of "foo"
        assert_eq!(line_end_char(rope, 1), 6); // 'r' of "bar"
    }

    #[test]
    fn line_end_char_empty_line_returns_line_start() {
        let mut buffer = Buffer::new();
        for (i, ch) in "\n".chars().enumerate() {
            buffer.insert_char(i, ch);
        }
        let rope = buffer.slice();
        assert_eq!(line_end_char(rope, 0), 0);
    }

    #[test]
    fn line_end_insert_returns_position_after_last_char() {
        let mut buffer = Buffer::new();
        for (i, ch) in "foo\n".chars().enumerate() {
            buffer.insert_char(i, ch);
        }
        let rope = buffer.slice();
        assert_eq!(line_end_insert(rope, 0), 3); // after 'o', before '\n'
    }

    #[test]
    fn prev_paragraph_finds_previous_empty_line() {
        let mut buffer = Buffer::new();
        for (i, ch) in "foo\n\nbar".chars().enumerate() {
            buffer.insert_char(i, ch);
        }
        let rope = buffer.slice();
        // char_index 5 is 'b' in "bar" on line 2; prev empty line is line 1 (char 4)
        assert_eq!(prev_paragraph(rope, 5), 4);
    }

    #[test]
    fn next_paragraph_finds_next_empty_line() {
        let mut buffer = Buffer::new();
        for (i, ch) in "foo\n\nbar".chars().enumerate() {
            buffer.insert_char(i, ch);
        }
        let rope = buffer.slice();
        // char_index 0 is 'f' on line 0; next empty line is line 1 (char 4)
        assert_eq!(next_paragraph(rope, 0), 4);
    }

    #[test]
    fn prev_paragraph_no_empty_line_stays_at_first_line() {
        let mut buffer = Buffer::new();
        for (i, ch) in "foo\nbar".chars().enumerate() {
            buffer.insert_char(i, ch);
        }
        let rope = buffer.slice();
        assert_eq!(prev_paragraph(rope, 5), 0);
    }

    #[test]
    fn next_paragraph_no_empty_line_stays_at_last_line() {
        let mut buffer = Buffer::new();
        for (i, ch) in "foo\nbar".chars().enumerate() {
            buffer.insert_char(i, ch);
        }
        let rope = buffer.slice();
        // No empty line; last line starts at char 4
        assert_eq!(next_paragraph(rope, 0), 4);
    }

    #[test]
    fn move_head_to_line_start_goes_to_column_zero() {
        let mut buffer = Buffer::new();
        for (i, ch) in "  foo\n  bar".chars().enumerate() {
            buffer.insert_char(i, ch);
        }
        let mut s = selection_at(&buffer, 7); // on 'b' of line 2
        buffer.move_head_to_line_start(s.primary_mut());
        assert_eq!(s.primary().head().char_index, 6); // line 2 start
    }

    #[test]
    fn move_head_to_first_non_blank_skips_leading_ws() {
        let mut buffer = Buffer::new();
        for (i, ch) in "  foo".chars().enumerate() {
            buffer.insert_char(i, ch);
        }
        let mut s = selection_at(&buffer, 0);
        buffer.move_head_to_first_non_blank(s.primary_mut());
        assert_eq!(s.primary().head().char_index, 2);
    }

    #[test]
    fn move_head_to_line_end_lands_on_last_char() {
        let mut buffer = Buffer::new();
        for (i, ch) in "foo\nbar".chars().enumerate() {
            buffer.insert_char(i, ch);
        }
        let mut s = selection_at(&buffer, 0);
        buffer.move_head_to_line_end(s.primary_mut());
        assert_eq!(s.primary().head().char_index, 2); // last 'o'
    }

    #[test]
    fn move_head_after_line_end_lands_after_last_char() {
        let mut buffer = Buffer::new();
        for (i, ch) in "foo\n".chars().enumerate() {
            buffer.insert_char(i, ch);
        }
        let mut s = selection_at(&buffer, 0);
        buffer.move_head_after_line_end(s.primary_mut());
        assert_eq!(s.primary().head().char_index, 3); // after 'o', before '\n'
    }

    #[test]
    fn move_head_to_last_line_goes_to_last_line_start() {
        let mut buffer = Buffer::new();
        for (i, ch) in "foo\nbar\nbaz".chars().enumerate() {
            buffer.insert_char(i, ch);
        }
        let mut s = selection_at(&buffer, 0);
        buffer.move_head_to_last_line(s.primary_mut());
        assert_eq!(s.primary().head().char_index, 8); // start of "baz"
    }

    #[test]
    fn move_head_to_prev_paragraph_jumps_to_empty_line() {
        let mut buffer = Buffer::new();
        for (i, ch) in "foo\n\nbar".chars().enumerate() {
            buffer.insert_char(i, ch);
        }
        let mut s = selection_at(&buffer, 5); // 'b' of "bar"
        buffer.move_head_to_prev_paragraph(s.primary_mut());
        assert_eq!(s.primary().head().char_index, 4); // empty line
    }

    #[test]
    fn move_head_to_next_paragraph_jumps_to_empty_line() {
        let mut buffer = Buffer::new();
        for (i, ch) in "foo\n\nbar".chars().enumerate() {
            buffer.insert_char(i, ch);
        }
        let mut s = selection_at(&buffer, 0); // 'f' of "foo"
        buffer.move_head_to_next_paragraph(s.primary_mut());
        assert_eq!(s.primary().head().char_index, 4); // empty line
    }

    #[test]
    fn move_head_right_clamps_and_collapsed() {
        let mut b = Buffer::new();
        b.insert_char(0, 'a');
        b.insert_char(1, 'b');
        let mut s = single_sel(TextOffset::origin());
        b.move_head_right(s.primary_mut(), 5);
        Buffer::collapse_to_head(s.primary_mut());
        assert_eq!(s.primary().head().char_index, 2);
        assert_eq!(s.primary().anchor, s.primary().head());
    }

    #[test]
    fn move_head_down_clamps_col_then_collapse() {
        let mut b = Buffer::new();
        b.insert_at_selections(&mut single_sel(TextOffset::origin()), "hello\nab\nworld");
        let mut s = single_sel(TextOffset { char_index: 4 });
        b.clamp_selection(s.primary_mut());
        b.move_head_down(s.primary_mut(), 1);
        assert_eq!(
            b.text_point(s.primary().head()),
            TextPoint { row: 1, col: 2 }
        );
        Buffer::collapse_to_head(s.primary_mut());
        assert_eq!(s.primary().anchor, s.primary().head());
    }

    #[test]
    fn open_missing_is_new_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("nope.txt");
        let mut b = Buffer::new();
        b.open_path(path.to_str().unwrap()).unwrap();
        assert_eq!(b.backing_state(), BufferBackingState::Unmaterialized);
    }

    #[test]
    fn open_non_utf8_is_open_failed() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("bin.dat");
        std::fs::write(&path, [0xFF, 0xFE, 0xC0]).unwrap();
        let mut b = Buffer::new();
        assert!(b.open_path(path.to_str().unwrap()).is_err());
    }

    #[test]
    fn open_existing_is_materialized() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("f.txt");
        std::fs::write(&path, "hi").unwrap();
        let mut b = Buffer::new();
        b.open_path(path.to_str().unwrap()).unwrap();
        assert_eq!(b.backing_state(), BufferBackingState::Materialized);
        assert_eq!(b.slice().to_string(), "hi");
    }

    #[test]
    fn move_head_left_keeps_anchor_and_makes_non_empty() {
        let mut b = Buffer::new();
        b.insert_char(0, 'a');
        b.insert_char(1, 'b');
        b.insert_char(2, 'c');
        let mut s = single_sel(cur(3));
        let anchor_before = s.primary().anchor;
        b.move_head_left(s.primary_mut(), 2);
        assert_eq!(s.primary().head().char_index, 1);
        assert_eq!(s.primary().anchor, anchor_before);
        assert!(s.primary().anchor != s.primary().head());
    }

    #[test]
    fn collapse_to_head_makes_anchor_eq_head() {
        let mut s = single_sel(cur(0));
        s.primary_mut().head = cur(3);
        Buffer::collapse_to_head(s.primary_mut());
        assert_eq!(s.primary().anchor, s.primary().head());
        assert_eq!(s.primary().anchor.char_index, 3);
    }

    #[test]
    fn move_head_up_down_keeps_anchor() {
        let mut b = Buffer::new();
        b.insert_at_selections(&mut single_sel(TextOffset::origin()), "hello\nab\nworld");
        let mut s = single_sel(cur(4));
        let anchor_before = s.primary().anchor;
        b.move_head_down(s.primary_mut(), 1);
        assert_eq!(b.text_point(s.primary().head()).row, 1);
        assert_eq!(s.primary().anchor, anchor_before);
        assert!(s.primary().anchor != s.primary().head());
    }

    #[test]
    fn insert_at_non_empty_selection_replaces_range() {
        let mut b = Buffer::new();
        b.insert_at_selections(&mut single_sel(TextOffset::origin()), "hello");
        let mut s = {
            let mut sel = Selection::collapsed(cur(1));
            sel.head = cur(4);
            Selections::single(sel)
        };
        b.insert_at_selections(&mut s, "XY");
        assert_eq!(b.slice().to_string(), "hXYo");
        assert_eq!(s.primary().head().char_index, 3);
        assert_eq!(s.primary().anchor, s.primary().head());
    }

    #[test]
    fn delete_at_non_empty_selection_removes_range() {
        let mut b = Buffer::new();
        b.insert_at_selections(&mut single_sel(TextOffset::origin()), "hello");
        let mut s = {
            let mut sel = Selection::collapsed(cur(1));
            sel.head = cur(4);
            Selections::single(sel)
        };
        b.delete_at_selections(&mut s, -1);
        assert_eq!(b.slice().to_string(), "ho");
        assert_eq!(s.primary().head().char_index, 1);
        assert_eq!(s.primary().anchor, s.primary().head());
    }

    #[test]
    fn insert_at_collapsed_keeps_point_semantics() {
        let mut b = Buffer::new();
        b.insert_char(0, 'a');
        b.insert_char(1, 'b');
        let mut s = single_sel(cur(1));
        b.insert_at_selections(&mut s, "X");
        assert_eq!(b.slice().to_string(), "aXb");
        assert_eq!(s.primary().head().char_index, 2);
        assert_eq!(s.primary().anchor, s.primary().head());
    }

    #[test]
    fn crlf_is_one_logical_step_for_horizontal_movement_and_deletion() {
        let mut buffer = Buffer::new();
        buffer.insert_at_selections(&mut single_sel(TextOffset::origin()), "a\r\nb");
        let mut selection = single_sel(cur(1));

        buffer.move_head_right(selection.primary_mut(), 1);
        assert_eq!(selection.primary().head().char_index, 3);
        assert_eq!(
            buffer.text_point(selection.primary().head()),
            TextPoint { row: 1, col: 0 }
        );
        buffer.move_head_left(selection.primary_mut(), 1);
        assert_eq!(selection.primary().head().char_index, 1);

        buffer.delete_at_selections(&mut selection, 1);
        assert_eq!(buffer.slice().to_string(), "ab");

        let mut buffer = Buffer::new();
        buffer.insert_at_selections(&mut single_sel(TextOffset::origin()), "a\r\nb");
        let mut selection = single_sel(cur(3));
        buffer.delete_at_selections(&mut selection, -1);
        assert_eq!(buffer.slice().to_string(), "ab");
        assert_eq!(selection.primary().head().char_index, 1);
    }

    #[test]
    fn indent_merges_adjacent_and_overlapping_line_blocks() {
        let mut buffer = Buffer::new();
        buffer.insert_at_selections(&mut single_sel(TextOffset::origin()), "a\nb\nc\n");
        let selections = Selections::from_parts(
            vec![
                Selection::collapsed(cur(0)),
                Selection {
                    anchor: cur(2),
                    head: cur(3),
                },
                Selection {
                    anchor: cur(4),
                    head: cur(2),
                },
            ],
            1,
        );

        let plan = buffer.plan_edit(
            EditCommand::IndentLines(IndentationConfig {
                indent_width: 2,
                insert_spaces: true,
            }),
            &selections,
        );
        let ContentAction::Text(change) = plan.action.expect("indent changes text");

        assert_eq!(change.to_edits().unwrap().len(), 3);
        buffer.apply_content_change(change).unwrap();
        assert_eq!(buffer.slice().to_string(), "  a\n  b\n  c\n");
        assert_eq!(plan.selections.primary().anchor.char_index, 6);
        assert_eq!(plan.selections.primary().head.char_index, 7);
    }

    #[test]
    fn outdent_handles_tabs_spaces_empty_lines_and_crlf() {
        let mut buffer = Buffer::new();
        buffer.insert_at_selections(
            &mut single_sel(TextOffset::origin()),
            "\tfoo\r\n  bar\r\n\r\nbaz",
        );
        let mut selections = Selections::single(Selection {
            anchor: cur(0),
            head: cur(13),
        });

        buffer.outdent_lines_at_selections(
            &mut selections,
            IndentationConfig {
                indent_width: 4,
                insert_spaces: true,
            },
        );

        assert_eq!(buffer.slice().to_string(), "foo\r\nbar\r\n\r\nbaz");
        assert_eq!(selections.primary().anchor.char_index, 0);
        assert_eq!(selections.primary().head.char_index, 10);
    }

    #[test]
    fn duplicate_last_line_preserves_crlf_and_targets_the_copy() {
        let mut buffer = Buffer::new();
        buffer.insert_at_selections(&mut single_sel(TextOffset::origin()), "one\r\ntwo");
        let mut selections = single_sel(cur(6));

        buffer.duplicate_lines_at_selections(&mut selections);

        assert_eq!(buffer.slice().to_string(), "one\r\ntwo\r\ntwo");
        assert_eq!(selections.primary().head.char_index, 11);
        assert_eq!(selections.primary().anchor, selections.primary().head);
    }

    #[test]
    fn duplicate_empty_final_line_creates_one_more_line() {
        let mut buffer = Buffer::new();
        buffer.insert_at_selections(&mut single_sel(TextOffset::origin()), "one\n");
        let mut selections = single_sel(cur(4));

        buffer.duplicate_lines_at_selections(&mut selections);

        assert_eq!(buffer.slice().to_string(), "one\n\n");
        assert_eq!(selections.primary().head.char_index, 5);
    }

    #[test]
    fn duplicate_disjoint_blocks_accounts_for_earlier_insertions() {
        let mut buffer = Buffer::new();
        buffer.insert_at_selections(&mut single_sel(TextOffset::origin()), "0\n1\n2\n3");
        let mut selections = Selections::from_parts(
            vec![Selection::collapsed(cur(0)), Selection::collapsed(cur(4))],
            1,
        );

        buffer.duplicate_lines_at_selections(&mut selections);

        assert_eq!(buffer.slice().to_string(), "0\n0\n1\n2\n2\n3");
        assert_eq!(
            selections
                .all()
                .map(|selection| selection.head.char_index)
                .collect::<Vec<_>>(),
            vec![2, 8]
        );
    }

    #[test]
    fn invalid_indentation_config_is_a_safe_no_op() {
        let mut buffer = Buffer::new();
        buffer.insert_at_selections(&mut single_sel(TextOffset::origin()), "text");
        let mut selections = single_sel(cur(2));

        buffer.indent_lines_at_selections(
            &mut selections,
            IndentationConfig {
                indent_width: usize::MAX,
                insert_spaces: true,
            },
        );

        assert_eq!(buffer.slice().to_string(), "text");
        assert_eq!(selections.primary().head.char_index, 2);
        assert!(buffer.take_last_change().is_none());
    }

    #[test]
    fn move_lines_across_unterminated_last_line_preserves_separators() {
        let mut buffer = Buffer::new();
        buffer.insert_at_selections(&mut single_sel(TextOffset::origin()), "one\r\ntwo\r\nthree");
        let mut selections = Selections::single(Selection {
            anchor: cur(5),
            head: cur(7),
        });

        buffer.move_lines_down_at_selections(&mut selections);
        assert_eq!(buffer.slice().to_string(), "one\r\nthree\r\ntwo");
        assert_eq!(selections.primary().anchor.char_index, 12);
        assert_eq!(selections.primary().head.char_index, 14);

        buffer.move_lines_up_at_selections(&mut selections);
        assert_eq!(buffer.slice().to_string(), "one\r\ntwo\r\nthree");
        assert_eq!(selections.primary().anchor.char_index, 5);
        assert_eq!(selections.primary().head.char_index, 7);
    }

    #[test]
    fn move_line_edges_are_no_ops() {
        let mut buffer = Buffer::new();
        buffer.insert_at_selections(&mut single_sel(TextOffset::origin()), "one\ntwo");
        let mut top = single_sel(cur(1));
        buffer.move_lines_up_at_selections(&mut top);
        assert_eq!(buffer.slice().to_string(), "one\ntwo");
        assert!(buffer.take_last_change().is_none());

        let mut bottom = single_sel(cur(5));
        buffer.move_lines_down_at_selections(&mut bottom);
        assert_eq!(buffer.slice().to_string(), "one\ntwo");
        assert!(buffer.take_last_change().is_none());
    }

    #[test]
    fn disjoint_line_blocks_move_independently() {
        let mut buffer = Buffer::new();
        buffer.insert_at_selections(&mut single_sel(TextOffset::origin()), "0\n1\n2\n3\n4");
        let mut selections = Selections::from_parts(
            vec![Selection::collapsed(cur(2)), Selection::collapsed(cur(6))],
            0,
        );

        buffer.move_lines_up_at_selections(&mut selections);

        assert_eq!(buffer.slice().to_string(), "1\n0\n3\n2\n4");
        assert_eq!(
            selections
                .all()
                .map(|selection| selection.head.char_index)
                .collect::<Vec<_>>(),
            vec![0, 4]
        );
    }

    #[test]
    fn editing_crlf_buffer_preserves_its_line_ending_style() {
        let mut buffer = Buffer::new();
        buffer.insert_at_selections(&mut single_sel(TextOffset::origin()), "a\r\nb");
        let mut selection = single_sel(cur(4));

        buffer.insert_at_selections(&mut selection, "\n");

        assert_eq!(buffer.slice().to_string(), "a\r\nb\r\n");
        assert_eq!(selection.primary().head().char_index, 6);
    }

    #[test]
    fn strategy_newline_preserves_crlf_and_places_cursor_inside_rust_block() {
        let mut buffer = Buffer::new();
        buffer.insert_at_selections(&mut single_sel(TextOffset::origin()), "fn main() {}\r\n");
        let mut selections = single_sel(TextOffset { char_index: 11 });

        buffer.insert_newline_at_selections(&mut selections, "    ", Some(""));

        assert_eq!(buffer.slice().to_string(), "fn main() {\r\n    \r\n}\r\n");
        assert_eq!(selections.primary().head.char_index, 17);
    }

    #[test]
    fn line_comment_toggle_handles_partial_lines_and_empty_lines() {
        let mut buffer = Buffer::new();
        buffer.insert_at_selections(&mut single_sel(TextOffset::origin()), "  alpha\n\n  beta");
        let mut selections = Selections::single(Selection {
            anchor: TextOffset { char_index: 3 },
            head: TextOffset { char_index: 8 },
        });

        buffer.toggle_line_comment_at_selections(&mut selections, "//");
        assert_eq!(buffer.slice().to_string(), "  // alpha\n// \n  beta");

        buffer.toggle_line_comment_at_selections(&mut selections, "//");
        assert_eq!(buffer.slice().to_string(), "  alpha\n\n  beta");
    }

    #[test]
    fn pair_primitives_wrap_skip_and_delete_as_one_change() {
        let mut buffer = Buffer::new();
        buffer.insert_at_selections(&mut single_sel(TextOffset::origin()), "name");
        let mut selected = Selections::single(Selection {
            anchor: TextOffset { char_index: 4 },
            head: TextOffset::origin(),
        });

        buffer.insert_pair_at_selections(&mut selected, "(", ")");
        assert_eq!(buffer.slice().to_string(), "(name)");
        assert_eq!(selected.primary().anchor.char_index, 5);
        assert_eq!(selected.primary().head.char_index, 1);

        let mut cursor = single_sel(TextOffset { char_index: 5 });
        buffer.insert_closing_pair_at_selections(&mut cursor, ")");
        assert_eq!(buffer.slice().to_string(), "(name)");
        assert_eq!(cursor.primary().head.char_index, 6);
        assert!(buffer.take_last_change().is_none());

        let mut empty = single_sel(TextOffset { char_index: 6 });
        buffer.insert_pair_at_selections(&mut empty, "\"", "\"");
        assert_eq!(buffer.slice().to_string(), "(name)\"\"");
        buffer.delete_pair_backward_at_selections(&mut empty, "\"", "\"");
        assert_eq!(buffer.slice().to_string(), "(name)");
        assert_eq!(empty.primary().head.char_index, 6);
    }

    #[test]
    fn block_comment_toggle_wraps_and_unwraps_selection() {
        let mut buffer = Buffer::new();
        buffer.insert_at_selections(&mut single_sel(TextOffset::origin()), "value");
        let mut selections = Selections::single(Selection {
            anchor: TextOffset::origin(),
            head: TextOffset { char_index: 5 },
        });

        buffer.toggle_block_comment_at_selections(&mut selections, "/*", "*/");
        assert_eq!(buffer.slice().to_string(), "/*value*/");
        assert_eq!(selections.primary().anchor.char_index, 2);
        assert_eq!(selections.primary().head.char_index, 7);

        selections = Selections::single(Selection {
            anchor: TextOffset::origin(),
            head: TextOffset { char_index: 9 },
        });
        buffer.toggle_block_comment_at_selections(&mut selections, "/*", "*/");
        assert_eq!(buffer.slice().to_string(), "value");
        assert_eq!(selections.primary().head.char_index, 5);
    }

    #[test]
    fn character_clipboard_cuts_and_distributes_matching_fragments() {
        let mut source = Buffer::new();
        source.insert_at_selections(&mut single_sel(TextOffset::origin()), "alpha beta");
        let source_selections = Selections::from_parts(
            vec![
                Selection {
                    anchor: cur(0),
                    head: cur(5),
                },
                Selection {
                    anchor: cur(6),
                    head: cur(10),
                },
            ],
            0,
        );

        let (payload, cut) = source.plan_cut(&source_selections, ClipboardKind::CharacterWise);
        assert_eq!(payload.fragments, vec!["alpha", "beta"]);
        let ContentAction::Text(change) = cut.action.unwrap();
        source.apply_content_change(change).unwrap();
        assert_eq!(source.slice().to_string(), " ");

        let mut target = Buffer::new();
        target.insert_at_selections(&mut single_sel(TextOffset::origin()), "--");
        let target_selections = Selections::from_parts(
            vec![Selection::collapsed(cur(0)), Selection::collapsed(cur(2))],
            0,
        );
        let paste = target.plan_paste(&target_selections, &payload, PastePlacement::Before);
        let ContentAction::Text(change) = paste.action.unwrap();
        target.apply_content_change(change).unwrap();

        assert_eq!(target.slice().to_string(), "alpha--beta");
        assert_eq!(
            paste
                .selections
                .all()
                .map(|selection| selection.head.char_index)
                .collect::<Vec<_>>(),
            vec![5, 11]
        );
    }

    #[test]
    fn character_paste_repeats_joined_payload_when_counts_differ() {
        let mut buffer = Buffer::new();
        buffer.insert_at_selections(&mut single_sel(TextOffset::origin()), "x");
        let payload = ClipboardPayload {
            kind: ClipboardKind::CharacterWise,
            fragments: vec!["a".into(), "b".into()],
        };

        let paste = buffer.plan_paste(
            &single_sel(TextOffset::origin()),
            &payload,
            PastePlacement::Before,
        );
        let ContentAction::Text(change) = paste.action.unwrap();
        buffer.apply_content_change(change).unwrap();

        assert_eq!(buffer.slice().to_string(), "abx");
        assert_eq!(paste.selections.primary().head.char_index, 2);

        let after = buffer.plan_paste(
            &single_sel(TextOffset::origin()),
            &ClipboardPayload::character("z"),
            PastePlacement::After,
        );
        let ContentAction::Text(change) = after.action.unwrap();
        buffer.apply_content_change(change).unwrap();
        assert_eq!(buffer.slice().to_string(), "azbx");
        assert_eq!(after.selections.primary().head.char_index, 2);
    }

    #[test]
    fn linewise_clipboard_preserves_target_line_endings_and_unterminated_eof() {
        let mut source = Buffer::new();
        source.insert_at_selections(&mut single_sel(TextOffset::origin()), "one\ntwo");
        let payload =
            source.copy_selections(&single_sel(TextOffset::origin()), ClipboardKind::LineWise);
        let (_, cut) = source.plan_cut(&single_sel(TextOffset::origin()), ClipboardKind::LineWise);
        let ContentAction::Text(change) = cut.action.unwrap();
        source.apply_content_change(change).unwrap();
        assert_eq!(source.slice().to_string(), "two");

        let mut target = Buffer::new();
        target.insert_at_selections(&mut single_sel(TextOffset::origin()), "x\r\nz");
        let paste = target.plan_paste(
            &single_sel(TextOffset::origin()),
            &payload,
            PastePlacement::After,
        );
        let ContentAction::Text(change) = paste.action.unwrap();
        target.apply_content_change(change).unwrap();
        assert_eq!(target.slice().to_string(), "x\r\none\r\nz");

        let last = target.copy_selections(&single_sel(cur(8)), ClipboardKind::LineWise);
        let paste = target.plan_paste(&single_sel(cur(8)), &last, PastePlacement::After);
        let ContentAction::Text(change) = paste.action.unwrap();
        target.apply_content_change(change).unwrap();
        assert_eq!(target.slice().to_string(), "x\r\none\r\nz\r\nz");
    }

    #[test]
    fn empty_character_clipboard_is_a_safe_no_op() {
        let mut buffer = Buffer::new();
        buffer.insert_at_selections(&mut single_sel(TextOffset::origin()), "text");
        let payload = ClipboardPayload::character("");

        let plan = buffer.plan_paste(
            &single_sel(TextOffset::origin()),
            &payload,
            PastePlacement::Before,
        );

        assert!(plan.action.is_none());
        assert_eq!(buffer.slice().to_string(), "text");
    }

    #[test]
    fn no_op_edits_do_not_mark_buffer_modified_or_advance_revision() {
        let mut buffer = Buffer::new();
        let mut selection = single_sel(TextOffset::origin());

        buffer.delete_at_selections(&mut selection, -1);
        buffer.insert_at_selections(&mut selection, "");
        buffer.join_lines_at_selections(&mut selection);
        buffer.toggle_case_at_selections(&mut selection);

        assert_eq!(buffer.revision(), 0);
        assert!(!buffer.modified());
    }

    #[test]
    fn join_lines_removes_complete_leading_whitespace_graphemes() {
        let mut buffer = Buffer::new();
        buffer.insert_at_selections(&mut single_sel(TextOffset::origin()), "a\n \u{301}b");
        let mut selection = selection_at(&buffer, 0);

        buffer.join_lines_at_selections(&mut selection);

        assert_eq!(buffer.slice().to_string(), "a b");
        assert_eq!(selection.primary().head().char_index, 1);
    }

    #[test]
    fn toggle_case_keeps_all_scalars_from_unicode_mapping() {
        let mut buffer = Buffer::new();
        buffer.insert_at_selections(&mut single_sel(TextOffset::origin()), "ßx");
        let mut selection = single_sel(TextOffset::origin());

        buffer.toggle_case_at_selections(&mut selection);

        assert_eq!(buffer.slice().to_string(), "SSx");
        assert_eq!(selection.primary().head().char_index, 2);
    }

    #[test]
    fn toggle_case_replaces_a_complete_grapheme() {
        let mut buffer = Buffer::new();
        buffer.insert_at_selections(&mut single_sel(TextOffset::origin()), "e\u{301}x");
        let mut selection = selection_at(&buffer, 0);

        buffer.toggle_case_at_selections(&mut selection);

        assert_eq!(buffer.slice().to_string(), "E\u{301}x");
        assert_eq!(selection.primary().head().char_index, 2);
    }

    #[test]
    fn cursor_moves_only_between_extended_graphemes() {
        let mut buffer = Buffer::new();
        buffer.insert_at_selections(
            &mut single_sel(TextOffset::origin()),
            "e\u{301}👩\u{200d}🔬🇨🇳👍🏽x",
        );
        let mut selection = selection_at(&buffer, 0);

        for expected in [2, 5, 7, 9, 10] {
            buffer.move_head_right(selection.primary_mut(), 1);
            assert_eq!(selection.primary().head().char_index, expected);
        }
        for expected in [9, 7, 5, 2, 0] {
            buffer.move_head_left(selection.primary_mut(), 1);
            assert_eq!(selection.primary().head().char_index, expected);
        }
    }

    #[test]
    fn delete_removes_a_whole_extended_grapheme() {
        let mut buffer = Buffer::new();
        buffer.insert_at_selections(&mut single_sel(TextOffset::origin()), "e\u{301}x");
        let mut forward = selection_at(&buffer, 0);

        buffer.delete_at_selections(&mut forward, 1);

        assert_eq!(buffer.slice().to_string(), "x");
        assert_eq!(forward.primary().head().char_index, 0);

        let mut backward = selection_at(&buffer, 1);
        buffer.delete_at_selections(&mut backward, -1);
        assert_eq!(buffer.slice().len_chars(), 0);
        assert_eq!(backward.primary().head().char_index, 0);
    }

    #[test]
    fn vertical_motion_preserves_grapheme_column() {
        let mut buffer = Buffer::new();
        buffer.insert_at_selections(
            &mut single_sel(TextOffset::origin()),
            "e\u{301}x\na\u{301}b",
        );
        let mut selection = selection_at(&buffer, 2);

        buffer.move_head_down(selection.primary_mut(), 1);
        assert_eq!(selection.primary().head().char_index, 6);
        buffer.move_head_up(selection.primary_mut(), 1);
        assert_eq!(selection.primary().head().char_index, 2);
    }

    #[test]
    fn content_changes_may_change_grapheme_composition() {
        let mut buffer = Buffer::new();
        buffer.insert_at_selections(&mut single_sel(TextOffset::origin()), "e\u{301}x");
        let change = TextChangeSet::from_edits(3, vec![TextEdit::new(1..1, "z")]).unwrap();

        buffer.apply_content_change(change).unwrap();

        assert_eq!(buffer.slice().to_string(), "ez\u{301}x");
    }

    #[test]
    fn first_non_blank_motion_stays_on_a_grapheme_boundary() {
        let mut buffer = Buffer::new();
        buffer.insert_at_selections(&mut single_sel(TextOffset::origin()), " \u{301}x");
        let mut selection = selection_at(&buffer, 2);

        buffer.move_head_to_first_non_blank(selection.primary_mut());

        assert_eq!(selection.primary().head().char_index, 0);
    }

    #[test]
    fn selection_reconciliation_snaps_to_the_next_grapheme_boundary() {
        let mut buffer = Buffer::new();
        buffer.insert_at_selections(&mut single_sel(TextOffset::origin()), "e\u{301}x");
        let mut selection = single_sel(cur(1));

        assert!(buffer.reconcile_selections(&mut selection));
        assert_eq!(selection.primary().head().char_index, 2);
        assert_eq!(selection.primary().anchor.char_index, 2);
    }
}
