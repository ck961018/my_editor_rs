use ropey::Rope;
use std::borrow::Cow;
use std::io;
use std::path::PathBuf;

use crate::core::keymap::Keymap;
use crate::protocol::selection::{CursorPos, Selection, Selections};
use crate::protocol::status::StatusMessage;

pub struct Buffer {
    rope: Rope,
    path: Option<PathBuf>,
    revision: u64,
    modified: bool,
    status: StatusMessage,
    /// 静态 Content 捕获链使用的普通 keymap；模式化按键由 View 的 ModeInstance 处理。
    keymap: Keymap,
}

impl Buffer {
    pub fn new() -> Self {
        Self {
            rope: Rope::new(),
            path: None,
            revision: 0,
            modified: false,
            status: StatusMessage::None,
            keymap: Keymap::new(),
        }
    }

    pub(crate) fn keymap(&self) -> &Keymap {
        &self.keymap
    }

    #[allow(dead_code)] // Static Content API reserves keymap mutation for future bindings.
    pub(crate) fn keymap_mut(&mut self) -> &mut Keymap {
        &mut self.keymap
    }

    pub fn load_from_file(&mut self, path: &str) -> io::Result<()> {
        self.path = Some(PathBuf::from(path));
        match std::fs::read_to_string(path) {
            Ok(text) => {
                self.rope = Rope::from_str(&text);
                self.advance_revision();
                self.modified = false;
                Ok(())
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                self.rope = Rope::new();
                self.advance_revision();
                self.modified = false;
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

    pub fn mark_saved(&mut self, revision: u64) -> bool {
        if self.revision != revision {
            return false;
        }
        self.modified = false;
        true
    }

    fn advance_revision(&mut self) {
        self.revision = self
            .revision
            .checked_add(1)
            .expect("buffer revision overflow");
    }

    fn mark_modified(&mut self) {
        self.advance_revision();
        self.modified = true;
    }

    pub fn set_status(&mut self, msg: StatusMessage) {
        self.status = msg;
    }

    pub fn status(&self) -> StatusMessage {
        self.status.clone()
    }

    #[allow(dead_code)] // 测试辅助：生产路径走 executor::execute→insert_at_selections
    pub fn insert_char(&mut self, char_idx: usize, ch: char) {
        self.rope.insert_char(char_idx, ch);
        self.mark_modified();
    }

    #[allow(dead_code)] // v0.2 预留：生产路径走 delete_at_selections
    pub fn delete_backward(&mut self, char_idx: usize) -> bool {
        if char_idx == 0 {
            return false;
        }
        self.rope.remove(char_idx - 1..char_idx);
        self.mark_modified();
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
        self.modified
    }

    // ——编辑原语：底层点操作（pub(crate)，操作 head）——

    pub fn recompute_cursor(&self, cur: &mut CursorPos) {
        let clamped = cur.char_index.min(self.rope.len_chars());
        cur.row = self.rope.char_to_line(clamped);
        let line_start = self.rope.line_to_char(cur.row);
        cur.col = clamped - line_start;
    }

    pub(crate) fn move_cursor_by(&self, cur: &mut CursorPos, chars: isize, lines: isize) {
        if chars != 0 {
            let len = self.rope.len_chars() as isize;
            let target = (cur.char_index as isize + chars).clamp(0, len) as usize;
            cur.char_index = target;
        }
        if lines != 0 {
            let max_row = self.rope.len_lines().saturating_sub(1);
            let target_row = (cur.row as isize + lines).clamp(0, max_row as isize) as usize;
            let line_len = line_content_len(&self.rope, target_row);
            let new_col = cur.col.min(line_len);
            cur.char_index = self.rope.line_to_char(target_row) + new_col;
        }
        self.recompute_cursor(cur);
    }

    pub(crate) fn move_cursor_left(&self, cur: &mut CursorPos, n: usize) {
        cur.char_index = cur.char_index.saturating_sub(n);
        self.recompute_cursor(cur);
    }

    pub(crate) fn move_cursor_right(&self, cur: &mut CursorPos, n: usize) {
        cur.char_index = (cur.char_index + n).min(self.rope.len_chars());
        self.recompute_cursor(cur);
    }

    pub(crate) fn move_cursor_up(&self, cur: &mut CursorPos, n: usize) {
        let target_row = cur.row.saturating_sub(n);
        let line_len = line_content_len(&self.rope, target_row);
        let new_col = cur.col.min(line_len);
        cur.char_index = self.rope.line_to_char(target_row) + new_col;
        self.recompute_cursor(cur);
    }

    pub(crate) fn move_cursor_down(&self, cur: &mut CursorPos, n: usize) {
        let max_row = self.rope.len_lines().saturating_sub(1);
        let target_row = (cur.row + n).min(max_row);
        let line_len = line_content_len(&self.rope, target_row);
        let new_col = cur.col.min(line_len);
        cur.char_index = self.rope.line_to_char(target_row) + new_col;
        self.recompute_cursor(cur);
    }

    pub(crate) fn set_cursor(&self, cur: &mut CursorPos, char_idx: usize, _line_idx: usize) {
        cur.char_index = char_idx.min(self.rope.len_chars());
        self.recompute_cursor(cur);
    }

    // ——编辑原语：selection 层（pub，head/anchor 独立，守恒由调用方决定）——

    /// recompute head + anchor 的 row/col（独立 recompute，v0.3 真选区启用）。
    #[allow(dead_code)] // v0.3：生产路径用 move_head_*/shrink 直接维护 row/col；测试与未来多 selection 用
    pub fn recompute_selection(&self, sel: &mut Selection) {
        self.recompute_cursor(&mut sel.head);
        self.recompute_cursor(&mut sel.anchor);
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

    pub fn move_head_up(&self, sel: &mut Selection, n: usize) {
        self.move_cursor_up(&mut sel.head, n);
    }

    pub fn move_head_down(&self, sel: &mut Selection, n: usize) {
        self.move_cursor_down(&mut sel.head, n);
    }

    pub fn move_head_word_forward(&self, sel: &mut Selection) {
        let target = forward_word_start(&self.rope, sel.head.char_index);
        sel.head.char_index = target;
        self.recompute_cursor(&mut sel.head);
    }

    pub fn move_head_word_backward(&self, sel: &mut Selection) {
        let target = backward_word_start(&self.rope, sel.head.char_index);
        sel.head.char_index = target;
        self.recompute_cursor(&mut sel.head);
    }

    pub fn move_head_word_end(&self, sel: &mut Selection) {
        let target = forward_word_end(&self.rope, sel.head.char_index);
        sel.head.char_index = target;
        self.recompute_cursor(&mut sel.head);
    }

    pub fn move_head_to_line_start(&self, sel: &mut Selection) {
        let row = self
            .rope
            .char_to_line(sel.head.char_index.min(self.rope.len_chars()));
        sel.head.char_index = self.rope.line_to_char(row);
        self.recompute_cursor(&mut sel.head);
    }

    pub fn move_head_to_first_non_blank(&self, sel: &mut Selection) {
        let row = self
            .rope
            .char_to_line(sel.head.char_index.min(self.rope.len_chars()));
        sel.head.char_index = first_non_blank_in_line(&self.rope, row);
        self.recompute_cursor(&mut sel.head);
    }

    pub fn move_head_to_line_end(&self, sel: &mut Selection) {
        let row = self
            .rope
            .char_to_line(sel.head.char_index.min(self.rope.len_chars()));
        sel.head.char_index = line_end_char(&self.rope, row);
        self.recompute_cursor(&mut sel.head);
    }

    pub fn move_head_after_line_end(&self, sel: &mut Selection) {
        let row = self
            .rope
            .char_to_line(sel.head.char_index.min(self.rope.len_chars()));
        sel.head.char_index = line_end_insert(&self.rope, row);
        self.recompute_cursor(&mut sel.head);
    }

    pub fn move_head_to_last_line(&self, sel: &mut Selection) {
        let max_row = self.rope.len_lines().saturating_sub(1);
        sel.head.char_index = self.rope.line_to_char(max_row);
        self.recompute_cursor(&mut sel.head);
    }

    pub fn move_head_to_prev_paragraph(&self, sel: &mut Selection) {
        sel.head.char_index = prev_paragraph(&self.rope, sel.head.char_index);
        self.recompute_cursor(&mut sel.head);
    }

    pub fn move_head_to_next_paragraph(&self, sel: &mut Selection) {
        sel.head.char_index = next_paragraph(&self.rope, sel.head.char_index);
        self.recompute_cursor(&mut sel.head);
    }

    /// 设 head，不碰 anchor。
    pub fn set_head(&self, sel: &mut Selection, char_idx: usize, line_idx: usize) {
        self.set_cursor(&mut sel.head, char_idx, line_idx);
    }

    /// anchor = head（collapsed 守恒，由调用方决定时机）。
    pub fn collapse_to_head(sel: &mut Selection) {
        sel.anchor = sel.head;
    }

    /// 在每个 selection 插入文本：非空时先删 [min,max] 再插入，head 到插入末尾，collapse。
    /// 空时在 head 点插入，head 前移 text_len，collapse。
    pub fn insert_at_selections(&mut self, selections: &mut Selections, text: &str) {
        let text_len = text.chars().count();
        // 1) 非空 selection 先删 range（按 min 降序，避免索引偏移）
        let mut del_ranges: Vec<(usize, usize)> = selections
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
        del_ranges.sort_unstable_by_key(|b| std::cmp::Reverse(b.0));
        del_ranges.dedup();
        for (start, end) in del_ranges {
            if end > start {
                self.rope.remove(start..end);
            }
        }
        // 2) 在 min 端点插入（空 selection 在 head）
        let mut insert_indices: Vec<usize> = selections
            .all()
            .map(|s| s.anchor.char_index.min(s.head.char_index))
            .collect();
        insert_indices.sort_unstable_by(|a, b| b.cmp(a));
        insert_indices.dedup();
        for idx in insert_indices {
            self.rope.insert(idx, text);
        }
        self.mark_modified();
        // 3) 更新每个 selection：head = 插入点 + text_len，collapse（编辑后重置 anchor）
        for sel in selections.all_mut() {
            let insert_at = sel.anchor.char_index.min(sel.head.char_index);
            sel.head.char_index = insert_at + text_len;
            self.recompute_cursor(&mut sel.head);
            Self::collapse_to_head(sel);
        }
    }

    /// 在每个 selection 删除：非空时删 [min,max]，head=min，collapse。
    /// 空时按方向删 n，head 回退（backward）或不动（forward），collapse。
    pub fn delete_at_selections(&mut self, selections: &mut Selections, n: isize) {
        let len = self.rope.len_chars();
        // 1) 计算每个 selection 的删除区间
        let mut ranges: Vec<(usize, usize)> = selections
            .all()
            .map(|s| {
                if s.anchor != s.head {
                    let (a, b) = (s.anchor.char_index, s.head.char_index);
                    (a.min(b), a.max(b))
                } else {
                    // 空：按方向删 n
                    let ci = s.head.char_index.min(len);
                    if n < 0 {
                        let start = ci.saturating_sub((-n) as usize);
                        (start, ci)
                    } else {
                        let end = (ci + n as usize).min(len);
                        (ci, end)
                    }
                }
            })
            .collect();
        ranges.sort_unstable_by_key(|b| std::cmp::Reverse(b.0));
        ranges.dedup();
        for (start, end) in ranges {
            if end > start {
                self.rope.remove(start..end);
            }
        }
        self.mark_modified();
        // 2) 更新每个 selection
        for sel in selections.all_mut() {
            if sel.anchor != sel.head {
                // 非空：head = min 端点
                sel.head.char_index = sel.anchor.char_index.min(sel.head.char_index);
            } else if n < 0 {
                // 空 backward：head 回退
                sel.head.char_index = sel.head.char_index.saturating_sub((-n) as usize);
            }
            // 空 forward：head 不动（删除在 head 之后）
            self.recompute_cursor(&mut sel.head);
            Self::collapse_to_head(sel);
        }
    }

    pub fn delete_word_backward_at_selections(&mut self, selections: &mut Selections) {
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
        let mut ranges: Vec<(usize, usize)> = selections
            .all()
            .zip(starts.iter().copied())
            .map(|(selection, start)| {
                let end = selection.anchor.char_index.max(selection.head.char_index);
                (start, end)
            })
            .collect();

        ranges.sort_unstable_by_key(|range| range.0);
        let mut normalized_ranges: Vec<(usize, usize)> = Vec::with_capacity(ranges.len());
        for (start, end) in ranges {
            if let Some((_, previous_end)) = normalized_ranges.last_mut()
                && start <= *previous_end
            {
                *previous_end = (*previous_end).max(end);
            } else {
                normalized_ranges.push((start, end));
            }
        }

        for &(start, end) in normalized_ranges.iter().rev() {
            if end > start {
                self.rope.remove(start..end);
            }
        }
        self.mark_modified();
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
            self.recompute_cursor(&mut selection.head);
            Self::collapse_to_head(selection);
        }
    }

    pub fn delete_to_line_start_at_selections(&mut self, selections: &mut Selections) {
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
        let mut sorted = ranges.clone();
        sorted.sort_unstable_by_key(|b| std::cmp::Reverse(b.0));
        sorted.dedup();
        for (start, end) in &sorted {
            if end > start {
                self.rope.remove(*start..*end);
            }
        }
        self.mark_modified();
        for (sel, (start, _)) in selections.all_mut().zip(ranges.iter()) {
            let mut deleted_before = 0;
            for &(r_start, r_end) in &sorted {
                if r_end <= *start {
                    deleted_before += r_end - r_start;
                }
            }
            sel.head.char_index = start - deleted_before;
            self.recompute_cursor(&mut sel.head);
            Self::collapse_to_head(sel);
        }
    }

    pub fn delete_to_line_end_at_selections(&mut self, selections: &mut Selections) {
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
        let mut sorted = ranges.clone();
        sorted.sort_unstable_by_key(|b| std::cmp::Reverse(b.0));
        sorted.dedup();
        for (start, end) in &sorted {
            if end > start {
                self.rope.remove(*start..*end);
            }
        }
        self.mark_modified();
        for (sel, (start, _end)) in selections.all_mut().zip(ranges.iter()) {
            let mut deleted_before = 0;
            for &(r_start, r_end) in &sorted {
                if r_end <= *start {
                    deleted_before += r_end - r_start;
                }
            }
            sel.head.char_index = start - deleted_before;
            self.recompute_cursor(&mut sel.head);
            Self::collapse_to_head(sel);
        }
    }

    pub fn join_lines_at_selections(&mut self, selections: &mut Selections) {
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
                let next_line_start = newline_pos + 1;
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
        sorted_joins.sort_unstable_by_key(|j| std::cmp::Reverse(j.0));
        for (newline_pos, strip_end, _) in &sorted_joins {
            self.rope.remove(*newline_pos..*strip_end);
            self.rope.insert(*newline_pos, " ");
        }
        self.mark_modified();
        for (sel, (newline_pos, _, _)) in selections.all_mut().zip(joins.iter()) {
            sel.head.char_index = *newline_pos;
            self.recompute_cursor(&mut sel.head);
            Self::collapse_to_head(sel);
        }
    }

    pub fn toggle_case_at_selections(&mut self, selections: &mut Selections) {
        let len = self.rope.len_chars();
        let ranges: Vec<(usize, usize)> = selections
            .all()
            .map(|s| {
                if s.anchor != s.head {
                    let (a, b) = (s.anchor.char_index, s.head.char_index);
                    (a.min(b), a.max(b))
                } else {
                    let ci = s.head.char_index.min(len);
                    if ci < len { (ci, ci + 1) } else { (ci, ci) }
                }
            })
            .collect();
        for (start, end) in &ranges {
            if end > start {
                let slice = self.rope.slice(*start..*end);
                let flipped: String = slice
                    .chars()
                    .map(|c| {
                        if c.is_uppercase() {
                            c.to_lowercase().next().unwrap_or(c)
                        } else if c.is_lowercase() {
                            c.to_uppercase().next().unwrap_or(c)
                        } else {
                            c
                        }
                    })
                    .collect();
                self.rope.remove(*start..*end);
                self.rope.insert(*start, &flipped);
            }
        }
        self.mark_modified();
        for (sel, (_start, end)) in selections.all_mut().zip(ranges.iter()) {
            if sel.anchor == sel.head {
                // Collapsed: advance head by 1 unless at/past line end
                let row = self
                    .rope
                    .char_to_line(sel.head.char_index.min(self.rope.len_chars()));
                let line_end = line_end_char(&self.rope, row);
                if sel.head.char_index < line_end {
                    sel.head.char_index += 1;
                }
            } else {
                sel.head.char_index = *end;
            }
            self.recompute_cursor(&mut sel.head);
            Self::collapse_to_head(sel);
        }
    }

    pub fn insert_new_line_below_at_selections(&mut self, selections: &mut Selections) {
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
        sorted.sort_unstable_by(|a, b| b.cmp(a));
        sorted.dedup();
        for pos in &sorted {
            self.rope.insert(*pos, "\n");
        }
        self.mark_modified();
        for (sel, pos) in selections.all_mut().zip(insert_points.iter()) {
            sel.head.char_index = *pos + 1;
            self.recompute_cursor(&mut sel.head);
            Self::collapse_to_head(sel);
        }
    }

    pub fn insert_new_line_above_at_selections(&mut self, selections: &mut Selections) {
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
        sorted.sort_unstable_by(|a, b| b.cmp(a));
        sorted.dedup();
        for pos in &sorted {
            self.rope.insert(*pos, "\n");
        }
        self.mark_modified();
        for (sel, pos) in selections.all_mut().zip(insert_points.iter()) {
            sel.head.char_index = *pos;
            self.recompute_cursor(&mut sel.head);
            Self::collapse_to_head(sel);
        }
    }

    pub fn delete_line_content_at_selections(&mut self, selections: &mut Selections) {
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
        let mut sorted = ranges.clone();
        sorted.sort_unstable_by_key(|b| std::cmp::Reverse(b.0));
        sorted.dedup();
        for (start, end) in &sorted {
            if end > start {
                self.rope.remove(*start..*end);
            }
        }
        self.mark_modified();
        for (sel, (start, _)) in selections.all_mut().zip(ranges.iter()) {
            sel.head.char_index = *start;
            self.recompute_cursor(&mut sel.head);
            Self::collapse_to_head(sel);
        }
    }
}

impl Default for Buffer {
    fn default() -> Self {
        Self::new()
    }
}

fn line_content_len(rope: &Rope, row: usize) -> usize {
    let s = rope.line(row).to_string();
    match s.strip_suffix('\n') {
        Some(rest) => rest.chars().count(),
        None => s.chars().count(),
    }
}

fn backward_word_start(rope: &Rope, char_index: usize) -> usize {
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

fn forward_word_start(rope: &Rope, char_index: usize) -> usize {
    let len = rope.len_chars();
    let mut pos = char_index.min(len);
    if pos >= len {
        return len;
    }
    // Skip current word/punct unit (same class as char at pos)
    let start_class = char_class(rope.char(pos));
    while pos < len && char_class(rope.char(pos)) == start_class {
        pos += 1;
    }
    // Skip whitespace
    while pos < len && rope.char(pos).is_whitespace() {
        pos += 1;
    }
    pos
}

fn forward_word_end(rope: &Rope, char_index: usize) -> usize {
    let len = rope.len_chars();
    let mut pos = char_index.min(len);
    if pos >= len {
        return len;
    }
    // If on whitespace or at end of current unit, skip whitespace first
    if rope.char(pos).is_whitespace() {
        while pos < len && rope.char(pos).is_whitespace() {
            pos += 1;
        }
        if pos >= len {
            return len;
        }
    } else {
        // If not at end of current unit, the loop below advances to end.
        // If already at end of current unit, skip to next.
        let start_class = char_class(rope.char(pos));
        if pos + 1 < len && char_class(rope.char(pos + 1)) != start_class {
            // Already at end of unit; step past it, then skip whitespace to next word
            pos += 1;
            while pos < len && rope.char(pos).is_whitespace() {
                pos += 1;
            }
            if pos >= len {
                return len;
            }
        }
    }
    // Advance to end of current word/punct unit
    let end_class = char_class(rope.char(pos));
    while pos + 1 < len && char_class(rope.char(pos + 1)) == end_class {
        pos += 1;
    }
    pos
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

fn first_non_blank_in_line(rope: &Rope, row: usize) -> usize {
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

fn line_end_char(rope: &Rope, row: usize) -> usize {
    let line_start = rope.line_to_char(row);
    let content_len = line_content_len(rope, row);
    if content_len == 0 {
        line_start
    } else {
        line_start + content_len - 1
    }
}

fn line_end_insert(rope: &Rope, row: usize) -> usize {
    let line_start = rope.line_to_char(row);
    line_start + line_content_len(rope, row)
}

fn is_empty_line(rope: &Rope, row: usize) -> bool {
    line_content_len(rope, row) == 0
}

fn prev_paragraph(rope: &Rope, char_index: usize) -> usize {
    let cur_row = rope.char_to_line(char_index.min(rope.len_chars()));
    if cur_row == 0 {
        return 0;
    }
    let mut row = cur_row - 1;
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

fn next_paragraph(rope: &Rope, char_index: usize) -> usize {
    let cur_row = rope.char_to_line(char_index.min(rope.len_chars()));
    let max_row = rope.len_lines().saturating_sub(1);
    for row in (cur_row + 1)..=max_row {
        if is_empty_line(rope, row) {
            return rope.line_to_char(row);
        }
    }
    rope.line_to_char(max_row)
}

fn is_word_char(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_'
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::command::{Command, ContentCommand, EditCommand};
    use crate::core::mode::{ModeActionId, ModeId, ModeSet};
    use crate::protocol::key_event::{ArrowKey, KeyCode, KeyEvent};
    use crate::protocol::selection::{Selection, Selections};
    use tempfile::tempdir;

    fn cur(idx: usize) -> CursorPos {
        let mut c = CursorPos::origin();
        c.char_index = idx;
        Buffer::new().recompute_cursor(&mut c);
        c
    }

    fn single_sel(at: CursorPos) -> Selections {
        Selections::single(Selection::collapsed(at))
    }

    fn selection_at(buffer: &Buffer, char_index: usize) -> Selections {
        let mut cursor = CursorPos::origin();
        cursor.char_index = char_index;
        buffer.recompute_cursor(&mut cursor);
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
        b.mark_saved(b.revision());
        assert!(!b.modified());
    }

    #[test]
    fn stale_revision_does_not_clear_modified() {
        let mut b = Buffer::new();
        b.insert_char(0, 'x');
        let saved_revision = b.revision();
        b.insert_char(1, 'y');

        assert!(!b.mark_saved(saved_revision));
        assert!(b.modified());
    }

    #[test]
    fn insert_at_selections_single() {
        let mut b = Buffer::new();
        let mut s = single_sel(CursorPos::origin());
        b.insert_at_selections(&mut s, "hi");
        assert_eq!(b.slice().to_string(), "hi");
        assert_eq!(s.primary().head().char_index, 2);
        assert_eq!((s.primary().head().row, s.primary().head().col), (0, 2));
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
        let mut s = single_sel(CursorPos::origin());
        b.move_head_right(s.primary_mut(), 5);
        Buffer::collapse_to_head(s.primary_mut());
        assert_eq!(s.primary().head().char_index, 2);
        assert_eq!(s.primary().anchor, s.primary().head());
    }

    #[test]
    fn move_head_down_clamps_col_then_collapse() {
        let mut b = Buffer::new();
        b.insert_at_selections(&mut single_sel(CursorPos::origin()), "hello\nab\nworld");
        let mut s = single_sel(CursorPos {
            char_index: 4,
            row: 0,
            col: 0,
        });
        b.recompute_selection(s.primary_mut());
        b.move_head_down(s.primary_mut(), 1);
        assert_eq!((s.primary().head().row, s.primary().head().col), (1, 2));
        Buffer::collapse_to_head(s.primary_mut());
        assert_eq!(s.primary().anchor, s.primary().head());
    }

    #[test]
    fn buffer_keymap_shift_arrow_binds_extend() {
        // 模式化后 shift+方向键绑在 vim Insert keymap；Normal 无此绑定。
        let modes = ModeSet::vim();
        let mut runtime = modes.create_runtime();
        modes.execute(
            &mut runtime,
            ModeId::new("vim"),
            ModeActionId::new("enter-insert"),
        );
        assert_eq!(
            modes.resolve_key(&runtime, KeyEvent::shift_arrow(ArrowKey::Left)),
            Some(Command::Content(ContentCommand::Edit(
                EditCommand::ExtendLeftBy(1)
            )))
        );
        assert_eq!(
            modes.resolve_key(&runtime, KeyEvent::shift_arrow(ArrowKey::Right)),
            Some(Command::Content(ContentCommand::Edit(
                EditCommand::ExtendRightBy(1)
            )))
        );
        assert_eq!(
            modes.resolve_key(&runtime, KeyEvent::shift_arrow(ArrowKey::Up)),
            Some(Command::Content(ContentCommand::Edit(
                EditCommand::ExtendUpBy(1)
            )))
        );
        assert_eq!(
            modes.resolve_key(&runtime, KeyEvent::shift_arrow(ArrowKey::Down)),
            Some(Command::Content(ContentCommand::Edit(
                EditCommand::ExtendDownBy(1)
            )))
        );
    }

    #[test]
    fn buffer_keymap_escape_binds_collapse_selections() {
        // PlainEditMode（非 vim）Escape → CollapseSelections。
        // vim 的 Escape 语义由 vim_*_escape_* 测试覆盖。
        let modes = ModeSet::plain_edit();
        let runtime = modes.create_runtime();
        assert_eq!(
            modes.resolve_key(&runtime, KeyEvent::plain(KeyCode::Escape)),
            Some(Command::Content(ContentCommand::Edit(
                EditCommand::CollapseSelections
            )))
        );
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
        b.insert_at_selections(&mut single_sel(CursorPos::origin()), "hello\nab\nworld");
        let mut s = single_sel(cur(4));
        let anchor_before = s.primary().anchor;
        b.move_head_down(s.primary_mut(), 1);
        assert_eq!(s.primary().head().row, 1);
        assert_eq!(s.primary().anchor, anchor_before);
        assert!(s.primary().anchor != s.primary().head());
    }

    #[test]
    fn insert_at_non_empty_selection_replaces_range() {
        let mut b = Buffer::new();
        b.insert_at_selections(&mut single_sel(CursorPos::origin()), "hello");
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
        b.insert_at_selections(&mut single_sel(CursorPos::origin()), "hello");
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
}
