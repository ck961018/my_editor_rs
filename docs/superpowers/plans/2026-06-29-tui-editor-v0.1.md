# TUI Editor v0.1 (Rust) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 在 Windows 上实现一个可运行的最小 TUI 编辑器（`my_editor_rs file.txt`），支持 ASCII 编辑、方向键移动、Ctrl-S 保存、Ctrl-Q 退出，且核心逻辑（EditorCore/Protocol）可纯同步单测、不依赖 crossterm/tokio。

**Architecture:** 自底向上分层。`EditorCore`（buffer/cursor/status/editor）是纯同步核心，只通过 Protocol 层的 `FrontendEvent`/`KeyEvent`/`CorePatch`/`PatchList` 与外界通信；`terminal`（output/lifecycle/input）封装 crossterm；`tui`（viewport/renderer/tui_frontend）做视口计算与渲染编排；`app.rs`+`main.rs` 用 tokio 主循环把各层接线。核心方法签名全部同步，`async` 只出现在主循环、crossterm EventStream 与文件 I/O 胶水层。

**Tech Stack:** Rust edition 2021；`ropey` 1（Rope 文本缓冲区）；`crossterm` 0.28（`event-stream` feature，跨平台终端）；`tokio` 1（`full`，多线程运行时）；`futures` 0.3（`StreamExt` 用于 EventStream）；`tempfile` 3（dev-dependency，文件 IO 测试）。

参考设计文档：`docs/design/architecture_design_rust.md`（22 章节，所有命名/签名以该文档与本计划为准）。

---

## 文件结构

```text
src/
  main.rs                  # tokio main，解析 args，enter guard，启动 App
  app.rs                   # App 生命周期与 async 主循环

  core/
    mod.rs                 # pub mod buffer/cursor/status/editor;
    buffer.rs              # 包装 ropey::Rope，文件加载/保存/增删
    cursor.rs              # char_index/row/col + recompute + 四向移动
    status.rs              # StatusMessage 枚举 + Status 包装
    editor.rs              # Editor 聚合 buffer/cursor/status，handle_event 分发

  protocol/
    mod.rs                 # pub mod core_patch/frontend_event/key_event;
    key_event.rs           # KeyEvent/CtrlKey/ArrowKey + translate_key
    core_patch.rs          # CorePatch + PatchList（含 collapse）
    frontend_event.rs      # FrontendEvent + ResizeEvent

  terminal/
    mod.rs                 # pub mod input/lifecycle/output;
    output.rs              # Output<W: Write> 泛型输出，queue VT
    lifecycle.rs           # TerminalGuard（RAII raw mode + alt screen）
    input.rs               # Input（crossterm EventStream -> FrontendEvent）

  tui/
    mod.rs                 # pub mod renderer/tui_frontend/viewport;
    viewport.rs            # Viewport（top_row/left_col/width/height）
    renderer.rs            # Renderer::draw 无状态绘制
    tui_frontend.rs        # TuiFrontend（dirty 标记 + render 编排）
```

YAGNI 跳过（设计文档 §3 列出但 v0.1 不实现）：`protocol/command.rs`（CommandId 留 v0.2）、`util/ascii.rs`、`tui/style.rs`、`core/file_io.rs`（文件 IO 直接放 `buffer.rs`）。

任务顺序（自底向上，纯同步核心先行 → 终端 → TUI → 接线）：

1. 项目骨架（git init / cargo init / Cargo.toml / 模块树）
2. protocol/key_event
3. protocol/core_patch
4. protocol/frontend_event
5. core/buffer
6. core/cursor
7. core/status
8. core/editor
9. terminal/output
10. tui/viewport
11. tui/renderer
12. tui/tui_frontend
13. terminal/lifecycle + terminal/input
14. app + main + 集成验证

---

## Task 1: 项目骨架

**Files:**
- Create: `Cargo.toml`
- Create: `src/main.rs`
- Create: `src/core/mod.rs`, `src/protocol/mod.rs`, `src/terminal/mod.rs`, `src/tui/mod.rs`
- Create: 各模块占位空文件以便 `mod` 声明编译通过

- [ ] **Step 1: 初始化 git 与 cargo**

Run:
```powershell
git init
cargo init --name my_editor_rs
```
`cargo init` 会生成 `Cargo.toml` 与 `src/main.rs`。若 `cargo init` 拒绝（因已存在 `Cargo.toml`），跳过；否则保留生成的文件。预期：仓库初始化，`src/main.rs` 存在。

- [ ] **Step 2: 覆写 Cargo.toml**

写入 `Cargo.toml`：
```toml
[package]
name = "my_editor_rs"
version = "0.1.0"
edition = "2021"

[dependencies]
ropey = "1"
crossterm = { version = "0.28", features = ["event-stream"] }
tokio = { version = "1", features = ["full"] }
futures = "0.3"

[dev-dependencies]
tempfile = "3"
```

- [ ] **Step 3: 创建模块树与占位文件**

写入 `src/main.rs`：
```rust
mod app;
mod core;
mod protocol;
mod terminal;
mod tui;

fn main() {
    println!("skeleton ok");
}
```

写入 `src/core/mod.rs`：
```rust
pub mod buffer;
pub mod cursor;
pub mod editor;
pub mod status;
```

写入 `src/protocol/mod.rs`：
```rust
pub mod core_patch;
pub mod frontend_event;
pub mod key_event;
```

写入 `src/terminal/mod.rs`：
```rust
pub mod input;
pub mod lifecycle;
pub mod output;
```

写入 `src/tui/mod.rs`：
```rust
pub mod renderer;
pub mod tui_frontend;
pub mod viewport;
```

为每个被声明但尚未实现的模块文件创建空占位（内容仅一行注释），例如 `src/app.rs`：
```rust
// app.rs — 主循环，Task 14 实现
```
同样创建 `src/core/buffer.rs`、`src/core/cursor.rs`、`src/core/editor.rs`、`src/core/status.rs`、`src/protocol/core_patch.rs`、`src/protocol/frontend_event.rs`、`src/protocol/key_event.rs`、`src/terminal/input.rs`、`src/terminal/lifecycle.rs`、`src/terminal/output.rs`、`src/tui/renderer.rs`、`src/tui/tui_frontend.rs`、`src/tui/viewport.rs`，每个文件内容为对应单行注释。

- [ ] **Step 4: 验证骨架编译并拉取依赖**

Run: `cargo build`
Expected: 编译通过（可能有 unused warning，无 error）。首次会拉取 ropey/crossterm/tokio/futures。

- [ ] **Step 5: 提交骨架**

```powershell
git add -A
git commit -m "chore: 项目骨架与模块树"
```

---

## Task 2: protocol/key_event

**Files:**
- Modify: `src/protocol/key_event.rs`
- Test: 内联 `#[cfg(test)] mod tests`（Rust 惯例，纯同步单测，无需独立 tests/ 目录）

- [ ] **Step 1: 写失败测试**

