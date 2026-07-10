# 架构重构实现计划：Document/View 分离 + 各司其职

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 按 spec `docs/superpowers/specs/2026-06-30-architecture-refactor-design.md` 一次性重写架构，实现 Document/View 分离与各司其职。

**Architecture:** 自底向上重建：protocol（中立契约，含 `CursorPos`/`Viewport`/`EditView`/ids/status）→ core（Buffer + 纯函数 `handle_key`）→ layout（Space 持 viewport/cursor，引擎纯几何）→ tui（Content 渲染策略 + TuiFrontend）→ app（事件循环 + Frontend trait）→ main 接线。最后删除旧代码。

**Tech Stack:** Rust 2021, ropey 1, crossterm 0.28, tokio 1, taffy 0.5, tempfile 3(dev)

---

## ⚠️ 重写特性说明

本 plan 是**一次性重写**（spec 明定），非分阶段迁移。旧代码高度互相依赖（`ContentStore`/`Editor`/`TuiRenderer`/`App` 链条），**Task 1 起即破坏旧代码编译**。任务按依赖顺序进行：

- **Task 1–4**：新建 protocol/core 文件，单元测试可在该文件依赖就绪后单独跑（`cargo test --lib protocol::` / `core::`），但**整体 `cargo build` 在 Task 1 后即失败**，直到 Task 7 恢复。
- **Task 5–6**：重写 layout/tui，整体仍不可编译。
- **Task 7**：重写 app + main + 删旧，**恢复全量编译**。
- **Task 8**：`cargo build && cargo test` 全绿 + 清理。

implementer 每个任务 commit，但**只有 Task 7 之后才要求 `cargo build` 通过**。Task 1–6 的 commit 允许编译失败（commit message 标注 `[wip]`）。Task 1–4 的单元测试可用 `cargo test --lib <module>` 单独验证（前提是该模块依赖的更底层模块已就绪）。

---

## 文件结构（最终态）

| 文件 | 职责 | 动作 |
|---|---|---|
| `src/protocol/ids.rs` | `ContentId`/`SpaceId`/`SceneId` | 从 layout/ids.rs 移入 |
| `src/protocol/status.rs` | `StatusMessage` | 从 core/status.rs 移入 |
| `src/protocol/cursor.rs` | `CursorPos` 值类型 | 新建 |
| `src/protocol/viewport.rs` | `Viewport`（不含尺寸） | 新建 |
| `src/protocol/edit_view.rs` | `EditView`/`ContentLookup`/`SpaceState`/`RenderCtx`/`WrapMode` | 新建 |
| `src/protocol/frontend_event.rs` | `FrontendEvent`/`ResizeEvent` | 沿用 |
| `src/protocol/key_event.rs` | `KeyEvent`/`translate_key` | 沿用 |
| `src/protocol/core_patch.rs` | — | 删除 |
| `src/core/buffer.rs` | `Buffer` | 沿用（微调 use） |
| `src/core/status.rs` | `Status`（用 protocol::StatusMessage） | 改 |
| `src/core/edit.rs` | `handle_key`/`open_path`/移动纯函数 | 新建 |
| `src/core/editor.rs` | — | 删除 |
| `src/core/cursor.rs` | — | 删除（逻辑移入 edit.rs） |
| `src/layout/space.rs` | `Space` 持 viewport/cursor/wrap_mode | 改 |
| `src/layout/scene.rs` | `Scene`/`Rect`/`build_editor_scene` | 改（删 focused，返回 EditorScene） |
| `src/layout/resolved.rs` | `RenderItem`(带 state)/`ResolvedScene` | 改（删 Renderer/render） |
| `src/layout/taffy_engine.rs` | `layout(&scene)` 透传 state | 改 |
| `src/layout/content.rs` | — | 删除 |
| `src/layout/ids.rs` | — | 删除（移入 protocol） |
| `src/terminal/output.rs` | `Output` + `Canvas` trait | 加 Canvas |
| `src/terminal/input.rs` | `InputSource` | 沿用 |
| `src/terminal/lifecycle.rs` | `TerminalGuard` | 沿用 |
| `src/tui/content.rs` | `Content` trait + EditorContent/StatusBarContent | 新建 |
| `src/tui/tui_frontend.rs` | `TuiFrontend: impl Frontend` | 新建 |
| `src/tui/renderer.rs` | — | 删除 |
| `src/tui/viewport.rs` | — | 删除 |
| `src/app.rs` | `App`/`Document`/`Frontend` trait/`ContentLookup` impl | 改 |
| `src/main.rs` | 接线 | 改 |

**spec 细化（plan 内敲定，spec 未明确处）：**
1. `StatusMessage` 与 ids（`ContentId` 等）移入 protocol，使 `EditView`/`ContentLookup`（protocol）零依赖地引用它们。
2. `RenderItem` 保留 `content_id`，删 `space`/`parent` 预留字段（YAGNI）；用 `content_id` 匹配焦点 item（v0.2 无歧义）。
3. `RenderCtx` 删 `focused: SpaceId` 字段（未用）；只留 `contents`/`focused_state`/`focused_content`。`Frontend::render` 由 App 直接传入 `focused_state` + `focused_content`。
4. 引入 `Canvas` trait（`Output<W>` impl），使 `Content::render(canvas: &mut dyn Canvas)` object-safe（`Box<dyn Content>` 可用）。
5. 光标定位/`show_cursor` 收归 `TuiFrontend::render` 末尾（用焦点 `state.cursor` + 焦点 item rect 计算），`Content::render` 只画文本、不碰光标。
6. `build_editor_scene` 返回 `EditorScene { scene, editor_space, status_space }`，供 App 拿焦点 space id。

---

## Task 1: protocol 基础（ids / status / cursor / viewport）

**Files:**
- Create: `src/protocol/ids.rs`, `src/protocol/status.rs`, `src/protocol/cursor.rs`, `src/protocol/viewport.rs`
- Modify: `src/protocol/mod.rs`
- Delete: `src/protocol/core_patch.rs`

> 本任务后 `cargo build` 失败（旧 `editor.rs`/`layout/content.rs` 等仍引用旧路径），属预期。

- [ ] **Step 1: 写 `src/protocol/ids.rs`**

```rust
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct SceneId(pub u64);

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct SpaceId(pub u64);

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct ContentId(pub u64);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_copy_eq_hash() {
        let a = SpaceId(1);
        let b = a;
        assert_eq!(a, b);
        let mut set = std::collections::HashSet::new();
        set.insert(ContentId(2));
        assert!(set.contains(&ContentId(2)));
    }
}
```

- [ ] **Step 2: 写 `src/protocol/status.rs`**

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StatusMessage {
    None,
    Saved,
    SaveFailed,
    NewFile,
    OpenFailed,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_message_eq() {
        assert_eq!(StatusMessage::Saved, StatusMessage::Saved);
        assert_ne!(StatusMessage::Saved, StatusMessage::None);
    }
}
```

- [ ] **Step 3: 写 `src/protocol/cursor.rs`**（仅值类型，换算逻辑在 core/edit.rs）

```rust
/// 光标位置值类型。char_index 为权威字段，row/col 为派生缓存（由 core::edit 维护）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CursorPos {
    pub char_index: usize,
    pub row: usize,
    pub col: usize,
}

