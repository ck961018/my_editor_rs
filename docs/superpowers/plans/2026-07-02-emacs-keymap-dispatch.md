# Emacs 风格多层 keymap 捕获与 content 自治 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 把事件分发从 App 内联代码抽成独立 Dispatcher（捕获链 + 前缀键状态机），把 content 角色从 App 字段抽成 content 自描述（自持 keymap + 自描述 render），content 仅查表返回 Operation、executor 执行。

**Architecture:** `protocol`（纯前后端协议数据）← `core`（领域：operation/keymap/content trait/buffer/status_bar）← `frame`/`layout` ← `app`（dispatcher/executor/evloop）← `main`。捕获链：focused content keymap → parent 链 host content keymap → global keymap → focused content `default_binding`。content 仅查表返回 `Operation`，App 分流全局 Operation、executor 执行局部 Operation。`Cursors{primary, secondaries}` 预留多光标（v0.2 secondaries 空）。

**Tech Stack:** Rust 2021, ropey 1, crossterm 0.28, tokio 1 (multi_thread), taffy 0.5, tempfile 3 (dev)。二进制 crate（无 lib 目标，用 `cargo test <filter>` 而非 `cargo test --lib`）。

---

## 对 spec 的修正（实现时发现）

1. **`SpaceState` 保留 protocol**：`FrameItem.state: SpaceState` 且前端 painter 读 `state.viewport`（tui_frontend.rs:51-53），故 `SpaceState` 是前后端协议数据，留在 `protocol/edit_view.rs`，不移 core。`Cursors` 才是 core 内部多光标结构。
2. **executor 签名**：`execute(op, content: &mut dyn ContentHandler, space: &mut Space)`——直接操作 layout 的 `Space`（`cursors`+`viewport`），无需 core 的 `SpaceState`。
3. **`protocol/edit_view.rs` 保留 `SpaceState`**，删除 `EditView`/`ContentLookup`（合并进 core 的 `ContentHandler`/`ContentLookup`）。`WrapMode` 移 core。

## File Structure

| 文件 | 变更 | 责任 |
|---|---|---|
| `core/operation.rs` | 新建 | `Operation` 枚举 + `Direction` |
| `core/keymap.rs` | 新建 | `Keymap` 前缀树 + `KeyBinding` |
| `core/content.rs` | 新建 | `Cursors` + `ContentHandler` trait + `ContentLookup` trait + `WrapMode` + `RenderCtx` |
| `core/buffer.rs` | 改 | 加 `status`/`keymap` 字段、编辑原语、`impl ContentHandler`、`open_path`/`set_status` |
| `core/status_bar.rs` | 新建 | `StatusBar` content（持 `target_content_id`） |
| `core/edit.rs` | 删除 | 逻辑拆入 `buffer.rs`（原语）+ `operation.rs`（Direction） |
| `core/status.rs` | 删除 | 消息归 `Buffer.status` |
| `core/mod.rs` | 改 | 模块列表 |
| `protocol/edit_view.rs` | 改 | 删 `EditView`/`ContentLookup`/`WrapMode`，保留 `SpaceState` |
| `protocol/viewport.rs` | 改 | 加 `scroll_by(isize)` |
| `layout/space.rs` | 改 | `Space.cursor` → `Space.cursors: Cursors`；`WrapMode` 来源改 core |
| `layout/scene.rs` | 改 | `build_editor_scene` 返回 `(Scene, SpaceId)`；删 `EditorScene` |
| `layout/taffy_engine.rs` | 改 | collect 用 `cursors.primary` |
| `frame/mod.rs` | 改 | `build_frame` 调 `content.render(ctx)`，不再收角色 ID |
| `app/dispatcher.rs` | 新建 | `Dispatcher` 捕获链 + 前缀状态机 + `default_global_keymap` |
| `app/executor.rs` | 新建 | `execute(op, content, space)` |
| `app/content.rs` | 新建 | `ContentLookup for HashMap<ContentId, Box<dyn ContentHandler>>` |
| `app/document.rs` | 删除 | 删 `Document` |
| `app/mod.rs` | 改 | 去角色字段、调 dispatcher/executor、保存回环 `set_status` |
| `app/frontend.rs`, `tui/`, `main.rs` | 不变 | |

**[wip] 任务说明**：Task 4/5/6/9 改动跨模块类型契约，中间状态编译中断（trait 签名变更波及 buffer/status_bar/layout/frame/app）。Task 10 是编译关卡，全量 `cargo build` + `cargo test` 通过。各 [wip] 任务用 `cargo test <该任务模块>` 验证其自身测试，不要求全量编译。

---

## Task 1: `core/operation.rs`——Operation 枚举

**Files:**
- Create: `src/core/operation.rs`
- Modify: `src/core/mod.rs`

- [ ] **Step 1: 写失败测试**

Create `src/core/operation.rs`:
```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction { Left, Right, Up, Down }

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Operation {
    CursorMoveBy { chars: isize, lines: isize },
    CursorMoveLeftBy(usize),
    CursorMoveRightBy(usize),
    CursorMoveUpBy(usize),
    CursorMoveDownBy(usize),
    CursorMoveTo { char_idx: usize, line_idx: usize },
    CursorInsertText(String),
    CursorDelete(isize),
    ViewportScrollBy { lines: isize },
    Save,
    Quit,
    FocusNext,
    FocusPrev,
    CursorAddAtNextMatch(String),
    CursorRemoveSecondary,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direction_eq() {
        assert_eq!(Direction::Left, Direction::Left);
        assert_ne!(Direction::Left, Direction::Right);
    }

    #[test]
    fn operation_clone_eq() {
        let a = Operation::CursorInsertText("x".to_string());
        let b = a.clone();
        assert_eq!(a, b);
    }

    #[test]
    fn operation_variants_construct() {
        let _ = Operation::CursorMoveBy { chars: 1, lines: -1 };
        let _ = Operation::CursorMoveTo { char_idx: 0, line_idx: 0 };
        let _ = Operation::CursorDelete(-1);
        let _ = Operation::ViewportScrollBy { lines: 3 };
        let _ = Operation::Save;
        let _ = Operation::CursorAddAtNextMatch("foo".to_string());
        let _ = Operation::CursorRemoveSecondary;
    }
}
```

- [ ] **Step 2: 注册模块**

Modify `src/core/mod.rs`:
```rust
pub mod buffer;
pub mod content;
pub mod edit;
pub mod operation;
pub mod status;
```
（`content`/`operation` 新增；`edit`/`status` 此时仍存在，Task 4 删除。先加 `operation`，`content` 在 Task 3 加。此处只加 `pub mod operation;`）

实际改为：
```rust
pub mod buffer;
pub mod edit;
pub mod operation;
pub mod status;
```

- [ ] **Step 3: 运行测试验证通过**

Run: `cargo test core::operation`
Expected: PASS（3 tests）

- [ ] **Step 4: Commit**

```bash
git add src/core/operation.rs src/core/mod.rs
git commit -m "feat(core): Operation 枚举 + Direction"
```

---

## Task 2: `core/keymap.rs`——Keymap 前缀树

**Files:**
- Create: `src/core/keymap.rs`
- Modify: `src/core/mod.rs`

- [ ] **Step 1: 写失败测试**

Create `src/core/keymap.rs`:
```rust
use std::collections::HashMap;

use crate::core::operation::Operation;
use crate::protocol::key_event::KeyEvent;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeyBinding {
    Operation(Operation),
    Prefix(Keymap),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Keymap {
    bindings: HashMap<KeyEvent, KeyBinding>,
}

impl Keymap {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn lookup(&self, key: KeyEvent) -> Option<&KeyBinding> {
        self.bindings.get(&key)
    }
    pub fn bind(&mut self, key: KeyEvent, op: Operation) {
        self.bindings.insert(key, KeyBinding::Operation(op));
    }
    pub fn bind_prefix(&mut self, key: KeyEvent, sub: Keymap) {
        self.bindings.insert(key, KeyBinding::Prefix(sub));
    }
    pub fn unbind(&mut self, key: KeyEvent) {
        self.bindings.remove(&key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::key_event::ArrowKey;

    #[test]
    fn bind_and_lookup_operation() {
        let mut km = Keymap::new();
        km.bind(KeyEvent::Enter, Operation::CursorInsertText("\n".to_string()));
        let b = km.lookup(KeyEvent::Enter).unwrap();
        assert_eq!(b, &KeyBinding::Operation(Operation::CursorInsertText("\n".to_string())));
    }

    #[test]
    fn lookup_missing_is_none() {
        let km = Keymap::new();
        assert!(km.lookup(KeyEvent::Enter).is_none());
    }

    #[test]
    fn unbind_removes() {
        let mut km = Keymap::new();
        km.bind(KeyEvent::Backspace, Operation::CursorDelete(-1));
        km.unbind(KeyEvent::Backspace);
        assert!(km.lookup(KeyEvent::Backspace).is_none());
    }

    #[test]
    fn bind_prefix_nested() {
        let mut sub = Keymap::new();
        sub.bind(KeyEvent::Char(b's'), Operation::Save);
        let mut km = Keymap::new();
        km.bind_prefix(KeyEvent::Char(b'x'), sub);
        match km.lookup(KeyEvent::Char(b'x')).unwrap() {
            KeyBinding::Prefix(sub_km) => {
                assert!(matches!(
                    sub_km.lookup(KeyEvent::Char(b's')),
                    Some(KeyBinding::Operation(Operation::Save))
                ));
            }
            _ => panic!("expected Prefix"),
        }
    }

    #[test]
    fn keymap_clone_eq() {
        let mut km = Keymap::new();
        km.bind(KeyEvent::Arrow(ArrowKey::Left), Operation::CursorMoveLeftBy(1));
        let km2 = km.clone();
        assert_eq!(km, km2);
    }
}
```

