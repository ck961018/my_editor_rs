use ropey::Rope;
use std::borrow::Cow;
use std::io;
use std::path::PathBuf;

use crate::core::action::{ContentAction, ContentEditPlan};
use crate::core::command::{CharSearchDirection, EditCommand};
use crate::core::motion::{
    OperatorCommand, TextOperator, TextRange, TextTarget, forward_word_end, forward_word_start,
    line_end_insert, resolve_operator,
};
use crate::core::transaction::{
    Affinity, TextChangeSet, TextEdit, TextStateId, TextTransactionData, TextTransactionError,
    TransactionDirection,
};
use crate::protocol::selection::{Selection, Selections, TextOffset, TextPoint};
use crate::protocol::status::StatusMessage;

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
    status: StatusMessage,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BufferTransactionData {
    pub text: TextTransactionData,
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
            status: StatusMessage::None,
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
            status: self.status.clone(),
        };
        let mut selections = selections.clone();
        crate::core::edit::apply_edit(command, &mut scratch, &mut selections);
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
                Ok(())
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                self.rope = Rope::new();
                self.advance_revision();
                self.reset_to_saved_state();
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    /// 打开文件语义：NotFound→NewFile、非 UTF-8→OpenFailed、正常→None。
    pub fn open_path(&mut self, path: &str) -> io::Result<()> {
        let result = self.load_from_file(path);
        match &result {
            Ok(()) => {
                let is_new = !std::path::Path::new(path).exists();
                self.status = if is_new {
                    StatusMessage::NewFile
                } else {
                    StatusMessage::None
                };
            }
            Err(e) if e.kind() == io::ErrorKind::InvalidData => {
                self.status = StatusMessage::OpenFailed;
            }
            Err(_) => {
                self.status = StatusMessage::OpenFailed;
            }
        }
        result
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn state_id(&self) -> TextStateId {
        self.current_state
    }

    pub fn mark_saved(&mut self, state: TextStateId) -> bool {
        self.saved_state = state;
        self.current_state == state
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
        self.validate_crlf_boundaries(&changes)?;
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
        self.validate_crlf_boundaries(&change)?;
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

    fn validate_crlf_boundaries(
        &self,
        changes: &TextChangeSet,
    ) -> Result<(), TextTransactionError> {
        for edit in changes.to_edits()? {
            for offset in [edit.range.start, edit.range.end] {
                if offset > 0
                    && offset < self.rope.len_chars()
                    && self.rope.char(offset - 1) == '\r'
                    && self.rope.char(offset) == '\n'
                {
                    return Err(TextTransactionError::InvalidRange {
                        start: edit.range.start,
                        end: edit.range.end,
                    });
                }
            }
        }
        Ok(())
    }

    pub fn set_status(&mut self, msg: StatusMessage) {
        self.status = msg;
    }

    pub fn status(&self) -> StatusMessage {
        self.status.clone()
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
        if char_idx == 0 {
            return false;
        }
        let start = if char_idx >= 2
            && self.rope.char(char_idx - 2) == '\r'
            && self.rope.char(char_idx - 1) == '\n'
        {
            char_idx - 2
        } else {
            char_idx - 1
        };
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

    /// 取第 idx 行（含尾部换行），供 ContentQuery::lines 用。
    pub fn line(&self, idx: usize) -> Cow<'_, str> {
        Cow::Owned(self.slice().line(idx).to_string())
    }

    /// 文件名（path 末段），供 StatusBar::status_bar_data 用。
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
        cur.char_index = cur.char_index.min(self.rope.len_chars());
        if cur.char_index > 0
            && cur.char_index < self.rope.len_chars()
            && self.rope.char(cur.char_index - 1) == '\r'
            && self.rope.char(cur.char_index) == '\n'
        {
            cur.char_index += 1;
        }
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
            let point = self.text_point(*cur);
            let max_row = self.rope.len_lines().saturating_sub(1);
            let target_row = (point.row as isize + lines).clamp(0, max_row as isize) as usize;
            let line_len = line_content_len(&self.rope, target_row);
            let new_col = point.col.min(line_len);
            cur.char_index = self.rope.line_to_char(target_row) + new_col;
        }
        self.clamp_offset(cur);
    }

    pub(crate) fn move_cursor_left(&self, cur: &mut TextOffset, n: usize) {
        for _ in 0..n {
            cur.char_index = if cur.char_index >= 2
                && self.rope.char(cur.char_index - 2) == '\r'
                && self.rope.char(cur.char_index - 1) == '\n'
            {
                cur.char_index - 2
            } else {
                cur.char_index.saturating_sub(1)
            };
        }
        self.clamp_offset(cur);
    }

    pub(crate) fn move_cursor_right(&self, cur: &mut TextOffset, n: usize) {
        for _ in 0..n {
            cur.char_index = if cur.char_index + 1 < self.rope.len_chars()
                && self.rope.char(cur.char_index) == '\r'
                && self.rope.char(cur.char_index + 1) == '\n'
            {
                cur.char_index + 2
            } else {
                cur.char_index.saturating_add(1).min(self.rope.len_chars())
            };
        }
        self.clamp_offset(cur);
    }

    pub(crate) fn move_cursor_up(&self, cur: &mut TextOffset, n: usize) {
        let point = self.text_point(*cur);
        let target_row = point.row.saturating_sub(n);
        let line_len = line_content_len(&self.rope, target_row);
        let new_col = point.col.min(line_len);
        cur.char_index = self.rope.line_to_char(target_row) + new_col;
        self.clamp_offset(cur);
    }

    pub(crate) fn move_cursor_down(&self, cur: &mut TextOffset, n: usize) {
        let point = self.text_point(*cur);
        let max_row = self.rope.len_lines().saturating_sub(1);
        let target_row = point.row.saturating_add(n).min(max_row);
        let line_len = line_content_len(&self.rope, target_row);
        let new_col = point.col.min(line_len);
        cur.char_index = self.rope.line_to_char(target_row) + new_col;
        self.clamp_offset(cur);
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
        let point = self.text_point(sel.head);
        let line_start = self.rope.line_to_char(point.row);
        sel.head.char_index = sel.head.char_index.saturating_sub(n).max(line_start);
        self.clamp_offset(&mut sel.head);
    }

    pub fn move_head_within_line_right(&self, sel: &mut Selection, n: usize) {
        let point = self.text_point(sel.head);
        let line_end = line_end_char(&self.rope, point.row);
        sel.head.char_index = sel.head.char_index.saturating_add(n).min(line_end);
        self.clamp_offset(&mut sel.head);
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
        let column = self.text_point(sel.head).col;
        let row = line_index.min(self.rope.len_lines().saturating_sub(1));
        sel.head.char_index =
            self.rope.line_to_char(row) + column.min(line_content_len(&self.rope, row));
        self.clamp_offset(&mut sel.head);
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
                let start = head.saturating_add(1).min(line_end);
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
        sel.head.char_index = found;
        self.clamp_offset(&mut sel.head);
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
                let next_content_len = line_content_len(&self.rope, next_row);
                let next_content_start = next_line_start;
                // Count leading whitespace on next line
                let mut ws_len = 0;
                for i in 0..next_content_len {
                    if self.rope.char(next_content_start + i).is_whitespace() {
                        ws_len += 1;
                    } else {
                        break;
                    }
                }
                Some((newline_pos, next_content_start + ws_len, next_line_start))
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
                        (ci, ci + 1, true, at_line_end)
                    } else {
                        (ci, ci, true, true)
                    }
                }
            })
            .collect();
        let mut replacements = Vec::new();
        let mut targeted_chars: Vec<usize> = ranges
            .iter()
            .flat_map(|(start, end, _, _)| *start..*end)
            .collect();
        targeted_chars.sort_unstable();
        targeted_chars.dedup();
        for index in targeted_chars {
            let original = self.rope.char(index);
            let flipped: String = if original.is_uppercase() {
                original.to_lowercase().collect()
            } else if original.is_lowercase() {
                original.to_uppercase().collect()
            } else {
                original.to_string()
            };
            if flipped != original.to_string() {
                replacements.push((index, index + 1, flipped));
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
                let replacement = replacements.iter().find(|(r_start, _, _)| r_start == start);
                let new_start = rebase(*start);
                if *collapsed {
                    let new_end = replacement.map_or_else(
                        || rebase(*end),
                        |(_, _, text)| new_start + text.chars().count(),
                    );
                    if *at_line_end && new_end > new_start {
                        new_end - 1
                    } else {
                        new_end
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
        assert_eq!(b.status(), StatusMessage::None);
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
    fn set_status_changes_message() {
        let mut b = Buffer::new();
        b.set_status(StatusMessage::Saved);
        assert_eq!(b.status(), StatusMessage::Saved);
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
        assert_eq!(b.status(), StatusMessage::NewFile);
    }

    #[test]
    fn open_non_utf8_is_open_failed() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("bin.dat");
        std::fs::write(&path, [0xFF, 0xFE, 0xC0]).unwrap();
        let mut b = Buffer::new();
        let _ = b.open_path(path.to_str().unwrap());
        assert_eq!(b.status(), StatusMessage::OpenFailed);
    }

    #[test]
    fn open_existing_sets_none_status() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("f.txt");
        std::fs::write(&path, "hi").unwrap();
        let mut b = Buffer::new();
        b.open_path(path.to_str().unwrap()).unwrap();
        assert_eq!(b.status(), StatusMessage::None);
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
    fn editing_crlf_buffer_preserves_its_line_ending_style() {
        let mut buffer = Buffer::new();
        buffer.insert_at_selections(&mut single_sel(TextOffset::origin()), "a\r\nb");
        let mut selection = single_sel(cur(4));

        buffer.insert_at_selections(&mut selection, "\n");

        assert_eq!(buffer.slice().to_string(), "a\r\nb\r\n");
        assert_eq!(selection.primary().head().char_index, 6);
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
    fn toggle_case_keeps_all_scalars_from_unicode_mapping() {
        let mut buffer = Buffer::new();
        buffer.insert_at_selections(&mut single_sel(TextOffset::origin()), "ßx");
        let mut selection = single_sel(TextOffset::origin());

        buffer.toggle_case_at_selections(&mut selection);

        assert_eq!(buffer.slice().to_string(), "SSx");
        assert_eq!(selection.primary().head().char_index, 2);
    }
}