impl CursorPos {
    pub const fn origin() -> Self {
        Self { char_index: 0, row: 0, col: 0 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
```

- [ ] **Step 4: 写 `src/protocol/viewport.rs`**（不含 width/height，消除 height-1 越权）

```rust
/// 视口滚动位置。尺寸不存（从 layout 给的 rect 拿），消除「预留状态栏行」越权。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Viewport {
    pub top_row: usize,
    pub left_col: usize,
}

impl Viewport {
    pub const fn origin() -> Self {
        Self { top_row: 0, left_col: 0 }
    }

    /// 调整 top_row 使 cursor_row 在 [top_row, top_row+view_height) 内。
    pub fn ensure_cursor_visible(&mut self, cursor_row: usize, view_height: usize) {
        if view_height == 0 {
            self.top_row = cursor_row;
            return;
        }
        if cursor_row < self.top_row {
            self.top_row = cursor_row;
        } else if cursor_row >= self.top_row + view_height {
            self.top_row = cursor_row - view_height + 1;
        }
    }

    pub fn scroll_down(&mut self, n: usize, view_height: usize) {
        self.top_row = self.top_row.saturating_add(n);
        let _ = view_height; // v0.2 预留：未来按 view_height 钳位
    }

    pub fn scroll_up(&mut self, n: usize) {
        self.top_row = self.top_row.saturating_sub(n);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origin_is_zero() {
        let v = Viewport::origin();
        assert_eq!((v.top_row, v.left_col), (0, 0));
    }

    #[test]
    fn scroll_down_when_cursor_below() {
        let mut v = Viewport::origin();
        v.ensure_cursor_visible(25, 23);
        assert_eq!(v.top_row, 3);
    }

    #[test]
    fn scroll_up_when_cursor_above() {
        let mut v = Viewport { top_row: 10, left_col: 0 };
        v.ensure_cursor_visible(5, 23);
        assert_eq!(v.top_row, 5);
    }

    #[test]
    fn no_scroll_when_visible() {
        let mut v = Viewport { top_row: 5, left_col: 0 };
        v.ensure_cursor_visible(10, 23);
        assert_eq!(v.top_row, 5);
    }

    #[test]
    fn zero_height_sets_top_to_cursor() {
        let mut v = Viewport::origin();
        v.ensure_cursor_visible(7, 0);
        assert_eq!(v.top_row, 7);
    }
}
```

- [ ] **Step 5: 改 `src/protocol/mod.rs`**

```rust
pub mod cursor;
pub mod edit_view; // Task 2 创建
pub mod frontend_event;
pub mod ids;
pub mod key_event;
pub mod status;
pub mod viewport;
```

- [ ] **Step 6: 删除 `src/protocol/core_patch.rs`**

```bash
git rm src/protocol/core_patch.rs
```

- [ ] **Step 7: 单独验证 protocol 基础模块**

```bash
cargo test --lib protocol::ids protocol::status protocol::cursor protocol::viewport
```
Expected: 上述 4 模块测试 PASS（`edit_view` 模块在 Task 2 创建前 mod 声明会导致编译失败——临时把 `mod.rs` 里 `pub mod edit_view;` 注释掉，Task 2 再放开）。

- [ ] **Step 8: Commit**

```bash
git add -A && git commit -m "[wip] refactor: protocol 基础（ids/status/cursor/viewport），移除 core_patch"
```

---

## Task 2: protocol/edit_view.rs

**Files:**
- Create: `src/protocol/edit_view.rs`
- Modify: `src/protocol/mod.rs`（放开 `pub mod edit_view;`）

- [ ] **Step 1: 写 `src/protocol/edit_view.rs`**

```rust
use std::borrow::Cow;

use crate::protocol::cursor::CursorPos;
use crate::protocol::ids::ContentId;
use crate::protocol::status::StatusMessage;
use crate::protocol::viewport::Viewport;

/// 折行模式：v0.2 预留，默认 None（不折行，与现状一致）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WrapMode {
    None,
    Soft,
}

/// 文档投影：纯文档数据，不含 cursor/viewport（那些在 Space）。
/// Content 渲染策略经此 trait 读文档，不 import Editor。
pub trait EditView {
    fn line(&self, idx: usize) -> Cow<str>;
    fn len_lines(&self) -> usize;
    fn file_name(&self) -> Option<&str>;
    fn modified(&self) -> bool;
    fn status(&self) -> StatusMessage;
}

/// 按 ContentId 查文档。App impl，Frontend 用。
pub trait ContentLookup {
    fn get(&self, id: ContentId) -> Option<&dyn EditView>;
}

/// 单个 Space 的视图状态快照（透传给渲染）。
#[derive(Clone, Copy, Debug)]
pub struct SpaceState {
    pub viewport: Viewport,
    pub cursor: CursorPos,
}

/// 渲染上下文：文档查询能力 + 焦点信息（供 StatusBarContent 读焦点 cursor/文档）。
pub struct RenderCtx<'a> {
    pub contents: &'a dyn ContentLookup,
    pub focused_state: SpaceState,
    pub focused_content: ContentId,
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeDoc;
    impl EditView for FakeDoc {
        fn line(&self, idx: usize) -> Cow<str> { Cow::Owned(format!("line{idx}")) }
        fn len_lines(&self) -> usize { 3 }
        fn file_name(&self) -> Option<&str> { Some("f.txt") }
        fn modified(&self) -> bool { true }
        fn status(&self) -> StatusMessage { StatusMessage::Saved }
    }

    struct Lookup(FakeDoc);
    impl ContentLookup for Lookup {
        fn get(&self, _id: ContentId) -> Option<&dyn EditView> { Some(&self.0) }
    }

    #[test]
    fn edit_view_via_lookup() {
        let lk = Lookup(FakeDoc);
        let doc = lk.get(ContentId(0)).unwrap();
        assert_eq!(doc.len_lines(), 3);
        assert_eq!(doc.file_name(), Some("f.txt"));
        assert!(doc.modified());
        assert_eq!(doc.status(), StatusMessage::Saved);
        assert_eq!(doc.line(1).as_ref(), "line1");
    }

    #[test]
    fn wrap_mode_default_is_none() {
        let m = WrapMode::None;
        assert_eq!(m, WrapMode::None);
    }
}
```

- [ ] **Step 2: 放开 `src/protocol/mod.rs` 的 `pub mod edit_view;`**

- [ ] **Step 3: 验证**

```bash
cargo test --lib protocol::edit_view
```
Expected: PASS

- [ ] **Step 4: Commit**

```bash
git add -A && git commit -m "[wip] refactor: protocol::edit_view（EditView/ContentLookup/SpaceState/RenderCtx/WrapMode）"
```

---

## Task 3: core 重写（buffer 沿用 + status 改用 protocol + edit.rs 纯函数）

**Files:**
- Create: `src/core/edit.rs`
- Modify: `src/core/status.rs`, `src/core/buffer.rs`, `src/core/mod.rs`
- Delete: `src/core/editor.rs`, `src/core/cursor.rs`

- [ ] **Step 1: 改 `src/core/status.rs`**（`StatusMessage` 已移入 protocol，`Status` 包装它）

```rust
use crate::protocol::status::StatusMessage;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    message: StatusMessage,
}

impl Status {
    pub fn new() -> Self {
        Self { message: StatusMessage::None }
    }
    pub fn message(&self) -> &StatusMessage {
        &self.message
    }
    pub fn set(&mut self, message: StatusMessage) {
        self.message = message;
    }
}

impl Default for Status {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn defaults_to_none() {
        assert_eq!(Status::new().message(), &StatusMessage::None);
    }
    #[test]
    fn set_changes_message() {
        let mut s = Status::new();
        s.set(StatusMessage::Saved);
        assert_eq!(s.message(), &StatusMessage::Saved);
    }
}
```

- [ ] **Step 2: `src/core/buffer.rs` 沿用，无改动**（确认 `path()`/`modified()`/`line()`/`len_lines()`/`insert_char`/`delete_backward`/`load_from_file`/`save` 签名不变）

- [ ] **Step 3: 写 `src/core/edit.rs`**（编辑/移动/打开纯函数，逻辑从旧 `editor.rs`+`cursor.rs` 移植，操作传入的 buffer+cursor+status）

```rust
use std::io;

use ropey::Rope;