- [ ] **Step 2: 注册模块**

Modify `src/core/mod.rs`:
```rust
pub mod buffer;
pub mod edit;
pub mod keymap;
pub mod operation;
pub mod status;
```

- [ ] **Step 3: 运行测试验证通过**

Run: `cargo test core::keymap`
Expected: PASS（5 tests）

- [ ] **Step 4: Commit**

```bash
git add src/core/keymap.rs src/core/mod.rs
git commit -m "feat(core): Keymap 前缀树 + KeyBinding"
```

---

## Task 3: `core/content.rs`——Cursors + ContentHandler trait

**Files:**
- Create: `src/core/content.rs`
- Modify: `src/core/mod.rs`

> trait 定义无 impl，可独立编译。`buffer_mut` 引用 `Buffer`（已存在），不需 Buffer 已 impl。

- [ ] **Step 1: 写实现 + 测试**

Create `src/core/content.rs`:
```rust
use std::borrow::Cow;

use crate::core::buffer::Buffer;
use crate::core::keymap::Keymap;
use crate::core::operation::Operation;
use crate::protocol::cursor::CursorPos;
use crate::protocol::edit_view::SpaceState;
use crate::protocol::frame::FrameContent;
use crate::protocol::ids::ContentId;
use crate::protocol::key_event::KeyEvent;
use crate::protocol::status::StatusMessage;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WrapMode {
    None,
    Soft,
}

/// 多光标容器：primary 权威，secondaries 预留（v0.2 始终空）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cursors {
    pub primary: CursorPos,
    pub secondaries: Vec<CursorPos>,
}

impl Cursors {
    pub fn single(c: CursorPos) -> Self {
        Self { primary: c, secondaries: Vec::new() }
    }
    pub fn all(&self) -> impl Iterator<Item = &CursorPos> {
        std::iter::once(&self.primary).chain(self.secondaries.iter())
    }
    pub fn all_mut(&mut self) -> impl Iterator<Item = &mut CursorPos> {
        std::iter::once(&mut self.primary).chain(self.secondaries.iter_mut())
    }
}

/// 渲染上下文：content render 时按需使用。
pub struct RenderCtx<'a> {
    pub lookup: &'a dyn ContentLookup,
    pub focused_content_id: ContentId,
    pub state: SpaceState,
    pub rect_height: i32,
}

pub trait ContentLookup {
    fn get(&self, id: ContentId) -> Option<&dyn ContentHandler>;
}

/// content 多态契约：自持 keymap + 自描述 render + 暴露 buffer。
/// 不含事件执行逻辑——仅查表返回 Operation，执行在 executor。
pub trait ContentHandler {
    fn line(&self, _idx: usize) -> Cow<str> { Cow::Borrowed("") }
    fn len_lines(&self) -> usize { 0 }
    fn file_name(&self) -> Option<&str> { None }
    fn modified(&self) -> bool { false }
    fn status(&self) -> StatusMessage { StatusMessage::None }

    fn keymap(&self) -> &Keymap;
    fn keymap_mut(&mut self) -> &mut Keymap;

    /// keymap 未命中时的兜底（仍只返回 Operation，不执行）。
    fn default_binding(&self, _key: KeyEvent) -> Option<Operation> { None }

    /// 暴露内部 Buffer 供 executor 操作（非 buffer content 返回 None）。
    fn buffer_mut(&mut self) -> Option<&mut Buffer> { None }

    fn render(&self, ctx: &RenderCtx) -> FrameContent;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursors_single_has_no_secondaries() {
        let c = Cursors::single(CursorPos::origin());
        assert_eq!(c.primary, CursorPos::origin());
        assert!(c.secondaries.is_empty());
    }

    #[test]
    fn cursors_all_iterates_primary_then_secondaries() {
        let c = Cursors {
            primary: CursorPos { char_index: 0, row: 0, col: 0 },
            secondaries: vec![CursorPos { char_index: 5, row: 1, col: 1 }],
        };
        let idxs: Vec<usize> = c.all().map(|c| c.char_index).collect();
        assert_eq!(idxs, vec![0, 5]);
    }

    #[test]
    fn cursors_all_mut_updates_all() {
        let mut c = Cursors {
            primary: CursorPos::origin(),
            secondaries: vec![CursorPos::origin()],
        };
        for cur in c.all_mut() {
            cur.char_index = 3;
        }
        assert_eq!(c.primary.char_index, 3);
        assert_eq!(c.secondaries[0].char_index, 3);
    }

    #[test]
    fn wrap_mode_eq() {
        assert_eq!(WrapMode::None, WrapMode::None);
        assert_ne!(WrapMode::None, WrapMode::Soft);
    }
}
```

- [ ] **Step 2: 注册模块**

Modify `src/core/mod.rs`:
```rust
pub mod buffer;
pub mod content;
pub mod edit;
pub mod keymap;
pub mod operation;
pub mod status;
```

- [ ] **Step 3: 运行测试验证通过**

Run: `cargo test core::content`
Expected: PASS（4 tests）

- [ ] **Step 4: Commit**

```bash
git add src/core/content.rs src/core/mod.rs
git commit -m "feat(core): Cursors + ContentHandler trait + ContentLookup"
```

---

## Task 4: `core/buffer.rs` 改造 + 删 `core/edit.rs` + 删 `core/status.rs` [wip]

**Files:**
- Modify: `src/core/buffer.rs`
- Delete: `src/core/edit.rs`, `src/core/status.rs`
- Modify: `src/core/mod.rs`

> [wip] 删除 `edit.rs`/`status.rs` 会让 `app/mod.rs`（用 `handle_key`/`open_path`）和 `app/document.rs`（用 `Status`）编译中断，Task 10 接线。本任务用 `cargo test core::buffer` 验证 buffer 自身测试。

- [ ] **Step 1: 改造 `src/core/buffer.rs`**

