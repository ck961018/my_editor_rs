# Selection 模型 + View 实体归属 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 把 cursor 升级为 selection 退化形态（Helix 风 `Selections{ranges, primary_index}`），归属从 `App.cursors` 迁到 `View` 实体（按 SpaceId 索引），`ContentQuery::cursor(cid)` 改 `selections(sid)->Selections`。

**Architecture:** `protocol/selection.rs` 定义 `Selection{anchor,head}` + `Selections{ranges, primary_index}`（Helix 风，v0.2 恒 collapsed）。`app/view.rs` 引入 `View{content, selections}` 编辑会话实体，`App.views: HashMap<SpaceId, View>` 替代 `App.cursors`。编辑原语接 `Selection`/`Selections`，操作 head + 守恒 collapsed（`anchor=head`）。`ContentQuery` 返回完整 `Selections`，维度 ContentId→SpaceId。v0.2 不实现真选区编辑/多视图，模型就位为 v0.3 铺路。

**Tech Stack:** Rust 2024, ropey, taffy 0.11, crossterm 0.29, tokio。

**Spec:** `docs/superpowers/specs/2026-07-07-selection-model-and-view-ownership-design.md`

---

## File Structure

| 文件 | 变更 | 职责 |
|---|---|---|
| `protocol/selection.rs` | 新建（Task 1）→ 扩展（Task 6） | `CursorPos` + `Selection` + `Selections` 数据模型 |
| `protocol/cursor.rs` | 删（Task 6） | 内容并入 selection.rs |
| `protocol/mod.rs` | 改 | `pub mod selection;`（Task 1），删 `pub mod cursor;`（Task 6） |
| `protocol/content_query.rs` | 改（Task 5） | `cursor(cid)->CursorPos` → `selections(sid)->Selections` |
| `core/content.rs` | 改（Task 2） | 删 `Cursors` 定义，引用 `Selections` |
| `core/buffer.rs` | 改（Task 2） | 编辑原语接 `Selection`/`Selections`；底层 `move_cursor_*`/`set_cursor` 降 `pub(crate)` |
| `app/view.rs` | 新建（Task 3） | `View` 编辑会话实体 |
| `app/mod.rs` | 改（Task 2/4/5） | `cursors`→`views`、`AppQuery`、`App impl ContentQuery` |
| `app/executor.rs` | 改（Task 2） | 签名 `cursors: &mut Cursors` → `selections: &mut Selections` |
| `tui/scene_renderer.rs` | 改（Task 5） | `query.cursor(cid)` → `query.selections(focused).primary().head()` |
| `docs/design/current-architecture.md` | 改（Task 7） | 反映新模型 |

---

## Task 1: protocol/selection.rs——Selection + Selections 类型

**Files:**
- Create: `src/protocol/selection.rs`
- Modify: `src/protocol/mod.rs:1`（加 `pub mod selection;`）
- Test: `src/protocol/selection.rs`（内联 tests）

`CursorPos` 暂留在 `protocol/cursor.rs`（Task 6 才迁入），本任务 `selection.rs` 用 `use crate::protocol::cursor::CursorPos` 引用。

- [ ] **Step 1: 写失败测试**

在 `src/protocol/selection.rs` 写测试（先写测试，再写实现会编译失败）：

```rust
//! Selection 数据模型：cursor 是 selection 的退化形态（collapsed，anchor==head）。
//! Helix 风集合：ranges + primary_index。v0.2 恒 collapsed、ranges.len()==1。

use crate::protocol::cursor::CursorPos;

/// 选区：anchor 选择起点，head 光标位置（驱动编辑/渲染）。空 selection：anchor==head。
/// 方向隐含：head>anchor=forward。不加 direction 字段。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Selection {
    pub anchor: CursorPos,
    pub head: CursorPos,
}

impl Selection {
    pub fn collapsed(at: CursorPos) -> Self { Self { anchor: at, head: at } }
    pub fn is_empty(&self) -> bool { self.anchor == self.head }
    pub fn head(&self) -> CursorPos { self.head }
}

/// 多选区容器（Helix 风）。ranges 恒按 head.char_index 升序（v0.2 单元素，约定在）。
/// v0.2 不变量：ranges.len()==1、primary_index==0、所有 Selection collapsed。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Selections {
    ranges: Vec<Selection>,
    primary_index: usize,
}

impl Selections {
    pub fn single(sel: Selection) -> Self { Self { ranges: vec![sel], primary_index: 0 } }

    pub fn primary(&self) -> &Selection { &self.ranges[self.primary_index] }
    pub fn primary_mut(&mut self) -> &mut Selection { &mut self.ranges[self.primary_index] }
    pub fn all(&self) -> impl Iterator<Item = &Selection> { self.ranges.iter() }
    pub fn all_mut(&mut self) -> impl Iterator<Item = &mut Selection> { self.ranges.iter_mut() }

    /// 清除 secondary ranges，仅保留 primary（v0.2 noop：ranges 本就 len==1）。
    pub fn retain_primary(&mut self) {
        let primary = self.ranges[self.primary_index];
        self.ranges = vec![primary];
        self.primary_index = 0;
    }

    /// 测试构造器：多 ranges + 指定 primary_index。非 v0.2 正常路径使用。
    #[cfg(test)]
    pub(crate) fn from_parts(ranges: Vec<Selection>, primary_index: usize) -> Self {
        assert!(primary_index < ranges.len());
        Self { ranges, primary_index }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collapsed_is_empty() {
        let s = Selection::collapsed(CursorPos::origin());
        assert!(s.is_empty());
        assert_eq!(s.head(), CursorPos::origin());
    }

    #[test]
    fn non_empty_selection() {
        let s = Selection { anchor: CursorPos::origin(), head: CursorPos { char_index: 3, row: 0, col: 3 } };
        assert!(!s.is_empty());
    }

    #[test]
    fn single_has_one_range_primary_index_zero() {
        let s = Selections::single(Selection::collapsed(CursorPos::origin()));
        assert_eq!(s.primary(), &Selection::collapsed(CursorPos::origin()));
        let count = s.all().count();
        assert_eq!(count, 1);
    }

    #[test]
    fn primary_mut_updates_head() {
        let mut s = Selections::single(Selection::collapsed(CursorPos::origin()));
        s.primary_mut().head = CursorPos { char_index: 5, row: 0, col: 5 };
        assert_eq!(s.primary().head().char_index, 5);
    }

    #[test]
    fn all_mut_updates_all_ranges() {
        let mut s = Selections::from_parts(vec![
            Selection::collapsed(CursorPos::origin()),
            Selection::collapsed(CursorPos { char_index: 3, row: 0, col: 3 }),
        ], 0);
        for sel in s.all_mut() { sel.head = CursorPos { char_index: 9, row: 0, col: 9 }; }
        assert_eq!(s.all().count(), 2);
        assert!(s.all().all(|sel| sel.head.char_index == 9));
    }

    #[test]
    fn retain_primary_drops_secondaries() {
        let mut s = Selections::from_parts(vec![
            Selection::collapsed(CursorPos::origin()),
            Selection::collapsed(CursorPos { char_index: 3, row: 0, col: 3 }),
        ], 0);
        s.retain_primary();
        assert_eq!(s.all().count(), 1);
        assert_eq!(s.primary(), &Selection::collapsed(CursorPos::origin()));
    }

    #[test]
    fn retain_primary_on_single_is_noop() {
        let mut s = Selections::single(Selection::collapsed(CursorPos::origin()));
        s.retain_primary();
        assert_eq!(s.all().count(), 1);
    }
}
```