use crate::core::buffer::Buffer;
use crate::core::status::Status;
use crate::protocol::cursor::CursorPos;
use crate::protocol::key_event::{ArrowKey, CtrlKey, KeyEvent};
use crate::protocol::status::StatusMessage;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditAction {
    None,
    Saved,
    SaveFailed,
    Quit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

/// 根据 char_index 重算 row/col（ASCII 下 col == 行内 char 偏移）。
pub fn recompute(cur: &mut CursorPos, rope: &Rope) {
    let clamped = cur.char_index.min(rope.len_chars());
    cur.row = rope.char_to_line(clamped);
    let line_start = rope.line_to_char(cur.row);
    cur.col = clamped - line_start;
}

pub fn move_cursor(cur: &mut CursorPos, rope: &Rope, dir: Direction) {
    match dir {
        Direction::Left => {
            if cur.char_index > 0 {
                cur.char_index -= 1;
                recompute(cur, rope);
            }
        }
        Direction::Right => {
            if cur.char_index < rope.len_chars() {
                cur.char_index += 1;
                recompute(cur, rope);
            }
        }
        Direction::Up => {
            if cur.row > 0 {
                let target_row = cur.row - 1;
                let line_len = line_content_len(rope, target_row);
                let new_col = cur.col.min(line_len);
                cur.char_index = rope.line_to_char(target_row) + new_col;
                recompute(cur, rope);
            }
        }
        Direction::Down => {
            if cur.row + 1 < rope.len_lines() {
                let target_row = cur.row + 1;
                let line_len = line_content_len(rope, target_row);
                let new_col = cur.col.min(line_len);
                cur.char_index = rope.line_to_char(target_row) + new_col;
                recompute(cur, rope);
            }
        }
    }
}

/// 处理编辑键。操作传入的 buf/cur/status，返回动作（App 据 action 设 should_quit 等）。
pub fn handle_key(buf: &mut Buffer, cur: &mut CursorPos, status: &mut Status, key: KeyEvent) -> EditAction {
    match key {
        KeyEvent::Char(ch) => {
            let idx = cur.char_index;
            buf.insert_char(idx, ch as char);
            cur.char_index += 1;
            recompute(cur, buf.slice());
            EditAction::None
        }
        KeyEvent::Enter => {
            let idx = cur.char_index;
            buf.insert_char(idx, '\n');
            cur.char_index += 1;
            recompute(cur, buf.slice());
            EditAction::None
        }
        KeyEvent::Backspace => {
            let idx = cur.char_index;
            if buf.delete_backward(idx) {
                cur.char_index -= 1;
                recompute(cur, buf.slice());
            }
            EditAction::None
        }
        KeyEvent::Arrow(a) => {
            let dir = match a {
                ArrowKey::Left => Direction::Left,
                ArrowKey::Right => Direction::Right,
                ArrowKey::Up => Direction::Up,
                ArrowKey::Down => Direction::Down,
            };
            move_cursor(cur, buf.slice(), dir);
            EditAction::None
        }
        KeyEvent::Ctrl(CtrlKey::S) => match buf.save() {
            Ok(()) => {
                status.set(StatusMessage::Saved);
                EditAction::Saved
            }
            Err(_) => {
                status.set(StatusMessage::SaveFailed);
                EditAction::SaveFailed
            }
        },
        KeyEvent::Ctrl(CtrlKey::Q) => EditAction::Quit,
        KeyEvent::Escape | KeyEvent::Unknown => EditAction::None,
    }
}

/// 打开文件语义：NotFound→NewFile、非 UTF-8→OpenFailed、正常→None。
pub fn open_path(buf: &mut Buffer, status: &mut Status, path: &str) -> io::Result<()> {
    match buf.load_from_file(path) {
        Ok(()) => {
            let is_new = !std::path::Path::new(path).exists();
            status.set(if is_new { StatusMessage::NewFile } else { StatusMessage::None });
            Ok(())
        }
        Err(e) if e.kind() == io::ErrorKind::InvalidData => {
            status.set(StatusMessage::OpenFailed);
            Ok(())
        }
        Err(e) => {
            status.set(StatusMessage::OpenFailed);
            Err(e)
        }
    }
}

/// 返回某行内容长度（不含末尾 '\n'）。
fn line_content_len(rope: &Rope, row: usize) -> usize {
    let line = rope.line(row);
    let s = line.to_string();
    match s.strip_suffix('\n') {
        Some(rest) => rest.chars().count(),
        None => s.chars().count(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn rope(s: &str) -> Rope { Rope::from_str(s) }

    #[test]
    fn recompute_multi_line() {
        let r = rope("ab\ncd");
        let mut c = CursorPos::origin();
        c.char_index = 4;
        recompute(&mut c, &r);
        assert_eq!((c.row, c.col), (1, 1));
    }

    #[test]
    fn move_left_right_bounds() {
        let r = rope("abc");
        let mut c = CursorPos::origin();
        move_cursor(&mut c, &r, Direction::Right);
        move_cursor(&mut c, &r, Direction::Right);
        move_cursor(&mut c, &r, Direction::Right);
        assert_eq!(c.char_index, 3);
        move_cursor(&mut c, &r, Direction::Right);
        assert_eq!(c.char_index, 3);
        move_cursor(&mut c, &r, Direction::Left);
        assert_eq!(c.char_index, 2);
    }

    #[test]
    fn move_up_down_clamps_col() {
        let r = rope("hello\nab\nworld");
        let mut c = CursorPos { char_index: 4, row: 0, col: 4 };
        recompute(&mut c, &r);
        move_cursor(&mut c, &r, Direction::Down);
        assert_eq!((c.row, c.col), (1, 2));
    }

    #[test]
    fn handle_key_insert_and_move() {
        let mut buf = Buffer::new();
        let mut cur = CursorPos::origin();
        let mut st = Status::new();
        handle_key(&mut buf, &mut cur, &mut st, KeyEvent::Char(b'a'));
        handle_key(&mut buf, &mut cur, &mut st, KeyEvent::Char(b'b'));
        assert_eq!(buf.slice().to_string(), "ab");
        assert_eq!((cur.row, cur.col), (0, 2));
    }

    #[test]
    fn handle_key_enter_and_backspace() {
        let mut buf = Buffer::new();
        let mut cur = CursorPos::origin();
        let mut st = Status::new();
        handle_key(&mut buf, &mut cur, &mut st, KeyEvent::Char(b'a'));
        handle_key(&mut buf, &mut cur, &mut st, KeyEvent::Enter);
        assert_eq!(buf.slice().to_string(), "a\n");
        assert_eq!(cur.row, 1);
        handle_key(&mut buf, &mut cur, &mut st, KeyEvent::Backspace);
        assert_eq!(buf.slice().to_string(), "a");
    }

    #[test]
    fn handle_key_ctrl_q_returns_quit() {
        let mut buf = Buffer::new();
        let mut cur = CursorPos::origin();
        let mut st = Status::new();
        assert_eq!(handle_key(&mut buf, &mut cur, &mut st, KeyEvent::Ctrl(CtrlKey::Q)), EditAction::Quit);
    }

    #[test]
    fn handle_key_ctrl_s_saves() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("out.txt");
        let path_str = path.to_str().unwrap();
        let mut buf = Buffer::new();
        let mut cur = CursorPos::origin();
        let mut st = Status::new();
        open_path(&mut buf, &mut st, path_str).unwrap(); // 新文件
        handle_key(&mut buf, &mut cur, &mut st, KeyEvent::Char(b'x'));
        assert_eq!(handle_key(&mut buf, &mut cur, &mut st, KeyEvent::Ctrl(CtrlKey::S)), EditAction::Saved);
        assert_eq!(st.message(), &StatusMessage::Saved);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "x");
    }

    #[test]
    fn open_missing_is_new_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("nope.txt");
        let mut buf = Buffer::new();
        let mut st = Status::new();
        open_path(&mut buf, &mut st, path.to_str().unwrap()).unwrap();
        assert_eq!(st.message(), &StatusMessage::NewFile);
    }

    #[test]
    fn open_non_utf8_is_open_failed() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("bin.dat");
        std::fs::write(&path, [0xFF, 0xFE, 0xC0]).unwrap();
        let mut buf = Buffer::new();
        let mut st = Status::new();
        let res = open_path(&mut buf, &mut st, path.to_str().unwrap());
        assert!(res.is_ok());
        assert_eq!(st.message(), &StatusMessage::OpenFailed);
    }
}
```

- [ ] **Step 4: 改 `src/core/mod.rs`**

```rust
pub mod buffer;
pub mod edit;
pub mod status;
```

- [ ] **Step 5: 删除旧文件**

```bash
git rm src/core/editor.rs src/core/cursor.rs
```

- [ ] **Step 6: 验证 core 测试**

```bash
cargo test --lib core::edit core::status
```
Expected: PASS（`core::buffer` 沿用其测试也 PASS）

- [ ] **Step 7: Commit**

```bash
git add -A && git commit -m "[wip] refactor: core 退化为 Buffer+Status+纯函数 edit，删 editor/cursor"
```

---

## Task 4: terminal 加 Canvas trait

**Files:**
- Modify: `src/terminal/output.rs`

- [ ] **Step 1: 在 `src/terminal/output.rs` 顶部加 `Canvas` trait + impl**（保留原 `Output<W>` 不变）

在文件 `use` 之后、`pub struct Output` 之前插入：

```rust
/// 绘图画布抽象：使 Content::render 可写成 trait object（Box<dyn Content>）。
/// Output<W> 实现它。
pub trait Canvas {
    fn hide_cursor(&mut self) -> io::Result<()>;
    fn show_cursor(&mut self) -> io::Result<()>;
    fn move_cursor(&mut self, row: usize, col: usize) -> io::Result<()>;
    fn clear_line(&mut self) -> io::Result<()>;
    fn write_str(&mut self, s: &str) -> io::Result<()>;
    fn flush(&mut self) -> io::Result<()>;
}

impl<W: Write> Canvas for Output<W> {
    fn hide_cursor(&mut self) -> io::Result<()> { Output::hide_cursor(self) }
    fn show_cursor(&mut self) -> io::Result<()> { Output::show_cursor(self) }
    fn move_cursor(&mut self, row: usize, col: usize) -> io::Result<()> { Output::move_cursor(self, row, col) }
    fn clear_line(&mut self) -> io::Result<()> { Output::clear_line(self) }
    fn write_str(&mut self, s: &str) -> io::Result<()> { Output::write_str(self, s) }
    fn flush(&mut self) -> io::Result<()> { Output::flush(self) }
}
```

- [ ] **Step 2: 加测试**

在 `output.rs` 的 `#[cfg(test)] mod tests` 末尾加：

```rust
    #[test]
    fn canvas_dispatches_to_output() {
        let mut out = Output::new(Vec::new());
        let c: &mut dyn Canvas = &mut out;
        c.write_str("x").unwrap();
        c.move_cursor(2, 5).unwrap();
        let s = String::from_utf8(out.into_inner()).unwrap();
        assert!(s.contains('x'));
        assert!(s.contains("3;6"), "got: {s}");
    }
```

