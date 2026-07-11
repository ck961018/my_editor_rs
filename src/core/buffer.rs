use ropey::Rope;
use std::borrow::Cow;
use std::io;
use std::path::PathBuf;

use crate::core::command::{Command, ContentCommand, EditCommand};
use crate::core::keymap::Keymap;
use crate::core::mode::{Mode, ModeActionId, ModeId};
use crate::protocol::key_event::{ArrowKey, KeyCode, KeyEvent};
use crate::protocol::selection::{CursorPos, Selection, Selections};
use crate::protocol::status::StatusMessage;

pub struct Buffer {
    rope: Rope,
    path: Option<PathBuf>,
    modified: bool,
    status: StatusMessage,
    /// 静态 Content 分发使用的普通 keymap；模式化按键走 `modes`。
    keymap: Keymap,
    modes: BufferModes,
}

impl Buffer {
    pub fn new() -> Self {
        Self {
            rope: Rope::new(),
            path: None,
            modified: false,
            status: StatusMessage::None,
            keymap: Keymap::new(),
            modes: BufferModes::vim(),
        }
    }

    pub(crate) fn keymap(&self) -> &Keymap {
        &self.keymap
    }

    #[cfg(test)]
    pub(crate) fn keymap_mut(&mut self) -> &mut Keymap {
        &mut self.keymap
    }

    pub(crate) fn resolve_key(&self, key: KeyEvent) -> Option<Command> {
        self.modes.resolve_key(key)
    }

    pub(crate) fn handle_mode_command(&mut self, mode: ModeId, action: ModeActionId) {
        self.modes.handle_mode_command(mode, action);
    }