替换整个文件为：
```rust
use ropey::Rope;
use std::io;
use std::path::PathBuf;

use crate::core::content::{ContentHandler, ContentLookup, RenderCtx};
use crate::core::keymap::Keymap;
use crate::core::operation::Operation;
use crate::protocol::cursor::CursorPos;
use crate::protocol::edit_view::SpaceState;
use crate::protocol::frame::FrameContent;
use crate::protocol::key_event::{ArrowKey, KeyEvent};
use crate::protocol::status::StatusMessage;

pub struct Buffer {
    rope: Rope,
    path: Option<PathBuf>,
    modified: bool,
    status: StatusMessage,
    keymap: Keymap,
}

impl Buffer {
    pub fn new() -> Self {
        Self {
            rope: Rope::new(),
            path: None,
            modified: false,
            status: StatusMessage::None,
            keymap: default_buffer_keymap(),
        }
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
                self.status = if is_new { StatusMessage::NewFile } else { StatusMessage::None };
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
        self.status
    }

    pub fn insert_char(&mut self, char_idx: usize, ch: char) {
        self.rope.insert_char(char_idx, ch);
        self.modified = true;
    }

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

    pub fn modified(&self) -> bool {
        self.modified
    }

    // —— 编辑原语（从 core/edit.rs 搬来，改为 buffer 方法、多光标友好）——

    /// 根据 char_index 重算 row/col。
    pub fn recompute_cursor(&self, cur: &mut CursorPos) {
        let clamped = cur.char_index.min(self.rope.len_chars());
        cur.row = self.rope.char_to_line(clamped);
        let line_start = self.rope.line_to_char(cur.row);
        cur.col = clamped - line_start;
    }

    pub fn move_cursor_by(&self, cur: &mut CursorPos, chars: isize, lines: isize) {
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

    pub fn move_cursor_left(&self, cur: &mut CursorPos, n: usize) {
        cur.char_index = cur.char_index.saturating_sub(n);
        self.recompute_cursor(cur);
    }

    pub fn move_cursor_right(&self, cur: &mut CursorPos, n: usize) {
        cur.char_index = (cur.char_index + n).min(self.rope.len_chars());
        self.recompute_cursor(cur);
    }

    pub fn move_cursor_up(&self, cur: &mut CursorPos, n: usize) {
        let target_row = cur.row.saturating_sub(n);
        let line_len = line_content_len(&self.rope, target_row);
        let new_col = cur.col.min(line_len);
        cur.char_index = self.rope.line_to_char(target_row) + new_col;
        self.recompute_cursor(cur);
    }

    pub fn move_cursor_down(&self, cur: &mut CursorPos, n: usize) {
        let max_row = self.rope.len_lines().saturating_sub(1);
        let target_row = (cur.row + n).min(max_row);
        let line_len = line_content_len(&self.rope, target_row);
        let new_col = cur.col.min(line_len);
        cur.char_index = self.rope.line_to_char(target_row) + new_col;
        self.recompute_cursor(cur);
    }

    pub fn set_cursor(&self, cur: &mut CursorPos, char_idx: usize, _line_idx: usize) {
        cur.char_index = char_idx.min(self.rope.len_chars());
        self.recompute_cursor(cur);
    }

    /// 在每个光标处插入 text（按 char_index 降序避免索引偏移）。
    pub fn insert_at_cursors(&mut self, cursors: &mut crate::core::content::Cursors, text: &str) {
        let text_len = text.chars().count();
        let mut indices: Vec<usize> = cursors.all().map(|c| c.char_index).collect();
        indices.sort_unstable_by(|a, b| b.cmp(a));
        indices.dedup();
        for idx in indices {
            self.rope.insert(idx, text);
        }
        self.modified = true;
        for cur in cursors.all_mut() {
            cur.char_index += text_len;
            self.recompute_cursor(cur);
        }
    }

    /// 在每个光标处删除 n 字符（负向左、正向右）。
    pub fn delete_at_cursors(&mut self, cursors: &mut crate::core::content::Cursors, n: isize) {
        let len = self.rope.len_chars();
        let mut ranges: Vec<(usize, usize)> = cursors.all().map(|c| {
            if n < 0 {
                let start = c.char_index.saturating_sub((-n) as usize);
                (start, c.char_index)
            } else {
                let end = (c.char_index + n as usize).min(len);
                (c.char_index, end)
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
        for cur in cursors.all_mut() {
            if n < 0 {
                cur.char_index = cur.char_index.saturating_sub((-n) as usize);
            }
            self.recompute_cursor(cur);
        }
    }
}

impl Default for Buffer {
    fn default() -> Self { Self::new() }
}

impl ContentHandler for Buffer {
    fn line(&self, idx: usize) -> Cow<str> {
        Cow::Owned(self.slice().line(idx).to_string())
    }
    fn len_lines(&self) -> usize { Buffer::len_lines(self) }
    fn file_name(&self) -> Option<&str> {
        self.path().and_then(|p| p.file_name()).and_then(|n| n.to_str())
    }
    fn modified(&self) -> bool { self.modified }
    fn status(&self) -> StatusMessage { self.status }
    fn keymap(&self) -> &Keymap { &self.keymap }
    fn keymap_mut(&mut self) -> &mut Keymap { &mut self.keymap }
    fn default_binding(&self, key: KeyEvent) -> Option<Operation> {
        match key {
            KeyEvent::Char(ch) => Some(Operation::CursorInsertText((ch as char).to_string())),
            _ => None,
        }
    }
    fn buffer_mut(&mut self) -> Option<&mut Buffer> { Some(self) }
    fn render(&self, ctx: &RenderCtx) -> FrameContent {
        let total = self.len_lines();
        let mut lines = Vec::new();
        for row in 0..ctx.rect_height.max(0) as usize {
            let line_idx = ctx.state.viewport.top_row + row;
            if line_idx < total {
                lines.push(self.slice().line(line_idx).to_string().trim_end_matches('\n').to_string());
            } else {
                lines.push(String::new());
            }
        }
        FrameContent::Editor { lines }
    }
}

fn default_buffer_keymap() -> Keymap {
    let mut km = Keymap::new();
    km.bind(KeyEvent::Enter, Operation::CursorInsertText("\n".to_string()));
    km.bind(KeyEvent::Backspace, Operation::CursorDelete(-1));
    km.bind(KeyEvent::Arrow(ArrowKey::Left), Operation::CursorMoveLeftBy(1));
    km.bind(KeyEvent::Arrow(ArrowKey::Right), Operation::CursorMoveRightBy(1));
    km.bind(KeyEvent::Arrow(ArrowKey::Up), Operation::CursorMoveUpBy(1));
    km.bind(KeyEvent::Arrow(ArrowKey::Down), Operation::CursorMoveDownBy(1));
    km
}

/// 返回某行内容长度（不含末尾 '\n'）。
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
    use crate::core::content::Cursors;
    use tempfile::tempdir;

    fn cur(idx: usize) -> CursorPos {
        let mut c = CursorPos::origin();
        c.char_index = idx;
        Buffer::new().recompute_cursor(&mut c);
        c
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
    fn insert_at_cursors_single() {
        let mut b = Buffer::new();
        let mut c = Cursors::single(CursorPos::origin());
        b.insert_at_cursors(&mut c, "hi");
        assert_eq!(b.slice().to_string(), "hi");
        assert_eq!(c.primary.char_index, 2);
        assert_eq!((c.primary.row, c.primary.col), (0, 2));
    }

    #[test]
    fn delete_at_cursors_left() {
        let mut b = Buffer::new();
        let mut c = cur(3);
        b.delete_at_cursors(&mut c, -1);
        // 空 buffer，删除 noop
        assert_eq!(b.slice().to_string(), "");
        // 插入再删
        b.insert_char(0, 'a');
        b.insert_char(1, 'b');
        let mut c2 = cur(2);
        b.delete_at_cursors(&mut c2, -1);
        assert_eq!(b.slice().to_string(), "a");
    }

    #[test]
    fn move_cursor_right_clamps() {
        let mut b = Buffer::new();
        b.insert_char(0, 'a');
        b.insert_char(1, 'b');
        let mut c = CursorPos::origin();
        b.move_cursor_right(&mut c, 5);
        assert_eq!(c.char_index, 2);
    }

    #[test]
    fn move_cursor_up_down_clamps_col() {
        let mut b = Buffer::new();
        b.insert_at_cursors(&mut Cursors::single(CursorPos::origin()), "hello\nab\nworld");
        let mut c = cur(4); // row 0 col 4
        b.move_cursor_down(&mut c, 1);
        assert_eq!((c.row, c.col), (1, 2));
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

- [ ] **Step 2: 删除 `src/core/edit.rs` 和 `src/core/status.rs`**

```bash
git rm src/core/edit.rs src/core/status.rs
```

- [ ] **Step 3: 更新 `src/core/mod.rs`**

```rust
pub mod buffer;
pub mod content;
pub mod keymap;
pub mod operation;
```

- [ ] **Step 4: 运行 buffer 测试验证通过**

Run: `cargo test core::buffer`
Expected: PASS（13 tests）。注意：全量 `cargo build` 此时**会失败**（app 模块仍引用已删的 `handle_key`/`Status`/`Document`），属预期 [wip]，Task 10 接线。

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "[wip] refactor(core): Buffer impl ContentHandler + 删 edit.rs/status.rs"
```

---

## Task 5: `core/status_bar.rs`——StatusBar content [wip]

**Files:**
- Create: `src/core/status_bar.rs`
- Modify: `src/core/mod.rs`

> [wip] 依赖 Task 3 的 `ContentHandler` trait。可独立编译（trait 已定义）。

- [ ] **Step 1: 写实现 + 测试**

Create `src/core/status_bar.rs`:
```rust
use crate::core::content::{ContentHandler, ContentLookup, RenderCtx};
use crate::core::keymap::Keymap;
use crate::protocol::frame::FrameContent;
use crate::protocol::ids::ContentId;
use crate::protocol::status::StatusMessage;

/// 状态栏 content：观察 target_content_id 指向的 content，render 时主动查其
/// file_name/modified/status。自身不持显示数据，只持指针 + 空 keymap。
pub struct StatusBar {
    target_content_id: ContentId,
    keymap: Keymap,
}

impl StatusBar {
    pub fn new(target_content_id: ContentId) -> Self {
        Self { target_content_id, keymap: Keymap::new() }
    }
    pub fn target_content_id(&self) -> ContentId {
        self.target_content_id
    }
}

impl ContentHandler for StatusBar {
    fn keymap(&self) -> &Keymap { &self.keymap }
    fn keymap_mut(&mut self) -> &mut Keymap { &mut self.keymap }
    fn render(&self, ctx: &RenderCtx) -> FrameContent {
        let target = ctx.lookup.get(self.target_content_id);
        FrameContent::StatusBar {
            file_name: target.and_then(|c| c.file_name().map(|s| s.to_string())),
            modified: target.map(|c| c.modified()).unwrap_or(false),
            message: target.map(|c| c.status()).unwrap_or(StatusMessage::None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::buffer::Buffer;
    use crate::protocol::cursor::CursorPos;
    use crate::protocol::edit_view::SpaceState;
    use crate::protocol::ids::ContentId;
    use crate::protocol::viewport::Viewport;
    use std::collections::HashMap;

    fn ctx_with(buf: &Buffer, target: ContentId) -> RenderCtx<'_> {
        let mut map: HashMap<ContentId, Box<dyn ContentHandler>> = HashMap::new();
        // 借用冲突：用空 lookup 测 target 缺失；查到的情况用单独 lookup 结构
        struct Empty;
        impl ContentLookup for Empty {
            fn get(&self, _id: ContentId) -> Option<&dyn ContentHandler> { None }
        }
        RenderCtx {
            lookup: &Empty,
            focused_content_id: target,
            state: SpaceState { viewport: Viewport::origin(), cursor: CursorPos::origin() },
            rect_height: 1,
        }
    }

    #[test]
    fn render_target_missing_defaults() {
        let sb = StatusBar::new(ContentId(0));
        let buf = Buffer::new();
        let ctx = ctx_with(&buf, ContentId(0));
        match sb.render(&ctx) {
            FrameContent::StatusBar { file_name, modified, message } => {
                assert!(file_name.is_none());
                assert!(!modified);
                assert_eq!(message, StatusMessage::None);
            }
            _ => panic!("expected StatusBar"),
        }
    }

    #[test]
    fn target_content_id_stored() {
        let sb = StatusBar::new(ContentId(7));
        assert_eq!(sb.target_content_id(), ContentId(7));
    }
}
```

> 测试中 `ctx_with` 用空 lookup 验证 target 缺失分支。target 命中分支在 Task 10 headless 集成测试覆盖。

- [ ] **Step 2: 注册模块**

Modify `src/core/mod.rs`:
```rust
pub mod buffer;
pub mod content;
pub mod keymap;
pub mod operation;
pub mod status_bar;
```

- [ ] **Step 3: 运行测试验证通过**

Run: `cargo test core::status_bar`
Expected: PASS（2 tests）