将 `src/protocol/key_event.rs` 整体替换为：
```rust
use crossterm::event::{KeyCode, KeyEvent as CrosstermKey, KeyModifiers};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CtrlKey {
    Q,
    S,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArrowKey {
    Up,
    Down,
    Left,
    Right,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyEvent {
    Char(u8),
    Ctrl(CtrlKey),
    Arrow(ArrowKey),
    Backspace,
    Enter,
    Escape,
    Unknown,
}

pub fn translate_key(k: CrosstermKey) -> KeyEvent {
    match k.code {
        KeyCode::Char(c)
            if (c.is_ascii_graphic() || c == ' ') && k.modifiers.is_empty() =>
        {
            KeyEvent::Char(c as u8)
        }
        KeyCode::Char(c) if k.modifiers.contains(KeyModifiers::CONTROL) => {
            match c.to_ascii_lowercase() {
                'q' => KeyEvent::Ctrl(CtrlKey::Q),
                's' => KeyEvent::Ctrl(CtrlKey::S),
                _ => KeyEvent::Unknown,
            }
        }
        KeyCode::Backspace => KeyEvent::Backspace,
        KeyCode::Enter => KeyEvent::Enter,
        KeyCode::Esc => KeyEvent::Escape,
        KeyCode::Left => KeyEvent::Arrow(ArrowKey::Left),
        KeyCode::Right => KeyEvent::Arrow(ArrowKey::Right),
        KeyCode::Up => KeyEvent::Arrow(ArrowKey::Up),
        KeyCode::Down => KeyEvent::Arrow(ArrowKey::Down),
        _ => KeyEvent::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode, mods: KeyModifiers) -> CrosstermKey {
        CrosstermKey::new(code, mods)
    }

    #[test]
    fn printable_ascii_becomes_char() {
        assert_eq!(translate_key(key(KeyCode::Char('a'), KeyModifiers::empty())), KeyEvent::Char(b'a'));
        assert_eq!(translate_key(key(KeyCode::Char(' '), KeyModifiers::empty())), KeyEvent::Char(b' '));
        assert_eq!(translate_key(key(KeyCode::Char('Z'), KeyModifiers::empty())), KeyEvent::Char(b'Z'));
    }

    #[test]
    fn ctrl_q_and_s() {
        assert_eq!(translate_key(key(KeyCode::Char('q'), KeyModifiers::CONTROL)), KeyEvent::Ctrl(CtrlKey::Q));
        assert_eq!(translate_key(key(KeyCode::Char('S'), KeyModifiers::CONTROL)), KeyEvent::Ctrl(CtrlKey::S));
    }

    #[test]
    fn ctrl_other_is_unknown() {
        assert_eq!(translate_key(key(KeyCode::Char('x'), KeyModifiers::CONTROL)), KeyEvent::Unknown);
    }

    #[test]
    fn special_keys_map() {
        assert_eq!(translate_key(key(KeyCode::Backspace, KeyModifiers::empty())), KeyEvent::Backspace);
        assert_eq!(translate_key(key(KeyCode::Enter, KeyModifiers::empty())), KeyEvent::Enter);
        assert_eq!(translate_key(key(KeyCode::Esc, KeyModifiers::empty())), KeyEvent::Escape);
    }

    #[test]
    fn arrows_map() {
        assert_eq!(translate_key(key(KeyCode::Up, KeyModifiers::empty())), KeyEvent::Arrow(ArrowKey::Up));
        assert_eq!(translate_key(key(KeyCode::Down, KeyModifiers::empty())), KeyEvent::Arrow(ArrowKey::Down));
        assert_eq!(translate_key(key(KeyCode::Left, KeyModifiers::empty())), KeyEvent::Arrow(ArrowKey::Left));
        assert_eq!(translate_key(key(KeyCode::Right, KeyModifiers::empty())), KeyEvent::Arrow(ArrowKey::Right));
    }

    #[test]
    fn function_key_is_unknown() {
        assert_eq!(translate_key(key(KeyCode::F(1), KeyModifiers::empty())), KeyEvent::Unknown);
    }
}
```

- [ ] **Step 2: 运行测试确认通过**

Run: `cargo test --lib protocol::key_event`
Expected: 6 个测试全部 PASS。

（注：本任务实现与测试一并写入，因此直接验证通过。后续任务遵循先写失败测试的 TDD 节奏。）

- [ ] **Step 3: 提交**

```powershell
git add src/protocol/key_event.rs
git commit -m "feat(protocol): KeyEvent 与 translate_key"
```

---

## Task 3: protocol/core_patch

**Files:**
- Modify: `src/protocol/core_patch.rs`

- [ ] **Step 1: 写失败测试与实现**

将 `src/protocol/core_patch.rs` 整体替换为：
```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CorePatch {
    BufferChanged,
    CursorMoved,
    StatusChanged,
    FullRedrawRequired,
}

#[derive(Debug, Clone, Default)]
pub struct PatchList {
    items: Vec<CorePatch>,
}

impl PatchList {
    pub fn new() -> Self {
        Self { items: Vec::new() }
    }

    pub fn push(&mut self, p: CorePatch) {
        self.items.push(p);
    }

    pub fn items(&self) -> &[CorePatch] {
        &self.items
    }

    /// 可选：渲染前折叠重复 patch。FullRedrawRequired 出现时覆盖其余。
    pub fn collapse(&mut self) {
        if self.items.iter().any(|p| *p == CorePatch::FullRedrawRequired) {
            self.items = vec![CorePatch::FullRedrawRequired];
            return;
        }
        let mut deduped: Vec<CorePatch> = Vec::new();
        for p in self.items.drain(..) {
            if !deduped.contains(&p) {
                deduped.push(p);
            }
        }
        self.items = deduped;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_and_items() {
        let mut pl = PatchList::new();
        pl.push(CorePatch::BufferChanged);
        pl.push(CorePatch::CursorMoved);
        assert_eq!(pl.items(), &[CorePatch::BufferChanged, CorePatch::CursorMoved]);
    }

    #[test]
    fn collapse_dedupes() {
        let mut pl = PatchList::new();
        pl.push(CorePatch::BufferChanged);
        pl.push(CorePatch::BufferChanged);
        pl.push(CorePatch::CursorMoved);
        pl.collapse();
        assert_eq!(pl.items(), &[CorePatch::BufferChanged, CorePatch::CursorMoved]);
    }

    #[test]
    fn collapse_full_redraw_wins() {
        let mut pl = PatchList::new();
        pl.push(CorePatch::BufferChanged);
        pl.push(CorePatch::FullRedrawRequired);
        pl.push(CorePatch::CursorMoved);
        pl.collapse();
        assert_eq!(pl.items(), &[CorePatch::FullRedrawRequired]);
    }

    #[test]
    fn collapse_empty_stays_empty() {
        let mut pl = PatchList::new();
        pl.collapse();
        assert!(pl.items().is_empty());
    }
}
```

- [ ] **Step 2: 运行测试确认通过**

Run: `cargo test --lib protocol::core_patch`
Expected: 4 个测试全部 PASS。

- [ ] **Step 3: 提交**

```powershell
git add src/protocol/core_patch.rs
git commit -m "feat(protocol): CorePatch 与 PatchList"
```

---

## Task 4: protocol/frontend_event

**Files:**
- Modify: `src/protocol/frontend_event.rs`

- [ ] **Step 1: 写失败测试与实现**