- [ ] **Step 3: 验证**

```bash
cargo test --lib terminal::output
```
Expected: PASS

- [ ] **Step 4: Commit**

```bash
git add -A && git commit -m "[wip] refactor: terminal::output 加 Canvas trait（object-safe 绘图抽象）"
```

---

## Task 5: layout 重写（Space 持视图状态 + ResolvedScene 带 state + 引擎透传）

**Files:**
- Modify: `src/layout/space.rs`, `src/layout/scene.rs`, `src/layout/resolved.rs`, `src/layout/taffy_engine.rs`, `src/layout/mod.rs`
- Delete: `src/layout/content.rs`, `src/layout/ids.rs`

- [ ] **Step 1: 重写 `src/layout/space.rs`**（Space 加 viewport/cursor/wrap_mode）

```rust
use crate::protocol::cursor::CursorPos;
use crate::protocol::edit_view::WrapMode;
use crate::protocol::ids::{ContentId, SpaceId};
use crate::protocol::viewport::Viewport;

/// 空间节点：展示实例。持视图状态（viewport/cursor/wrap_mode）。
pub struct Space {
    pub id: SpaceId,
    pub kind: SpaceKind,
    pub sizing: Sizing,
    pub layer: Layer,
    pub viewport: Viewport,
    pub cursor: CursorPos,
    pub wrap_mode: WrapMode,
}

pub enum SpaceKind {
    Container { arrangement: Arrangement, children: Vec<SpaceId> },
    Host { content: ContentId },
}

pub enum Arrangement {
    Flex { direction: Axis, gap: i32, align: Align },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Axis { Horizontal, Vertical }

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Align { Stretch, Start, Center, End }

pub enum Sizing {
    Fixed(i32),
    Grow(u32),
}

#[repr(i32)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Layer {
    Base = 0,
    Overlay = 10,
    Modal = 20,
    Debug = 100,
}

impl PartialOrd for Layer {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> { Some(self.cmp(other)) }
}
impl Ord for Layer {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering { (*self as i32).cmp(&(*other as i32)) }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn layer_orders_by_discriminant() {
        assert!(Layer::Base < Layer::Overlay);
        assert!(Layer::Overlay < Layer::Modal);
        assert!(Layer::Modal < Layer::Debug);
    }
}
```

- [ ] **Step 2: 重写 `src/layout/scene.rs`**（删 `focused`；`build_editor_scene` 返回 `EditorScene`；`SceneBuilder::alloc` 初始化视图状态）

```rust
use std::collections::HashMap;

use crate::layout::ids::{ContentId, SpaceId}; // 重导出，见 mod.rs
use crate::layout::space::{Align, Arrangement, Axis, Layer, Sizing, Space, SpaceKind};
use crate::protocol::cursor::CursorPos;
use crate::protocol::edit_view::WrapMode;
use crate::protocol::viewport::Viewport;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Size { pub width: i32, pub height: i32 }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect { pub x: i32, pub y: i32, pub width: i32, pub height: i32 }

impl Rect {
    pub fn intersect(&self, other: &Rect) -> Option<Rect> {
        let x0 = self.x.max(other.x);
        let y0 = self.y.max(other.y);
        let x1 = (self.x + self.width).min(other.x + other.width);
        let y1 = (self.y + self.height).min(other.y + other.height);
        if x1 > x0 && y1 > y0 {
            Some(Rect { x: x0, y: y0, width: x1 - x0, height: y1 - y0 })
        } else {
            None
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Point { pub x: i32, pub y: i32 }

pub struct SpaceNode {
    pub id: SpaceId,
    pub parent: Option<SpaceId>,
    pub children: Vec<SpaceId>,
    pub space: Space,
}

pub struct Scene {
    pub root: SpaceId,
    pub size: Size,
    nodes: HashMap<SpaceId, SpaceNode>,
}

impl Scene {
    pub fn node(&self, id: SpaceId) -> &SpaceNode { self.nodes.get(&id).expect("space id exists") }
    pub fn node_mut(&mut self, id: SpaceId) -> &mut SpaceNode { self.nodes.get_mut(&id).expect("space id exists") }
    pub fn resize(&mut self, width: i32, height: i32) { self.size = Size { width, height }; }
}

#[derive(Debug, PartialEq, Eq)]
pub enum BuildError { UnknownRoot, CycleDetected, DanglingChild }

pub struct SceneBuilder {
    nodes: HashMap<SpaceId, SpaceNode>,
    next_id: u64,
}

impl SceneBuilder {
    pub fn new() -> Self { Self { nodes: HashMap::new(), next_id: 0 } }

    fn alloc(&mut self, kind: SpaceKind) -> SpaceId {
        let id = SpaceId(self.next_id);
        self.next_id += 1;
        let children = match &kind {
            SpaceKind::Container { children, .. } => children.clone(),
            SpaceKind::Host { .. } => Vec::new(),
        };
        let node = SpaceNode {
            id,
            parent: None,
            children,
            space: Space {
                id,
                kind,
                sizing: Sizing::Grow(1),
                layer: Layer::Base,
                viewport: Viewport::origin(),
                cursor: CursorPos::origin(),
                wrap_mode: WrapMode::None,
            },
        };
        self.nodes.insert(id, node);
        id
    }

    pub fn host(&mut self, content: ContentId) -> SpaceHandle {
        SpaceHandle { id: self.alloc(SpaceKind::Host { content }) }
    }

    pub fn container(&mut self, arrangement: Arrangement, children: Vec<SpaceId>) -> SpaceHandle {
        SpaceHandle { id: self.alloc(SpaceKind::Container { arrangement, children }) }
    }

    pub fn finish(mut self, root: SpaceId, size: Size) -> Result<Scene, BuildError> {
        if !self.nodes.contains_key(&root) { return Err(BuildError::UnknownRoot); }
        let mut visited: HashMap<SpaceId, ()> = HashMap::new();
        let mut stack: Vec<SpaceId> = vec![root];
        while let Some(sid) = stack.pop() {
            if visited.contains_key(&sid) { return Err(BuildError::CycleDetected); }
            visited.insert(sid, ());
            let children = self.nodes.get(&sid).ok_or(BuildError::DanglingChild)?.children.clone();
            for c in &children {
                if !self.nodes.contains_key(c) { return Err(BuildError::DanglingChild); }
                if let Some(cnode) = self.nodes.get_mut(c) { cnode.parent = Some(sid); }
                stack.push(*c);
            }
        }
        Ok(Scene { root, size, nodes: self.nodes })
    }
}

impl Default for SceneBuilder { fn default() -> Self { Self::new() } }

pub struct SpaceHandle { pub id: SpaceId }
impl SpaceHandle {
    pub fn fixed(self, b: &mut SceneBuilder, size: i32) -> SpaceId {
        if let Some(n) = b.nodes.get_mut(&self.id) { n.space.sizing = Sizing::Fixed(size); }
        self.id
    }
    pub fn grow(self, b: &mut SceneBuilder, weight: u32) -> SpaceId {
        if let Some(n) = b.nodes.get_mut(&self.id) { n.space.sizing = Sizing::Grow(weight); }
        self.id
    }
}

/// 标准布局：root Vertical [editor Grow(1), status Fixed(1)]。
/// 返回 EditorScene 带 editor/status space id，供 App 拿焦点。
pub fn build_editor_scene(width: i32, height: i32, editor: ContentId, status: ContentId) -> EditorScene {
    let mut b = SceneBuilder::new();
    let ed = b.host(editor).grow(&mut b, 1);
    let st = b.host(status).fixed(&mut b, 1);
    let root = b.container(
        Arrangement::Flex { direction: Axis::Vertical, gap: 0, align: Align::Stretch },
        vec![ed, st],
    );
    let scene = b.finish(root.id, Size { width, height }).expect("valid editor scene");
    EditorScene { scene, editor_space: ed, status_space: st }
}

pub struct EditorScene {
    pub scene: Scene,
    pub editor_space: SpaceId,
    pub status_space: SpaceId,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn build_editor_scene_has_two_hosts() {
        let es = build_editor_scene(80, 24, ContentId(0), ContentId(1));
        let root = es.scene.node(es.scene.root);
        match &root.space.kind {
            SpaceKind::Container { children, .. } => assert_eq!(children.len(), 2),
            _ => panic!("root must be container"),
        }
        assert_eq!(es.editor_space, SpaceId(0));
        assert_eq!(es.status_space, SpaceId(1));
    }
    #[test]
    fn rect_intersect() {
        let r = Rect { x: 0, y: 0, width: 10, height: 10 };
        let o = Rect { x: 5, y: 5, width: 10, height: 10 };
        assert_eq!(r.intersect(&o), Some(Rect { x: 5, y: 5, width: 5, height: 5 }));
        let far = Rect { x: 20, y: 20, width: 5, height: 5 };
        assert_eq!(r.intersect(&far), None);
    }
}
```