    pub fn load_from_file(&mut self, path: &str) -> io::Result<()> {
        self.path = Some(PathBuf::from(path));
        match std::fs::read_to_string(path) {
            Ok(text) => {
                self.rope = Rope::from_str(&text);
                self.modified = false;
                Ok(())
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                self.rope = Rope::new();
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

    pub fn mark_saved(&mut self) {
        self.modified = false;
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
        self.modified = true;
    }

    #[allow(dead_code)] // v0.2 预留：生产路径走 delete_at_selections
    pub fn delete_backward(&mut self, char_idx: usize) -> bool {
        if char_idx == 0 {
            return false;
        }
        self.rope.remove(char_idx - 1..char_idx);
        self.modified = true;
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
        del_ranges.sort_unstable_by(|a, b| b.0.cmp(&a.0));
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
        self.modified = true;
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
        ranges.sort_unstable_by(|a, b| b.0.cmp(&a.0));
        ranges.dedup();
        for (start, end) in ranges {
            if end > start {
                self.rope.remove(start..end);
            }
        }
        self.modified = true;
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
}

impl Default for Buffer {
    fn default() -> Self {
        Self::new()
    }
}

/// Buffer 模式 runtime：单 Base 层（默认 vim）。Box<dyn Mode> 非 Clone，
/// 但 Buffer 不需要 Clone（全仓无 derive/impl Clone、无 .clone() 调用）。
struct BufferModes {
    base: Box<dyn Mode>,
}

impl BufferModes {
    fn vim() -> Self {
        Self {
            base: Box::new(VimMode::new()),
        }
    }

    #[cfg(test)]
    fn plain_edit() -> Self {
        Self {
            base: Box::new(PlainEditMode::new()),
        }
    }

    // 不变式：mode keymap 不得使用 prefix 绑定。dispatcher 的 prefix 状态机只看
    // Content::keymap()（Buffer 保持空），看不到 mode runtime keymap；
    // 此处若命中 Prefix 会落入 typing 兜底而非挂起等待，前缀将被静默丢弃。
    fn resolve_key(&self, key: KeyEvent) -> Option<Command> {
        match self.base.keymap().lookup(key) {
            Some(crate::core::keymap::KeyBinding::Command(command)) => Some(command.clone()),
            Some(crate::core::keymap::KeyBinding::Prefix(_)) | None => self.base.typing(key),
        }
    }

    fn handle_mode_command(&mut self, mode: ModeId, action: ModeActionId) {
        if self.base.id() == mode {
            self.base.handle_mode_command(action);
        }
    }
}

#[cfg(test)]
struct PlainEditMode {
    keymap: Keymap,
}

#[cfg(test)]
impl PlainEditMode {
    fn new() -> Self {
        Self {
            keymap: plain_edit_keymap(),
        }
    }
}

#[cfg(test)]
impl Mode for PlainEditMode {
    fn id(&self) -> ModeId {
        ModeId::new("plain-edit")
    }

    fn label(&self) -> &str {
        "PLAIN"
    }

    fn keymap(&self) -> &Keymap {
        &self.keymap
    }

    fn typing(&self, key: KeyEvent) -> Option<Command> {
        key.is_plain_char()
            .map(|ch| EditCommand::InsertText(ch.to_string()).into())
    }

    fn handle_mode_command(&mut self, _action: ModeActionId) {}
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum VimState {
    Normal,
    Insert,
}

struct VimMode {
    state: VimState,
    normal_keymap: Keymap,
    insert_keymap: Keymap,
}

impl VimMode {
    fn new() -> Self {
        Self {
            state: VimState::Normal,
            normal_keymap: vim_normal_keymap(),
            insert_keymap: vim_insert_keymap(),
        }
    }
}

impl Mode for VimMode {
    fn id(&self) -> ModeId {
        ModeId::new("vim")
    }

    fn label(&self) -> &str {
        match self.state {
            VimState::Normal => "NORMAL",
            VimState::Insert => "INSERT",
        }
    }

    fn keymap(&self) -> &Keymap {
        match self.state {
            VimState::Normal => &self.normal_keymap,
            VimState::Insert => &self.insert_keymap,
        }
    }

    fn typing(&self, key: KeyEvent) -> Option<Command> {
        match self.state {
            VimState::Normal => None,
            VimState::Insert => key
                .is_plain_char()
                .map(|ch| EditCommand::InsertText(ch.to_string()).into()),
        }
    }

    fn handle_mode_command(&mut self, action: ModeActionId) {
        match action.as_str() {
            "enter-insert" => self.state = VimState::Insert,
            "enter-normal" => self.state = VimState::Normal,
            _ => {}
        }
    }
}

#[cfg(test)]
fn plain_edit_keymap() -> Keymap {
    default_text_keymap(true)
}

fn vim_insert_keymap() -> Keymap {
    default_text_keymap(false)
}

fn default_text_keymap(bind_escape_to_collapse: bool) -> Keymap {
    let mut km = Keymap::new();
    km.bind_edit(
        KeyEvent::plain(KeyCode::Enter),
        EditCommand::InsertText("\n".to_string()),
    );
    km.bind_edit(KeyEvent::plain(KeyCode::Backspace), EditCommand::Delete(-1));
    km.bind_edit(KeyEvent::arrow(ArrowKey::Left), EditCommand::MoveLeftBy(1));
    km.bind_edit(
        KeyEvent::arrow(ArrowKey::Right),
        EditCommand::MoveRightBy(1),
    );
    km.bind_edit(KeyEvent::arrow(ArrowKey::Up), EditCommand::MoveUpBy(1));
    km.bind_edit(KeyEvent::arrow(ArrowKey::Down), EditCommand::MoveDownBy(1));
    km.bind_edit(
        KeyEvent::shift_arrow(ArrowKey::Left),
        EditCommand::ExtendLeftBy(1),
    );
    km.bind_edit(
        KeyEvent::shift_arrow(ArrowKey::Right),
        EditCommand::ExtendRightBy(1),
    );
    km.bind_edit(
        KeyEvent::shift_arrow(ArrowKey::Up),
        EditCommand::ExtendUpBy(1),
    );
    km.bind_edit(
        KeyEvent::shift_arrow(ArrowKey::Down),
        EditCommand::ExtendDownBy(1),
    );
    if bind_escape_to_collapse {
        km.bind_edit(
            KeyEvent::plain(KeyCode::Escape),
            EditCommand::CollapseSelections,
        );
    } else {
        km.bind(
            KeyEvent::plain(KeyCode::Escape),
            Command::Content(ContentCommand::Mode {
                mode: ModeId::new("vim"),
                action: ModeActionId::new("enter-normal"),
            }),
        );
    }
    km
}

fn vim_normal_keymap() -> Keymap {
    let mut km = Keymap::new();
    km.bind_edit(KeyEvent::char('h'), EditCommand::MoveLeftBy(1));
    km.bind_edit(KeyEvent::char('j'), EditCommand::MoveDownBy(1));
    km.bind_edit(KeyEvent::char('k'), EditCommand::MoveUpBy(1));
    km.bind_edit(KeyEvent::char('l'), EditCommand::MoveRightBy(1));
    km.bind(
        KeyEvent::char('i'),
        Command::Content(ContentCommand::Mode {
            mode: ModeId::new("vim"),
            action: ModeActionId::new("enter-insert"),
        }),
    );
    km.bind(KeyEvent::plain(KeyCode::Escape), Command::Noop);
    km
}

fn line_content_len(rope: &Rope, row: usize) -> usize {
    let s = rope.line(row).to_string();
    match s.strip_suffix('\n') {
        Some(rest) => rest.chars().count(),
        None => s.chars().count(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
        b.mark_saved();
        assert!(!b.modified());
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
        let mut b = Buffer::new();
        b.handle_mode_command(ModeId::new("vim"), ModeActionId::new("enter-insert"));
        assert_eq!(
            b.resolve_key(KeyEvent::shift_arrow(ArrowKey::Left)),
            Some(Command::Content(ContentCommand::Edit(
                EditCommand::ExtendLeftBy(1)
            )))
        );
        assert_eq!(
            b.resolve_key(KeyEvent::shift_arrow(ArrowKey::Right)),
            Some(Command::Content(ContentCommand::Edit(
                EditCommand::ExtendRightBy(1)
            )))
        );
        assert_eq!(
            b.resolve_key(KeyEvent::shift_arrow(ArrowKey::Up)),
            Some(Command::Content(ContentCommand::Edit(
                EditCommand::ExtendUpBy(1)
            )))
        );
        assert_eq!(
            b.resolve_key(KeyEvent::shift_arrow(ArrowKey::Down)),
            Some(Command::Content(ContentCommand::Edit(
                EditCommand::ExtendDownBy(1)
            )))
        );
    }

    #[test]
    fn buffer_keymap_escape_binds_collapse_selections() {
        // PlainEditMode（非 vim）Escape → CollapseSelections。
        // vim 的 Escape 语义由 vim_*_escape_* 测试覆盖。
        let modes = BufferModes::plain_edit();
        assert_eq!(
            modes.resolve_key(KeyEvent::plain(KeyCode::Escape)),
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

    #[test]
    fn default_buffer_uses_vim_normal_and_plain_char_is_not_insert() {
        let b = Buffer::new();
        assert!(b.resolve_key(KeyEvent::char('a')).is_none());
    }

    #[test]
    fn vim_i_enters_insert_and_plain_char_inserts() {
        let mut b = Buffer::new();
        assert_eq!(
            b.resolve_key(KeyEvent::char('i')),
            Some(Command::Content(ContentCommand::Mode {
                mode: ModeId::new("vim"),
                action: ModeActionId::new("enter-insert"),
            }))
        );
        b.handle_mode_command(ModeId::new("vim"), ModeActionId::new("enter-insert"));
        assert_eq!(
            b.resolve_key(KeyEvent::char('a')),
            Some(Command::Content(ContentCommand::Edit(
                EditCommand::InsertText("a".to_string())
            )))
        );
    }

    #[test]
    fn vim_escape_returns_to_normal() {
        let mut b = Buffer::new();
        b.handle_mode_command(ModeId::new("vim"), ModeActionId::new("enter-insert"));
        assert_eq!(
            b.resolve_key(KeyEvent::plain(KeyCode::Escape)),
            Some(Command::Content(ContentCommand::Mode {
                mode: ModeId::new("vim"),
                action: ModeActionId::new("enter-normal"),
            }))
        );
        b.handle_mode_command(ModeId::new("vim"), ModeActionId::new("enter-normal"));
        assert!(b.resolve_key(KeyEvent::char('a')).is_none());
    }
}