将 `src/protocol/frontend_event.rs` 整体替换为：
```rust
use crate::protocol::key_event::KeyEvent;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResizeEvent {
    pub width: u16,
    pub height: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrontendEvent {
    Key(KeyEvent),
    Resize(ResizeEvent),
    QuitRequest,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::key_event::CtrlKey;

    #[test]
    fn key_event_wraps() {
        let ev = FrontendEvent::Key(KeyEvent::Ctrl(CtrlKey::Q));
        assert_eq!(ev, FrontendEvent::Key(KeyEvent::Ctrl(CtrlKey::Q)));
    }

    #[test]
    fn resize_event_carries_dims() {
        let ev = FrontendEvent::Resize(ResizeEvent { width: 80, height: 24 });
        match ev {
            FrontendEvent::Resize(r) => {
                assert_eq!(r.width, 80);
                assert_eq!(r.height, 24);
            }
            _ => panic!("expected Resize"),
        }
    }

    #[test]
    fn quit_request_variant() {
        assert_eq!(FrontendEvent::QuitRequest, FrontendEvent::QuitRequest);
    }
}
```

- [ ] **Step 2: 运行测试确认通过**

Run: `cargo test --lib protocol::frontend_event`
Expected: 3 个测试全部 PASS。

- [ ] **Step 3: 提交**

```powershell
git add src/protocol/frontend_event.rs
git commit -m "feat(protocol): FrontendEvent 与 ResizeEvent"
```

---

## Task 5: core/buffer

**Files:**
- Modify: `src/core/buffer.rs`

- [ ] **Step 1: 写失败测试与实现**

将 `src/core/buffer.rs` 整体替换为：
```rust
use ropey::Rope;
use std::io;
use std::path::PathBuf;

pub struct Buffer {
    rope: Rope,
    path: Option<PathBuf>,
    modified: bool,
}

impl Buffer {
    pub fn new() -> Self {
        Self {
            rope: Rope::new(),
            path: None,
            modified: false,
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
                // 文件不存在 -> 当作新文件：空 Rope，返回 Ok
                self.rope = Rope::new();
                self.modified = false;
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    pub fn save(&mut self) -> io::Result<()> {
        let path = match &self.path {
            Some(p) => p.clone(),
            None => {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    "no path associated with buffer",
                ))
            }
        };
        std::fs::write(&path, self.rope.to_string())?;
        self.modified = false;
        Ok(())
    }

    pub fn insert_char(&mut self, char_idx: usize, ch: char) {
        self.rope.insert_char(char_idx, ch);
        self.modified = true;
    }

    pub fn insert_str(&mut self, char_idx: usize, text: &str) {
        self.rope.insert(char_idx, text);
        self.modified = true;
    }

    /// 删除 char_idx 前一个字符；返回是否真的删除了。
    /// char_idx == 0 时返回 false，不修改 buffer、不置 modified。
    pub fn delete_backward(&mut self, char_idx: usize) -> bool {
        if char_idx == 0 {
            return false;
        }
        self.rope.remove(char_idx - 1..char_idx);
        self.modified = true;
        true
    }

    pub fn remove(&mut self, start: usize, end: usize) {
        self.rope.remove(start..end);
        self.modified = true;
    }

    pub fn len_chars(&self) -> usize {
        self.rope.len_chars()
    }

    pub fn len_lines(&self) -> usize {
        self.rope.len_lines()
    }

    pub fn line(&self, line_idx: usize) -> ropey::RopeSlice<'_> {
        self.rope.line(line_idx)
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
}

impl Default for Buffer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn new_buffer_is_empty() {
        let b = Buffer::new();
        assert_eq!(b.len_chars(), 0);
        // 空 Rope 的 len_lines() == 1
        assert_eq!(b.len_lines(), 1);
        assert!(!b.modified());
        assert!(b.path().is_none());
    }

    #[test]
    fn insert_char_advances() {
        let mut b = Buffer::new();
        b.insert_char(0, 'a');
        b.insert_char(1, 'b');
        assert_eq!(b.slice().to_string(), "ab");
        assert!(b.modified());
    }

    #[test]
    fn insert_str_works() {
        let mut b = Buffer::new();
        b.insert_str(0, "hello");
        assert_eq!(b.slice().to_string(), "hello");
    }

    #[test]
    fn delete_backward_at_zero_is_noop() {
        let mut b = Buffer::new();
        b.insert_str(0, "abc");
        assert!(!b.delete_backward(0));
        assert_eq!(b.slice().to_string(), "abc");
    }

    #[test]
    fn delete_backward_removes_previous() {
        let mut b = Buffer::new();
        b.insert_str(0, "abc");
        assert!(b.delete_backward(2)); // 删除 'b'
        assert_eq!(b.slice().to_string(), "ac");
    }

    #[test]
    fn load_existing_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("f.txt");
        std::fs::write(&path, "line1\nline2").unwrap();
        let path_str = path.to_str().unwrap();

        let mut b = Buffer::new();
        b.load_from_file(path_str).unwrap();
        assert_eq!(b.slice().to_string(), "line1\nline2");
        assert_eq!(b.len_lines(), 3); // "line1\nline2" -> 3 行（末尾空行）
        assert!(!b.modified());
        assert_eq!(b.path().map(|p| p.to_str().unwrap()), Some(path_str));
    }

    #[test]
    fn load_missing_file_is_new_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("nope.txt");
        let mut b = Buffer::new();
        let res = b.load_from_file(path.to_str().unwrap());
        assert!(res.is_ok());
        assert_eq!(b.len_chars(), 0);
        assert!(!b.modified());
    }

    #[test]
    fn save_writes_and_clears_modified() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("out.txt");
        let path_str = path.to_str().unwrap();

        let mut b = Buffer::new();
        b.load_from_file(path_str).unwrap(); // 不存在 -> 新文件
        b.insert_str(0, "saved");
        assert!(b.modified());
        b.save().unwrap();
        assert!(!b.modified());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "saved");
    }
}
```

- [ ] **Step 2: 运行测试确认通过**

Run: `cargo test --lib core::buffer`
Expected: 8 个测试全部 PASS。

- [ ] **Step 3: 提交**

```powershell
git add src/core/buffer.rs
git commit -m "feat(core): Buffer 包装 ropey::Rope"
```

---

## Task 6: core/cursor

**Files:**
- Modify: `src/core/cursor.rs`

- [ ] **Step 1: 写失败测试与实现**