- [ ] **Step 4: Commit**

```bash
git add src/core/status_bar.rs src/core/mod.rs
git commit -m "[wip] feat(core): StatusBar content（观察者，主动查 target）"
```

---

## Task 6: layout 适配 Cursors [wip]

**Files:**
- Modify: `src/layout/space.rs`, `src/layout/scene.rs`, `src/layout/taffy_engine.rs`
- Modify: `src/protocol/edit_view.rs`（删 EditView/ContentLookup/WrapMode，保留 SpaceState）

> [wip] `Space.cursor` → `cursors: Cursors` 波及 frame/app，Task 9/10 接线。本任务用 `cargo test layout` 验证。

- [ ] **Step 1: 改 `src/protocol/edit_view.rs`——只留 SpaceState**

替换整个文件：
```rust
use crate::protocol::cursor::CursorPos;
use crate::protocol::viewport::Viewport;

/// 单个 Space 的视图状态快照（透传给前端 painter）。
/// 前端读 viewport 算光标屏坐标，故为前后端协议数据，留 protocol。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpaceState {
    pub viewport: Viewport,
    pub cursor: CursorPos,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn space_state_constructs() {
        let s = SpaceState { viewport: Viewport::origin(), cursor: CursorPos::origin() };
        assert_eq!(s.cursor.char_index, 0);
        assert_eq!(s.viewport.top_row, 0);
    }
}
```

- [ ] **Step 2: 改 `src/layout/space.rs`——cursor → cursors**

修改 imports 与 Space 字段：
```rust
use crate::core::content::{Cursors, WrapMode};
use crate::protocol::ids::{ContentId, SpaceId};
use crate::protocol::viewport::Viewport;

pub struct Space {
    #[allow(dead_code)]
    pub id: SpaceId,
    pub kind: SpaceKind,
    pub sizing: Sizing,
    pub layer: Layer,
    pub viewport: Viewport,
    pub cursors: Cursors,
    #[allow(dead_code)]
    pub wrap_mode: WrapMode,
}
```
（其余 SpaceKind/Arrangement/Axis/Align/Sizing/Layer 不变）

- [ ] **Step 3: 改 `src/layout/scene.rs`——cursors 初始化 + build_editor_scene 返回 (Scene, SpaceId)**

修改 `alloc` 内 Space 构造：
```rust
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
        cursors: Cursors::single(CursorPos::origin()),
        wrap_mode: WrapMode::None,
    },
};
```

修改 `build_editor_scene` 与删除 `EditorScene`：
```rust
/// 标准布局：root Vertical [editor Grow(1), status Fixed(1)]。
/// 返回 (Scene, editor_space_id)。
pub fn build_editor_scene(width: i32, height: i32, editor: ContentId, status: ContentId) -> (Scene, SpaceId) {
    let mut b = SceneBuilder::new();
    let ed = b.host(editor).grow(&mut b, 1);
    let st = b.host(status).fixed(&mut b, 1);
    let root = b.container(
        Arrangement::Flex { direction: Axis::Vertical, gap: 0, align: Align::Stretch },
        vec![ed, st],
    );
    let scene = b.finish(root.id, Size { width, height }).expect("valid editor scene");
    (scene, ed)
}
```
删除 `pub struct EditorScene { ... }`。

修改 scene.rs 测试：
```rust
#[test]
fn build_editor_scene_has_two_hosts() {
    let (scene, editor_space) = build_editor_scene(80, 24, ContentId(0), ContentId(1));
    let root = scene.node(scene.root);
    match &root.space.kind {
        SpaceKind::Container { children, .. } => assert_eq!(children.len(), 2),
        _ => panic!("root must be container"),
    }
    assert_eq!(editor_space, SpaceId(0));
}
```

- [ ] **Step 4: 改 `src/layout/taffy_engine.rs`——collect 用 cursors.primary**

修改 collect 内 state 构造：
```rust
state: SpaceState { viewport: node.space.viewport, cursor: node.space.cursors.primary },
```

修改 taffy_engine 测试（`es.scene` → 解构）：
```rust
#[test]
fn editor_grows_and_status_fixed() {
    let (scene, _) = build_editor_scene(80, 24, ContentId(0), ContentId(1));
    let mut engine = TaffyEngine::new();
    let resolved = engine.layout(&scene);
    assert_eq!(item_for(&resolved, ContentId(0)).rect, Rect { x: 0, y: 0, width: 80, height: 23 });
    assert_eq!(item_for(&resolved, ContentId(1)).rect, Rect { x: 0, y: 23, width: 80, height: 1 });
}

#[test]
fn items_carry_state_and_dfs_order() {
    let (scene, _) = build_editor_scene(80, 24, ContentId(0), ContentId(1));
    let mut engine = TaffyEngine::new();
    let resolved = engine.layout(&scene);
    assert_eq!(resolved.items.len(), 2);
    assert_eq!(resolved.items[0].content_id, ContentId(0));
    assert_eq!(resolved.items[1].content_id, ContentId(1));
    assert_eq!(resolved.items[0].state.cursor, crate::protocol::cursor::CursorPos::origin());
}

#[test]
fn resize_changes_geometry() {
    let (mut scene, _) = build_editor_scene(80, 24, ContentId(0), ContentId(1));
    scene.resize(100, 40);
    let mut engine = TaffyEngine::new();
    let resolved = engine.layout(&scene);
    assert_eq!(item_for(&resolved, ContentId(0)).rect.height, 39);
    assert_eq!(item_for(&resolved, ContentId(0)).rect.width, 100);
}
```

- [ ] **Step 5: 运行 layout 测试验证通过**

Run: `cargo test layout`
Expected: PASS。全量 `cargo build` 仍失败（frame/app 未适配），属 [wip]。

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "[wip] refactor(layout): Space.cursors + build_editor_scene 返回 (Scene, SpaceId)"
```

---

## Task 7: `app/dispatcher.rs`——捕获链 + 前缀状态机

**Files:**
- Create: `src/app/dispatcher.rs`
- Modify: `src/app/mod.rs`（加 `mod dispatcher;`）

> 可独立单元测试（构造 Scene + HashMap contents）。但 app/mod.rs 此时仍 [wip]，`cargo test app::dispatcher` 跑该模块测试。

- [ ] **Step 1: 写实现 + 测试**

Create `src/app/dispatcher.rs`:
```rust
use crate::core::content::ContentLookup;
use crate::core::keymap::{KeyBinding, Keymap};
use crate::core::operation::Operation;
use crate::layout::scene::Scene;
use crate::layout::space::SpaceKind;
use crate::protocol::ids::{ContentId, SpaceId};
use crate::protocol::key_event::{CtrlKey, KeyEvent};

pub struct Dispatcher {
    global_keymap: Keymap,
    pending: Option<Keymap>,
}

impl Dispatcher {
    pub fn new(global_keymap: Keymap) -> Self {
        Self { global_keymap, pending: None }
    }

    pub fn is_pending(&self) -> bool {
        self.pending.is_some()
    }

    pub fn dispatch(
        &mut self,
        key: KeyEvent,
        focused: SpaceId,
        scene: &Scene,
        contents: &dyn ContentLookup,
    ) -> Option<Operation> {
        // 1) 前缀待续：在 pending 子表查
        if let Some(sub) = self.pending.take() {
            return match lookup_in(&sub, key) {
                LookupResult::Hit(op) => Some(op),
                LookupResult::Prefix(sub2) => {
                    self.pending = Some(sub2.clone());
                    None
                }
                LookupResult::Miss => None,
            };
        }
        // 2) Idle：沿捕获链查
        for km in self.capture_chain(focused, scene, contents) {
            match lookup_in(km, key) {
                LookupResult::Hit(op) => return Some(op),
                LookupResult::Prefix(sub) => {
                    self.pending = Some(sub.clone());
                    return None;
                }
                LookupResult::Miss => continue,
            }
        }
        // 3) 全链未命中：focused content 的 default_binding 兜底
        focused_content_id(scene, focused)
            .and_then(|cid| contents.get(cid))
            .and_then(|c| c.default_binding(key))
    }

    fn capture_chain<'a>(
        &'a self,
        focused: SpaceId,
        scene: &'a Scene,
        contents: &'a dyn ContentLookup,
    ) -> Vec<&'a Keymap> {
        let mut chain = Vec::new();
        let mut cur = Some(focused);
        while let Some(sid) = cur {
            let node = scene.node(sid);
            if let SpaceKind::Host { content } = &node.space.kind {
                if let Some(c) = contents.get(*content) {
                    chain.push(c.keymap());
                }
            }
            cur = node.parent;
        }
        chain.push(&self.global_keymap);
        chain
    }
}

enum LookupResult<'a> {
    Hit(Operation),
    Prefix(&'a Keymap),
    Miss,
}

fn lookup_in(keymap: &Keymap, key: KeyEvent) -> LookupResult {
    match keymap.lookup(key) {
        Some(KeyBinding::Operation(op)) => LookupResult::Hit(op.clone()),
        Some(KeyBinding::Prefix(sub)) => LookupResult::Prefix(sub),
        None => LookupResult::Miss,
    }
}

fn focused_content_id(scene: &Scene, focused: SpaceId) -> Option<ContentId> {
    let node = scene.node(focused);
    match &node.space.kind {
        SpaceKind::Host { content } => Some(*content),
        _ => None,
    }
}