- [ ] **Step 2: 注册模块**

`src/protocol/mod.rs` 第 1 行后加 `pub mod selection;`（保留 `pub mod cursor;`）：

```rust
pub mod content_query;
pub mod cursor;
pub mod selection;
pub mod frontend_event;
pub mod geometry;
pub mod ids;
pub mod key_event;
pub mod scene;
pub mod space;
pub mod status;
pub mod viewport;
```

- [ ] **Step 3: 运行测试验证通过**

Run: `cargo test protocol::selection`
Expected: 7 tests passed。

- [ ] **Step 4: 全量编译验证**

Run: `cargo build`
Expected: 无错误（cursor.rs 未动，selection.rs 新增）。

- [ ] **Step 5: Commit**

```bash
git add src/protocol/selection.rs src/protocol/mod.rs
git commit -m "feat(protocol): Selection + Selections 类型（Helix 风 ranges+primary_index）"
```

---

## Task 2: Cursors→Selections 类型替换全链路

把 `Cursors` 类型整体替换为 `Selections`，buffer 编辑原语接 `Selection`/`Selections`。本任务**不改**字段名（`App.cursors` 仍叫 cursors）、**不改** `ContentQuery::cursor(cid)` 签名、**不改** `App.cursors` 的 key（仍 ContentId）——这些留 Task 4/5。本任务只做类型替换 + 编辑原语升级。

**Files:**
- Modify: `src/core/content.rs`（删 Cursors 定义）
- Modify: `src/core/buffer.rs`（编辑原语升级）
- Modify: `src/app/executor.rs`（签名 + 调用）
- Modify: `src/app/mod.rs`（App.cursors 类型 + AppQuery.cursor + tests）

- [ ] **Step 1: core/content.rs 删 Cursors，引用 Selections**

`src/core/content.rs` 删除 `Cursors` struct + impl + 相关 tests，改引用 `protocol::selection::Selections`。完整新文件：

```rust
use crate::core::buffer::Buffer;
use crate::core::keymap::Keymap;
use crate::core::operation::Operation;
use crate::core::status_bar::StatusBar;
use crate::protocol::selection::Selections;
use crate::protocol::ids::ContentId;
use crate::protocol::key_event::KeyEvent;

pub trait ContentLookup {
    fn get(&self, id: ContentId) -> Option<&dyn ContentHandler>;
}

/// content 多态契约：自持 keymap + 类型查询。仅分发契约（查表返回 Operation），
/// 不含渲染——渲染由前端 pull ContentQuery 自治。
pub trait ContentHandler {
    fn keymap(&self) -> &Keymap;
    #[allow(dead_code)] // 测试用：生产路径只读 keymap
    fn keymap_mut(&mut self) -> &mut Keymap;
    fn default_binding(&self, _key: KeyEvent) -> Option<Operation> { None }
    fn buffer_mut(&mut self) -> Option<&mut Buffer> { None }
    /// 只读 Buffer 查询（ContentQuery impl 用）。
    fn as_buffer(&self) -> Option<&Buffer> { None }
    /// 只读 StatusBar 查询（ContentQuery impl 用）。
    fn as_status_bar(&self) -> Option<&StatusBar> { None }
}

#[cfg(test)]
mod tests {
    // Cursors 测试已移至 protocol::selection。本模块无剩余测试。
}
```

注：`Selections` 类型由 `protocol::selection` 定义，`core` 仅引用。`Cursors` 名字彻底移除。

- [ ] **Step 2: core/buffer.rs 编辑原语升级**

`src/core/buffer.rs` 改动：
1. import：`use crate::protocol::cursor::CursorPos;` → `use crate::protocol::selection::{CursorPos, Selection, Selections};`
2. `move_cursor_*`/`set_cursor` 降级 `pub(crate)`（底层，操作单点 head）
3. `recompute_cursor` 保持 `pub`（跨模块测试 helper 用）
4. 新增 `recompute_selection`/`move_selection_*`/`set_selection`（`pub`，守恒 collapsed）
5. `insert_at_cursors`/`delete_at_cursors` 改名 `insert_at_selections`/`delete_at_selections`，接 `&mut Selections`
6. tests 改用 `Selections`

编辑原语段（替换 `src/core/buffer.rs:123-221` 的 `recompute_cursor` 到 `delete_at_cursors` 整段）：

```rust
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

    // ——编辑原语：selection 层（pub，守恒 collapsed）——

    /// recompute head + anchor 的 row/col（v0.2 anchor==head，幂等）。
    pub fn recompute_selection(&self, sel: &mut Selection) {
        self.recompute_cursor(&mut sel.head);
        self.recompute_cursor(&mut sel.anchor);
    }

    /// v0.2：移动 head 并保持 collapsed（anchor=head）。
    pub fn move_selection_by(&self, sel: &mut Selection, chars: isize, lines: isize) {
        self.move_cursor_by(&mut sel.head, chars, lines);
        sel.anchor = sel.head;
    }

    pub fn move_selection_left(&self, sel: &mut Selection, n: usize) {
        self.move_cursor_left(&mut sel.head, n);
        sel.anchor = sel.head;
    }

    pub fn move_selection_right(&self, sel: &mut Selection, n: usize) {
        self.move_cursor_right(&mut sel.head, n);
        sel.anchor = sel.head;
    }

    pub fn move_selection_up(&self, sel: &mut Selection, n: usize) {
        self.move_cursor_up(&mut sel.head, n);
        sel.anchor = sel.head;
    }

    pub fn move_selection_down(&self, sel: &mut Selection, n: usize) {
        self.move_cursor_down(&mut sel.head, n);
        sel.anchor = sel.head;
    }

    pub fn set_selection(&self, sel: &mut Selection, char_idx: usize, line_idx: usize) {
        self.set_cursor(&mut sel.head, char_idx, line_idx);
        sel.anchor = sel.head;
    }

    /// 在每个 selection 的 head 插入文本，head 前移 text_len，anchor=head（守恒 collapsed）。
    pub fn insert_at_selections(&mut self, selections: &mut Selections, text: &str) {
        let text_len = text.chars().count();
        let mut indices: Vec<usize> = selections.all().map(|s| s.head.char_index).collect();
        indices.sort_unstable_by(|a, b| b.cmp(a));
        indices.dedup();
        for idx in indices {
            self.rope.insert(idx, text);
        }
        self.modified = true;
        for sel in selections.all_mut() {
            sel.head.char_index += text_len;
            sel.anchor = sel.head;
            self.recompute_cursor(&mut sel.head);
        }
    }

    /// 在每个 selection 的 head 方向删 n，head 回退，anchor=head（守恒 collapsed）。
    pub fn delete_at_selections(&mut self, selections: &mut Selections, n: isize) {
        let len = self.rope.len_chars();
        let mut ranges: Vec<(usize, usize)> = selections.all().map(|s| {
            let ci = s.head.char_index.min(len);
            if n < 0 {
                let start = ci.saturating_sub((-n) as usize);
                (start, ci)
            } else {
                let end = (ci + n as usize).min(len);
                (ci, end)
            }
        }).collect();
        ranges.sort_unstable_by(|a, b| b.0.cmp(&a.0));
        ranges.dedup();
        for (start, end) in ranges {
            if end > start {
                self.rope.remove(start..end);
            }
        }
        self.modified = true;
        for sel in selections.all_mut() {
            if n < 0 {
                sel.head.char_index = sel.head.char_index.saturating_sub((-n) as usize);
            }
            sel.anchor = sel.head;
            self.recompute_cursor(&mut sel.head);
        }
    }
```