- [ ] **Step 3: 重写 `src/layout/resolved.rs`**（RenderItem 带 state，删 Renderer/render）

```rust
use crate::layout::scene::Rect;
use crate::layout::space::Layer;
use crate::protocol::edit_view::SpaceState;
use crate::protocol::ids::ContentId;

#[derive(Clone)]
pub struct RenderItem {
    pub content_id: ContentId,
    pub rect: Rect,
    pub clip: Option<Rect>,
    pub state: SpaceState,
    pub layer: Layer,
    pub z_index: i32,
    pub order: u64,
}

pub struct ResolvedScene {
    pub items: Vec<RenderItem>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::cursor::CursorPos;
    use crate::protocol::viewport::Viewport;

    fn state() -> SpaceState {
        SpaceState { viewport: Viewport::origin(), cursor: CursorPos::origin() }
    }

    #[test]
    fn render_item_holds_state() {
        let it = RenderItem {
            content_id: ContentId(0),
            rect: Rect { x: 0, y: 0, width: 80, height: 23 },
            clip: None,
            state: state(),
            layer: Layer::Base,
            z_index: 0,
            order: 0,
        };
        assert_eq!(it.content_id, ContentId(0));
        assert_eq!(it.state.cursor.char_index, 0);
    }
}
```

- [ ] **Step 4: 重写 `src/layout/taffy_engine.rs`**（layout 不吃 store，collect 填 state）

```rust
use std::collections::HashMap;

use taffy::prelude::*;

use crate::layout::ids::SpaceId;
use crate::layout::resolved::{RenderItem, ResolvedScene};
use crate::layout::scene::{Rect, Scene, Size as SceneSize, SpaceNode};
use crate::layout::space::{Align, Arrangement, Axis, Sizing, SpaceKind};
use crate::protocol::edit_view::SpaceState;

pub struct TaffyEngine { tree: TaffyTree }

struct CollectOut { items: Vec<RenderItem>, order: u64 }

impl TaffyEngine {
    pub fn new() -> Self { Self { tree: TaffyTree::new() } }

    pub fn layout(&mut self, scene: &Scene) -> ResolvedScene {
        self.tree = TaffyTree::new();
        let mut map: HashMap<SpaceId, NodeId> = HashMap::new();
        let root_node = self.build_node(scene, scene.root, None, Some(scene.size), &mut map);
        let available = Size {
            width: AvailableSpace::Definite(scene.size.width as f32),
            height: AvailableSpace::Definite(scene.size.height as f32),
        };
        let _ = self.tree.compute_layout(root_node, available);
        let mut out = CollectOut { items: Vec::new(), order: 0 };
        self.collect(scene, scene.root, None, &map, &mut out);
        ResolvedScene { items: out.items }
    }

    fn build_node(&mut self, scene: &Scene, sid: SpaceId, parent_axis: Option<Axis>, root_size: Option<SceneSize>, map: &mut HashMap<SpaceId, NodeId>) -> NodeId {
        let node = scene.node(sid);
        let style = style_for(node, parent_axis, root_size);
        let taffy_id = match &node.space.kind {
            SpaceKind::Container { children, arrangement } => {
                let axis = match arrangement { Arrangement::Flex { direction, .. } => *direction };
                let child_ids: Vec<NodeId> = children.iter()
                    .map(|c| self.build_node(scene, *c, Some(axis), None, map)).collect();
                self.tree.new_with_children(style, &child_ids).unwrap()
            }
            SpaceKind::Host { .. } => self.tree.new_leaf(style).unwrap(),
        };
        map.insert(sid, taffy_id);
        taffy_id
    }

    fn collect(&self, scene: &Scene, sid: SpaceId, parent_clip: Option<Rect>, map: &HashMap<SpaceId, NodeId>, out: &mut CollectOut) {
        let node = scene.node(sid);
        let taffy_id = map[&sid];
        let layout = self.tree.layout(taffy_id).expect("layout computed");
        let rect = Rect {
            x: layout.location.x.round() as i32,
            y: layout.location.y.round() as i32,
            width: layout.size.width.round() as i32,
            height: layout.size.height.round() as i32,
        };
        let clip = match parent_clip { Some(p) => p.intersect(&rect), None => Some(rect) };
        let content_id = match &node.space.kind {
            SpaceKind::Host { content } => Some(*content),
            SpaceKind::Container { .. } => None,
        };
        if let Some(cid) = content_id {
            out.items.push(RenderItem {
                content_id: cid,
                rect,
                clip,
                state: SpaceState { viewport: node.space.viewport, cursor: node.space.cursor },
                layer: node.space.layer,
                z_index: 0,
                order: out.order,
            });
            out.order += 1;
        }
        if let SpaceKind::Container { children, .. } = &node.space.kind {
            for c in children { self.collect(scene, *c, clip, map, out); }
        }
    }
}

impl Default for TaffyEngine { fn default() -> Self { Self::new() } }

fn style_for(node: &SpaceNode, parent_axis: Option<Axis>, root_size: Option<SceneSize>) -> Style {
    let mut style = Style::default();
    match (parent_axis, &node.space.sizing) {
        (Some(Axis::Vertical), Sizing::Fixed(x)) => { style.size.height = LengthPercentageAuto::Length(*x as f32).into(); }
        (Some(Axis::Horizontal), Sizing::Fixed(x)) => { style.size.width = LengthPercentageAuto::Length(*x as f32).into(); }
        (_, Sizing::Grow(w)) => { style.flex_grow = *w as f32; }
        (None, Sizing::Fixed(_)) => {}
    }
    if let Some(s) = root_size {
        style.size.width = LengthPercentageAuto::Length(s.width as f32).into();
        style.size.height = LengthPercentageAuto::Length(s.height as f32).into();
    }
    if let SpaceKind::Container { arrangement, .. } = &node.space.kind {
        let (direction, gap, align) = match arrangement { Arrangement::Flex { direction, gap, align } => (*direction, *gap, *align) };
        style.display = Display::Flex;
        style.flex_direction = match direction { Axis::Vertical => FlexDirection::Column, Axis::Horizontal => FlexDirection::Row };
        let gap_val = LengthPercentage::Length(gap as f32);
        style.gap = Size { width: gap_val, height: gap_val };
        style.align_items = match align {
            Align::Stretch => Some(AlignItems::Stretch),
            Align::Start => Some(AlignItems::FlexStart),
            Align::Center => Some(AlignItems::Center),
            Align::End => Some(AlignItems::FlexEnd),
        };
    }
    style
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::scene::build_editor_scene;
    use crate::protocol::ids::ContentId;

    fn item_for(scene: &ResolvedScene, content: ContentId) -> &RenderItem {
        scene.items.iter().find(|i| i.content_id == content).unwrap()
    }

    #[test]
    fn editor_grows_and_status_fixed() {
        let es = build_editor_scene(80, 24, ContentId(0), ContentId(1));
        let mut engine = TaffyEngine::new();
        let resolved = engine.layout(&es.scene);
        assert_eq!(item_for(&resolved, ContentId(0)).rect, Rect { x: 0, y: 0, width: 80, height: 23 });
        assert_eq!(item_for(&resolved, ContentId(1)).rect, Rect { x: 0, y: 23, width: 80, height: 1 });
    }

    #[test]
    fn items_carry_state_and_dfs_order() {
        let es = build_editor_scene(80, 24, ContentId(0), ContentId(1));
        let mut engine = TaffyEngine::new();
        let resolved = engine.layout(&es.scene);
        assert_eq!(resolved.items.len(), 2); // 仅 Host 进 items（container 不进）
        assert_eq!(resolved.items[0].content_id, ContentId(0));
        assert_eq!(resolved.items[1].content_id, ContentId(1));
        assert_eq!(resolved.items[0].state.cursor, crate::protocol::cursor::CursorPos::origin());
    }

    #[test]
    fn resize_changes_geometry() {
        let mut es = build_editor_scene(80, 24, ContentId(0), ContentId(1));
        es.scene.resize(100, 40);
        let mut engine = TaffyEngine::new();
        let resolved = engine.layout(&es.scene);
        assert_eq!(item_for(&resolved, ContentId(0)).rect.height, 39);
        assert_eq!(item_for(&resolved, ContentId(0)).rect.width, 100);
    }
}
```

- [ ] **Step 5: 改 `src/layout/mod.rs`**（重导出 ids，删 content/ids 模块）

```rust
pub mod resolved;
pub mod scene;
pub mod space;
pub mod taffy_engine;

// ids 已移入 protocol；此处重导出以兼容 `use crate::layout::ids::*`。
pub mod ids {
    pub use crate::protocol::ids::{ContentId, SceneId, SpaceId};
}
```

- [ ] **Step 6: 删除 `src/layout/content.rs`、`src/layout/ids.rs`**

```bash
git rm src/layout/content.rs src/layout/ids.rs
```

- [ ] **Step 7: Commit**（整体仍不可编译——tui/app 还引用旧类型）