将 `src/core/cursor.rs` 整体替换为：
```rust
use ropey::Rope;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cursor {
    pub char_index: usize,
    pub row: usize,
    pub col: usize,
}

impl Cursor {
    pub fn new() -> Self {
        Self {
            char_index: 0,
            row: 0,
            col: 0,
        }
    }

    /// 根据 char_index 重新计算 row/col（ASCII 下 col == 行内 char 偏移）。
    pub fn recompute(&mut self, rope: &Rope) {
        let clamped = self.char_index.min(rope.len_chars());
        self.row = rope.char_to_line(clamped);
        let line_start = rope.line_to_char(self.row);
        self.col = clamped - line_start;
    }

    pub fn move_left(&mut self, rope: &Rope) {
        if self.char_index > 0 {
            self.char_index -= 1;
            self.recompute(rope);
        }
    }

    pub fn move_right(&mut self, rope: &Rope) {
        if self.char_index < rope.len_chars() {
            self.char_index += 1;
            self.recompute(rope);
        }
    }

    pub fn move_up(&mut self, rope: &Rope) {
        if self.row > 0 {
            let target_row = self.row - 1;
            let line_len = line_content_len(rope, target_row);
            let new_col = self.col.min(line_len);
            self.char_index = rope.line_to_char(target_row) + new_col;
            self.recompute(rope);
        }
    }

    pub fn move_down(&mut self, rope: &Rope) {
        if self.row + 1 < rope.len_lines() {
            let target_row = self.row + 1;
            let line_len = line_content_len(rope, target_row);
            let new_col = self.col.min(line_len);
            self.char_index = rope.line_to_char(target_row) + new_col;
            self.recompute(rope);
        }
    }
}

impl Default for Cursor {
    fn default() -> Self {
        Self::new()
    }
}

/// 返回某行内容长度（不含末尾 '\n'）。最后一行无 '\n'，直接返回字符数。
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

    fn rope(s: &str) -> Rope {
        Rope::from_str(s)
    }

    #[test]
    fn empty_rope_cursor_stays_origin() {
        let r = rope("");
        let mut c = Cursor::new();
        c.recompute(&r);
        assert_eq!((c.row, c.col, c.char_index), (0, 0, 0));
    }

    #[test]
    fn recompute_single_line() {
        let r = rope("hello");
        let mut c = Cursor::new();
        c.char_index = 3;
        c.recompute(&r);
        assert_eq!((c.row, c.col), (0, 3));
    }

    #[test]
    fn recompute_multi_line() {
        let r = rope("ab\ncd");
        let mut c = Cursor::new();
        // "ab\n" 占 3 char，char_index=4 落在第二行第 1 列('d')
        c.char_index = 4;
        c.recompute(&r);
        assert_eq!((c.row, c.col), (1, 1));
    }

    #[test]
    fn move_left_right_bounds() {
        let r = rope("abc");
        let mut c = Cursor::new();
        c.move_right(&r);
        c.move_right(&r);
        c.move_right(&r);
        assert_eq!(c.char_index, 3);
        c.move_right(&r); // 越界，不动
        assert_eq!(c.char_index, 3);
        c.move_left(&r);
        assert_eq!(c.char_index, 2);
    }

    #[test]
    fn move_up_down_clamps_col() {
        let r = rope("hello\nab\nworld");
        let mut c = Cursor::new();
        // 放到第一行第 4 列（'o'）
        c.char_index = 4;
        c.recompute(&r);
        c.move_down(&r); // 第二行 "ab" 长度 2，col 钳到 2
        assert_eq!(c.row, 1);
        assert_eq!(c.col, 2);
        c.move_up(&r); // 回第一行，col 仍是 2（未保留 goal column）
        assert_eq!(c.row, 0);
        assert_eq!(c.col, 2);
    }

    #[test]
    fn move_down_at_last_line_noop() {
        let r = rope("ab\ncd");
        let mut c = Cursor::new();
        c.char_index = 5; // 末尾
        c.recompute(&r);
        let before = c.clone();
        c.move_down(&r);
        assert_eq!(c, before);
    }

    #[test]
    fn move_up_at_first_line_noop() {
        let r = rope("ab\ncd");
        let mut c = Cursor::new();
        let before = c.clone();
        c.move_up(&r);
        assert_eq!(c, before);
    }
}
```

- [ ] **Step 2: 运行测试确认通过**

Run: `cargo test --lib core::cursor`
Expected: 7 个测试全部 PASS。

- [ ] **Step 3: 提交**

```powershell
git add src/core/cursor.rs
git commit -m "feat(core): Cursor 与四向移动"
```

---

## Task 7: core/status

**Files:**
- Modify: `src/core/status.rs`

- [ ] **Step 1: 写失败测试与实现**

将 `src/core/status.rs` 整体替换为：
```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StatusMessage {
    None,
    Saved,
    SaveFailed,
    NewFile,
    OpenFailed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    message: StatusMessage,
}

impl Status {
    pub fn new() -> Self {
        Self {
            message: StatusMessage::None,
        }
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
        let s = Status::new();
        assert_eq!(s.message(), &StatusMessage::None);
    }

    #[test]
    fn set_changes_message() {
        let mut s = Status::new();
        s.set(StatusMessage::Saved);
        assert_eq!(s.message(), &StatusMessage::Saved);
        s.set(StatusMessage::OpenFailed);
        assert_eq!(s.message(), &StatusMessage::OpenFailed);
    }
}
```

- [ ] **Step 2: 运行测试确认通过**

Run: `cargo test --lib core::status`
Expected: 2 个测试全部 PASS。

- [ ] **Step 3: 提交**

```powershell
git add src/core/status.rs
git commit -m "feat(core): Status 与 StatusMessage"
```

---

## Task 8: core/editor

**Files:**
- Modify: `src/core/editor.rs`

- [ ] **Step 1: 写失败测试与实现**