`insert_char`/`delete_backward` 的 `#[allow(dead_code)]` 注释里提到的 `insert_at_cursors`/`delete_at_cursors` 改名引用：
- `src/core/buffer.rs:80` 注释 `生产路径走 executor::execute→insert_at_cursors` → `→insert_at_selections`
- `src/core/buffer.rs:86` 注释 `生产路径走 delete_at_cursors` → `→delete_at_selections`

`src/core/buffer.rs` tests 改写（替换 `tests` mod 内涉及 Cursors 的部分）：

```rust
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
        assert_eq!(s.primary().anchor, s.primary().head); // collapsed 守恒
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
        assert_eq!(s2.primary().anchor, s2.primary().head);
    }

    #[test]
    fn move_selection_right_clamps_and_collapsed() {
        let mut b = Buffer::new();
        b.insert_char(0, 'a');
        b.insert_char(1, 'b');
        let mut s = single_sel(CursorPos::origin());
        b.move_selection_right(&mut s.primary_mut(), 5);
        assert_eq!(s.primary().head().char_index, 2);
        assert_eq!(s.primary().anchor, s.primary().head);
    }

    #[test]
    fn move_selection_up_down_clamps_col() {
        let mut b = Buffer::new();
        b.insert_at_selections(&mut single_sel(CursorPos::origin()), "hello\nab\nworld");
        let mut s = single_sel(CursorPos { char_index: 4, row: 0, col: 0 });
        b.recompute_selection(&mut s.primary_mut());
        b.move_selection_down(&mut s.primary_mut(), 1);
        assert_eq!((s.primary().head().row, s.primary().head().col), (1, 2));
        assert_eq!(s.primary().anchor, s.primary().head());
    }

    #[test]
    fn default_binding_char_to_insert() {
        let b = Buffer::new();
        let op = b.default_binding(KeyEvent::Char(b'a')).unwrap();
        assert_eq!(op, Operation::CursorInsertText("a".to_string()));
    }

    #[test]
    fn default_binding_non_char_is_none() {
        let b = Buffer::new();
        assert!(b.default_binding(KeyEvent::Escape).is_none());
    }

    #[test]
    fn buffer_mut_returns_self() {
        let mut b = Buffer::new();
        assert!(b.buffer_mut().is_some());
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
}
```

- [ ] **Step 3: app/executor.rs 签名 + 调用 + tests**

`src/app/executor.rs` 完整新文件：

```rust
use crate::core::content::ContentHandler;
use crate::core::operation::Operation;
use crate::protocol::selection::Selections;

/// 执行局部 Operation（选区/文本）。全局/多光标变体不进此处（App 分流）。
pub fn execute(op: Operation, content: &mut dyn ContentHandler, selections: &mut Selections) {
    let Some(buf) = content.buffer_mut() else { return; };
    match op {
        Operation::CursorMoveBy { chars, lines } => {
            for sel in selections.all_mut() { buf.move_selection_by(sel, chars, lines); }
        }
        Operation::CursorMoveLeftBy(n) => {
            for sel in selections.all_mut() { buf.move_selection_left(sel, n); }
        }
        Operation::CursorMoveRightBy(n) => {
            for sel in selections.all_mut() { buf.move_selection_right(sel, n); }
        }
        Operation::CursorMoveUpBy(n) => {
            for sel in selections.all_mut() { buf.move_selection_up(sel, n); }
        }
        Operation::CursorMoveDownBy(n) => {
            for sel in selections.all_mut() { buf.move_selection_down(sel, n); }
        }
        Operation::CursorMoveTo { char_idx, line_idx } => {
            buf.set_selection(selections.primary_mut(), char_idx, line_idx);
            selections.retain_primary();
        }
        Operation::CursorInsertText(text) => buf.insert_at_selections(selections, &text),
        Operation::CursorDelete(n) => buf.delete_at_selections(selections, n),
        // 全局/多光标变体不进 executor
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::buffer::Buffer;
    use crate::core::operation::Operation;
    use crate::protocol::selection::{Selection, Selections};
    use crate::protocol::cursor::CursorPos;

    fn single_sel(at: CursorPos) -> Selections {
        Selections::single(Selection::collapsed(at))
    }

    #[test]
    fn insert_text_changes_buffer_and_selection() {
        let mut buf = Buffer::new();
        let mut s = single_sel(CursorPos::origin());
        execute(Operation::CursorInsertText("hi".to_string()), &mut buf, &mut s);
        assert_eq!(buf.slice().to_string(), "hi");
        assert_eq!(s.primary().head().char_index, 2);
        assert_eq!(s.primary().anchor, s.primary().head());
    }

    #[test]
    fn delete_left_removes_char() {
        let mut buf = Buffer::new();
        buf.insert_char(0, 'a');
        buf.insert_char(1, 'b');
        let mut s = {
            let mut c = CursorPos::origin();
            c.char_index = 2;
            buf.recompute_cursor(&mut c);
            single_sel(c)
        };
        execute(Operation::CursorDelete(-1), &mut buf, &mut s);
        assert_eq!(buf.slice().to_string(), "a");
        assert_eq!(s.primary().head().char_index, 1);
        assert_eq!(s.primary().anchor, s.primary().head());
    }

    #[test]
    fn move_right_advances_head() {
        let mut buf = Buffer::new();
        buf.insert_char(0, 'a');
        let mut s = single_sel(CursorPos::origin());
        execute(Operation::CursorMoveRightBy(1), &mut buf, &mut s);
        assert_eq!(s.primary().head().char_index, 1);
        assert_eq!(s.primary().anchor, s.primary().head());
    }

    #[test]
    fn move_to_retains_primary_clears_secondaries() {
        let mut buf = Buffer::new();
        buf.insert_char(0, 'a');
        let mut s = Selections::from_parts(vec![
            Selection::collapsed(CursorPos::origin()),
            Selection::collapsed(CursorPos::origin()),
        ], 0);
        execute(Operation::CursorMoveTo { char_idx: 0, line_idx: 0 }, &mut buf, &mut s);
        assert_eq!(s.all().count(), 1);
        assert_eq!(s.primary().anchor, s.primary().head());
    }
}
```

- [ ] **Step 4: app/mod.rs 类型替换（字段名 cursors 保留，类型 Cursors→Selections）**

`src/app/mod.rs` 改动点：