```bash
git add -A && git commit -m "[wip] refactor: layout 重写（Space 持视图状态，引擎透传 state），删 ContentStore/旧 ids"
```

---

## Task 6: tui 重写（Content trait + EditorContent/StatusBarContent + TuiFrontend）

**Files:**
- Create: `src/tui/content.rs`, `src/tui/tui_frontend.rs`
- Modify: `src/tui/mod.rs`
- Delete: `src/tui/renderer.rs`, `src/tui/viewport.rs`

- [ ] **Step 1: 写 `src/tui/content.rs`**

```rust
use std::io;

use crate::layout::scene::Rect;
use crate::protocol::edit_view::{EditView, RenderCtx, SpaceState};
use crate::protocol::ids::ContentId;
use crate::protocol::status::StatusMessage;
use crate::terminal::output::Canvas;

/// 内容渲染策略。render 只画文本，不碰光标（光标由 TuiFrontend 末尾统一处理）。
pub trait Content {
    fn render(&self, content_id: ContentId, ctx: &RenderCtx, state: &SpaceState, rect: Rect, canvas: &mut dyn Canvas) -> io::Result<()>;
}

/// 编辑文本区：用 state.viewport + state.cursor + 文档行画可见行。
pub struct EditorContent;

impl Content for EditorContent {
    fn render(&self, content_id: ContentId, ctx: &RenderCtx, state: &SpaceState, rect: Rect, canvas: &mut dyn Canvas) -> io::Result<()> {
        let doc = match ctx.contents.get(content_id) { Some(d) => d, None => return Ok(()) };
        let total = doc.len_lines();
        for row in 0..rect.height {
            let line_idx = state.viewport.top_row + row as usize;
            let screen_row = (rect.y + row) as usize;
            canvas.move_cursor(screen_row, rect.x as usize)?;
            canvas.clear_line()?;
            if line_idx < total {
                let line = doc.line(line_idx).to_string();
                canvas.write_str(line.trim_end_matches('\n'))?;
            }
        }
        Ok(())
    }
}

/// 状态栏：用焦点 cursor + 焦点文档画状态行。
pub struct StatusBarContent;

impl Content for StatusBarContent {
    fn render(&self, _content_id: ContentId, ctx: &RenderCtx, _state: &SpaceState, rect: Rect, canvas: &mut dyn Canvas) -> io::Result<()> {
        let doc = match ctx.contents.get(ctx.focused_content) { Some(d) => d, None => return Ok(()) };
        canvas.move_cursor(rect.y as usize, rect.x as usize)?;
        canvas.clear_line()?;
        canvas.write_str(&status_line(doc, &ctx.focused_state))?;
        Ok(())
    }
}

fn status_line(doc: &dyn EditView, state: &SpaceState) -> String {
    let name = doc.file_name().unwrap_or("[No Name]");
    let modified = if doc.modified() { "[+]" } else { "" };
    let row = state.cursor.row;
    let col = state.cursor.col;
    let msg = match doc.status() {
        StatusMessage::None => "",
        StatusMessage::Saved => "Saved",
        StatusMessage::SaveFailed => "SaveFailed",
        StatusMessage::NewFile => "NewFile",
        StatusMessage::OpenFailed => "OpenFailed",
    };
    format!("{name} {modified}  {row}:{col}  {msg}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::borrow::Cow;
    use crate::protocol::cursor::CursorPos;
    use crate::protocol::ids::ContentId;
    use crate::protocol::status::StatusMessage;
    use crate::protocol::viewport::Viewport;
    use crate::terminal::output::Output;

    struct Doc { text: String, name: &'static str, modified: bool, status: StatusMessage }
    impl EditView for Doc {
        fn line(&self, idx: usize) -> Cow<str> {
            Cow::Owned(self.text.lines().nth(idx).unwrap_or("").to_string())
        }
        fn len_lines(&self) -> usize { self.text.lines().count().max(1) }
        fn file_name(&self) -> Option<&str> { Some(self.name) }
        fn modified(&self) -> bool { self.modified }
        fn status(&self) -> StatusMessage { self.status.clone() }
    }

    struct Lookup(Doc);
    impl crate::protocol::edit_view::ContentLookup for Lookup {
        fn get(&self, _id: ContentId) -> Option<&dyn EditView> { Some(&self.0) }
    }

    fn ctx<'a>(lk: &'a Lookup) -> RenderCtx<'a> {
        RenderCtx {
            contents: lk,
            focused_state: SpaceState { viewport: Viewport::origin(), cursor: CursorPos { char_index: 2, row: 0, col: 2 } },
            focused_content: ContentId(0),
        }
    }

    #[test]
    fn editor_content_draws_lines() {
        let lk = Lookup(Doc { text: "hi".into(), name: "f.txt", modified: true, status: StatusMessage::None });
        let c = ctx(&lk);
        let state = SpaceState { viewport: Viewport::origin(), cursor: CursorPos::origin() };
        let mut out = Output::new(Vec::new());
        EditorContent.render(ContentId(0), &c, &state, Rect { x: 0, y: 0, width: 10, height: 1 }, &mut out).unwrap();
        let s = String::from_utf8(out.into_inner()).unwrap();
        assert!(s.contains("hi"), "{s}");
    }

    #[test]
    fn status_bar_draws_name_and_cursor() {
        let lk = Lookup(Doc { text: "hi".into(), name: "f.txt", modified: true, status: StatusMessage::None });
        let c = ctx(&lk);
        let state = SpaceState { viewport: Viewport::origin(), cursor: CursorPos::origin() };
        let mut out = Output::new(Vec::new());
        StatusBarContent.render(ContentId(1), &c, &state, Rect { x: 0, y: 1, width: 40, height: 1 }, &mut out).unwrap();
        let s = String::from_utf8(out.into_inner()).unwrap();
        assert!(s.contains("f.txt"), "{s}");
        assert!(s.contains("[+]"), "{s}");
        assert!(s.contains("0:2"), "{s}"); // 焦点 cursor row=0,col=2
    }
}
```

- [ ] **Step 2: 写 `src/tui/tui_frontend.rs`**

```rust
use std::io;

use crate::layout::resolved::ResolvedScene;
use crate::layout::scene::Rect;
use crate::protocol::edit_view::{ContentLookup, RenderCtx, SpaceState};
use crate::protocol::ids::ContentId;
use crate::terminal::input::{Input, InputSource};
use crate::terminal::output::{Canvas, Output};
use crate::tui::content::{Content, EditorContent, StatusBarContent};

/// TUI 前端：持 Content 注册表 + IO + 终端生命周期。
pub struct TuiFrontend<W: io::Write> {
    registry: std::collections::HashMap<ContentId, Box<dyn Content>>,
    input: Input,
    output: Output<W>,
}

impl<W: io::Write> TuiFrontend<W> {
    pub fn new(output: Output<W>, editor: ContentId, status: ContentId) -> Self {
        let mut registry: std::collections::HashMap<ContentId, Box<dyn Content>> = std::collections::HashMap::new();
        registry.insert(editor, Box::new(EditorContent));
        registry.insert(status, Box::new(StatusBarContent));
        Self { registry, input: Input::new(), output }
    }

    /// 取回内部 Output（测试断言 VT 输出）。
    #[cfg(test)]
    pub fn into_output(self) -> Output<W> { self.output }

    fn focused_rect(&self, scene: &ResolvedScene, focused_content: ContentId) -> Option<Rect> {
        scene.items.iter().find(|i| i.content_id == focused_content).map(|i| i.rect)
    }
}

use crate::protocol::frontend_event::FrontendEvent;

impl<W: io::Write> crate::app::Frontend for TuiFrontend<W> {
    async fn next_event(&mut self) -> io::Result<Option<FrontendEvent>> {
        self.input.next_event().await
    }

    fn render(&mut self, contents: &dyn ContentLookup, scene: &ResolvedScene, focused_state: SpaceState, focused_content: ContentId) -> io::Result<()> {
        let ctx = RenderCtx { contents, focused_state, focused_content };
        self.output.hide_cursor()?;
        // 排序：按 (layer, z_index, order) 拷贝排序
        let mut items = scene.items.clone();
        items.sort_by_key(|i| (i.layer, i.z_index, i.order));
        for item in &items {
            if let Some(c) = self.registry.get(&item.content_id) {
                c.render(item.content_id, &ctx, &item.state, item.rect, &mut self.output as &mut dyn Canvas)?;
            }
        }
        // 光标定位：用焦点 cursor + 焦点 item rect
        if let Some(rect) = self.focused_rect(scene, focused_content) {
            let screen_row = focused_state.cursor.row.saturating_sub(focused_state.viewport.top_row) + rect.y as usize;
            let screen_col = focused_state.cursor.col.saturating_sub(focused_state.viewport.left_col) + rect.x as usize;
            self.output.move_cursor(screen_row, screen_col)?;
            self.output.show_cursor()?;
        }
        self.output.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::scene::build_editor_scene;
    use crate::layout::taffy_engine::TaffyEngine;
    use crate::protocol::cursor::CursorPos;
    use crate::protocol::edit_view::EditView;
    use crate::protocol::viewport::Viewport;
    use std::borrow::Cow;

    struct Doc { text: String }
    impl EditView for Doc {
        fn line(&self, idx: usize) -> Cow<str> { Cow::Owned(self.text.lines().nth(idx).unwrap_or("").to_string()) }
        fn len_lines(&self) -> usize { self.text.lines().count().max(1) }
        fn file_name(&self) -> Option<&str> { Some("f.txt") }
        fn modified(&self) -> bool { true }
        fn status(&self) -> crate::protocol::status::StatusMessage { crate::protocol::status::StatusMessage::None }
    }
    struct Lookup(Doc);
    impl ContentLookup for Lookup { fn get(&self, _id: ContentId) -> Option<&dyn EditView> { Some(&self.0) } }

    #[test]
    fn render_outputs_text_status_and_cursor() {
        let lk = Lookup(Doc { text: "hi".into() });
        let es = build_editor_scene(40, 5, ContentId(0), ContentId(1));
        let mut engine = TaffyEngine::new();
        let resolved = engine.layout(&es.scene);
        let mut fe = TuiFrontend::new(Output::new(Vec::new()), ContentId(0), ContentId(1));
        let focused_state = SpaceState { viewport: Viewport::origin(), cursor: CursorPos { char_index: 2, row: 0, col: 2 } };
        fe.render(&lk, &resolved, focused_state, ContentId(0)).unwrap();
        let s = String::from_utf8(fe.into_output().into_inner()).unwrap();
        assert!(s.contains("hi"), "{s}");
        assert!(s.contains("f.txt"), "{s}");
        // 光标 (col=2,row=0) -> ESC[1;3H；show_cursor ESC[?25h
        assert!(s.contains("\u{1b}[1;3H"), "cursor pos: {s:?}");
        assert!(s.contains("\u{1b}[?25h"), "show cursor: {s:?}");
    }
}
```