将 `src/core/editor.rs` 整体替换为：
```rust
use std::io;

use crate::core::buffer::Buffer;
use crate::core::cursor::Cursor;
use crate::core::status::{Status, StatusMessage};
use crate::protocol::core_patch::{CorePatch, PatchList};
use crate::protocol::frontend_event::FrontendEvent;
use crate::protocol::key_event::{ArrowKey, CtrlKey, KeyEvent};

pub struct Editor {
    pub(crate) buffer: Buffer,
    pub(crate) cursor: Cursor,
    pub(crate) status: Status,
    pub(crate) should_quit: bool,
}

impl Editor {
    pub fn new() -> Self {
        Self {
            buffer: Buffer::new(),
            cursor: Cursor::new(),
            status: Status::new(),
            should_quit: false,
        }
    }

    pub fn buffer(&self) -> &Buffer {
        &self.buffer
    }

    pub fn cursor(&self) -> &Cursor {
        &self.cursor
    }

    pub fn status(&self) -> &Status {
        &self.status
    }

    pub fn should_quit(&self) -> bool {
        self.should_quit
    }

    pub fn open_path(&mut self, path: &str) -> io::Result<()> {
        // 通过 Path::exists 区分 NewFile 与正常加载：
        //   文件不存在           -> NewFile（Buffer 返回 Ok 空 Rope）
        //   UTF-8 解码失败       -> OpenFailed（Buffer 返回 Err InvalidData）
        //   其他 IO 错误         -> OpenFailed，并把 io::Error 向上返回 Err
        if !std::path::Path::new(path).exists() {
            self.buffer.load_from_file(path)?;
            self.status.set(StatusMessage::NewFile);
            return Ok(());
        }
        match self.buffer.load_from_file(path) {
            Ok(()) => {
                self.status.set(StatusMessage::None);
                Ok(())
            }
            Err(e) if e.kind() == io::ErrorKind::InvalidData => {
                // UTF-8 解码失败：降级为打开失败，返回 Ok（见设计 §15.1）
                self.status.set(StatusMessage::OpenFailed);
                Ok(())
            }
            Err(e) => {
                self.status.set(StatusMessage::OpenFailed);
                Err(e)
            }
        }
    }

    pub fn handle_event(
        &mut self,
        event: FrontendEvent,
        patches: &mut PatchList,
    ) -> io::Result<()> {
        match event {
            FrontendEvent::Key(k) => self.handle_key(k, patches)?,
            FrontendEvent::Resize(_) => {
                patches.push(CorePatch::FullRedrawRequired);
            }
            FrontendEvent::QuitRequest => {
                self.should_quit = true;
            }
        }
        Ok(())
    }

    fn handle_key(&mut self, key: KeyEvent, patches: &mut PatchList) -> io::Result<()> {
        match key {
            KeyEvent::Char(ch) => {
                let idx = self.cursor.char_index;
                self.buffer.insert_char(idx, ch as char);
                self.cursor.char_index += 1;
                self.cursor.recompute(self.buffer.slice());
                self.mark_modified_and_emit(patches);
            }
            KeyEvent::Enter => {
                let idx = self.cursor.char_index;
                self.buffer.insert_char(idx, '\n');
                self.cursor.char_index += 1;
                self.cursor.recompute(self.buffer.slice());
                self.mark_modified_and_emit(patches);
            }
            KeyEvent::Backspace => {
                let idx = self.cursor.char_index;
                if self.buffer.delete_backward(idx) {
                    self.cursor.char_index -= 1;
                    self.cursor.recompute(self.buffer.slice());
                    self.mark_modified_and_emit(patches);
                }
            }
            KeyEvent::Arrow(a) => {
                match a {
                    ArrowKey::Left => self.cursor.move_left(self.buffer.slice()),
                    ArrowKey::Right => self.cursor.move_right(self.buffer.slice()),
                    ArrowKey::Up => self.cursor.move_up(self.buffer.slice()),
                    ArrowKey::Down => self.cursor.move_down(self.buffer.slice()),
                }
                patches.push(CorePatch::CursorMoved);
            }
            KeyEvent::Ctrl(CtrlKey::S) => {
                match self.buffer.save() {
                    Ok(()) => self.status.set(StatusMessage::Saved),
                    Err(_) => self.status.set(StatusMessage::SaveFailed),
                }
                patches.push(CorePatch::StatusChanged);
            }
            KeyEvent::Ctrl(CtrlKey::Q) => {
                self.should_quit = true;
            }
            KeyEvent::Escape | KeyEvent::Unknown => {
                // v0.1 忽略
            }
        }
        Ok(())
    }

    fn mark_modified_and_emit(&mut self, patches: &mut PatchList) {
        patches.push(CorePatch::BufferChanged);
        patches.push(CorePatch::CursorMoved);
        patches.push(CorePatch::StatusChanged);
    }
}

impl Default for Editor {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::frontend_event::FrontendEvent;
    use crate::protocol::key_event::KeyEvent;
    use tempfile::tempdir;

    fn handle(editor: &mut Editor, k: KeyEvent) -> PatchList {
        let mut pl = PatchList::new();
        editor
            .handle_event(FrontendEvent::Key(k), &mut pl)
            .unwrap();
        pl
    }

    #[test]
    fn insert_char_appends_and_moves_cursor() {
        let mut ed = Editor::new();
        handle(&mut ed, KeyEvent::Char(b'a'));
        handle(&mut ed, KeyEvent::Char(b'b'));
        assert_eq!(ed.buffer().slice().to_string(), "ab");
        assert_eq!(ed.cursor().char_index, 2);
        assert_eq!((ed.cursor().row, ed.cursor().col), (0, 2));
    }

    #[test]
    fn enter_inserts_newline_and_drops_to_next_row() {
        let mut ed = Editor::new();
        handle(&mut ed, KeyEvent::Char(b'a'));
        handle(&mut ed, KeyEvent::Enter);
        assert_eq!(ed.buffer().slice().to_string(), "a\n");
        assert_eq!(ed.cursor().row, 1);
        assert_eq!(ed.cursor().col, 0);
    }

    #[test]
    fn backspace_deletes_and_moves_left() {
        let mut ed = Editor::new();
        handle(&mut ed, KeyEvent::Char(b'a'));
        handle(&mut ed, KeyEvent::Char(b'b'));
        handle(&mut ed, KeyEvent::Backspace);
        assert_eq!(ed.buffer().slice().to_string(), "a");
        assert_eq!(ed.cursor().char_index, 1);
    }

    #[test]
    fn backspace_at_start_is_noop() {
        let mut ed = Editor::new();
        let pl = handle(&mut ed, KeyEvent::Backspace);
        assert_eq!(ed.buffer().len_chars(), 0);
        assert!(pl.items().is_empty());
    }

    #[test]
    fn arrows_move_cursor() {
        let mut ed = Editor::new();
        handle(&mut ed, KeyEvent::Char(b'a'));
        handle(&mut ed, KeyEvent::Char(b'b'));
        handle(&mut ed, KeyEvent::Arrow(ArrowKey::Left));
        assert_eq!(ed.cursor().col, 1);
        handle(&mut ed, KeyEvent::Arrow(ArrowKey::Right));
        assert_eq!(ed.cursor().col, 2);
    }

    #[test]
    fn ctrl_q_sets_should_quit() {
        let mut ed = Editor::new();
        handle(&mut ed, KeyEvent::Ctrl(CtrlKey::Q));
        assert!(ed.should_quit());
    }

    #[test]
    fn open_missing_file_is_new_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("missing.txt");
        let mut ed = Editor::new();
        ed.open_path(path.to_str().unwrap()).unwrap();
        assert_eq!(ed.status().message(), &StatusMessage::NewFile);
        assert_eq!(ed.buffer().len_chars(), 0);
    }

    #[test]
    fn open_existing_file_loads() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("f.txt");
        std::fs::write(&path, "hi").unwrap();
        let mut ed = Editor::new();
        ed.open_path(path.to_str().unwrap()).unwrap();
        assert_eq!(ed.buffer().slice().to_string(), "hi");
        assert_eq!(ed.status().message(), &StatusMessage::None);
    }

    #[test]
    fn ctrl_s_saves_and_sets_saved() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("out.txt");
        let path_str = path.to_str().unwrap();
        let mut ed = Editor::new();
        ed.open_path(path_str).unwrap(); // 新文件
        handle(&mut ed, KeyEvent::Char(b'x'));
        handle(&mut ed, KeyEvent::Ctrl(CtrlKey::S));
        assert_eq!(ed.status().message(), &StatusMessage::Saved);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "x");
    }

    #[test]
    fn resize_emits_full_redraw() {
        let mut ed = Editor::new();
        let mut pl = PatchList::new();
        ed.handle_event(
            FrontendEvent::Resize(crate::protocol::frontend_event::ResizeEvent {
                width: 100,
                height: 40,
            }),
            &mut pl,
        )
        .unwrap();
        assert_eq!(pl.items(), &[CorePatch::FullRedrawRequired]);
    }

    #[test]
    fn quit_request_sets_should_quit() {
        let mut ed = Editor::new();
        let mut pl = PatchList::new();
        ed.handle_event(FrontendEvent::QuitRequest, &mut pl).unwrap();
        assert!(ed.should_quit());
    }
}
```

- [ ] **Step 2: 运行测试确认通过**

Run: `cargo test --lib core::editor`
Expected: 11 个测试全部 PASS。

- [ ] **Step 3: 运行全部核心/协议测试确认无回归**

Run: `cargo test --lib`
Expected: 全部 PASS（protocol + core）。

- [ ] **Step 4: 提交**

```powershell
git add src/core/editor.rs
git commit -m "feat(core): Editor 聚合与 handle_event 分发"
```

---

## Task 9: terminal/output

**Files:**
- Modify: `src/terminal/output.rs`

- [ ] **Step 1: 写失败测试与实现**