1. import（`src/app/mod.rs:20`）：
   - `use crate::core::content::{ContentHandler, ContentLookup, Cursors};` → `use crate::core::content::{ContentHandler, ContentLookup};`
   - 加 `use crate::protocol::selection::{Selection, Selections};`（`src/app/mod.rs:29` 的 `use crate::protocol::cursor::CursorPos;` 保留，CursorPos 仍在 cursor.rs）

2. `App.cursors` 字段类型（`src/app/mod.rs:39`）：
   - `cursors: HashMap<ContentId, Cursors>,` → `cursors: HashMap<ContentId, Selections>,`

3. `App::new` 建 cursors（`src/app/mod.rs:68-70`）：
   ```rust
   let mut cursors: HashMap<ContentId, Selections> = HashMap::new();
   cursors.insert(editor_content, Selections::single(Selection::collapsed(CursorPos::origin())));
   cursors.insert(status_content, Selections::single(Selection::collapsed(CursorPos::origin())));
   ```

4. `AppQuery` 字段（`src/app/mod.rs:226`）：
   - `cursors: &'a HashMap<ContentId, Cursors>,` → `cursors: &'a HashMap<ContentId, Selections>,`

5. `AppQuery::cursor` 实现（`src/app/mod.rs:246-248`）：
   ```rust
   fn cursor(&self, cid: ContentId) -> CursorPos {
       self.cursors.get(&cid).map(|c| c.primary().head()).unwrap_or_else(CursorPos::origin)
   }
   ```

6. `App impl ContentQuery::cursor`（`src/app/mod.rs:261-263`）保持委托 AppQuery（无需改，因 AppQuery::cursor 签名不变）。

7. tests（`src/app/mod.rs:328` 等）改用 `Selections`：
   - `src/app/mod.rs:328`: `let cursor = app.cursors.get(&editor_cid()).expect("editor cursor exists").primary;` → `.primary().head();`
   - `src/app/mod.rs:295`: `assert_eq!(ContentQuery::cursor(&app, editor_cid()), CursorPos::origin());` 不变（cursor 返回 CursorPos）。

`AppQuery` 完整段（`src/app/mod.rs:224-252`）改后：

```rust
/// 借 App 数据字段的查询适配器：render 时用它做 `&dyn ContentQuery`，
/// 与 `&mut self.frontend` 不冲突（字段级 split borrow）。
struct AppQuery<'a> {
    contents: &'a HashMap<ContentId, Box<dyn ContentHandler>>,
    cursors: &'a HashMap<ContentId, Selections>,
}

impl<'a> ContentQuery for AppQuery<'a> {
    fn lines(&self, cid: ContentId, range: RowRange) -> Vec<String> {
        let Some(buf) = self.contents.get(&cid).and_then(|c| c.as_buffer()) else { return Vec::new() };
        let total = buf.len_lines();
        let start = range.start.min(total);
        let end = range.end.min(total).max(start);
        (start..end).map(|i| buf.line(i).trim_end_matches('\n').to_string()).collect()
    }
    fn status_bar(&self, cid: ContentId) -> StatusBarData {
        let Some(c) = self.contents.get(&cid) else {
            return StatusBarData { file_name: None, modified: false, message: StatusMessage::None };
        };
        match c.as_status_bar() {
            Some(sb) => sb.status_bar_data(self.contents as &dyn ContentLookup),
            None => StatusBarData { file_name: None, modified: false, message: StatusMessage::None },
        }
    }
    fn cursor(&self, cid: ContentId) -> CursorPos {
        self.cursors.get(&cid).map(|c| c.primary().head()).unwrap_or_else(CursorPos::origin)
    }
    fn line_count(&self, cid: ContentId) -> usize {
        self.contents.get(&cid).and_then(|c| c.as_buffer()).map(|b| b.len_lines()).unwrap_or(0)
    }
}
```

`app/mod.rs` tests 中 `run_supports_backspace_and_arrows` 的断言（`src/app/mod.rs:328`）：

```rust
        let cursor = app.cursors.get(&editor_cid()).expect("editor cursor exists").primary().head();
        assert_eq!(cursor.col, 0);
```

- [ ] **Step 5: 运行测试验证全绿**

Run: `cargo test`
Expected: 全绿。若有 `Cursors` 残留引用，编译错误指向它，按提示修复（应已全覆盖）。

- [ ] **Step 6: Commit**

```bash
git add src/core/content.rs src/core/buffer.rs src/app/executor.rs src/app/mod.rs
git commit -m "refactor: Cursors→Selections 类型替换 + 编辑原语升级（守恒 collapsed）"
```

---

## Task 3: app/view.rs——View 编辑会话实体

**Files:**
- Create: `src/app/view.rs`
- Modify: `src/app/mod.rs:5-8`（加 `mod view;`）
- Test: `src/app/view.rs`（内联 tests）

- [ ] **Step 1: 写 View 类型 + 失败测试**

创建 `src/app/view.rs`：

```rust
//! 视图实例的编辑会话：绑定一个 content + 持选区。
//! 按 SpaceId 索引（App.views），同 content 可被多个 View 绑定（多视图铺路）。

use crate::protocol::ids::ContentId;
use crate::protocol::selection::{CursorPos, Selection, Selections};

pub struct View {
    content: ContentId,
    selections: Selections,
}

impl View {
    pub fn new(content: ContentId) -> Self {
        Self {
            content,
            selections: Selections::single(Selection::collapsed(CursorPos::origin())),
        }
    }
    pub fn content(&self) -> ContentId { self.content }
    pub fn selections(&self) -> &Selections { &self.selections }
    pub fn selections_mut(&mut self) -> &mut Selections { &mut self.selections }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_view_has_collapsed_origin_selection() {
        let v = View::new(ContentId(0));
        assert_eq!(v.content(), ContentId(0));
        let s = v.selections();
        assert_eq!(s.all().count(), 1);
        assert_eq!(s.primary().head(), CursorPos::origin());
        assert_eq!(s.primary().anchor, s.primary().head());
    }

    #[test]
    fn selections_mut_allows_edit() {
        let mut v = View::new(ContentId(1));
        v.selections_mut().primary_mut().head = CursorPos { char_index: 5, row: 0, col: 5 };
        assert_eq!(v.selections().primary().head().char_index, 5);
    }
}
```

- [ ] **Step 2: 注册模块**

`src/app/mod.rs` 的 mod 声明段（`src/app/mod.rs:5-8`）加 `mod view;`：

```rust
mod content;
mod dispatcher;
mod executor;
mod frontend;
mod view;
```

- [ ] **Step 3: 运行测试验证通过**

Run: `cargo test app::view`
Expected: 2 tests passed。

- [ ] **Step 4: 全量编译验证**

Run: `cargo build`
Expected: 无错误。

- [ ] **Step 5: Commit**

```bash
git add src/app/view.rs src/app/mod.rs
git commit -m "feat(app): View 编辑会话实体（content + selections，按 SpaceId 索引铺路）"
```

---

## Task 4: App.cursors→views 字段迁移

把 `App.cursors: HashMap<ContentId, Selections>` 改为 `App.views: HashMap<SpaceId, View>`，按 SpaceId 索引。`ContentQuery::cursor(cid)` 签名**不变**（Task 5 才改）——`AppQuery::cursor(cid)` 临时按 content 反查 view（过渡，Task 5 替换为 `selections(sid)`）。