pub fn default_global_keymap() -> Keymap {
    let mut km = Keymap::new();
    km.bind(KeyEvent::Ctrl(CtrlKey::Q), Operation::Quit);
    km.bind(KeyEvent::Ctrl(CtrlKey::S), Operation::Save);
    km
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::buffer::Buffer;
    use crate::core::content::ContentHandler;
    use crate::core::operation::Operation;
    use crate::core::status_bar::StatusBar;
    use crate::layout::scene::build_editor_scene;
    use crate::protocol::ids::ContentId;
    use crate::protocol::key_event::ArrowKey;
    use std::collections::HashMap;

    fn fixture() -> (Dispatcher, crate::layout::scene::Scene, SpaceId, HashMap<ContentId, Box<dyn ContentHandler>>) {
        let editor = ContentId(0);
        let status = ContentId(1);
        let (scene, ed_space) = build_editor_scene(40, 5, editor, status);
        let mut contents: HashMap<ContentId, Box<dyn ContentHandler>> = HashMap::new();
        contents.insert(editor, Box::new(Buffer::new()));
        contents.insert(status, Box::new(StatusBar::new(editor)));
        let d = Dispatcher::new(default_global_keymap());
        (d, scene, ed_space, contents)
    }

    #[test]
    fn char_falls_through_to_default_binding() {
        let (mut d, scene, focused, contents) = fixture();
        let op = d.dispatch(KeyEvent::Char(b'a'), focused, &scene, &contents).unwrap();
        assert_eq!(op, Operation::CursorInsertText("a".to_string()));
    }

    #[test]
    fn buffer_keymap_enter_inserts_newline() {
        let (mut d, scene, focused, contents) = fixture();
        let op = d.dispatch(KeyEvent::Enter, focused, &scene, &contents).unwrap();
        assert_eq!(op, Operation::CursorInsertText("\n".to_string()));
    }

    #[test]
    fn buffer_keymap_arrow_left() {
        let (mut d, scene, focused, contents) = fixture();
        let op = d.dispatch(KeyEvent::Arrow(ArrowKey::Left), focused, &scene, &contents).unwrap();
        assert_eq!(op, Operation::CursorMoveLeftBy(1));
    }

    #[test]
    fn global_quit_when_content_no_bind() {
        let (mut d, scene, focused, contents) = fixture();
        let op = d.dispatch(KeyEvent::Ctrl(CtrlKey::Q), focused, &scene, &contents).unwrap();
        assert_eq!(op, Operation::Quit);
    }

    #[test]
    fn global_save_when_content_no_bind() {
        let (mut d, scene, focused, contents) = fixture();
        let op = d.dispatch(KeyEvent::Ctrl(CtrlKey::S), focused, &scene, &contents).unwrap();
        assert_eq!(op, Operation::Save);
    }

    #[test]
    fn content_overrides_global() {
        let (mut d, scene, focused, mut contents) = fixture();
        // 让 Buffer keymap 绑 Ctrl+Q 到 InsertText（覆盖 global Quit）
        contents.get_mut(&ContentId(0)).unwrap().keymap_mut()
            .bind(KeyEvent::Ctrl(CtrlKey::Q), Operation::CursorInsertText("q".to_string()));
        let op = d.dispatch(KeyEvent::Ctrl(CtrlKey::Q), focused, &scene, &contents).unwrap();
        assert_eq!(op, Operation::CursorInsertText("q".to_string()));
    }

    #[test]
    fn unbound_key_returns_none() {
        let (mut d, scene, focused, contents) = fixture();
        // Escape 无绑定、default_binding 返回 None
        assert!(d.dispatch(KeyEvent::Escape, focused, &scene, &contents).is_none());
    }

    #[test]
    fn prefix_key_waits_then_completes() {
        let (mut d, scene, focused, mut contents) = fixture();
        // 绑 'x' 前缀，子表 's' → Save
        let mut sub = Keymap::new();
        sub.bind(KeyEvent::Char(b's'), Operation::Save);
        contents.get_mut(&ContentId(0)).unwrap().keymap_mut()
            .bind_prefix(KeyEvent::Char(b'x'), sub);
        // 第一次：进入 pending，返回 None
        assert!(d.dispatch(KeyEvent::Char(b'x'), focused, &scene, &contents).is_none());
        assert!(d.is_pending());
        // 第二次：命中 Save
        let op = d.dispatch(KeyEvent::Char(b's'), focused, &scene, &contents).unwrap();
        assert_eq!(op, Operation::Save);
        assert!(!d.is_pending());
    }

    #[test]
    fn prefix_interrupt_resets() {
        let (mut d, scene, focused, mut contents) = fixture();
        let mut sub = Keymap::new();
        sub.bind(KeyEvent::Char(b's'), Operation::Save);
        contents.get_mut(&ContentId(0)).unwrap().keymap_mut()
            .bind_prefix(KeyEvent::Char(b'x'), sub);
        d.dispatch(KeyEvent::Char(b'x'), focused, &scene, &contents);
        assert!(d.is_pending());
        // 前缀中断键（'z' 不在 sub 表）：返回 None，重置 Idle
        assert!(d.dispatch(KeyEvent::Char(b'z'), focused, &scene, &contents).is_none());
        assert!(!d.is_pending());
    }

    #[test]
    fn nested_prefix() {
        let (mut d, scene, focused, mut contents) = fixture();
        let mut inner = Keymap::new();
        inner.bind(KeyEvent::Char(b's'), Operation::Save);
        let mut outer = Keymap::new();
        outer.bind_prefix(KeyEvent::Char(b'c'), inner);
        contents.get_mut(&ContentId(0)).unwrap().keymap_mut()
            .bind_prefix(KeyEvent::Char(b'x'), outer);
        assert!(d.dispatch(KeyEvent::Char(b'x'), focused, &scene, &contents).is_none());
        assert!(d.dispatch(KeyEvent::Char(b'c'), focused, &scene, &contents).is_none());
        let op = d.dispatch(KeyEvent::Char(b's'), focused, &scene, &contents).unwrap();
        assert_eq!(op, Operation::Save);
    }
}
```

- [ ] **Step 2: 注册模块**

在 `src/app/mod.rs` 顶部 `mod document;` 旁加（Task 4 后 `mod document` 仍存在但 document.rs 已删——Task 9 处理；此处先加 dispatcher）：
```rust
mod dispatcher;
mod document;
mod frontend;
```
> 若 `mod document;` 编译失败（document.rs 已删），临时注释掉 `mod document;` 及 `pub use document::Document;`，Task 9/10 接线。实际：Task 4 已删 document.rs？不——Task 4 删 edit.rs/status.rs，document.rs 在 Task 9 删。所以 document.rs 仍存在（但引用已删的 Status/EditView，编译断）。为让 dispatcher 测试独立跑，临时在 app/mod.rs 注释 document 相关行。

实际操作：编辑 `src/app/mod.rs`，注释 `mod document;` 和 `pub use document::Document;`，加 `mod dispatcher;`。这会让 app/mod.rs 其他用 Document 的地方编译断——但 `cargo test app::dispatcher` 只编译 dispatcher 模块及其依赖？Rust 编译整个 crate，不能只编译单模块。所以 [wip] 期间全量编译断，`cargo test app::dispatcher` 也跑不了。

**调整策略**：Task 7 不单独跑测试，标注 [wip]，Task 10 一起验证。或 Task 7 推迟到 Task 9 后。

为简化，Task 7 标注 [wip]：写代码 + 测试，不要求独立编译通过，Task 10 编译关卡统一验证。

- [ ] **Step 3: Commit（[wip]，不跑测试）**

```bash
git add src/app/dispatcher.rs src/app/mod.rs
git commit -m "[wip] feat(app): Dispatcher 捕获链 + 前缀状态机"
```

---

## Task 8: `app/executor.rs` + `Viewport::scroll_by` [wip]

**Files:**
- Create: `src/app/executor.rs`
- Modify: `src/protocol/viewport.rs`（加 `scroll_by`）
- Modify: `src/app/mod.rs`（加 `mod executor;`）

> [wip]，Task 10 统一验证。

- [ ] **Step 1: 加 `Viewport::scroll_by`**

在 `src/protocol/viewport.rs` 的 `impl Viewport` 内加：
```rust
    /// 按 lines 滚动（负向上、正向下）。v0.2 不绑键，预留 executor 路径。
    pub fn scroll_by(&mut self, lines: isize) {
        if lines >= 0 {
            self.top_row = self.top_row.saturating_add(lines as usize);
        } else {
            self.top_row = self.top_row.saturating_sub((-lines) as usize);
        }
    }
```

加测试：
```rust
    #[test]
    fn scroll_by_positive_down() {
        let mut v = Viewport::origin();
        v.scroll_by(3);
        assert_eq!(v.top_row, 3);
    }
    #[test]
    fn scroll_by_negative_up() {
        let mut v = Viewport { top_row: 10, left_col: 0 };
        v.scroll_by(-4);
        assert_eq!(v.top_row, 6);
    }
```

- [ ] **Step 2: 写 `src/app/executor.rs`**

```rust
use crate::core::content::ContentHandler;
use crate::core::operation::Operation;
use crate::layout::space::Space;