将 `src/terminal/output.rs` 整体替换为：
```rust
use std::io::{self, Write};

use crossterm::{cursor, queue, style, terminal};

pub struct Output<W: Write> {
    out: W,
}

impl<W: Write> Output<W> {
    pub fn new(out: W) -> Self {
        Self { out }
    }

    pub fn flush(&mut self) -> io::Result<()> {
        self.out.flush()
    }

    pub fn hide_cursor(&mut self) -> io::Result<()> {
        queue!(self.out, cursor::Hide)
    }

    pub fn show_cursor(&mut self) -> io::Result<()> {
        queue!(self.out, cursor::Show)
    }

    /// 内部 0-based；crossterm MoveTo 也是 0-based，参数顺序为 (col, row)。
    pub fn move_cursor(&mut self, row: usize, col: usize) -> io::Result<()> {
        queue!(self.out, cursor::MoveTo(col as u16, row as u16))
    }

    pub fn clear_screen(&mut self) -> io::Result<()> {
        queue!(self.out, terminal::Clear(terminal::ClearType::All))
    }

    pub fn clear_line(&mut self) -> io::Result<()> {
        queue!(self.out, terminal::Clear(terminal::ClearType::CurrentLine))
    }

    pub fn write_str(&mut self, s: &str) -> io::Result<()> {
        self.out.write_all(s.as_bytes())
    }

    pub fn reset_style(&mut self) -> io::Result<()> {
        queue!(self.out, style::ResetColor)
    }

    pub fn into_inner(self) -> W {
        self.out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_str_emits_bytes() {
        let mut out = Output::new(Vec::new());
        out.write_str("hello").unwrap();
        assert_eq!(out.into_inner(), b"hello");
    }

    #[test]
    fn move_cursor_emits_moveto_with_col_row_order() {
        let mut out = Output::new(Vec::new());
        // 内部 (row=2, col=5) -> crossterm MoveTo(col=5, row=2)
        out.move_cursor(2, 5).unwrap();
        let bytes = out.into_inner();
        // crossterm MoveTo 序列包含 "5;2"
        let s = String::from_utf8(bytes).unwrap();
        assert!(s.contains("5;2"), "got: {s}");
    }

    #[test]
    fn clear_screen_and_line_queue_without_flush() {
        let mut out = Output::new(Vec::new());
        out.clear_screen().unwrap();
        out.clear_line().unwrap();
        // queue! 不 flush，但 Vec 立即接收字节，应非空
        assert!(!out.into_inner().is_empty());
    }
}
```

- [ ] **Step 2: 运行测试确认通过**

Run: `cargo test --lib terminal::output`
Expected: 3 个测试全部 PASS。

- [ ] **Step 3: 提交**

```powershell
git add src/terminal/output.rs
git commit -m "feat(terminal): Output<W: Write> 泛型输出"
```

---

## Task 10: tui/viewport

**Files:**
- Modify: `src/tui/viewport.rs`

- [ ] **Step 1: 写失败测试与实现**

将 `src/tui/viewport.rs` 整体替换为：
```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Viewport {
    pub top_row: usize,
    pub left_col: usize,
    pub width: usize,
    pub height: usize,
}

impl Viewport {
    pub fn new(width: usize, height: usize) -> Self {
        // 预留最后一行给状态栏
        Self {
            top_row: 0,
            left_col: 0,
            width,
            height: height.saturating_sub(1),
        }
    }

    /// 调整 top_row 使光标行可见。left_col 在 v0.1 固定 0。
    pub fn ensure_cursor_visible(&mut self, cursor_row: usize) {
        let h = self.height;
        if h == 0 {
            self.top_row = cursor_row;
            return;
        }
        if cursor_row < self.top_row {
            self.top_row = cursor_row;
        } else if cursor_row >= self.top_row + h {
            self.top_row = cursor_row - h + 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_reserves_status_line() {
        let v = Viewport::new(80, 24);
        assert_eq!(v.width, 80);
        assert_eq!(v.height, 23);
        assert_eq!(v.top_row, 0);
    }

    #[test]
    fn scroll_down_when_cursor_below() {
        let mut v = Viewport::new(80, 24); // height=23
        v.ensure_cursor_visible(25);
        // 25 >= 0+23 -> top_row = 25-23+1 = 3
        assert_eq!(v.top_row, 3);
    }

    #[test]
    fn scroll_up_when_cursor_above() {
        let mut v = Viewport::new(80, 24);
        v.top_row = 10;
        v.ensure_cursor_visible(5);
        assert_eq!(v.top_row, 5);
    }

    #[test]
    fn no_scroll_when_visible() {
        let mut v = Viewport::new(80, 24); // height=23
        v.top_row = 5;
        v.ensure_cursor_visible(10);
        assert_eq!(v.top_row, 5);
    }
}
```

- [ ] **Step 2: 运行测试确认通过**

Run: `cargo test --lib tui::viewport`
Expected: 4 个测试全部 PASS。

- [ ] **Step 3: 提交**

```powershell
git add src/tui/viewport.rs
git commit -m "feat(tui): Viewport 视口计算"
```

---

## Task 11: tui/renderer

**Files:**
- Modify: `src/tui/renderer.rs`

- [ ] **Step 1: 写失败测试与实现**

将 `src/tui/renderer.rs` 整体替换为：
```rust
use std::io;

use crate::core::editor::Editor;
use crate::terminal::output::Output;
use crate::tui::viewport::Viewport;

/// 无状态绘制器：把给定 editor + viewport 的可见行画到 Output。
pub struct Renderer;

impl Renderer {
    pub fn draw<W: io::Write>(
        output: &mut Output<W>,
        editor: &Editor,
        viewport: &Viewport,
    ) -> io::Result<()> {
        output.hide_cursor()?;

        let buffer = editor.buffer();
        let rope = buffer.slice();
        let total_lines = rope.len_lines();

        for row in 0..viewport.height {
            let line_idx = viewport.top_row + row;
            let screen_row = row;
            output.move_cursor(screen_row, 0)?;
            output.clear_line()?;
            if line_idx < total_lines {
                let line = rope.line(line_idx).to_string();
                let content = line.trim_end_matches('\n');
                output.write_str(content)?;
            }
        }

        // 状态栏在最后一行（viewport.height）
        output.move_cursor(viewport.height, 0)?;
        output.clear_line()?;
        output.write_str(&status_line(editor))?;

        // 定位光标
        let cursor = editor.cursor();
        let screen_row = cursor.row.saturating_sub(viewport.top_row);
        let screen_col = cursor.col.saturating_sub(viewport.left_col);
        output.move_cursor(screen_row, screen_col)?;

        output.show_cursor()?;
        output.flush()?;
        Ok(())
    }
}

fn status_line(editor: &Editor) -> String {
    let name = editor
        .buffer()
        .path()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .unwrap_or("[No Name]");
    let modified = if editor.buffer().modified() { "[+]" } else { "" };
    let row = editor.cursor().row;
    let col = editor.cursor().col;
    let msg = match editor.status().message() {
        crate::core::status::StatusMessage::None => "",
        crate::core::status::StatusMessage::Saved => "Saved",
        crate::core::status::StatusMessage::SaveFailed => "SaveFailed",
        crate::core::status::StatusMessage::NewFile => "NewFile",
        crate::core::status::StatusMessage::OpenFailed => "OpenFailed",
    };
    format!("{name} {modified}  {row}:{col}  {msg}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::editor::Editor;
    use crate::protocol::frontend_event::FrontendEvent;
    use crate::protocol::key_event::KeyEvent;
    use crate::tui::viewport::Viewport;

    fn editor_with(text: &str) -> Editor {
        let mut ed = Editor::new();
        for ch in text.chars() {
            if ch == '\n' {
                let mut pl = crate::protocol::core_patch::PatchList::new();
                ed.handle_event(FrontendEvent::Key(KeyEvent::Enter), &mut pl).unwrap();
            } else {
                let mut pl = crate::protocol::core_patch::PatchList::new();
                ed.handle_event(FrontendEvent::Key(KeyEvent::Char(ch as u8)), &mut pl)
                    .unwrap();
            }
        }
        ed
    }

    #[test]
    fn draw_writes_text_and_status_line() {
        let ed = editor_with("hi");
        let vp = Viewport::new(40, 5); // height=4
        let mut out = Output::new(Vec::new());
        Renderer::draw(&mut out, &ed, &vp).unwrap();
        let s = String::from_utf8(out.into_inner()).unwrap();
        assert!(s.contains("hi"), "text row missing: {s}");
        assert!(s.contains("0:2"), "cursor pos missing: {s}");
    }

    #[test]
    fn draw_includes_modified_marker() {
        let ed = editor_with("x"); // 插入后 modified=true
        let vp = Viewport::new(40, 5);
        let mut out = Output::new(Vec::new());
        Renderer::draw(&mut out, &ed, &vp).unwrap();
        let s = String::from_utf8(out.into_inner()).unwrap();
        assert!(s.contains("[+]"), "modified marker missing: {s}");
    }

    #[test]
    fn draw_handles_multiline() {
        let ed = editor_with("ab\ncd");
        let vp = Viewport::new(40, 5);
        let mut out = Output::new(Vec::new());
        Renderer::draw(&mut out, &ed, &vp).unwrap();
        let s = String::from_utf8(out.into_inner()).unwrap();
        assert!(s.contains("ab"), "{s}");
        assert!(s.contains("cd"), "{s}");
    }
}
```