**Files:**
- Modify: `src/app/mod.rs`（struct + new + focused_content_id + execute_operation + AppQuery + tests）

- [ ] **Step 1: 改 App struct 字段**

`src/app/mod.rs:36-47` 的 `App` struct：

```rust
pub struct App {
    contents: HashMap<ContentId, Box<dyn ContentHandler>>,
    scene: Scene,
    views: HashMap<SpaceId, View>,
    focused: SpaceId,
    dispatcher: Dispatcher,
    should_quit: bool,
    frontend: FrontendImpl,
    bg_tx: mpsc::Sender<BgResult>,
    bg_rx: mpsc::Receiver<BgResult>,
    pending_save: Option<ContentId>,
}
```

删 `cursors` 字段，加 `views: HashMap<SpaceId, View>`。

import 段（`src/app/mod.rs:13-29`）调整：
- `use std::collections::HashMap;` 保留
- `use crate::protocol::ids::{ContentId, SpaceId};`（已有 SpaceId）
- 加 `use crate::app::view::View;`（或 `use self::view::View;`）

在 `use crate::app::dispatcher::...` 后加：

```rust
use crate::app::view::View;
```

- [ ] **Step 2: 改 App::new 建 views**

`src/app/mod.rs:66-84` 段（建 cursors → 建 views）：

```rust
        let (scene, editor_space) =
            build_editor_scene(width as i32, height as i32, editor_content, status_content);
        let views = build_views(&scene);
        let dispatcher = Dispatcher::new(default_global_keymap());
        let (bg_tx, bg_rx) = mpsc::channel::<BgResult>(8);
        Ok(Self {
            contents,
            views,
            scene,
            focused: editor_space,
            dispatcher,
            should_quit: false,
            frontend,
            bg_tx,
            bg_rx,
            pending_save: None,
        })
```

在 `App` impl 内（`App::new` 前）加 helper：

```rust
/// 遍历 scene 所有 Host space，为每个建 View（绑定其 content）。
fn build_views(scene: &Scene) -> HashMap<SpaceId, View> {
    let mut views = HashMap::new();
    collect_host_spaces(scene, scene.root, &mut views);
    views
}

fn collect_host_spaces(scene: &Scene, sid: SpaceId, out: &mut HashMap<SpaceId, View>) {
    let node = scene.node(sid);
    match &node.space.kind {
        SpaceKind::Host { content } => {
            out.insert(sid, View::new(*content));
        }
        SpaceKind::Container { children, .. } => {
            for c in children { collect_host_spaces(scene, *c, out); }
        }
    }
}
```

注：`build_views`/`collect_host_spaces` 是 `app/mod.rs` 内的自由函数（非 impl 方法），放 `App` impl 块外。`SpaceKind` 已 import（`src/app/mod.rs:25`）。

- [ ] **Step 3: 改 focused_content_id**

`src/app/mod.rs:209-214` 的 `focused_content_id`：

```rust
    fn focused_content_id(&self) -> ContentId {
        self.views.get(&self.focused).map(|v| v.content()).unwrap_or(ContentId(0))
    }
```

不再从 scene 反查，直接从 View 取。

- [ ] **Step 4: 改 execute_operation**

`src/app/mod.rs:135-158` 的 `execute_operation`，编辑分支（`_ => { ... }`）：

```rust
            _ => {
                let cid = self.focused_content_id();
                let content: &mut dyn ContentHandler = self
                    .contents
                    .get_mut(&cid)
                    .map(|b| b.as_mut())
                    .expect("focused content exists");
                let view = self
                    .views
                    .get_mut(&self.focused)
                    .expect("focused view exists");
                executor::execute(op, content, view.selections_mut());
            }
```

- [ ] **Step 5: 改 AppQuery（过渡：cursor(cid) 按 content 反查 view）**

`src/app/mod.rs:224-252` 的 `AppQuery`：

```rust
/// 借 App 数据字段的查询适配器：render 时用它做 `&dyn ContentQuery`，
/// 与 `&mut self.frontend` 不冲突（字段级 split borrow）。
struct AppQuery<'a> {
    contents: &'a HashMap<ContentId, Box<dyn ContentHandler>>,
    views: &'a HashMap<SpaceId, View>,
}

impl<'a> ContentQuery for AppQuery<'a> {
    fn lines(&self, cid: ContentId, range: RowRange) -> Vec<String> {
        let Some(buf) = self.contents.get(&cid).and_then(|c| c.as_buffer()) else { return Vec::new() };
        let total = buf.len_lines();
        let start = range.start.min(total);
        let end = range.end.min(total).max(start);
        (start..end).map(|i| buf.line(i).trim_end_matches('\n').to_string()).collect()
    }
    fn status_bar(&self, cid: ContentId) -> StatusBarData {
        let Some(c) = self.contents.get(&cid) else {
            return StatusBarData { file_name: None, modified: false, message: StatusMessage::None };
        };
        match c.as_status_bar() {
            Some(sb) => sb.status_bar_data(self.contents as &dyn ContentLookup),
            None => StatusBarData { file_name: None, modified: false, message: StatusMessage::None },
        }
    }
    // 过渡：cursor(cid) 按 content 反查 view。Task 5 改 selections(sid) 后删除。
    fn cursor(&self, cid: ContentId) -> CursorPos {
        self.views
            .values()
            .find(|v| v.content() == cid)
            .map(|v| v.selections().primary().head())
            .unwrap_or_else(CursorPos::origin)
    }
    fn line_count(&self, cid: ContentId) -> usize {
        self.contents.get(&cid).and_then(|c| c.as_buffer()).map(|b| b.len_lines()).unwrap_or(0)
    }
}
```

`App::render` 中构造 AppQuery（`src/app/mod.rs:216-219`）：

```rust
    fn render(&mut self) -> io::Result<()> {
        let query = AppQuery { contents: &self.contents, views: &self.views };
        self.frontend.render(&self.scene, &query as &dyn ContentQuery, self.focused)
    }
```

`App impl ContentQuery`（`src/app/mod.rs:254-267`）委托更新：

```rust
impl ContentQuery for App {
    fn lines(&self, cid: ContentId, range: RowRange) -> Vec<String> {
        AppQuery { contents: &self.contents, views: &self.views }.lines(cid, range)
    }
    fn status_bar(&self, cid: ContentId) -> StatusBarData {
        AppQuery { contents: &self.contents, views: &self.views }.status_bar(cid)
    }
    fn cursor(&self, cid: ContentId) -> CursorPos {
        AppQuery { contents: &self.contents, views: &self.views }.cursor(cid)
    }
    fn line_count(&self, cid: ContentId) -> usize {
        AppQuery { contents: &self.contents, views: &self.views }.line_count(cid)
    }
}
```

- [ ] **Step 6: 改 app tests（cursors→views 断言）**

`src/app/mod.rs` tests 中 `run_supports_backspace_and_arrows`（`src/app/mod.rs:328`）：

```rust
        let cursor = app.views.get(&app.focused).expect("view exists").selections().primary().head();
        assert_eq!(cursor.col, 0);
```