/// 执行局部 Operation（光标/文本/视口）。全局/多光标变体不进此处（App 分流）。
pub fn execute(op: Operation, content: &mut dyn ContentHandler, space: &mut Space) {
    let Some(buf) = content.buffer_mut() else { return; };
    match op {
        Operation::CursorMoveBy { chars, lines } => {
            for c in space.cursors.all_mut() { buf.move_cursor_by(c, chars, lines); }
        }
        Operation::CursorMoveLeftBy(n) => {
            for c in space.cursors.all_mut() { buf.move_cursor_left(c, n); }
        }
        Operation::CursorMoveRightBy(n) => {
            for c in space.cursors.all_mut() { buf.move_cursor_right(c, n); }
        }
        Operation::CursorMoveUpBy(n) => {
            for c in space.cursors.all_mut() { buf.move_cursor_up(c, n); }
        }
        Operation::CursorMoveDownBy(n) => {
            for c in space.cursors.all_mut() { buf.move_cursor_down(c, n); }
        }
        Operation::CursorMoveTo { char_idx, line_idx } => {
            buf.set_cursor(&mut space.cursors.primary, char_idx, line_idx);
            space.cursors.secondaries.clear();
        }
        Operation::CursorInsertText(text) => {
            buf.insert_at_cursors(&mut space.cursors, &text);
        }
        Operation::CursorDelete(n) => {
            buf.delete_at_cursors(&mut space.cursors, n);
        }
        Operation::ViewportScrollBy { lines } => {
            space.viewport.scroll_by(lines);
        }
        // 全局/多光标变体不进 executor
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::buffer::Buffer;
    use crate::core::content::Cursors;
    use crate::core::operation::Operation;
    use crate::layout::space::{Layer, Sizing, Space, SpaceKind};
    use crate::protocol::cursor::CursorPos;
    use crate::protocol::ids::{ContentId, SpaceId};
    use crate::protocol::viewport::Viewport;

    fn space_with(cursors: Cursors) -> Space {
        Space {
            id: SpaceId(0),
            kind: SpaceKind::Host { content: ContentId(0) },
            sizing: Sizing::Grow(1),
            layer: Layer::Base,
            viewport: Viewport::origin(),
            cursors,
            wrap_mode: crate::core::content::WrapMode::None,
        }
    }

    #[test]
    fn insert_text_changes_buffer_and_cursor() {
        let mut buf = Buffer::new();
        let mut sp = space_with(Cursors::single(CursorPos::origin()));
        execute(Operation::CursorInsertText("hi".to_string()), &mut buf, &mut sp);
        assert_eq!(buf.slice().to_string(), "hi");
        assert_eq!(sp.cursors.primary.char_index, 2);
    }

    #[test]
    fn delete_left_removes_char() {
        let mut buf = Buffer::new();
        buf.insert_char(0, 'a');
        buf.insert_char(1, 'b');
        let mut c = CursorPos::origin();
        c.char_index = 2;
        buf.recompute_cursor(&mut c);
        let mut sp = space_with(Cursors::single(c));
        execute(Operation::CursorDelete(-1), &mut buf, &mut sp);
        assert_eq!(buf.slice().to_string(), "a");
        assert_eq!(sp.cursors.primary.char_index, 1);
    }

    #[test]
    fn move_right_advances_cursor() {
        let mut buf = Buffer::new();
        buf.insert_char(0, 'a');
        let mut sp = space_with(Cursors::single(CursorPos::origin()));
        execute(Operation::CursorMoveRightBy(1), &mut buf, &mut sp);
        assert_eq!(sp.cursors.primary.char_index, 1);
    }

    #[test]
    fn move_to_clears_secondaries() {
        let mut buf = Buffer::new();
        buf.insert_char(0, 'a');
        let mut sp = space_with(Cursors {
            primary: CursorPos::origin(),
            secondaries: vec![CursorPos::origin()],
        });
        execute(Operation::CursorMoveTo { char_idx: 0, line_idx: 0 }, &mut buf, &mut sp);
        assert!(sp.cursors.secondaries.is_empty());
    }

    #[test]
    fn viewport_scroll_changes_top_row() {
        let mut buf = Buffer::new();
        let mut sp = space_with(Cursors::single(CursorPos::origin()));
        execute(Operation::ViewportScrollBy { lines: 5 }, &mut buf, &mut sp);
        assert_eq!(sp.viewport.top_row, 5);
    }
}
```

- [ ] **Step 3: 注册模块**

在 `src/app/mod.rs` 加 `mod executor;`（与 `mod dispatcher;` 并列）。

- [ ] **Step 4: Commit（[wip]）**

```bash
git add src/app/executor.rs src/protocol/viewport.rs src/app/mod.rs
git commit -m "[wip] feat(app): executor + Viewport::scroll_by"
```

---

## Task 9: `frame/mod.rs` 改造 + `app/content.rs` + 删 `app/document.rs` [wip]

**Files:**
- Modify: `src/frame/mod.rs`
- Create: `src/app/content.rs`
- Delete: `src/app/document.rs`
- Modify: `src/app/mod.rs`

> [wip]，Task 10 接线 app/mod.rs。

- [ ] **Step 1: 改 `src/frame/mod.rs`——build_frame 调 content.render**

替换整个文件：
```rust
//! 中性帧构建：ResolvedScene + ContentLookup → Frame。调 content.render 自描述渲染。
//! 不依赖 tui/crossterm。依赖 core（ContentHandler/ContentLookup/RenderCtx）。

use crate::core::content::{ContentLookup, RenderCtx};
use crate::layout::resolved::ResolvedScene;
use crate::protocol::cursor::CursorPos;
use crate::protocol::frame::{Frame, FrameContent, FrameItem, Rect as FrameRect};
use crate::protocol::ids::ContentId;

/// 构建 neutral Frame。每个 item 的 FrameContent 由 content.render(ctx) 产出。
/// 不再收 editor_content/status_content 角色 ID——content 自描述。
pub fn build_frame(
    scene: &ResolvedScene,
    contents: &dyn ContentLookup,
    focused_content_id: ContentId,
    focused_cursor: Option<CursorPos>,
) -> Frame {
    let mut items = Vec::new();
    for ri in &scene.items {
        let content = match contents.get(ri.content_id) {
            Some(c) => c,
            None => continue,
        };
        let ctx = RenderCtx {
            lookup: contents,
            focused_content_id,
            state: ri.state,
            rect_height: ri.rect.height,
        };
        let frame_content = content.render(&ctx);
        items.push(FrameItem {
            content_id: ri.content_id,
            rect: FrameRect {
                x: ri.rect.x,
                y: ri.rect.y,
                width: ri.rect.width,
                height: ri.rect.height,
            },
            state: ri.state,
            content: frame_content,
        });
    }
    Frame { items, focused_content: focused_content_id, focused_cursor }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::buffer::Buffer;
    use crate::core::content::ContentHandler;
    use crate::core::status_bar::StatusBar;
    use crate::layout::scene::build_editor_scene;
    use crate::layout::taffy_engine::TaffyEngine;
    use crate::protocol::cursor::CursorPos;
    use crate::protocol::ids::ContentId;
    use crate::protocol::status::StatusMessage;
    use std::collections::HashMap;

    #[test]
    fn build_frame_renders_buffer_and_statusbar() {
        let editor = ContentId(0);
        let status = ContentId(1);
        let (scene, _) = build_editor_scene(40, 5, editor, status);
        let mut engine = TaffyEngine::new();
        let resolved = engine.layout(&scene);

        let mut buf = Buffer::new();
        buf.insert_char(0, 'h');
        buf.insert_char(1, 'i');
        let mut contents: HashMap<ContentId, Box<dyn ContentHandler>> = HashMap::new();
        contents.insert(editor, Box::new(buf));
        contents.insert(status, Box::new(StatusBar::new(editor)));

        let frame = build_frame(&resolved, &contents as &dyn ContentLookup, editor, Some(CursorPos::origin()));

        let editor_item = frame.items.iter().find(|i| i.content_id == editor).unwrap();
        match &editor_item.content {
            FrameContent::Editor { lines } => {
                assert_eq!(lines.len(), 4); // height 5 - status 1
                assert_eq!(lines[0], "hi");
            }
            _ => panic!("expected Editor"),
        }
        let status_item = frame.items.iter().find(|i| i.content_id == status).unwrap();
        match &status_item.content {
            FrameContent::StatusBar { file_name, modified, message } => {
                // Buffer 无 path → file_name None
                assert!(file_name.is_none());
                assert!(modified);
                assert_eq!(*message, StatusMessage::None);
            }
            _ => panic!("expected StatusBar"),
        }
        assert_eq!(frame.focused_content, editor);
    }
}
```

- [ ] **Step 2: 创建 `src/app/content.rs`**

```rust
//! ContentLookup for contents map。替代旧 document.rs。

use std::collections::HashMap;

use crate::core::content::{ContentHandler, ContentLookup};
use crate::protocol::ids::ContentId;

impl ContentLookup for HashMap<ContentId, Box<dyn ContentHandler>> {
    fn get(&self, id: ContentId) -> Option<&dyn ContentHandler> {
        HashMap::get(self, &id).map(|c| c.as_ref())
    }
}
```

- [ ] **Step 3: 删 `src/app/document.rs`**

```bash
git rm src/app/document.rs
```

- [ ] **Step 4: 改 `src/app/mod.rs` 模块声明**

把 `mod document;` + `pub use document::Document;` 替换为：
```rust
mod content;
mod dispatcher;
mod executor;
mod frontend;
```
（`pub use frontend::{Frontend, FrontendImpl, HeadlessFrontend};` 保留）

> app/mod.rs 其余代码（App 结构、handle_event 等）仍引用旧 Document/editor_content——Task 10 重写。

- [ ] **Step 5: Commit（[wip]）**

```bash
git add -A
git commit -m "[wip] refactor(frame+app): build_frame 调 content.render + app/content.rs + 删 document.rs"
```

---

## Task 10: `app/mod.rs` 重写 + headless 集成 + 清理 + 编译关卡

**Files:**
- Modify: `src/app/mod.rs`（重写 App）
- Modify: `src/app/frontend.rs`（HeadlessFrontend 测试 import 调整，若需要）
- Verify: `cargo build` + `cargo clippy -- -D warnings` + `cargo test`

> **编译关卡**：本任务后全量编译 + 测试通过。

- [ ] **Step 1: 重写 `src/app/mod.rs`**

替换整个文件：
```rust
//! App：tokio::select! 多路复用 evloop。不感知 tui/gui（只依赖 Frontend trait + Frame）。
//! 事件分发委托 Dispatcher（捕获链 + 前缀状态机），Operation 执行委托 executor。
//! 不持 editor_content/status_content 角色 ID——从 scene/focused 推导。

mod content;
mod dispatcher;
mod executor;
mod frontend;

#[allow(unused_imports)]
pub use frontend::{Frontend, FrontendImpl, HeadlessFrontend};

use std::collections::HashMap;
use std::io;

use tokio::sync::mpsc;

use crate::app::dispatcher::{default_global_keymap, Dispatcher};
use crate::app::executor;
use crate::core::buffer::Buffer;
use crate::core::content::{ContentHandler, ContentLookup};
use crate::core::operation::Operation;
use crate::core::status_bar::StatusBar;
use crate::frame::build_frame;
use crate::layout::scene::{build_editor_scene, Scene};
use crate::layout::space::{Space, SpaceKind};
use crate::layout::taffy_engine::TaffyEngine;
use crate::protocol::frontend_event::FrontendEvent;
use crate::protocol::ids::{ContentId, SpaceId};
use crate::protocol::status::StatusMessage;

#[derive(Debug)]
enum BgResult {
    SaveResult(ContentId, io::Result<()>),
}

pub struct App {
    contents: HashMap<ContentId, Box<dyn ContentHandler>>,
    scene: Scene,
    engine: TaffyEngine,
    focused: SpaceId,
    dispatcher: Dispatcher,
    should_quit: bool,
    frontend: FrontendImpl,
    bg_tx: mpsc::Sender<BgResult>,
    bg_rx: mpsc::Receiver<BgResult>,
    pending_save: Option<ContentId>,
}

impl App {
    pub fn new(
        path: Option<&str>,
        width: usize,
        height: usize,
        frontend: FrontendImpl,
    ) -> io::Result<Self> {
        let editor_content = ContentId(0);
        let status_content = ContentId(1);
        let mut buffer = Buffer::new();
        if let Some(p) = path {
            buffer.open_path(p)?;
        }
        let status_bar = StatusBar::new(editor_content);
        let mut contents: HashMap<ContentId, Box<dyn ContentHandler>> = HashMap::new();
        contents.insert(editor_content, Box::new(buffer));
        contents.insert(status_content, Box::new(status_bar));
        let (scene, editor_space) =
            build_editor_scene(width as i32, height as i32, editor_content, status_content);
        let dispatcher = Dispatcher::new(default_global_keymap());
        let (bg_tx, bg_rx) = mpsc::channel::<BgResult>(8);
        Ok(Self {
            contents,
            scene,
            engine: TaffyEngine::new(),
            focused: editor_space,
            dispatcher,
            should_quit: false,
            frontend,
            bg_tx,
            bg_rx,
            pending_save: None,
        })
    }

    pub async fn run(&mut self) -> io::Result<()> {
        self.render()?;
        loop {
            tokio::select! {
                ev = self.frontend.next_event() => {
                    if let Some(e) = ev? {
                        self.handle_event(e).await?;
                    }
                }
                res = self.bg_rx.recv() => {
                    if let Some(r) = res {
                        self.handle_bg_result(r)?;
                    }
                }
            }
            if self.should_quit {
                break;
            }
            self.render()?;
        }
        Ok(())
    }

    async fn handle_event(&mut self, event: FrontendEvent) -> io::Result<()> {
        match event {
            FrontendEvent::Resize(r) => {
                self.scene.resize(r.width as i32, r.height as i32);
            }
            FrontendEvent::Key(k) => {
                if let Some(op) = self
                    .dispatcher
                    .dispatch(k, self.focused, &self.scene, &self.contents)
                {
                    self.execute_operation(op)?;
                }
            }
            FrontendEvent::QuitRequest => self.should_quit = true,
        }
        Ok(())
    }

    fn execute_operation(&mut self, op: Operation) -> io::Result<()> {
        match op {
            Operation::Save => {
                self.spawn_save(self.focused_content_id());
            }
            Operation::Quit => self.should_quit = true,
            Operation::FocusNext | Operation::FocusPrev => {}
            Operation::CursorAddAtNextMatch(_) | Operation::CursorRemoveSecondary => {}
            _ => {
                let cid = self.focused_content_id();
                let content: &mut dyn ContentHandler = self
                    .contents
                    .get_mut(&cid)
                    .expect("focused content exists");
                let space: &mut Space = &mut self.scene.node_mut(self.focused).space;
                executor::execute(op, content, space);
            }
        }
        Ok(())
    }

    fn handle_bg_result(&mut self, res: BgResult) -> io::Result<()> {
        match res {
            BgResult::SaveResult(id, result) => {
                self.pending_save = None;
                let buf = self
                    .contents
                    .get_mut(&id)
                    .and_then(|c| c.buffer_mut())
                    .expect("saved buffer exists");
                match result {
                    Ok(()) => {
                        buf.mark_saved();
                        buf.set_status(StatusMessage::Saved);
                    }
                    Err(_) => buf.set_status(StatusMessage::SaveFailed),
                }
            }
        }
        Ok(())
    }

    /// 发起异步保存。返回是否真正发起（pending_save 已存在时忽略）。
    fn spawn_save(&mut self, id: ContentId) -> bool {
        if self.pending_save.is_some() {
            return false;
        }
        let (path, bytes) = {
            let buf = match self.contents.get(&id).and_then(|c| c.buffer_mut()) {
                Some(b) => b,
                None => return false,
            };
            let path = match buf.path().map(|p| p.to_path_buf()) {
                Some(p) => p,
                None => {
                    buf.set_status(StatusMessage::SaveFailed);
                    return false;
                }
            };
            (path, buf.slice().to_string())
        };
        let tx = self.bg_tx.clone();
        self.pending_save = Some(id);
        tokio::spawn(async move {
            let res = tokio::fs::write(path, bytes).await;
            let _ = tx.send(BgResult::SaveResult(id, res)).await;
        });
        true
    }

    fn focused_content_id(&self) -> ContentId {
        match &self.scene.node(self.focused).space.kind {
            SpaceKind::Host { content } => *content,
            _ => ContentId(0),
        }
    }

    fn render(&mut self) -> io::Result<()> {
        let resolved = self.engine.layout(&self.scene);
        // 焦点 viewport 跟随 primary cursor
        let focused_cid = self.focused_content_id();
        if let Some(item) = resolved.items.iter().find(|i| i.content_id == focused_cid) {
            let space = &mut self.scene.node_mut(self.focused).space;
            let row = space.cursors.primary.row;
            space
                .viewport
                .ensure_cursor_visible(row, item.rect.height as usize);
        }
        let focused_cursor = self.scene.node(self.focused).space.cursors.primary;
        let frame = build_frame(
            &resolved,
            &self.contents as &dyn ContentLookup,
            focused_cid,
            Some(focused_cursor),
        );
        self.frontend.render(&frame)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::frontend_event::ResizeEvent;
    use crate::protocol::key_event::{ArrowKey, CtrlKey, KeyEvent};

    fn make_app(events: Vec<FrontendEvent>, path: Option<&str>) -> App {
        App::new(path, 40, 5, FrontendImpl::Headless(HeadlessFrontend::new(events)))
            .unwrap()
    }

    fn editor_cid() -> ContentId {
        ContentId(0)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn run_inserts_char_then_quits() {
        let mut app = make_app(
            vec![
                FrontendEvent::Key(KeyEvent::Char(b'a')),
                FrontendEvent::Key(KeyEvent::Ctrl(CtrlKey::Q)),
            ],
            None,
        );
        app.run().await.unwrap();
        let buf = app.contents.get(&editor_cid()).and_then(|c| c.buffer_mut()).unwrap();
        assert_eq!(buf.slice().to_string(), "a");
        assert!(app.should_quit);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn run_supports_backspace_and_arrows() {
        let mut app = make_app(
            vec![
                FrontendEvent::Key(KeyEvent::Char(b'a')),
                FrontendEvent::Key(KeyEvent::Char(b'b')),
                FrontendEvent::Key(KeyEvent::Backspace),
                FrontendEvent::Key(KeyEvent::Arrow(ArrowKey::Left)),
                FrontendEvent::Key(KeyEvent::Ctrl(CtrlKey::Q)),
            ],
            None,
        );
        app.run().await.unwrap();
        let buf = app.contents.get(&editor_cid()).and_then(|c| c.buffer_mut()).unwrap();
        assert_eq!(buf.slice().to_string(), "a");
        let space = app.scene.node(app.focused).space;
        assert_eq!(space.cursors.primary.col, 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn run_forwards_resize_to_scene() {
        let mut app = make_app(
            vec![
                FrontendEvent::Resize(ResizeEvent { width: 100, height: 40 }),
                FrontendEvent::Key(KeyEvent::Ctrl(CtrlKey::Q)),
            ],
            None,
        );
        app.run().await.unwrap();
        assert_eq!(app.scene.size.width, 100);
        assert_eq!(app.scene.size.height, 40);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn ctrl_s_saves_file_and_marks_saved() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.txt");
        std::fs::write(&path, "hi").unwrap();
        let path_str = path.to_str().unwrap().to_owned();
        let mut app = make_app(
            vec![
                FrontendEvent::Key(KeyEvent::Char(b'X')),
                FrontendEvent::Key(KeyEvent::Ctrl(CtrlKey::S)),
                FrontendEvent::Key(KeyEvent::Ctrl(CtrlKey::Q)),
            ],
            Some(&path_str),
        );
        app.run().await.unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "Xhi");
        let buf = app.contents.get(&editor_cid()).and_then(|c| c.buffer_mut()).unwrap();
        assert!(!buf.modified());
        assert_eq!(buf.status(), StatusMessage::Saved);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn status_bar_renders_focused_buffer_info() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.txt");
        std::fs::write(&path, "hi").unwrap();
        let path_str = path.to_str().unwrap().to_owned();
        let mut app = make_app(
            vec![FrontendEvent::Key(KeyEvent::Ctrl(CtrlKey::Q))],
            Some(&path_str),
        );
        app.run().await.unwrap();
        // 取第一帧（run 开头 render 一次）
        if let FrontendImpl::Headless(h) = &app.frontend {
            let frame = h.frames.first().expect("frame captured");
            let status = frame
                .items
                .iter()
                .find(|i| i.content_id == ContentId(1))
                .expect("status item");
            match &status.content {
                FrameContent::StatusBar { file_name, modified, .. } => {
                    assert_eq!(file_name.as_deref(), Some("f.txt"));
                    assert!(!modified);
                }
                _ => panic!("expected StatusBar"),
            }
        } else {
            panic!("expected headless frontend");
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn prefix_key_sequence_saves() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("g.txt");
        std::fs::write(&path, "x").unwrap();
        let path_str = path.to_str().unwrap().to_owned();
        // 绑 'z' 前缀 + 's' → Save（覆盖 Ctrl+S 测试前缀路径）
        let mut app = make_app(
            vec![
                FrontendEvent::Key(KeyEvent::Char(b'z')),
                FrontendEvent::Key(KeyEvent::Char(b's')),
                FrontendEvent::Key(KeyEvent::Ctrl(CtrlKey::Q)),
            ],
            Some(&path_str),
        );
        // 给 Buffer 绑前缀
        app.contents
            .get_mut(&editor_cid())
            .and_then(|c| c.buffer_mut())
            .map(|_| ());
        {
            let buf = app.contents.get_mut(&editor_cid()).and_then(|c| c.buffer_mut()).unwrap();
            let mut sub = crate::core::keymap::Keymap::new();
            sub.bind(KeyEvent::Char(b's'), Operation::Save);
            buf.keymap_mut().bind_prefix(KeyEvent::Char(b'z'), sub);
        }
        app.run().await.unwrap();
        // 未修改 buffer，Save 仍 mark_saved（无变化）+ Saved 状态
        let buf = app.contents.get(&editor_cid()).and_then(|c| c.buffer_mut()).unwrap();
        assert_eq!(buf.status(), StatusMessage::Saved);
    }
}
```

> 注意 `app.frontend` 字段访问：`FrontendImpl` 是枚举，测试里 `if let FrontendImpl::Headless(h) = &app.frontend`。`frontend` 字段需非 pub 才能在同模块测试访问——同模块测试可访问私有字段。✓

> `HeadlessFrontend.frames` 字段当前 pub（frontend.rs:44 `pub frames`）。✓

- [ ] **Step 2: 调整 `src/app/frontend.rs` 测试 import（若编译报错）**

frontend.rs 测试当前 `use crate::protocol::edit_view::{ContentLookup, EditView, SpaceState};`——EditView 已删。修改 frontend.rs 测试块：测试只构造 Frame/FrameContent，不依赖 EditView。检查 frontend.rs:106-107 import，删除 `EditView`/`ContentLookup` 引用，仅保留 `SpaceState`（若测试用到）。若 frontend.rs 测试不构造 Lookup，直接删相关 import。

具体：frontend.rs 测试 `frame_with` 用 `ContentLookup`/`EditView`/`build_frame`——但 build_frame 签名已变（Task 9）。frontend.rs 测试需重写或删除 `frame_with`，改用直接构造 Frame。替换 frontend.rs 测试为：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::frame::{Frame, FrameContent, FrameItem, Rect};
    use crate::protocol::cursor::CursorPos;
    use crate::protocol::edit_view::SpaceState;
    use crate::protocol::ids::ContentId;
    use crate::protocol::key_event::{CtrlKey, KeyEvent};
    use crate::protocol::viewport::Viewport;

    #[tokio::test]
    async fn headless_drains_events_and_captures_frames() {
        let mut fe = HeadlessFrontend::new(vec![FrontendEvent::Key(KeyEvent::Ctrl(CtrlKey::Q))]);
        let first = fe.next_event().await.unwrap();
        assert!(matches!(first, Some(FrontendEvent::Key(KeyEvent::Ctrl(CtrlKey::Q)))));
        let second = fe.next_event().await.unwrap();
        assert!(second.is_none());
        let frame = Frame {
            items: vec![FrameItem {
                content_id: ContentId(0),
                rect: Rect { x: 0, y: 0, width: 40, height: 4 },
                state: SpaceState { viewport: Viewport::origin(), cursor: CursorPos::origin() },
                content: FrameContent::Editor { lines: vec!["hi".to_string()] },
            }],
            focused_content: ContentId(0),
            focused_cursor: Some(CursorPos::origin()),
        };
        fe.render(&frame).unwrap();
        assert_eq!(fe.frames.len(), 1);
    }
}
```

> 原 frontend.rs 的 `render_outputs_text_status_and_cursor` / `paint_item_writes_editor_lines` 测试依赖旧 build_frame + EditView Lookup，移除（其覆盖由 Task 10 headless 集成测试 + frame/mod.rs 测试替代）。

- [ ] **Step 3: 调整 `src/tui/tui_frontend.rs` 测试 import**

tui_frontend.rs 测试 `use crate::protocol::edit_view::{ContentLookup, EditView, SpaceState};` + `frame_with` 用旧 build_frame。删除 `render_outputs_text_status_and_cursor` 测试（依赖旧 build_frame 签名），保留 `paint_item_writes_editor_lines`（直接构造 FrameItem，不依赖 build_frame）。删除 import 中 `ContentLookup`/`EditView`，保留 `SpaceState`。

具体修改 tui_frontend.rs 测试块 imports：
```rust
    use crate::protocol::edit_view::SpaceState;
```
删除 `render_outputs_text_status_and_cursor` 测试函数与 `frame_with`/`Lookup`/`Doc` 辅助结构。保留 `paint_item_writes_editor_lines`。

- [ ] **Step 4: 全量编译**

Run: `cargo build`
Expected: 成功（无错误）。若有错误，按错误信息修复遗漏的 import/类型不匹配（常见：`Space.cursor` → `cursors`、`Document` → `Box<dyn ContentHandler>`、`editor_content` 字段移除后残留引用）。

- [ ] **Step 5: 全量测试**

Run: `cargo test`
Expected: 全部通过。预期测试数：core(operation 3 + keymap 5 + content 4 + buffer 13 + status_bar 2) + protocol(viewport +2、edit_view 1、frame 2) + layout(scene 2 + taffy 3 + space 1) + frame(1) + app(dispatcher 10 + executor 5 + frontend 1 + mod 6) ≈ 60+。

- [ ] **Step 6: clippy 零警告**

Run: `cargo clippy -- -D warnings`
Expected: 无警告。若有 dead_code 警告：
- `Operation::FocusNext`/`FocusPrev`/`CursorAddAtNextMatch`/`CursorRemoveSecondary`：v0.2 预留，加 `#[allow(dead_code)]` 于变体或枚举上方注释说明预留。
- `WrapMode::Soft`：加 `#[allow(dead_code)]`。
- `StatusBar::target_content_id` 方法若仅测试用：保留（测试调用）。
- `Viewport::scroll_by`/`scroll_up`/`scroll_down`：v0.2 预留，加 `#[allow(dead_code)]`。

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "refactor(app): App 去角色化 + dispatcher/executor 接线 + headless 集成测试"
```

---

## Self-Review 结论

**Spec 覆盖**：
- §1 目标（多层捕获/content 自治/App 去角色化/前缀键/多光标预留）→ Task 7/3+4+5/10/7/3
- §2 模块布局 → File Structure + 各 Task
- §3 核心类型（Operation/Keymap/ContentHandler/Cursors）→ Task 1/2/3
- §4 Dispatcher 捕获链 + 前缀状态机 → Task 7
- §5 Executor → Task 8
- §6 App 去角色化 + 数据流 → Task 10
- §7 build_frame 改造 → Task 9
- §8 默认 keymap → Task 4（buffer）+ Task 7（global）
- §9 错误处理 → Task 4（open_path）+ Task 10（保存回环）
- §10 测试策略 → 各 Task TDD + Task 10 headless 集成
- §11 多光标预留 → Task 3 Cursors + Task 8 executor + Task 1 Operation 变体
- §12 迁移删除 → Task 4/9/10

**类型一致性**：`Cursors`/`ContentHandler`/`Operation`/`Keymap` 在各 Task 签名一致；`Space.cursors`/`build_editor_scene -> (Scene, SpaceId)`/`build_frame` 签名跨 Task 一致；`executor::execute(op, content, space)` 签名 Task 8 定义、Task 10 调用一致。

**已知 [wip] 编译中断**：Task 4/6/7/8/9 中间状态全量编译失败（类型契约变更波及），Task 10 编译关卡统一修复。每个 [wip] Task 提交其自身可独立验证的模块测试（core/layout 可独立跑；app 模块 Task 7/8/9 不独立跑，Task 10 统一验证）。