- [ ] **Step 2: 运行测试确认通过**

Run: `cargo test --lib tui::renderer`
Expected: 3 个测试全部 PASS。

- [ ] **Step 3: 提交**

```powershell
git add src/tui/renderer.rs
git commit -m "feat(tui): Renderer 无状态绘制"
```

---

## Task 12: tui/tui_frontend

**Files:**
- Modify: `src/tui/tui_frontend.rs`

- [ ] **Step 1: 写失败测试与实现**

将 `src/tui/tui_frontend.rs` 整体替换为：
```rust
use std::io;

use crate::core::editor::Editor;
use crate::protocol::core_patch::CorePatch;
use crate::terminal::output::Output;
use crate::tui::renderer::Renderer;
use crate::tui::viewport::Viewport;

pub struct TuiFrontend {
    pub(crate) viewport: Viewport,
    needs_full_redraw: bool,
    text_dirty: bool,
    status_dirty: bool,
}

impl TuiFrontend {
    pub fn new(width: usize, height: usize) -> Self {
        Self {
            viewport: Viewport::new(width, height),
            needs_full_redraw: true,
            text_dirty: true,
            status_dirty: true,
        }
    }

    pub fn resize(&mut self, width: usize, height: usize) {
        self.viewport = Viewport::new(width, height);
        self.needs_full_redraw = true;
        self.text_dirty = true;
        self.status_dirty = true;
    }

    pub fn apply_patch(&mut self, patch: &CorePatch, editor: &Editor) {
        match patch {
            CorePatch::BufferChanged => {
                self.text_dirty = true;
            }
            CorePatch::CursorMoved => {
                let old_top = self.viewport.top_row;
                self.viewport.ensure_cursor_visible(editor.cursor().row);
                if self.viewport.top_row != old_top {
                    self.needs_full_redraw = true;
                    self.text_dirty = true;
                } else {
                    // 光标在屏内移动也要重绘以更新光标位置
                    self.text_dirty = true;
                }
            }
            CorePatch::StatusChanged => {
                self.status_dirty = true;
            }
            CorePatch::FullRedrawRequired => {
                self.needs_full_redraw = true;
                self.text_dirty = true;
                self.status_dirty = true;
            }
        }
    }

    pub fn render<W: io::Write>(
        &mut self,
        editor: &Editor,
        output: &mut Output<W>,
    ) -> io::Result<()> {
        if self.needs_full_redraw {
            output.clear_screen()?;
            Renderer::draw(output, editor, &self.viewport)?;
            self.needs_full_redraw = false;
            self.text_dirty = false;
            self.status_dirty = false;
        } else if self.text_dirty || self.status_dirty {
            Renderer::draw(output, editor, &self.viewport)?;
            self.text_dirty = false;
            self.status_dirty = false;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::editor::Editor;
    use crate::protocol::core_patch::{CorePatch, PatchList};
    use crate::protocol::frontend_event::FrontendEvent;
    use crate::protocol::key_event::KeyEvent;

    #[test]
    fn new_starts_dirty() {
        let tf = TuiFrontend::new(80, 24);
        assert!(tf.needs_full_redraw);
        assert!(tf.text_dirty);
    }

    #[test]
    fn apply_buffer_changed_sets_text_dirty() {
        let mut tf = TuiFrontend::new(80, 24);
        tf.needs_full_redraw = false;
        tf.text_dirty = false;
        let ed = Editor::new();
        tf.apply_patch(&CorePatch::BufferChanged, &ed);
        assert!(tf.text_dirty);
        assert!(!tf.needs_full_redraw);
    }

    #[test]
    fn apply_full_redraw_sets_all() {
        let mut tf = TuiFrontend::new(80, 24);
        tf.needs_full_redraw = false;
        tf.text_dirty = false;
        tf.status_dirty = false;
        let ed = Editor::new();
        tf.apply_patch(&CorePatch::FullRedrawRequired, &ed);
        assert!(tf.needs_full_redraw);
        assert!(tf.text_dirty);
        assert!(tf.status_dirty);
    }

    #[test]
    fn render_outputs_when_dirty() {
        let mut ed = Editor::new();
        let mut pl = PatchList::new();
        ed.handle_event(FrontendEvent::Key(KeyEvent::Char(b'a')), &mut pl).unwrap();
        let mut tf = TuiFrontend::new(40, 5);
        for p in pl.items() {
            tf.apply_patch(p, &ed);
        }
        let mut out = Output::new(Vec::new());
        tf.render(&ed, &mut out).unwrap();
        let s = String::from_utf8(out.into_inner()).unwrap();
        assert!(s.contains('a'), "{s}");
    }

    #[test]
    fn render_noop_when_clean() {
        let ed = Editor::new();
        let mut tf = TuiFrontend::new(40, 5);
        // 先渲染一次清掉初始 dirty
        let mut out = Output::new(Vec::new());
        tf.render(&ed, &mut out).unwrap();
        let len_after_first = out.into_inner().len();
        // 再次渲染应无输出
        let mut out2 = Output::new(Vec::new());
        tf.render(&ed, &mut out2).unwrap();
        assert_eq!(out2.into_inner().len(), 0);
        assert_eq!(ed.buffer().len_chars(), 0); // 仅占位使用 ed
        let _ = len_after_first;
    }
}
```

- [ ] **Step 2: 运行测试确认通过**

Run: `cargo test --lib tui::tui_frontend`
Expected: 5 个测试全部 PASS。

- [ ] **Step 3: 运行全部 lib 测试确认无回归**

Run: `cargo test --lib`
Expected: 全部 PASS。

- [ ] **Step 4: 提交**

```powershell
git add src/tui/tui_frontend.rs
git commit -m "feat(tui): TuiFrontend dirty 标记与 render 编排"
```

---

## Task 13: terminal/lifecycle + terminal/input