`content_query_lines_and_cursor`（`src/app/mod.rs:286-296`）保持不变（`ContentQuery::cursor(&app, editor_cid())` 仍走 AppQuery.cursor 按 content 反查 editor view，返回 origin）。

- [ ] **Step 7: 运行测试验证全绿**

Run: `cargo test`
Expected: 全绿。`ContentQuery::cursor(cid)` 经 AppQuery 按 content 反查 view 返回，行为不变。

- [ ] **Step 8: Commit**

```bash
git add src/app/mod.rs
git commit -m "refactor(app): cursors→views 字段迁移（按 SpaceId 索引 View 实体）"
```

---

## Task 5: ContentQuery::cursor(cid)→selections(sid)

把 `ContentQuery::cursor(cid)->CursorPos` 改为 `selections(sid: SpaceId)->Selections`，删除 Task 4 的过渡 `cursor(cid)` 反查。

**Files:**
- Modify: `src/protocol/content_query.rs`（trait 签名 + tests）
- Modify: `src/app/mod.rs`（AppQuery + App impl）
- Modify: `src/tui/scene_renderer.rs`（调用 + StubQuery + tests）

- [ ] **Step 1: 改 ContentQuery trait 签名**

`src/protocol/content_query.rs` 完整新文件：

```rust
//! 前端 pull 后端内容的契约。同进程同步调用，返回 owned 数据。

use crate::protocol::ids::{ContentId, SpaceId};
use crate::protocol::selection::Selections;
use crate::protocol::status::StatusMessage;

/// 行范围 [start, end)，前端按可见行拉取。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RowRange { pub start: usize, pub end: usize }

/// 状态栏显示数据（owned）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatusBarData {
    pub file_name: Option<String>,
    pub modified: bool,
    pub message: StatusMessage,
}

/// 前端查询后端内容的契约。同进程同步调用。
/// 返回 Vec 长度 = min(range.len(), line_count - start)；超出末尾的行不返回。
pub trait ContentQuery {
    fn lines(&self, cid: ContentId, range: RowRange) -> Vec<String>;
    fn status_bar(&self, cid: ContentId) -> StatusBarData;
    fn selections(&self, sid: SpaceId) -> Selections;
    fn line_count(&self, cid: ContentId) -> usize;
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn row_range_constructs() {
        let r = RowRange { start: 1, end: 5 };
        assert_eq!(r.start, 1);
        assert_eq!(r.end, 5);
    }
    #[test]
    fn status_bar_data_eq() {
        let a = StatusBarData { file_name: None, modified: false, message: StatusMessage::None };
        assert_eq!(a, a.clone());
    }
}
```

- [ ] **Step 2: 改 AppQuery + App impl（删除过渡 cursor，加 selections）**

`src/app/mod.rs` `AppQuery`：

```rust
struct AppQuery<'a> {
    contents: &'a HashMap<ContentId, Box<dyn ContentHandler>>,
    views: &'a HashMap<SpaceId, View>,
}

impl<'a> ContentQuery for AppQuery<'a> {
    fn lines(&self, cid: ContentId, range: RowRange) -> Vec<String> {
        let Some(buf) = self.contents.get(&cid).and_then(|c| c.as_buffer()) else { return Vec::new() };
        let total = buf.len_lines();
        let start = range.start.min(total);
        let end = range.end.min(total).max(start);
        (start..end).map(|i| buf.line(i).trim_end_matches('\n').to_string()).collect()
    }
    fn status_bar(&self, cid: ContentId) -> StatusBarData {
        let Some(c) = self.contents.get(&cid) else {
            return StatusBarData { file_name: None, modified: false, message: StatusMessage::None };
        };
        match c.as_status_bar() {
            Some(sb) => sb.status_bar_data(self.contents as &dyn ContentLookup),
            None => StatusBarData { file_name: None, modified: false, message: StatusMessage::None },
        }
    }
    fn selections(&self, sid: SpaceId) -> Selections {
        self.views
            .get(&sid)
            .map(|v| v.selections().clone())
            .unwrap_or_else(|| Selections::single(Selection::collapsed(CursorPos::origin())))
    }
    fn line_count(&self, cid: ContentId) -> usize {
        self.contents.get(&cid).and_then(|c| c.as_buffer()).map(|b| b.len_lines()).unwrap_or(0)
    }
}
```

`App impl ContentQuery`：

```rust
impl ContentQuery for App {
    fn lines(&self, cid: ContentId, range: RowRange) -> Vec<String> {
        AppQuery { contents: &self.contents, views: &self.views }.lines(cid, range)
    }
    fn status_bar(&self, cid: ContentId) -> StatusBarData {
        AppQuery { contents: &self.contents, views: &self.views }.status_bar(cid)
    }
    fn selections(&self, sid: SpaceId) -> Selections {
        AppQuery { contents: &self.contents, views: &self.views }.selections(sid)
    }
    fn line_count(&self, cid: ContentId) -> usize {
        AppQuery { contents: &self.contents, views: &self.views }.line_count(cid)
    }
}
```

`src/app/mod.rs:29` 的 `use crate::protocol::cursor::CursorPos;` 保留（AppQuery selections 默认值用 `CursorPos::origin()`，CursorPos 仍在 cursor.rs，Task 6 迁移）。

`src/app/mod.rs` tests 中 `content_query_lines_and_cursor`（`src/app/mod.rs:286-296`）改用 `selections`：

```rust
    #[test]
    fn content_query_lines_and_selections() {
        let mut app = make_app(vec![], None);
        let buf = app.contents.get_mut(&editor_cid()).and_then(|c| c.buffer_mut()).unwrap();
        buf.insert_char(0, 'h');
        buf.insert_char(1, 'i');
        let lines = ContentQuery::lines(&app, editor_cid(), RowRange { start: 0, end: 5 });
        assert_eq!(lines, vec!["hi".to_string()]);
        assert_eq!(ContentQuery::line_count(&app, editor_cid()), 1);
        let sels = ContentQuery::selections(&app, app.focused);
        assert_eq!(sels.primary().head(), CursorPos::origin());
    }
```

`run_supports_backspace_and_arrows` 断言（`src/app/mod.rs` tests）保持 `app.views.get(&app.focused)...selections().primary().head()`（Task 4 已改）。

- [ ] **Step 3: 改 scene_renderer 调用**

`src/tui/scene_renderer.rs` `render` 方法（`src/tui/scene_renderer.rs:36-60`）——两处 `query.cursor(cid)` 改 `query.selections(focused).primary().head()`：

```rust
    pub fn render(
        &mut self,
        scene: &Scene,
        query: &dyn ContentQuery,
        focused: SpaceId,
        canvas: &mut dyn Canvas,
    ) -> io::Result<()> {
        let resolved: ResolvedScene = self.engine.layout(scene);
        canvas.hide_cursor()?;
        // 焦点 viewport 跟随
        let focused_cid = focused_content_id(scene, focused);
        let focused_head = query.selections(focused).primary().head();
        if let Some(cid) = focused_cid {
            if let Some(item) = resolved.items.iter().find(|i| i.content_id == cid) {
                let vp = self.viewports.entry(focused).or_insert_with(Viewport::origin);
                vp.ensure_cursor_visible(focused_head.row, item.rect.height as usize);
            }
        }
        // 逐 Host item paint
        for item in &resolved.items {
            paint_item(item, scene, query, &self.viewports, canvas)?;
        }
        // 焦点光标定位
        if let Some(cid) = focused_cid {
            if let Some(item) = resolved.items.iter().find(|i| i.content_id == cid) {
                let vp = self.viewports.get(&focused).copied().unwrap_or_else(Viewport::origin);
                let screen_row = focused_head.row.saturating_sub(vp.top_row) + item.rect.y as usize;
                let screen_col = focused_head.col.saturating_sub(vp.left_col) + item.rect.x as usize;
                canvas.move_cursor(screen_row, screen_col)?;
                canvas.show_cursor()?;
            }
        }
        canvas.flush()
    }
```