- [ ] **Step 3: 改 `src/tui/mod.rs`**

```rust
pub mod content;
pub mod tui_frontend;
```

- [ ] **Step 4: 删除 `src/tui/renderer.rs`、`src/tui/viewport.rs`**

```bash
git rm src/tui/renderer.rs src/tui/viewport.rs
```

- [ ] **Step 5: Commit**（仍不可编译——app.rs 还引用旧类型；Task 7 恢复）

```bash
git add -A && git commit -m "[wip] refactor: tui 重写（Content trait + EditorContent/StatusBarContent + TuiFrontend），删 renderer/viewport"
```

---

## Task 7: app 重写 + main 接线 + 删旧引用（恢复全量编译）

**Files:**
- Modify: `src/app.rs`, `src/main.rs`

- [ ] **Step 1: 重写 `src/app.rs`**

```rust
use std::collections::HashMap;
use std::io;

use crate::core::buffer::Buffer;
use crate::core::edit::{handle_key, open_path, EditAction};
use crate::core::status::Status;
use crate::layout::ids::{ContentId, SpaceId};
use crate::layout::resolved::ResolvedScene;
use crate::layout::scene::{build_editor_scene, EditorScene};
use crate::layout::taffy_engine::TaffyEngine;
use crate::protocol::cursor::CursorPos;
use crate::protocol::edit_view::{ContentLookup, EditView, SpaceState};
use crate::protocol::frontend_event::FrontendEvent;
use crate::protocol::status::StatusMessage;
use crate::terminal::input::InputSource;

/// 前端抽象：App 经此 trait 不感知 tui/gui。
/// next_event 获取事件（IO），render 渲染。定义在 app 层，由 tui 层实现（依赖倒置）。
pub trait Frontend {
    async fn next_event(&mut self) -> io::Result<Option<FrontendEvent>>;
    fn render(&mut self, contents: &dyn ContentLookup, scene: &ResolvedScene, focused_state: SpaceState, focused_content: ContentId) -> io::Result<()>;
}

/// 文档容器：buffer + status。impl EditView 供 Frontend 读。
pub struct Document {
    pub buffer: Buffer,
    pub status: Status,
}

impl EditView for Document {
    fn line(&self, idx: usize) -> std::borrow::Cow<str> { self.buffer.line(idx).to_string().into() }
    fn len_lines(&self) -> usize { self.buffer.len_lines() }
    fn file_name(&self) -> Option<&str> {
        self.buffer.path().and_then(|p| p.file_name()).and_then(|n| n.to_str())
    }
    fn modified(&self) -> bool { self.buffer.modified() }
    fn status(&self) -> StatusMessage { self.status.message().clone() }
}

pub struct App<I: InputSource> {
    contents: HashMap<ContentId, Document>,
    editor_content: ContentId,
    status_content: ContentId,
    scene: EditorScene,
    engine: TaffyEngine,
    focused: SpaceId,
    should_quit: bool,
    input: I,
    frontend: Box<dyn Frontend>,
}

impl<I: InputSource> App<I> {
    pub fn new(path: Option<&str>, width: usize, height: usize, input: I, frontend: Box<dyn Frontend>) -> io::Result<Self> {
        let editor_content = ContentId(0);
        let status_content = ContentId(1);
        let mut buffer = Buffer::new();
        let mut status = Status::new();
        if let Some(p) = path {
            open_path(&mut buffer, &mut status, p)?;
        }
        let mut contents = HashMap::new();
        contents.insert(editor_content, Document { buffer, status });
        let scene = build_editor_scene(width as i32, height as i32, editor_content, status_content);
        Ok(Self {
            contents,
            editor_content,
            status_content,
            scene,
            engine: TaffyEngine::new(),
            focused: SpaceId(0), // editor space（build_editor_scene 保证 editor=SpaceId(0)）
            should_quit: false,
            input,
            frontend,
        })
    }

    pub async fn run(&mut self) -> io::Result<()> {
        self.render()?;
        while !self.should_quit {
            let event = match self.input.next_event().await? {
                Some(e) => e,
                None => continue,
            };
            self.handle_event(event)?;
            self.render()?;
        }
        Ok(())
    }

    fn handle_event(&mut self, event: FrontendEvent) -> io::Result<()> {
        match event {
            FrontendEvent::Resize(r) => {
                self.scene.scene.resize(r.width as i32, r.height as i32);
            }
            FrontendEvent::Key(k) => {
                let content_id = self.editor_content; // v0.2 焦点即编辑区
                let doc = self.contents.get_mut(&content_id).expect("editor content exists");
                let space = self.scene.scene.node_mut(self.focused);
                let action = handle_key(&mut doc.buffer, &mut space.space.cursor, &mut doc.status, k);
                if action == EditAction::Quit {
                    self.should_quit = true;
                }
            }
            FrontendEvent::QuitRequest => {
                self.should_quit = true;
            }
        }
        Ok(())
    }

    fn render(&mut self) -> io::Result<()> {
        let resolved = self.engine.layout(&self.scene.scene);
        // ensure_cursor_visible：用焦点 item rect 的 height
        if let Some(item) = resolved.items.iter().find(|i| i.content_id == self.editor_content) {
            let space = self.scene.scene.node_mut(self.focused);
            space.space.viewport.ensure_cursor_visible(space.space.cursor.row, item.rect.height as usize);
        }
        let focused_state = {
            let space = self.scene.scene.node(self.focused);
            SpaceState { viewport: space.space.viewport, cursor: space.space.cursor }
        };
        let contents: &dyn ContentLookup = self;
        self.frontend.render(contents, &resolved, focused_state, self.editor_content)
    }
}

impl<I: InputSource> ContentLookup for App<I> {
    fn get(&self, id: ContentId) -> Option<&dyn EditView> {
        self.contents.get(&id).map(|d| d as &dyn EditView)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    use crate::protocol::frontend_event::ResizeEvent;
    use crate::protocol::key_event::{ArrowKey, CtrlKey, KeyEvent};

    struct ScriptedInput { events: VecDeque<FrontendEvent> }
    impl ScriptedInput {
        fn new(events: Vec<FrontendEvent>) -> Self { Self { events: events.into() } }
    }
    impl InputSource for ScriptedInput {
        async fn next_event(&mut self) -> io::Result<Option<FrontendEvent>> {
            match self.events.pop_front() {
                Some(e) => Ok(Some(e)),
                None => Err(io::Error::other("scripted input exhausted: 测试脚本必须以 quit 事件结束")),
            }
        }
    }

    /// 记录型 Frontend：不真渲染，记录最后一次 render 的 focused_state.cursor 供断言。
    struct RecordingFrontend { last_cursor: Option<CursorPos> }
    impl Frontend for RecordingFrontend {
        async fn next_event(&mut self) -> io::Result<Option<FrontendEvent>> { Ok(None) }
        fn render(&mut self, _c: &dyn ContentLookup, _s: &ResolvedScene, focused_state: SpaceState, _fc: ContentId) -> io::Result<()> {
            self.last_cursor = Some(focused_state.cursor);
            Ok(())
        }
    }

    fn make_app(input: ScriptedInput, path: Option<&str>) -> App<ScriptedInput> {
        App::new(path, 40, 5, input, Box::new(RecordingFrontend { last_cursor: None })).unwrap()
    }

    #[tokio::test]
    async fn run_inserts_char_then_quits() {
        let input = ScriptedInput::new(vec![
            FrontendEvent::Key(KeyEvent::Char(b'a')),
            FrontendEvent::Key(KeyEvent::Ctrl(CtrlKey::Q)),
        ]);
        let mut app = make_app(input, None);
        app.run().await.unwrap();
        assert_eq!(app.contents[&app.editor_content].buffer.slice().to_string(), "a");
        assert!(app.should_quit);
    }

    #[tokio::test]
    async fn run_supports_backspace_and_arrows() {
        let input = ScriptedInput::new(vec![
            FrontendEvent::Key(KeyEvent::Char(b'a')),
            FrontendEvent::Key(KeyEvent::Char(b'b')),
            FrontendEvent::Key(KeyEvent::Backspace),
            FrontendEvent::Key(KeyEvent::Arrow(ArrowKey::Left)),
            FrontendEvent::Key(KeyEvent::Ctrl(CtrlKey::Q)),
        ]);
        let mut app = make_app(input, None);
        app.run().await.unwrap();
        assert_eq!(app.contents[&app.editor_content].buffer.slice().to_string(), "a");
        let space = app.scene.scene.node(app.focused);
        assert_eq!(space.space.cursor.col, 0);
    }

    #[tokio::test]
    async fn run_forwards_resize_to_scene() {
        let input = ScriptedInput::new(vec![
            FrontendEvent::Resize(ResizeEvent { width: 100, height: 40 }),
            FrontendEvent::Key(KeyEvent::Ctrl(CtrlKey::Q)),
        ]);
        let mut app = make_app(input, None);
        app.run().await.unwrap();
        assert_eq!(app.scene.scene.size, crate::layout::scene::Size { width: 100, height: 40 });
    }

    #[tokio::test]
    async fn run_opens_file_and_saves() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.txt");
        std::fs::write(&path, "hi").unwrap();
        let path_str = path.to_str().unwrap().to_owned();
        let input = ScriptedInput::new(vec![
            FrontendEvent::Key(KeyEvent::Ctrl(CtrlKey::S)),
            FrontendEvent::Key(KeyEvent::Ctrl(CtrlKey::Q)),
        ]);
        let mut app = make_app(input, Some(&path_str));
        app.run().await.unwrap();
        assert_eq!(app.contents[&app.editor_content].buffer.slice().to_string(), "hi");
        assert_ne!(*app.contents[&app.editor_content].status.message(), StatusMessage::SaveFailed);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hi");
    }
}
```