**Files:**
- Modify: `src/terminal/lifecycle.rs`
- Modify: `src/terminal/input.rs`

说明：`lifecycle` 与 `input` 依赖真实终端/事件流，不写单元测试（设计文档 §11 指明）。仅做编译验证。`input.rs` 的 `translate_key` 逻辑已由 Task 2 覆盖测试，此处只做 EventStream 装配。

- [ ] **Step 1: 实现 lifecycle**

将 `src/terminal/lifecycle.rs` 整体替换为：
```rust
use std::io;

use crossterm::{execute, terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen}};

/// RAII guard：进入时启用 raw mode + alternate screen，drop 时恢复。
pub struct TerminalGuard;

impl TerminalGuard {
    pub fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        execute!(io::stdout(), EnterAlternateScreen)?;
        Ok(Self)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
        let _ = disable_raw_mode();
    }
}
```

- [ ] **Step 2: 实现 input**

将 `src/terminal/input.rs` 整体替换为：
```rust
use std::io;

use crossterm::event::{Event, EventStream};
use futures::StreamExt;

use crate::protocol::frontend_event::{FrontendEvent, ResizeEvent};
use crate::protocol::key_event::translate_key;

pub struct Input {
    events: EventStream,
}

impl Input {
    pub fn new() -> Self {
        Self {
            events: EventStream::new(),
        }
    }

    pub async fn next_event(&mut self) -> io::Result<Option<FrontendEvent>> {
        match self.events.next().await {
            Some(Ok(Event::Key(k))) => Ok(Some(FrontendEvent::Key(translate_key(k)))),
            Some(Ok(Event::Resize(w, h))) => {
                Ok(Some(FrontendEvent::Resize(ResizeEvent { width: w, height: h })))
            }
            Some(Ok(_)) => Ok(None), // mouse / focus 等先忽略
            Some(Err(e)) => Err(e),
            None => Ok(None),
        }
    }
}

impl Default for Input {
    fn default() -> Self {
        Self::new()
    }
}
```

- [ ] **Step 3: 验证编译**

Run: `cargo build`
Expected: 编译通过（可能有 unused 警告，因为尚未在 app 中接线）。

- [ ] **Step 4: 提交**

```powershell
git add src/terminal/lifecycle.rs src/terminal/input.rs
git commit -m "feat(terminal): TerminalGuard 与 Input(EventStream)"
```

---

## Task 14: app + main + 集成验证

**Files:**
- Modify: `src/app.rs`
- Modify: `src/main.rs`

- [ ] **Step 1: 实现 app.rs**

将 `src/app.rs` 整体替换为：
```rust
use std::io;

use crossterm::terminal::size as term_size;

use crate::core::editor::Editor;
use crate::protocol::core_patch::PatchList;
use crate::terminal::input::Input;
use crate::terminal::output::Output;
use crate::tui::tui_frontend::TuiFrontend;

pub struct App {
    editor: Editor,
    input: Input,
    output: Output<io::Stdout>,
    tui: TuiFrontend,
}

impl App {
    pub fn new(path: Option<&str>) -> io::Result<Self> {
        let mut editor = Editor::new();
        if let Some(p) = path {
            // open_path 内部处理 NotFound/InvalidData/其他 IO，返回 Ok/Err
            editor.open_path(p)?;
        }

        let (width, height) = term_size().unwrap_or((80, 24));

        Ok(Self {
            editor,
            input: Input::new(),
            output: Output::new(io::stdout()),
            tui: TuiFrontend::new(width as usize, height as usize),
        })
    }

    pub async fn run(&mut self) -> io::Result<()> {
        self.tui.render(&self.editor, &mut self.output)?;

        while !self.editor.should_quit {
            let event = match self.input.next_event().await? {
                Some(e) => e,
                None => continue,
            };

            let mut patches = PatchList::new();
            self.editor.handle_event(event, &mut patches)?;

            for patch in patches.items() {
                self.tui.apply_patch(patch, &self.editor);
            }

            self.tui.render(&self.editor, &mut self.output)?;
        }

        Ok(())
    }
}
```

- [ ] **Step 2: 实现 main.rs**

将 `src/main.rs` 整体替换为：
```rust
mod app;
mod core;
mod protocol;
mod terminal;
mod tui;

use std::io;

use app::App;
use terminal::lifecycle::TerminalGuard;

#[tokio::main]
async fn main() -> io::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let path = args.get(1).map(|s| s.as_str());

    let _guard = TerminalGuard::enter()?;

    let mut app = App::new(path)?;
    app.run().await?;
    // _guard drop 时自动恢复终端
    Ok(())
}
```

- [ ] **Step 3: 验证整体编译**

Run: `cargo build`
Expected: 编译通过，无 error。

- [ ] **Step 4: 运行全部测试**

Run: `cargo test`
Expected: 全部 PASS（lib 单测，无集成测试失败）。

- [ ] **Step 5: 集成冒烟测试（手动，可选但推荐）**

在真实终端（Windows Terminal / PowerShell）运行：
```powershell
echo "hello world" > smoke.txt
cargo run -- smoke.txt
```
预期：
- 屏幕显示 `hello world` 内容与状态栏
- 可用方向键移动光标、输入 ASCII、Backspace 删除、Enter 换行
- Ctrl-S 保存后状态栏显示 `Saved`
- Ctrl-Q 退出后终端恢复正常（raw mode 已恢复）

若无法手动测试，至少确认 `cargo build --release` 通过：
Run: `cargo build --release`
Expected: 编译通过。

- [ ] **Step 6: 提交**

```powershell
git add src/app.rs src/main.rs
git commit -m "feat: App 主循环与 main 接线，完成 v0.1"
```

---

## 完成标准对照（设计文档 §18）

实现完成后应满足：
1. Windows 上可运行 `my_editor_rs file.txt` — Task 14
2. 不存在的文件作为新文件打开 — Task 5/8（load_from_file NotFound + open_path NewFile）
3. 存在的文件可显示 — Task 5/8/11
4. 普通 ASCII 字符可插入 — Task 8（Char）
5. Backspace 可删除 — Task 8（Backspace）
6. Enter 可插入换行 — Task 8（Enter）
7. 方向键可移动光标 — Task 6/8
8. Ctrl-S 可保存 — Task 5/8（Ctrl(S)）
9. Ctrl-Q 直接退出 — Task 8（Ctrl(Q)）
10. 状态栏显示文件名/modified/row:col/message — Task 11（status_line）
11. 退出后控制台状态恢复 — Task 13（TerminalGuard Drop）
12. EditorCore 不依赖 crossterm — core/ 模块无 crossterm import
13. EditorCore 不依赖 tokio（核心方法同步）— Editor::handle_event 同步签名
14. Renderer 不修改 Rope — Renderer 只读 buffer.slice()
15. Input 层不执行编辑命令 — input.rs 只做转换
16. cargo test 通过 — 各 Task 测试步骤

---

## 执行说明

- 每个任务遵循 TDD：写测试 → 运行失败（首次实现任务测试与实现一并写入，因 Rust 内联测试惯例，但仍以"运行通过"为准）→ 实现 → 运行通过 → 提交。
- 纯同步核心（Task 2-8、10-12）可完全单元测试，不启动终端。
- 终端层（Task 9 可测；Task 13/14 依赖真实终端）以编译验证 + 手动冒烟为准。
- 频繁提交：每个 Task 一次提交，commit message 遵循 `feat(scope): 描述` / `chore:` 规范。