- [ ] **Step 4: 改 scene_renderer tests StubQuery**

`src/tui/scene_renderer.rs:146-212` tests 段，StubQuery 改 `cursor`→`selections`：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::content_query::{ContentQuery, RowRange, StatusBarData};
    use crate::protocol::cursor::CursorPos;
    use crate::protocol::ids::{ContentId, SpaceId};
    use crate::protocol::scene::build_editor_scene;
    use crate::protocol::selection::{Selection, Selections};
    use crate::protocol::status::StatusMessage;
    use crate::terminal::output::Output;

    struct StubQuery {
        editor_cid: ContentId,
        lines: Vec<String>,
        selections: Selections,
    }
    impl ContentQuery for StubQuery {
        fn lines(&self, cid: ContentId, range: RowRange) -> Vec<String> {
            assert_eq!(cid, self.editor_cid, "only editor content has lines");
            self.lines.iter().skip(range.start).take(range.end.saturating_sub(range.start)).cloned().collect()
        }
        fn status_bar(&self, _cid: ContentId) -> StatusBarData {
            StatusBarData { file_name: Some("f.txt".to_string()), modified: false, message: StatusMessage::None }
        }
        fn selections(&self, _sid: SpaceId) -> Selections {
            self.selections.clone()
        }
        fn line_count(&self, cid: ContentId) -> usize {
            if cid == self.editor_cid { self.lines.len() } else { 0 }
        }
    }

    #[test]
    fn renders_editor_lines_and_status() {
        let (scene, ed) = build_editor_scene(40, 5, ContentId(0), ContentId(1));
        let query = StubQuery {
            editor_cid: ContentId(0),
            lines: vec!["hello".to_string(), "world".to_string()],
            selections: Selections::single(Selection::collapsed(CursorPos::origin())),
        };
        let mut r = SceneRenderer::new();
        let mut out = Output::new(Vec::new());
        r.render(&scene, &query, ed, &mut out as &mut dyn Canvas).unwrap();
        let s = String::from_utf8(out.into_inner()).unwrap();
        assert!(s.contains("hello"), "{s}");
        assert!(s.contains("f.txt"), "{s}");
    }

    #[test]
    fn viewport_follows_cursor_below() {
        let (scene, ed) = build_editor_scene(40, 5, ContentId(0), ContentId(1));
        let lines: Vec<String> = (0..30).map(|i| format!("line{i}")).collect();
        let query = StubQuery {
            editor_cid: ContentId(0),
            lines,
            selections: Selections::single(Selection::collapsed(CursorPos { char_index: 0, row: 25, col: 0 })),
        };
        let mut r = SceneRenderer::new();
        let mut out = Output::new(Vec::new());
        r.render(&scene, &query, ed, &mut out as &mut dyn Canvas).unwrap();
        let s = String::from_utf8(out.into_inner()).unwrap();
        assert!(s.contains("line25"), "{s}");
        assert!(!s.contains("line0"), "{s}");
    }
}
```

注：`_sid: SpaceId` 参数未在断言中用（单 editor space），但签名要求。`SpaceId` import 已加。

- [ ] **Step 5: 运行测试验证全绿**

Run: `cargo test`
Expected: 全绿。`query.cursor` 调用已全部改为 `query.selections`。

- [ ] **Step 6: Commit**

```bash
git add src/protocol/content_query.rs src/app/mod.rs src/tui/scene_renderer.rs
git commit -m "refactor: ContentQuery::cursor(cid)→selections(sid) 返回完整 Selections"
```

---

## Task 6: CursorPos 迁入 selection.rs + 删 cursor.rs

把 `CursorPos` 从 `protocol/cursor.rs` 移入 `protocol/selection.rs`，删 `cursor.rs`，所有 `crate::protocol::cursor::` import 改 `crate::protocol::selection::`。

**Files:**
- Modify: `src/protocol/selection.rs`（加 CursorPos）
- Delete: `src/protocol/cursor.rs`
- Modify: `src/protocol/mod.rs`（删 `pub mod cursor;`）
- Modify: 6 处 import（见下）

- [ ] **Step 1: 把 CursorPos 移入 selection.rs**

`src/protocol/selection.rs` 顶部，把 `use crate::protocol::cursor::CursorPos;` 替换为 CursorPos 定义：

```rust
//! Selection 数据模型：cursor 是 selection 的退化形态（collapsed，anchor==head）。
//! Helix 风集合：ranges + primary_index。v0.2 恒 collapsed、ranges.len()==1。

/// 光标位置值类型。char_index 权威，row/col 派生缓存（由 core::buffer 维护）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CursorPos {
    pub char_index: usize,
    pub row: usize,
    pub col: usize,
}

impl CursorPos {
    pub const fn origin() -> Self { Self { char_index: 0, row: 0, col: 0 } }
}

// Selection / Selections 定义不变（已在此文件，原 use cursor::CursorPos 删除）
```

保留 `Selection`/`Selections` 及其 tests（原 `CursorPos` tests 也要迁入——见下）。

在 `selection.rs` 的 `#[cfg(test)] mod tests` 顶部加（从 cursor.rs 迁来的 CursorPos 测试）：

```rust
    #[test]
    fn origin_is_zero() {
        let c = CursorPos::origin();
        assert_eq!((c.char_index, c.row, c.col), (0, 0, 0));
    }

    #[test]
    fn copy_and_eq() {
        let a = CursorPos { char_index: 3, row: 1, col: 2 };
        let b = a;
        assert_eq!(a, b);
    }
```

- [ ] **Step 2: 删 cursor.rs + mod 声明**

删 `src/protocol/cursor.rs`。

`src/protocol/mod.rs` 删 `pub mod cursor;`：

```rust
pub mod content_query;
pub mod selection;
pub mod frontend_event;
pub mod geometry;
pub mod ids;
pub mod key_event;
pub mod scene;
pub mod space;
pub mod status;
pub mod viewport;
```

- [ ] **Step 3: 改 6 处 import**

把所有 `crate::protocol::cursor::` 改 `crate::protocol::selection::`：