- [ ] **Step 2: 重写 `src/main.rs`**

```rust
mod app;
mod core;
mod layout;
mod protocol;
mod terminal;
mod tui;

use std::io::{self, Stdout};

use app::{App, Frontend};
use crossterm::terminal::size as term_size;
use terminal::input::Input;
use terminal::lifecycle::TerminalGuard;
use terminal::output::Output;
use tui::tui_frontend::TuiFrontend;

#[tokio::main]
async fn main() -> io::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let path = args.get(1).map(|s| s.as_str());

    let _guard = TerminalGuard::enter()?;

    let (width, height) = term_size().unwrap_or((80, 24));
    let frontend: Box<dyn Frontend> = Box::new(TuiFrontend::new(
        Output::new(io::stdout()),
        app::Document_id_editor(), // 见下注：实际用 ContentId 常量
    ));
    let mut app = App::new(path, width as usize, height as usize, Input::new(), frontend)?;
    app.run().await?;
    Ok(())
}
```

> **注：** 上面 `TuiFrontend::new` 需要 `editor: ContentId` 和 `status: ContentId` 两个参数。修正 main.rs 第 23–26 行为：

```rust
    let frontend: Box<dyn Frontend> = Box::new(TuiFrontend::new(
        Output::new(io::stdout()),
        layout::ids::ContentId(0),
        layout::ids::ContentId(1),
    ));
```

（删掉占位的 `app::Document_id_editor()` 调用，用 `ContentId(0)`/`ContentId(1)`，与 App::new 内部一致。）

- [ ] **Step 3: 恢复全量编译**

```bash
cargo build
```
Expected: 编译通过。若失败，按错误修 use 路径/签名（常见：`Document` 的 `line()` 返回 `Cow`——确认 `ropey::RopeSlice` 的 `.to_string()` 转 `Cow::Owned`；`Status::message()` 返回 `&StatusMessage`，`EditView::status()` 需 `StatusMessage`（Clone）——`.clone()`）。

- [ ] **Step 4: 全量测试**

```bash
cargo test
```
Expected: 全部 PASS。

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "refactor: app 重写 + main 接线，恢复全量编译（Document/View 分离架构落地）"
```

---

## Task 8: 清理 + 最终验证

**Files:** 全项目

- [ ] **Step 1: 检查无残留旧引用**

```bash
cargo build 2>&1 | Select-String -Pattern "warning"
```
确认无 `unresolved import`、无 `dead_code` 指向已删类型。

- [ ] **Step 2: 清理 `#[allow(dead_code)]`**

审查残留的 `#[allow(dead_code)]`：spec §6.5 列的 22 处预留中，本次重写后保留的（如 `Axis::Horizontal`、`Align::{Start,Center,End}`、`Layer::{Overlay,Modal,Debug}`、`WrapMode::Soft`、`Output::clear_screen/into_inner`）是**有意预留**，保留其 `#[allow(dead_code)]`；已删类型（`CorePatch`、`ContentKind` 多数变体、`ContentStore`、`EditorState` 等）的 allow 应随文件删除自动消失。

- [ ] **Step 3: clippy**

```bash
cargo clippy -- -D warnings
```
Expected: 无警告（若 clippy 报风格问题，修复）。

- [ ] **Step 4: 最终全量测试**

```bash
cargo test
```
Expected: 全绿。

- [ ] **Step 5: 手动冒烟（可选）**

```bash
cargo run -- README.md
```
确认能打开文件、编辑、光标移动、Ctrl+S 保存、Ctrl+Q 退出。

- [ ] **Step 6: Commit**

```bash
git add -A && git commit -m "refactor: 清理 + clippy 通过，架构重构完成"
```

---

## Self-Review 记录

**Spec coverage：**
- §2 目标 1（App 不感知 tui）：Task 7 `Frontend` trait + App 只持 `Box<dyn Frontend>` ✓
- §2 目标 2（layout 不感知 editor/不处理事件）：Task 5 layout 仅依赖 protocol，无 store 参数 ✓
- §2 目标 3（Content 不感知 Editor）：Task 6 `Content::render` 经 `EditView` 读文档 ✓
- §2 目标 4（保持 Scene/Space/Content）：Task 5 保留 Scene/Space/ContentId ✓
- §2 目标 5（Document/View 分离）：Task 5 Space 持 cursor/viewport，Task 3 core 退化 ✓
- §4 各层详解：Task 1–7 覆盖 ✓
- §7 待定项（wrap 归 Space、状态栏 Option、多 space 同步 future）：Task 5 `wrap_mode` 字段、状态栏 space 视图状态闲置（未用 Option，直接闲置——与 spec §7「倾向 Option」略有出入，但 v0.2 功能等价，implementer 可改 Option）✓
- §8 scope（v0.2 交付/不实现）：Task 7 集成测试覆盖 v0.2 功能 ✓
- §9 测试策略：各 Task 单测 + Task 7 集成测试 ✓
- §10 消失旧物：Task 1/3/5/6 删除 ✓

**已知偏离 spec（plan 内细化，已注明）：**
1. `StatusMessage`/ids 移入 protocol（spec §3.2 protocol 零依赖的必要推论）。
2. `RenderItem` 删 `space`/`parent`，保留 `content_id`。
3. `RenderCtx` 删 `focused: SpaceId`。
4. 引入 `Canvas` trait（object safety）。
5. 光标定位收归 `TuiFrontend::render`。
6. `build_editor_scene` 返回 `EditorScene`。
7. 状态栏 Space 视图状态直接闲置（未用 Option），与 spec §7 倾向略有出入。

**Type consistency：** `CursorPos::origin()`、`Viewport::origin()`、`ContentId(n)`、`SpaceId(n)`、`handle_key(buf, cur, status, key) -> EditAction`、`Frontend::render(contents, scene, focused_state, focused_content)` 跨任务签名一致。`Document::line()` 返回 `Cow<str>`（`RopeSlice.to_string().into()`）。