1. `src/core/buffer.rs:9`：`use crate::protocol::cursor::CursorPos;` → `use crate::protocol::selection::{CursorPos, Selection, Selections};`（Task 2 已是 selection，但若还残留 cursor::CursorPos 则合并）。实际 Task 2 已改为 `use crate::protocol::selection::{CursorPos, Selection, Selections};`——本步确认无 `cursor::` 残留。
2. `src/core/content.rs:5`：Task 2 已改为 `use crate::protocol::selection::Selections;`——确认无 `cursor::` 残留。
3. `src/protocol/content_query.rs:3`：Task 5 已改为 `use crate::protocol::selection::Selections;`——确认无 `cursor::` 残留。
4. `src/app/mod.rs:29`：`use crate::protocol::cursor::CursorPos;` → `use crate::protocol::selection::CursorPos;`（AppQuery selections 默认值用）。若 Task 2/4/5 已 import `Selection, Selections`，合并为一行 `use crate::protocol::selection::{CursorPos, Selection, Selections};`。
5. `src/app/executor.rs:44`（tests）：`use crate::protocol::cursor::CursorPos;` → `use crate::protocol::selection::CursorPos;`。executor.rs 顶部 `use crate::protocol::selection::Selections;` 已有（Task 2），tests 内 CursorPos import 单独改。
6. `src/tui/scene_renderer.rs:150`（tests）：`use crate::protocol::cursor::CursorPos;` → `use crate::protocol::selection::CursorPos;`（或合并到 Task 5 已加的 `use crate::protocol::selection::{Selection, Selections};` 为 `{CursorPos, Selection, Selections}`）。

- [ ] **Step 4: 运行测试验证全绿**

Run: `cargo test`
Expected: 全绿。`protocol::cursor` 引用全部消失。

验证无残留：`grep -r "protocol::cursor" src/` 应无输出（PowerShell: `Select-String -Path "src\**\*.rs" "protocol::cursor"`）。

- [ ] **Step 5: Commit**

```bash
git add src/protocol/selection.rs src/protocol/mod.rs src/core/buffer.rs src/core/content.rs src/protocol/content_query.rs src/app/mod.rs src/app/executor.rs src/tui/scene_renderer.rs
git rm src/protocol/cursor.rs
git commit -m "refactor(protocol): CursorPos 迁入 selection.rs，删 cursor.rs"
```

---

## Task 7: docs/design/current-architecture.md 更新

**Files:**
- Modify: `docs/design/current-architecture.md`

- [ ] **Step 1: 更新架构文档反映 selection 模型 + View 实体**

`docs/design/current-architecture.md` 改动点：

1. `Cursors` 描述（`docs/design/current-architecture.md:67,85,129,133,154,226`）改为 `Selections`：
   - `content.rs    ContentHandler trait / ContentLookup / Cursors` → `content.rs    ContentHandler trait / ContentLookup`（Selections 已移 protocol/selection.rs）
   - 第 85 行 `Cursors` 段：改为 `Selections`（protocol/selection.rs），描述 `ranges + primary_index`（Helix 风），v0.2 恒 collapsed；cursor 归 `App.views: HashMap<SpaceId, View>`，View 持 selections。

2. 第 129 行 `App` 持字段：`cursors: HashMap<ContentId, Cursors>` → `views: HashMap<SpaceId, View>`（View 在 app/view.rs，持 content + selections）。

3. 第 133 行 executor：`操作 Cursors` → `操作 Selections`；签名 `execute(op, content, &mut Selections)`。

4. 第 154 行数据流：`委派 Buffer 操作 Cursors` → `委派 Buffer 操作 Selections（move_selection_*/insert_at_selections/delete_at_selections，守恒 collapsed）`。

5. ContentQuery 描述：`cursor(cid)->CursorPos` → `selections(sid)->Selections`（按 SpaceId 查 View.selections）。

6. 第 226 行 core 描述：`content（Cursors 迭代）` → `content（ContentHandler/ContentLookup）`；加 `protocol/selection（Selection/Selections，Helix 风集合）`。

7. 加 View 实体描述：`app/view.rs    View{content, selections} 编辑会话实体，按 SpaceId 索引`。

8. 决策记录段（若有）加：cursor 是 selection 退化形态（Helix 风 ranges+primary_index）；归属 View 实体（后端，编辑态须 executor 可达）；v0.2 恒 collapsed，真选区编辑/多视图留 v0.3。

- [ ] **Step 2: 验证文档无残留 Cursors/cursor(cid) 旧描述**

Run: `Select-String -Path "docs\design\current-architecture.md" "cursors|Cursors|cursor\(cid\)"`
Expected: 无输出（或仅 v0.3 follow-up 上下文提及）。

- [ ] **Step 3: Commit**

```bash
git add docs/design/current-architecture.md
git commit -m "docs: 更新 current-architecture 反映 selection 模型 + View 实体归属"
```

---

## Self-Review

**1. Spec coverage:**
- §4.1 Selection/Selections（Helix 风 ranges+primary_index）→ Task 1 ✓
- §4.1 v0.2 不变量（collapsed、len==1、primary_index==0）→ Task 1 测试 + Task 2 守恒断言 ✓
- §4.2 View 实体 → Task 3 ✓
- §4.3 App.views 替代 cursors + App::new 遍历 Host + focused_content_id 收进 View → Task 4 ✓
- §4.4 ContentQuery::selections(sid)->Selections owned → Task 5 ✓
- §4.5 AppQuery { contents, views } → Task 4/5 ✓
- §4.6 编辑原语升级（move_selection_* 守恒、底层 move_cursor_* pub(crate)、recompute_selection）→ Task 2 ✓
- §4.7 executor 签名 + retain_primary → Task 2 ✓
- §5 数据流 → Task 4/5（scene_renderer 调用）✓
- §7 测试策略（collapsed 守恒、AppQuery::selections、集成测试改 views）→ Task 1-5 ✓
- §8 迁移清单（新建/删除/改写/不变）→ Task 1-6 ✓
- §3 protocol/cursor.rs 删除、mod 声明 → Task 6 ✓
- current-architecture.md 更新 → Task 7 ✓

**2. Placeholder scan:** 无 TBD/TODO/"实现细节后补"。Task 4 Step 2 的 `build_views`/`collect_host_spaces` 给了完整代码。Task 6 Step 3 的 import 改动给了具体文件行。✓

**3. Type consistency:**
- `Selections::primary() -> &Selection`、`primary_mut() -> &mut Selection`、`all()`/`all_mut()`、`retain_primary()`、`from_parts()`（test）——Task 1 定义，Task 2/4/5 调用一致 ✓
- `Selection::collapsed`/`is_empty`/`head()` ——Task 1 定义，Task 2/5 调用一致 ✓
- `View::new(content)`/`content()`/`selections()`/`selections_mut()` ——Task 3 定义，Task 4 调用一致 ✓
- `move_selection_*`/`set_selection`/`insert_at_selections`/`delete_at_selections`/`recompute_selection` ——Task 2 定义，Task 2 executor 调用一致 ✓
- `ContentQuery::selections(sid: SpaceId) -> Selections` ——Task 5 定义，Task 5 AppQuery/App impl/StubQuery 一致 ✓
- `execute(op, content, selections: &mut Selections)` ——Task 2 定义，Task 4 execute_operation 调用一致 ✓
- Task 4 过渡 `AppQuery::cursor(cid)` 按 content 反查 view ——Task 5 删除替换为 `selections(sid)` ✓
- `App.cursors`（Task 2 类型替换）→ `App.views`（Task 4 字段迁移）→ `AppQuery.selections`（Task 5）三阶段一致 ✓
