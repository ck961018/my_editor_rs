# evloop/前端解耦重构 实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 把 `App` 改造为 `tokio::select!` 多路复用 evloop，引入中性 `Frame` widget 树契约与 trait+enum 双重分发，使 evloop 完全不感知 tui/gui，并以异步保存落地后台 channel 机制。

**Architecture:** 自底向上：`protocol::frame` 定义中性 `Frame` → `frame::build_frame` 纯函数把 `ResolvedScene`+`ContentLookup` 解析成 `Frame` → `App`（select! 循环）调 `build_frame` 产出帧、经 `FrontendImpl` 枚举（trait+enum 双重分发）交给前端 paint。`App` 持 `mpsc::Receiver` 接收后台保存任务结果。运行时切到 `multi_thread`，前端 `next_event` 用原生 `async fn`（移除装箱 future）。

**Tech Stack:** Rust 2021 / MSRV 1.75，tokio 1（full），crossterm 0.28（event-stream），ropey 1，taffy 0.5，tempfile 3（dev）。

---

## 重要执行说明（先读）

本计划 7 个任务。**Task 1/2/3/6/7 可独立编译通过**；**Task 4/5 是 `[wip]` 耦合任务**——`Frontend` trait 签名变更会同时波及 `app`/`tui`/`main`，中间态 crate 无法编译。执行方式：

- Task 1/2/3：标准 TDD，每步 `cargo test` 验证。
- Task 4/5：`[wip]` 提交，**只做代码评审**（无法 `cargo test`）。测试代码照写，但到 Task 6 才能运行。
- Task 6：**编译关卡**——`cargo build` + `cargo test` 全绿。
- Task 7：`cargo clippy -- -D warnings` 零警告 + 更新记忆 + 最终评审。

所有任务在专用 worktree 中执行（由 subagent-driven-development 负责创建）。

---

## 文件结构

| 文件 | 操作 | 职责 |
|---|---|---|
| `src/protocol/frame.rs` | 新建 | 中性 `Frame`/`FrameItem`/`FrameContent`/`Rect`，不依赖 crossterm/layout |
| `src/protocol/mod.rs` | 改 | 注册 `pub mod frame;` |
| `src/protocol/edit_view.rs` | 改 | `SpaceState` 派生 `PartialEq, Eq`（供 `Frame` 派生 `PartialEq`） |
| `src/core/edit.rs` | 改 | `EditAction` 改 `{None, Save, Quit}`；`handle_key` 去掉 `status` 参数，Ctrl+S 返回 `Save` |
| `src/core/buffer.rs` | 改 | 删 `save()`，加 `mark_saved(&mut self)` |
| `src/app.rs` | 改（Task 2 临时）→ 删（Task 4） | Task 2 临时改 call site + 删 save 测试；Task 4 整体删除 |
| `src/frame/mod.rs` | 新建 | 纯函数 `build_frame` + `build_editor_lines` |
| `src/frame.rs`/`src/lib` | 改 | main.rs 加 `mod frame;` |
| `src/app/mod.rs` | 新建 | `App` select! evloop + channel + 异步保存 |
| `src/app/frontend.rs` | 新建 | `Frontend` trait（async fn）+ `FrontendImpl` 枚举 + `HeadlessFrontend` |
| `src/app/document.rs` | 新建 | `Document` + `ContentLookup for HashMap` |
| `src/tui/tui_frontend.rs` | 重写 | 薄 painter：`render(&Frame)→VT` + `next_event` |
| `src/tui/content.rs` | 删 | 渲染逻辑上移至 `frame::build_frame` |
| `src/tui/mod.rs` | 改 | 删 `pub mod content;` |
| `src/main.rs` | 重写 | `multi_thread` runtime + `FrontendImpl::Tui` 接线 |

---

## Task 1: `protocol::frame` 中性帧契约

**Files:**
- Create: `src/protocol/frame.rs`
- Modify: `src/protocol/mod.rs`
- Modify: `src/protocol/edit_view.rs`（`SpaceState` 派生）

- [ ] **Step 1: 写失败测试（新建 frame.rs 含测试）**

Create `src/protocol/frame.rs`:

```rust
use crate::protocol::cursor::CursorPos;
use crate::protocol::edit_view::SpaceState;
use crate::protocol::ids::ContentId;
use crate::protocol::status::StatusMessage;

/// 中性矩形（不依赖 layout::scene::Rect，避免 protocol→layout 反向依赖）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

/// 中性渲染帧：App 经 build_frame 产出，前端只 paint。
/// 不依赖 crossterm/任何前端。Clone 供 HeadlessFrontend 捕获。
#[derive(Clone, Debug, PartialEq)]
pub struct Frame {
    pub items: Vec<FrameItem>,
    pub focused_content: ContentId,
    pub focused_cursor: Option<CursorPos>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FrameItem {
    pub content_id: ContentId,
    pub rect: Rect,
    pub state: SpaceState,
    pub content: FrameContent,
}

#[derive(Clone, Debug, PartialEq)]
pub enum FrameContent {
    Editor { lines: Vec<String> },
    StatusBar {
        file_name: Option<String>,
        modified: bool,
        message: StatusMessage,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::viewport::Viewport;

    #[test]
    fn frame_constructs_and_compares() {
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
        assert_eq!(frame.items.len(), 1);
        assert_eq!(frame.focused_content, ContentId(0));
        // PartialEq 派生可用
        let frame2 = frame.clone();
        assert_eq!(frame, frame2);
    }

    #[test]
    fn status_bar_variant_carries_fields() {
        let c = FrameContent::StatusBar {
            file_name: Some("f.txt".to_string()),
            modified: true,
            message: StatusMessage::Saved,
        };
        match c {
            FrameContent::StatusBar { file_name, modified, message } => {
                assert_eq!(file_name.as_deref(), Some("f.txt"));
                assert!(modified);
                assert_eq!(message, StatusMessage::Saved);
            }
            _ => panic!("expected StatusBar"),
        }
    }
}
```

- [ ] **Step 2: 注册模块 + 派生 SpaceState PartialEq**

Modify `src/protocol/mod.rs` — 在末尾加一行：

```rust
pub mod frame;
```

完整文件应为：
```rust
pub mod cursor;
pub mod edit_view;
pub mod frame;
pub mod frontend_event;
pub mod ids;
pub mod key_event;
pub mod status;
pub mod viewport;
```

Modify `src/protocol/edit_view.rs:32-36` — `SpaceState` 派生加 `PartialEq, Eq`：

旧：
```rust
#[derive(Clone, Copy, Debug)]
pub struct SpaceState {
    pub viewport: Viewport,
    pub cursor: CursorPos,
}
```

新：
```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpaceState {
    pub viewport: Viewport,
    pub cursor: CursorPos,
}
```

- [ ] **Step 3: 运行测试，验证通过**

Run: `cargo test --lib protocol::frame`
Expected: 2 passed。

- [ ] **Step 4: 全量编译检查**

Run: `cargo build`
Expected: 编译通过（`frame` 模块 `pub`，无 dead_code 警告）。

- [ ] **Step 5: 提交**

```bash
git add src/protocol/frame.rs src/protocol/mod.rs src/protocol/edit_view.rs
git commit -m "feat(protocol): 中性 Frame widget 树契约（Frame/FrameItem/FrameContent/Rect）"
```

---

## Task 2: core `EditAction::Save` 意图 + `Buffer::mark_saved`

**Files:**
- Modify: `src/core/edit.rs`
- Modify: `src/core/buffer.rs`
- Modify: `src/app.rs`（临时：改 call site + 删 save 测试，Task 4 会整体删 app.rs）

- [ ] **Step 1: 改 `EditAction` + `handle_key`（去 status 参数）**

Modify `src/core/edit.rs`。替换 `EditAction` 枚举（行 11-17）：

旧：
```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditAction {
    None,
    Saved,
    SaveFailed,
    Quit,
}
```

新：
```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditAction {
    None,
    Save,
    Quit,
}
```

替换 `handle_key` 签名与 Ctrl+S 分支（行 70-118）。旧签名 `pub fn handle_key(buf: &mut Buffer, cur: &mut CursorPos, status: &mut Status, key: KeyEvent) -> EditAction`，新签名去掉 `status`：

新 `handle_key`（完整替换行 70-118）：
```rust
/// 处理编辑键。操作传入的 buf/cur，返回动作意图（App 据 action 决定保存/退出）。
/// Ctrl+S 不再同步落盘，只返回 EditAction::Save，由 App 异步执行。
pub fn handle_key(buf: &mut Buffer, cur: &mut CursorPos, key: KeyEvent) -> EditAction {
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
        KeyEvent::Ctrl(CtrlKey::S) => EditAction::Save,
        KeyEvent::Ctrl(CtrlKey::Q) => EditAction::Quit,
        KeyEvent::Escape | KeyEvent::Unknown => EditAction::None,
    }
}
```

注意：`use crate::core::status::Status;`（行 5）和 `use crate::protocol::status::StatusMessage;`（行 9）现在在 `edit.rs` 顶部可能不再被 `handle_key` 使用。检查：`open_path` 仍用 `Status` 和 `StatusMessage`（行 121-136）。所以这两个 `use` 保留。

- [ ] **Step 2: 更新 `edit.rs` 测试**

`edit.rs` 测试模块（行 149-256）中所有 `handle_key(&mut buf, &mut cur, &mut st, ...)` 调用要去掉 `&mut st`。逐个改：

`handle_key_insert_and_move`（行 189-197）、`handle_key_enter_and_backspace`（行 199-210）、`handle_key_ctrl_q_returns_quit`（行 212-218）中：
旧：`handle_key(&mut buf, &mut cur, &mut st, ...)`
新：`handle_key(&mut buf, &mut cur, ...)`

（这些测试里 `let mut st = Status::new();` 仍可保留不报错——未使用变量会警告，删掉更干净。删除这三处 `let mut st = Status::new();` 行。）

替换 `handle_key_ctrl_s_saves` 测试（行 220-233）为：

```rust
    #[test]
    fn handle_key_ctrl_s_returns_save_intent() {
        let mut buf = Buffer::new();
        let mut cur = CursorPos::origin();
        // Ctrl+S 现在只返回意图，不落盘、不动 status
        assert_eq!(handle_key(&mut buf, &mut cur, KeyEvent::Ctrl(CtrlKey::S)), EditAction::Save);
    }
```

（该测试不再需要 tempdir/path/open_path，删掉相关代码。`tempdir` import 若变为未使用，保留——其它测试 `open_missing_is_new_file`/`open_non_utf8_is_open_failed` 仍用 `tempdir`。）

- [ ] **Step 3: 改 `Buffer`——删 `save()`，加 `mark_saved()`**

Modify `src/core/buffer.rs`。替换 `save` 方法（行 38-51）：

旧：
```rust
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
```

新：
```rust
    /// 标记 buffer 已保存（清 modified）。实际落盘由 App 的异步任务完成。
    pub fn mark_saved(&mut self) {
        self.modified = false;
    }
```

- [ ] **Step 4: 更新 `buffer.rs` 测试**

替换 `save_writes_and_clears_modified` 测试（行 182-195）为：

```rust
    #[test]
    fn mark_saved_clears_modified() {
        let mut b = Buffer::new();
        b.insert_str(0, "x");
        assert!(b.modified());
        b.mark_saved();
        assert!(!b.modified());
    }
```

注意：`load_existing_file`（行 156-169）用 `load_from_file`，不受影响，保留。

- [ ] **Step 5: 临时改 `app.rs` call site + 删 save 测试**

`app.rs` 当前 `handle_event`（行 109-117）调用 `handle_key(&mut doc.buffer, &mut space.space.cursor, &mut doc.status, k)`。改为去掉 `&mut doc.status`：

旧（行 113）：
```rust
                let action = handle_key(&mut doc.buffer, &mut space.space.cursor, &mut doc.status, k);
```
新：
```rust
                let action = handle_key(&mut doc.buffer, &mut space.space.cursor, k);
```

`app.rs` 测试模块中 `run_opens_file_and_saves`（行 217-231）依赖同步保存，现在会失败。删除整个 `run_opens_file_and_saves` 测试（行 217-231，含其上的空行）。该测试的异步版本会在 Task 4 的 `app/mod.rs` 重新编写。

删除后，`app.rs` 测试模块的 `use` 中 `StatusMessage` 若不再使用会警告。检查：删除的测试是唯一用 `StatusMessage` 的地方（行 229）。删除后把 `use crate::protocol::status::StatusMessage;`（行 15，顶部）也删掉——但顶部那行是模块级 import，被 `Document::status()` 返回类型用（行 44 `fn status(&self) -> StatusMessage`）。保留顶部 import。测试模块内无单独 `use StatusMessage`，所以无需改。

- [ ] **Step 6: 运行测试**

Run: `cargo test`
Expected: 全部通过（原 61 个减 1 = 60 个；`run_opens_file_and_saves` 已删，Task 4 补回异步版）。

- [ ] **Step 7: 提交**

```bash
git add src/core/edit.rs src/core/buffer.rs src/app.rs
git commit -m "refactor(core): handle_key 返回 Save 意图去 status 参数；Buffer 删 save 加 mark_saved"
```

---

## Task 3: `frame::build_frame` 中性帧构建纯函数

**Files:**
- Create: `src/frame/mod.rs`
- Modify: `src/main.rs`（加 `mod frame;`）

`build_frame` 吸收 `tui/content.rs` 的「Document→rect 内可见行」逻辑，上移为中性纯函数。它消费 `ResolvedScene` + `ContentLookup`，主动拉取文本产出 `Frame`。

- [ ] **Step 1: 写失败测试（新建 frame/mod.rs 含测试）**

Create `src/frame/mod.rs`:

```rust
//! 中性帧构建：把 ResolvedScene + ContentLookup 解析成前端无关的 Frame。
//! 依赖 layout（ResolvedScene）+ protocol（Frame/ContentLookup）。不依赖 tui/crossterm。

use crate::layout::resolved::ResolvedScene;
use crate::protocol::cursor::CursorPos;
use crate::protocol::edit_view::{ContentLookup, SpaceState};
use crate::protocol::frame::{Frame, FrameContent, FrameItem, Rect as FrameRect};
use crate::protocol::ids::ContentId;

/// 构建 neutral Frame。editor_content/status_content 由 App 传入（角色知识从 TuiFrontend
/// registry 上移至此）。focused_cursor 透传焦点光标，前端据焦点 item rect 算屏坐标。
pub fn build_frame(
    scene: &ResolvedScene,
    contents: &dyn ContentLookup,
    editor_content: ContentId,
    status_content: ContentId,
    focused_content: ContentId,
    focused_cursor: Option<CursorPos>,
) -> Frame {
    let mut items = Vec::new();
    for ri in &scene.items {
        let rect = FrameRect {
            x: ri.rect.x,
            y: ri.rect.y,
            width: ri.rect.width,
            height: ri.rect.height,
        };
        let content = if ri.content_id == editor_content {
            FrameContent::Editor {
                lines: build_editor_lines(contents, editor_content, &ri.state, ri.rect.height),
            }
        } else if ri.content_id == status_content {
            let doc = match contents.get(focused_content) {
                Some(d) => d,
                None => continue,
            };
            FrameContent::StatusBar {
                file_name: doc.file_name().map(|s| s.to_string()),
                modified: doc.modified(),
                message: doc.status(),
            }
        } else {
            continue;
        };
        items.push(FrameItem {
            content_id: ri.content_id,
            rect,
            state: ri.state,
            content,
        });
    }
    Frame { items, focused_content, focused_cursor }
}

/// 按 viewport + rect.height 收集编辑器可见行；越界行填空串（前端 clear_line 处理）。
fn build_editor_lines(
    contents: &dyn ContentLookup,
    editor: ContentId,
    state: &SpaceState,
    height: i32,
) -> Vec<String> {
    let doc = match contents.get(editor) {
        Some(d) => d,
        None => return Vec::new(),
    };
    let total = doc.len_lines();
    let mut lines = Vec::new();
    for row in 0..height.max(0) as usize {
        let line_idx = state.viewport.top_row + row;
        if line_idx < total {
            lines.push(doc.line(line_idx).trim_end_matches('\n').to_string());
        } else {
            lines.push(String::new());
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::borrow::Cow;
    use crate::layout::scene::build_editor_scene;
    use crate::layout::taffy_engine::TaffyEngine;
    use crate::protocol::cursor::CursorPos;
    use crate::protocol::status::StatusMessage;
    use crate::protocol::viewport::Viewport;

    struct Doc {
        text: String,
        name: &'static str,
        modified: bool,
        status: StatusMessage,
    }
    impl crate::protocol::edit_view::EditView for Doc {
        fn line(&self, idx: usize) -> Cow<str> {
            Cow::Owned(self.text.lines().nth(idx).unwrap_or("").to_string())
        }
        fn len_lines(&self) -> usize {
            self.text.lines().count().max(1)
        }
        fn file_name(&self) -> Option<&str> {
            Some(self.name)
        }
        fn modified(&self) -> bool {
            self.modified
        }
        fn status(&self) -> StatusMessage {
            self.status.clone()
        }
    }

    struct Lookup(Doc);
    impl ContentLookup for Lookup {
        fn get(&self, _id: ContentId) -> Option<&dyn crate::protocol::edit_view::EditView> {
            Some(&self.0)
        }
    }

    fn resolved() -> (crate::layout::resolved::ResolvedScene, TaffyEngine) {
        let es = build_editor_scene(40, 5, ContentId(0), ContentId(1));
        let mut engine = TaffyEngine::new();
        let resolved = engine.layout(&es.scene);
        (resolved, engine)
    }

    #[test]
    fn builds_editor_and_status_items() {
        let (resolved, _e) = resolved();
        let lk = Lookup(Doc {
            text: "hi".into(),
            name: "f.txt",
            modified: true,
            status: StatusMessage::None,
        });
        let frame = build_frame(
            &resolved,
            &lk as &dyn ContentLookup,
            ContentId(0),
            ContentId(1),
            ContentId(0),
            Some(CursorPos::origin()),
        );
        // editor rect height = 4（5 - status 1）
        let editor = frame.items.iter().find(|i| i.content_id == ContentId(0)).unwrap();
        match &editor.content {
            FrameContent::Editor { lines } => {
                assert_eq!(lines.len(), 4);
                assert_eq!(lines[0], "hi");
                assert_eq!(lines[1], ""); // 越界空行
            }
            _ => panic!("expected Editor"),
        }
        let status = frame.items.iter().find(|i| i.content_id == ContentId(1)).unwrap();
        match &status.content {
            FrameContent::StatusBar { file_name, modified, message } => {
                assert_eq!(file_name.as_deref(), Some("f.txt"));
                assert!(modified);
                assert_eq!(*message, StatusMessage::None);
            }
            _ => panic!("expected StatusBar"),
        }
        assert_eq!(frame.focused_content, ContentId(0));
        assert_eq!(frame.focused_cursor, Some(CursorPos::origin()));
    }

    #[test]
    fn editor_lines_respect_viewport_top_row() {
        let (resolved, _e) = resolved();
        let lk = Lookup(Doc {
            text: "a\nb\nc\nd".into(),
            name: "f",
            modified: false,
            status: StatusMessage::None,
        });
        // 构造一个 viewport.top_row=2 的 state：直接改 resolved item 的 state
        let mut scene_items = resolved.items.clone();
        for it in scene_items.iter_mut() {
            if it.content_id == ContentId(0) {
                it.state.viewport.top_row = 2;
            }
        }
        let patched = crate::layout::resolved::ResolvedScene { items: scene_items };
        let frame = build_frame(&patched, &lk, ContentId(0), ContentId(1), ContentId(0), None);
        let editor = frame.items.iter().find(|i| i.content_id == ContentId(0)).unwrap();
        match &editor.content {
            FrameContent::Editor { lines } => assert_eq!(lines[0], "c"), // top_row=2 → 第 3 行
            _ => panic!("expected Editor"),
        }
    }
}
```

- [ ] **Step 2: 注册 frame 模块**

Modify `src/main.rs`（行 1-6 的模块声明），加 `mod frame;`：

```rust
mod app;
mod core;
mod frame;
mod layout;
mod protocol;
mod terminal;
mod tui;
```

- [ ] **Step 3: 运行测试，验证通过**

Run: `cargo test --lib frame`
Expected: 2 passed。

- [ ] **Step 4: 全量编译**

Run: `cargo build`
Expected: 编译通过（`build_frame` 是 `pub fn`，无 dead_code 警告）。

- [ ] **Step 5: 提交**

```bash
git add src/frame/mod.rs src/main.rs
git commit -m "feat(frame): build_frame 纯函数——ResolvedScene+ContentLookup→中性 Frame"
```

---

## Task 4: [wip] 新 `app/` 模块——Frontend trait + FrontendImpl + App select! evloop

**⚠️ [wip] 耦合任务**：本任务删除 `app.rs`、创建 `app/` 模块目录。`Frontend` trait 签名变更会破坏 `tui_frontend.rs`（仍 impl 旧 trait）和 `main.rs`。**本任务后 crate 无法编译**，到 Task 6 才修复。测试代码照写，但运行推迟到 Task 6。评审方式：代码检查（spec 符合性 + 代码质量）。

**Files:**
- Delete: `src/app.rs`
- Create: `src/app/mod.rs`
- Create: `src/app/frontend.rs`
- Create: `src/app/document.rs`

- [ ] **Step 1: 创建 `src/app/document.rs`**

```rust
//! Document：buffer + status 容器，impl EditView 供 build_frame 读。
//! ContentLookup for HashMap 使 render/build_frame 可不可变借用 contents。

use std::collections::HashMap;

use crate::core::buffer::Buffer;
use crate::core::status::Status;
use crate::protocol::edit_view::{ContentLookup, EditView};
use crate::protocol::ids::ContentId;
use crate::protocol::status::StatusMessage;

pub struct Document {
    pub buffer: Buffer,
    pub status: Status,
}

impl EditView for Document {
    fn line(&self, idx: usize) -> std::borrow::Cow<str> {
        self.buffer.line(idx).to_string().into()
    }
    fn len_lines(&self) -> usize {
        self.buffer.len_lines()
    }
    fn file_name(&self) -> Option<&str> {
        self.buffer.path().and_then(|p| p.file_name()).and_then(|n| n.to_str())
    }
    fn modified(&self) -> bool {
        self.buffer.modified()
    }
    fn status(&self) -> StatusMessage {
        self.status.message().clone()
    }
}

/// 对 contents map 实现（非 App），使 build_frame/render 可同时不可变借用 contents
/// 与可变借用 frontend（disjoint fields）。
impl ContentLookup for HashMap<ContentId, Document> {
    fn get(&self, id: ContentId) -> Option<&dyn EditView> {
        HashMap::get(self, &id).map(|d| d as &dyn EditView)
    }
}
```

- [ ] **Step 2: 创建 `src/app/frontend.rs`**

注意：本任务 `FrontendImpl` 只含 `Headless` 变体；`Tui` 变体在 Task 5 加入（依赖重写后的 `TuiFrontend`）。

```rust
//! 前端抽象：App 经此 trait + FrontendImpl 枚举不感知 tui/gui。
//! next_event 用原生 async fn（FrontendImpl 是具体枚举，非 Box<dyn>，故无需装箱 future）。
//! 定义在 app 层，由 tui 层实现（依赖倒置）。

use std::collections::VecDeque;
use std::io;

use crate::protocol::frontend_event::FrontendEvent;
use crate::protocol::frame::Frame;

pub trait Frontend {
    async fn next_event(&mut self) -> io::Result<Option<FrontendEvent>>;
    fn render(&mut self, frame: &Frame) -> io::Result<()>;
}

/// trait + enum 双重分发（rsvim 风格）。App 持此枚举，只调 trait 方法、从不 match 变体。
/// 新增前端：加变体 + match 臂，App 零改动。
pub enum FrontendImpl {
    Headless(HeadlessFrontend),
    // Tui(...)  在 Task 5 加入
}

impl Frontend for FrontendImpl {
    async fn next_event(&mut self) -> io::Result<Option<FrontendEvent>> {
        match self {
            Self::Headless(f) => f.next_event().await,
        }
    }
    fn render(&mut self, frame: &Frame) -> io::Result<()> {
        match self {
            Self::Headless(f) => f.render(frame),
        }
    }
}

/// 测试/未来 headless 模式用：脚本事件队列 + 捕获渲染帧。
pub struct HeadlessFrontend {
    events: VecDeque<FrontendEvent>,
    pub frames: Vec<Frame>,
}

impl HeadlessFrontend {
    pub fn new(events: Vec<FrontendEvent>) -> Self {
        Self { events: events.into(), frames: Vec::new() }
    }
    pub fn last_frame(&self) -> Option<&Frame> {
        self.frames.last()
    }
}

impl Frontend for HeadlessFrontend {
    async fn next_event(&mut self) -> io::Result<Option<FrontendEvent>> {
        Ok(self.events.pop_front())
    }
    fn render(&mut self, frame: &Frame) -> io::Result<()> {
        self.frames.push(frame.clone());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::frame::{Frame, FrameContent, Rect};
    use crate::protocol::key_event::{CtrlKey, KeyEvent};

    #[tokio::test]
    async fn headless_drains_events_and_captures_frames() {
        let mut fe = HeadlessFrontend::new(vec![FrontendEvent::Key(KeyEvent::Ctrl(CtrlKey::Q))]);
        let first = fe.next_event().await.unwrap();
        assert!(matches!(first, Some(FrontendEvent::Key(KeyEvent::Ctrl(CtrlKey::Q)))));
        let second = fe.next_event().await.unwrap();
        assert!(second.is_none()); // 队列空
        let frame = Frame {
            items: Vec::new(),
            focused_content: crate::protocol::ids::ContentId(0),
            focused_cursor: None,
        };
        fe.render(&frame).unwrap();
        assert_eq!(fe.frames.len(), 1);
        let _ = Rect { x: 0, y: 0, width: 0, height: 0 };
        let _ = FrameContent::Editor { lines: vec![] };
    }
}
```

- [ ] **Step 3: 创建 `src/app/mod.rs`**

```rust
//! App：tokio::select! 多路复用 evloop。不感知 tui/gui（只依赖 Frontend trait + Frame）。
//! 后台保存经 mpsc channel 回环；pending_save 单 Option 防并发写。

mod document;
mod frontend;

pub use document::Document;
pub use frontend::{Frontend, FrontendImpl, HeadlessFrontend};

use std::collections::HashMap;
use std::io;

use tokio::sync::mpsc;

use crate::core::buffer::Buffer;
use crate::core::edit::{handle_key, open_path, EditAction};
use crate::core::status::Status;
use crate::frame::build_frame;
use crate::layout::scene::{build_editor_scene, EditorScene};
use crate::layout::taffy_engine::TaffyEngine;
use crate::protocol::edit_view::ContentLookup;
use crate::protocol::frontend_event::FrontendEvent;
use crate::protocol::ids::{ContentId, SpaceId};
use crate::protocol::status::StatusMessage;

#[derive(Debug)]
enum BgResult {
    SaveResult(ContentId, io::Result<()>),
}

pub struct App {
    contents: HashMap<ContentId, Document>,
    editor_content: ContentId,
    status_content: ContentId,
    scene: EditorScene,
    engine: TaffyEngine,
    focused: SpaceId,
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
        let mut status = Status::new();
        if let Some(p) = path {
            open_path(&mut buffer, &mut status, p)?;
        }
        let mut contents = HashMap::new();
        contents.insert(editor_content, Document { buffer, status });
        let scene = build_editor_scene(
            width as i32,
            height as i32,
            editor_content,
            status_content,
        );
        let focused = scene.editor_space;
        let (bg_tx, bg_rx) = mpsc::channel::<BgResult>(8);
        Ok(Self {
            contents,
            editor_content,
            status_content,
            scene,
            engine: TaffyEngine::new(),
            focused,
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
                self.scene.scene.resize(r.width as i32, r.height as i32);
            }
            FrontendEvent::Key(k) => {
                let content_id = self.editor_content;
                let action = {
                    let doc = self
                        .contents
                        .get_mut(&content_id)
                        .expect("editor content exists");
                    let space = self.scene.scene.node_mut(self.focused);
                    handle_key(&mut doc.buffer, &mut space.space.cursor, k)
                };
                match action {
                    EditAction::Save => {
                        self.spawn_save(content_id);
                    }
                    EditAction::Quit => self.should_quit = true,
                    EditAction::None => {}
                }
            }
            FrontendEvent::QuitRequest => self.should_quit = true,
        }
        Ok(())
    }

    fn handle_bg_result(&mut self, res: BgResult) -> io::Result<()> {
        match res {
            BgResult::SaveResult(id, result) => {
                self.pending_save = None;
                let doc = self
                    .contents
                    .get_mut(&id)
                    .expect("saved content exists");
                match result {
                    Ok(()) => {
                        doc.buffer.mark_saved();
                        doc.status.set(StatusMessage::Saved);
                    }
                    Err(_) => {
                        doc.status.set(StatusMessage::SaveFailed);
                    }
                }
            }
        }
        Ok(())
    }

    /// 发起异步保存。返回是否真正发起（pending_save 已存在时忽略，防并发写）。
    fn spawn_save(&mut self, id: ContentId) -> bool {
        if self.pending_save.is_some() {
            return false;
        }
        let path = match self
            .contents
            .get(&id)
            .and_then(|d| d.buffer.path().map(|p| p.to_path_buf()))
        {
            Some(p) => p,
            None => {
                self.contents
                    .get_mut(&id)
                    .expect("content exists")
                    .status
                    .set(StatusMessage::SaveFailed);
                return false;
            }
        };
        let bytes = self.contents[&id].buffer.slice().to_string();
        let tx = self.bg_tx.clone();
        self.pending_save = Some(id);
        tokio::spawn(async move {
            let res = tokio::fs::write(path, bytes).await.map_err(Into::into);
            let _ = tx.send(BgResult::SaveResult(id, res)).await;
        });
        true
    }

    fn render(&mut self) -> io::Result<()> {
        let resolved = self.engine.layout(&self.scene.scene);
        // 焦点 viewport 跟随 cursor
        if let Some(item) = resolved.items.iter().find(|i| i.content_id == self.editor_content) {
            let space = self.scene.scene.node_mut(self.focused);
            let row = space.space.cursor.row;
            space
                .space
                .viewport
                .ensure_cursor_visible(row, item.rect.height as usize);
        }
        let focused_cursor = {
            let space = self.scene.scene.node(self.focused);
            space.space.cursor
        };
        let frame = build_frame(
            &resolved,
            &self.contents as &dyn ContentLookup,
            self.editor_content,
            self.status_content,
            self.editor_content,
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
        App::new(
            path,
            40,
            5,
            FrontendImpl::Headless(HeadlessFrontend::new(events)),
        )
        .unwrap()
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
        assert_eq!(
            app.contents[&app.editor_content].buffer.slice().to_string(),
            "a"
        );
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
        assert_eq!(
            app.contents[&app.editor_content].buffer.slice().to_string(),
            "a"
        );
        let space = app.scene.scene.node(app.focused);
        assert_eq!(space.space.cursor.col, 0);
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
        assert_eq!(
            app.scene.scene.size,
            crate::layout::scene::Size { width: 100, height: 40 }
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn spawn_save_writes_file_and_marks_saved() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.txt");
        std::fs::write(&path, "hi").unwrap();
        let path_str = path.to_str().unwrap().to_owned();
        let mut app = make_app(vec![], Some(&path_str));
        // 修改 buffer
        app.contents
            .get_mut(&app.editor_content)
            .unwrap()
            .buffer
            .insert_char(0, 'x');
        let id = app.editor_content;
        assert!(app.spawn_save(id));
        // 等待后台保存结果回环（确定性：直接 await channel）
        let res = app.bg_rx.recv().await.expect("save result");
        match res {
            BgResult::SaveResult(_, Ok(())) => {}
            other => panic!("expected Ok, got {other:?}"),
        }
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "xhi");
        app.handle_bg_result(BgResult::SaveResult(id, Ok(())))
            .unwrap();
        assert!(!app.contents[&id].buffer.modified());
        assert_eq!(
            app.contents[&id].status.message(),
            &StatusMessage::Saved
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn spawn_save_ignored_while_pending() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.txt");
        let path_str = path.to_str().unwrap().to_owned();
        let mut app = make_app(vec![], Some(&path_str));
        let id = app.editor_content;
        assert!(app.spawn_save(id));
        assert_eq!(app.pending_save, Some(id));
        // 在途保存：第二次应被忽略
        assert!(!app.spawn_save(id));
        assert_eq!(app.pending_save, Some(id));
        let res = app.bg_rx.recv().await.expect("save result");
        app.handle_bg_result(res).unwrap();
        assert_eq!(app.pending_save, None);
    }
}
```

- [ ] **Step 4: 删除 `src/app.rs`**

删除整个 `src/app.rs` 文件（其内容已被 `app/mod.rs` + `app/frontend.rs` + `app/document.rs` 取代）。

- [ ] **Step 5: [wip] 提交（不要求编译通过）**

Run: `cargo build`（预期失败：`tui_frontend.rs` 仍 impl 旧 `Frontend` trait；`main.rs` 仍用 `Box<dyn Frontend>`。这是预期的 [wip] 状态。）

```bash
git add src/app/ src/app.rs
git rm src/app.rs 2>$null  # 若 git add 未捕获删除
git add -A src/app.rs src/app
git commit -m "[wip] refactor(app): 新 app 模块——Frontend trait(async fn)+FrontendImpl+select! evloop+异步保存"
```

注：评审此任务时检查：`Frontend` trait 用原生 `async fn` 无装箱；`FrontendImpl` 只调 trait 方法不 match 变体（除 dispatch）；`App::run` 用 `select!` 双分支；`spawn_save` 拿 owned 数据不碰共享状态；`pending_save` 防并发。

---

## Task 5: [wip] `tui` 重写为薄 painter + `FrontendImpl::Tui` 变体

**⚠️ [wip] 耦合任务**：重写 `TuiFrontend` 为 `render(&Frame)→VT` 薄 painter，删 `tui/content.rs`，给 `FrontendImpl` 加 `Tui` 变体。本任务后 `tui` 编译通过新 `Frontend` trait，但 `main.rs` 仍用旧 `Box<dyn Frontend>` 接线——crate 仍无法编译。到 Task 6 修复。评审方式：代码检查。

**Files:**
- Rewrite: `src/tui/tui_frontend.rs`
- Delete: `src/tui/content.rs`
- Modify: `src/tui/mod.rs`
- Modify: `src/app/frontend.rs`（加 `Tui` 变体）

- [ ] **Step 1: 重写 `src/tui/tui_frontend.rs`**

整个文件替换为：

```rust
//! TUI 前端：薄 painter。render(&Frame) 把中性 Frame 写成 VT。
//! 不再持 Content 注册表、不再解释 scene、不再查 ContentLookup——只做 Frame→VT 映射。

use std::io;

use crate::app::Frontend;
use crate::protocol::frame::{Frame, FrameContent, FrameItem};
use crate::protocol::frontend_event::FrontendEvent;
use crate::protocol::status::StatusMessage;
use crate::terminal::input::Input;
use crate::terminal::output::{Canvas, Output};

pub struct TuiFrontend<W: io::Write> {
    input: Input,
    output: Output<W>,
}

impl<W: io::Write> TuiFrontend<W> {
    pub fn new(output: Output<W>) -> Self {
        Self {
            input: Input::new(),
            output,
        }
    }

    /// 取回内部 Output（测试断言 VT 输出）。
    #[cfg(test)]
    pub fn into_output(self) -> Output<W> {
        self.output
    }
}

impl<W: io::Write> Frontend for TuiFrontend<W> {
    async fn next_event(&mut self) -> io::Result<Option<FrontendEvent>> {
        self.input.next_event().await
    }

    fn render(&mut self, frame: &Frame) -> io::Result<()> {
        self.output.hide_cursor()?;
        for item in &frame.items {
            paint_item(item, &mut self.output as &mut dyn Canvas)?;
        }
        // 光标定位：用焦点 item rect + focused_cursor + viewport 算屏坐标
        if let Some(cur) = frame.focused_cursor {
            if let Some(fi) = frame
                .items
                .iter()
                .find(|i| i.content_id == frame.focused_content)
            {
                let screen_row =
                    cur.row.saturating_sub(fi.state.viewport.top_row) + fi.rect.y as usize;
                let screen_col =
                    cur.col.saturating_sub(fi.state.viewport.left_col) + fi.rect.x as usize;
                self.output.move_cursor(screen_row, screen_col)?;
                self.output.show_cursor()?;
            }
        }
        self.output.flush()
    }
}

fn paint_item(item: &FrameItem, canvas: &mut dyn Canvas) -> io::Result<()> {
    match &item.content {
        FrameContent::Editor { lines } => {
            for (row, line) in lines.iter().enumerate() {
                let screen_row = (item.rect.y + row as i32) as usize;
                canvas.move_cursor(screen_row, item.rect.x as usize)?;
                canvas.clear_line()?;
                canvas.write_str(line)?;
            }
        }
        FrameContent::StatusBar {
            file_name,
            modified,
            message,
        } => {
            let screen_row = item.rect.y as usize;
            canvas.move_cursor(screen_row, item.rect.x as usize)?;
            canvas.clear_line()?;
            canvas.write_str(&status_line(file_name.as_deref(), *modified, message))?;
        }
    }
    Ok(())
}

fn status_line(file_name: Option<&str>, modified: bool, message: &StatusMessage) -> String {
    let name = file_name.unwrap_or("[No Name]");
    let modified = if modified { "[+]" } else { "" };
    let msg = match message {
        StatusMessage::None => "",
        StatusMessage::Saved => "Saved",
        StatusMessage::SaveFailed => "SaveFailed",
        StatusMessage::NewFile => "NewFile",
        StatusMessage::OpenFailed => "OpenFailed",
    };
    format!("{name} {modified}  {msg}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::Frontend;
    use crate::layout::scene::build_editor_scene;
    use crate::layout::taffy_engine::TaffyEngine;
    use crate::protocol::cursor::CursorPos;
    use crate::protocol::edit_view::{ContentLookup, EditView, SpaceState};
    use crate::protocol::frame::{Frame, FrameContent, FrameItem, Rect};
    use crate::protocol::ids::ContentId;
    use crate::protocol::status::StatusMessage;
    use crate::protocol::viewport::Viewport;
    use std::borrow::Cow;

    struct Doc {
        text: String,
    }
    impl EditView for Doc {
        fn line(&self, idx: usize) -> Cow<str> {
            Cow::Owned(self.text.lines().nth(idx).unwrap_or("").to_string())
        }
        fn len_lines(&self) -> usize {
            self.text.lines().count().max(1)
        }
        fn file_name(&self) -> Option<&str> {
            Some("f.txt")
        }
        fn modified(&self) -> bool {
            true
        }
        fn status(&self) -> StatusMessage {
            StatusMessage::None
        }
    }
    struct Lookup(Doc);
    impl ContentLookup for Lookup {
        fn get(&self, _id: ContentId) -> Option<&dyn EditView> {
            Some(&self.0)
        }
    }

    fn frame_with(lk: &Lookup) -> Frame {
        let es = build_editor_scene(40, 5, ContentId(0), ContentId(1));
        let mut engine = TaffyEngine::new();
        let resolved = engine.layout(&es.scene);
        crate::frame::build_frame(
            &resolved,
            lk as &dyn ContentLookup,
            ContentId(0),
            ContentId(1),
            ContentId(0),
            Some(CursorPos { char_index: 2, row: 0, col: 2 }),
        )
    }

    #[test]
    fn render_outputs_text_status_and_cursor() {
        let lk = Lookup(Doc { text: "hi".into() });
        let frame = frame_with(&lk);
        let mut fe = TuiFrontend::new(Output::new(Vec::new()));
        fe.render(&frame).unwrap();
        let s = String::from_utf8(fe.into_output().into_inner()).unwrap();
        assert!(s.contains("hi"), "{s}");
        assert!(s.contains("f.txt"), "{s}");
        // 光标 (col=2,row=0) + 焦点 rect.y=0 → ESC[1;3H；show_cursor ESC[?25h
        assert!(s.contains("\u{1b}[1;3H"), "cursor pos: {s:?}");
        assert!(s.contains("\u{1b}[?25h"), "show cursor: {s:?}");
    }

    #[test]
    fn paint_item_writes_editor_lines() {
        let item = FrameItem {
            content_id: ContentId(0),
            rect: Rect { x: 0, y: 0, width: 10, height: 2 },
            state: SpaceState { viewport: Viewport::origin(), cursor: CursorPos::origin() },
            content: FrameContent::Editor { lines: vec!["hi".to_string(), String::new()] },
        };
        let mut out = Output::new(Vec::new());
        paint_item(&item, &mut out as &mut dyn Canvas).unwrap();
        let s = String::from_utf8(out.into_inner()).unwrap();
        assert!(s.contains("hi"), "{s}");
    }
}
```

- [ ] **Step 2: 删 `src/tui/content.rs`**

删除整个 `src/tui/content.rs` 文件（其 `EditorContent`/`StatusBarContent` 渲染逻辑已上移至 `frame::build_frame`，`status_line` 已移入 `tui_frontend.rs`）。

- [ ] **Step 3: 改 `src/tui/mod.rs`**

整个文件替换为：

```rust
pub mod tui_frontend;
```

- [ ] **Step 4: 给 `FrontendImpl` 加 `Tui` 变体**

Modify `src/app/frontend.rs`。替换 `FrontendImpl` 枚举与 `impl Frontend for FrontendImpl`（Task 4 创建的那段）：

旧：
```rust
pub enum FrontendImpl {
    Headless(HeadlessFrontend),
    // Tui(...)  在 Task 5 加入
}

impl Frontend for FrontendImpl {
    async fn next_event(&mut self) -> io::Result<Option<FrontendEvent>> {
        match self {
            Self::Headless(f) => f.next_event().await,
        }
    }
    fn render(&mut self, frame: &Frame) -> io::Result<()> {
        match self {
            Self::Headless(f) => f.render(frame),
        }
    }
}
```

新：
```rust
/// `Tui` 变体绑定 `TuiFrontend<io::Stdout>`（生产用）；测试用 `Headless`。
pub enum FrontendImpl {
    Tui(crate::tui::tui_frontend::TuiFrontend<std::io::Stdout>),
    Headless(HeadlessFrontend),
}

impl Frontend for FrontendImpl {
    async fn next_event(&mut self) -> io::Result<Option<FrontendEvent>> {
        match self {
            Self::Tui(f) => f.next_event().await,
            Self::Headless(f) => f.next_event().await,
        }
    }
    fn render(&mut self, frame: &Frame) -> io::Result<()> {
        match self {
            Self::Tui(f) => f.render(frame),
            Self::Headless(f) => f.render(frame),
        }
    }
}
```

- [ ] **Step 5: [wip] 提交（不要求编译通过）**

Run: `cargo build`（预期失败：`main.rs` 仍用 `Box<dyn Frontend>` + 旧 `TuiFrontend::new(output, ContentId(0), ContentId(1))` 签名。预期 [wip]。）

```bash
git add -A src/tui src/app/frontend.rs
git commit -m "[wip] refactor(tui): TuiFrontend 薄 painter(render &Frame)+删 content.rs+FrontendImpl::Tui 变体"
```

注：评审此任务时检查：`TuiFrontend::render` 只消费 `&Frame`、不查 `ContentLookup`/不解释 scene；`paint_item` 纯映射；光标定位用 `focused_content`+`focused_cursor`+viewport；`status_line` 与原逻辑一致（注意：按 spec，状态栏不再显示 `row:col`，仅 `name [+] msg`）。

---

## Task 6: `main.rs` 接线 + 编译关卡

**Files:**
- Rewrite: `src/main.rs`

本任务修复最后一处编译断点：`main.rs` 改用 `multi_thread` runtime + `FrontendImpl::Tui`。**这是编译关卡**——此前所有 [wip] 测试在此首次运行。

- [ ] **Step 1: 重写 `src/main.rs`**

整个文件替换为：

```rust
mod app;
mod core;
mod frame;
mod layout;
mod protocol;
mod terminal;
mod tui;

use std::io;

use app::{App, FrontendImpl};
use crossterm::terminal::size as term_size;
use terminal::lifecycle::TerminalGuard;
use terminal::output::Output;
use tui::tui_frontend::TuiFrontend;

#[tokio::main(flavor = "multi_thread")]
async fn main() -> io::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let path = args.get(1).map(|s| s.as_str());

    let _guard = TerminalGuard::enter()?;

    let (width, height) = term_size().unwrap_or((80, 24));
    let frontend = FrontendImpl::Tui(TuiFrontend::new(Output::new(io::stdout())));
    let mut app = App::new(path, width as usize, height as usize, frontend)?;
    app.run().await?;
    Ok(())
}
```

- [ ] **Step 2: 编译关卡——build**

Run: `cargo build`
Expected: 编译通过。

**若失败于 Send 约束**（`App::run` future 非 `Send`，通常因 `Input`/crossterm `EventStream` future 在 Windows 非 Send），按 spec §6 退路处理：
- 退路 A（首选）：在 `terminal/input.rs` 的 `Input::next_event` 内部用 `tokio::task::spawn_blocking` 包裹同步 `crossterm::event::poll`+`read`，返回 `async` 适配，使其 future `Send`。若采用，在本任务内修改 `input.rs` 并补测试。
- 退路 B：把 `Frontend::next_event` 改回 `fn next_event(&mut self) -> Pin<Box<dyn Future<Output = io::Result<Option<FrontendEvent>>> + Send + '_>>`（`+ Send` 装箱 future），`FrontendImpl` 各变体 `Box::pin`。回退部分 spec 收益但保证编译。
- 无论哪条退路，在 spec 第 6 节记录实际选择。

- [ ] **Step 3: 编译关卡——test**

Run: `cargo test`
Expected: 全部通过。应包含：Task 1（frame 2）、Task 2（core 调整后）、Task 3（build_frame 2）、Task 4（app 5：insert/backspace/resize/spawn_save×2）、Task 5（tui 2）、原有 layout/protocol/terminal/buffer 等未动测试。总数应 ≥ 60。

- [ ] **Step 4: 手动冒烟（可选但推荐）**

Run: `cargo run -- README.md`
Expected: 终端进入编辑器，显示 README 内容 + 状态栏；键入字符可见；Ctrl+S 保存（状态栏显 Saved）；Ctrl+Q 退出。

- [ ] **Step 5: 提交**

```bash
git add src/main.rs
git commit -m "refactor(main): multi_thread runtime + FrontendImpl::Tui 接线，恢复全量编译"
```

---

## Task 7: clippy 零警告 + 更新记忆 + 最终评审

**Files:**
- Modify: `C:\Users\chengke\.claude\projects\D--workspace-my-editor-rs\memory\frontend-boxed-future-runtime.md`
- Modify: `C:\Users\chengke\.claude\projects\D--workspace-my-editor-rs\memory\MEMORY.md`

- [ ] **Step 1: clippy 严苛检查**

Run: `cargo clippy -- -D warnings`
Expected: 零警告。

若有警告：保留项加 `#[allow(dead_code)]`（参照 commit 8d1cd92 的策略），遗留项移除。常见可能项：`StatusMessage` 未使用变体（不应有）、`Rect::intersect`（layout 保留）。逐个处理，不允许提交带警告的代码。

- [ ] **Step 2: 更新项目记忆 `frontend-boxed-future-runtime.md`**

该记忆记录的「boxed future + current_thread」结论已被本次重构取代。整文件替换为：

```markdown
---
name: frontend-boxed-future-runtime
description: Frontend trait 用原生 async fn（trait+enum 分发，非 Box<dyn>）；main 用 multi_thread runtime——Send 约束经评估可行
metadata:
  type: project
---

架构重构（2026-07，evloop/前端解耦）后，`Frontend` trait（src/app/frontend.rs）的 `next_event` 用**原生 `async fn`**，不再装箱 future。原因：`App` 持具体枚举 `FrontendImpl`（trait+enum 双重分发，rsvim 风格），非 `Box<dyn Frontend>`，故 native async fn in trait（Rust 1.75+）可直接 dyn-free 调度，无需 `Pin<Box<dyn Future>>`。`main.rs` 用 `#[tokio::main(flavor = "multi_thread")]`。

**Why:** 旧设计（commit 8d1cd92）用 `Box<dyn Frontend>` + 装箱 future + `current_thread` 回避 Send。改用 enum 分发后，future 是具体类型，Send 性由各变体 body 决定，不需 `+ Send` bound。multi_thread 让后台保存任务（`tokio::spawn` + `tokio::fs::write`）真并行，经 mpsc channel 回环唤醒 `select!` 主循环。

**How to apply:** `App::run` 在 multi_thread 下须 `Send`。已验证项：`App` 字段、`FrontendImpl`/`TuiFrontend`、`HeadlessFrontend`、后台闭包、`mpsc::Receiver` 均 Send。唯一曾风险：crossterm `EventStream` future 在 Windows 的 Send 性——Task 6 编译关卡验证通过。`Content` trait（已删，渲染逻辑上移 `frame::build_frame`）不再相关。若未来再切运行时或改前端分发模型，重审 `FrontendImpl` 各变体 Send 性。相关：[[architecture-document-view-split]]。
```

- [ ] **Step 3: 更新 `MEMORY.md` 索引行**

`MEMORY.md` 中该记忆的指针行改为反映新结论：

旧：
```
- [Frontend boxed future & runtime](frontend-boxed-future-runtime.md) — Frontend 用 boxed future 非 async fn；main 用 current_thread，切多线程需重审 Send 级联
```

新：
```
- [Frontend async fn & runtime](frontend-boxed-future-runtime.md) — Frontend 用原生 async fn（trait+enum 分发）；main 用 multi_thread，Send 已验证
```

- [ ] **Step 4: 最终评审提交**

若 Step 1-3 有改动：

```bash
git add -A
git commit -m "chore: clippy 零警告 + 更新 evloop/前端解耦记忆"
```

- [ ] **Step 5: 通知用户合并**

Run: `git log --oneline -10` 确认提交序列。通知用户：所有任务完成，`cargo build` + `cargo clippy -- -D warnings` + `cargo test` 全绿，可合并到 main（Fast-forward 或 PR）。

---

## Self-Review（计划自审）

**1. Spec 覆盖：**
- §2 模块布局 → Task 1（protocol::frame）、Task 3（frame/）、Task 4（app/ 拆分）、Task 5（tui 重写）、Task 6（main）。✅
- §3 Frontend trait + FrontendImpl + Frame → Task 1（Frame 类型）、Task 4（trait+enum+Headless）、Task 5（Tui 变体）。✅
- §4 select! evloop + channel + 异步保存 + pending_save → Task 4（App::run/spawn_save/handle_bg_result）。✅
- §5 错误处理（保存失败转 status）→ Task 4 `handle_bg_result` Err 分支。✅
- §6 multi_thread + Send 风险 → Task 6 编译关卡 + 退路。✅
- §7 三层测试 → Task 3（build_frame 层 1）、Task 5（painter 层 2）、Task 4（Headless 集成层 3）。✅
- §9 记忆更新 → Task 7。✅

**2. 占位符扫描：** 无 TBD/TODO/"实现细节后补"。Task 6 退路给了具体 A/B 代码方向，非占位。✅

**3. 类型一致性：**
- `Frame`/`FrameItem`/`FrameContent`/`Rect`（protocol::frame）跨 Task 1/3/4/5 一致。✅
- `build_frame` 签名（Task 3 定义、Task 4 `App::render` 调用、Task 5 测试调用）一致：`(scene, contents, editor_content, status_content, focused_content, focused_cursor) -> Frame`。✅
- `Frontend` trait（Task 4 定义 `async fn next_event` + `fn render(&Frame)`、Task 5 TuiFrontend impl）一致。✅
- `FrontendImpl` 变体：Task 4 `{Headless}`、Task 5 加 `Tui`——`App::new`（Task 4）取 `FrontendImpl`、`main`（Task 6）构造 `FrontendImpl::Tui`、测试构造 `FrontendImpl::Headless`。✅
- `handle_key` 签名：Task 2 改为 `(buf, cur, key)` 去 status；Task 4 `App::handle_event` 调用 `handle_key(&mut doc.buffer, &mut space.space.cursor, k)` 一致。✅
- `Buffer::mark_saved`：Task 2 定义、Task 4 `handle_bg_result` 调用 `doc.buffer.mark_saved()` 一致。✅
- `spawn_save -> bool`：Task 4 定义并测试断言返回值；`handle_event` 调用 `self.spawn_save(content_id);` 忽略返回值——一致。✅
- `BgResult::SaveResult(ContentId, io::Result<()>)`：Task 4 定义、spawn、handle_bg_result、测试一致。✅

**4. 已知偏离 spec（已在计划中注明）：**
- `spawn_save` 返回 `bool`（spec §4.3 为 `()`）——为确定性测试 pending_save 忽略语义。prod 调用忽略返回值，无副作用。
- 状态栏不再显示 `row:col`（spec §3.3 `FrameContent::StatusBar` 无 cursor 字段）——按 spec 执行，painter 只显示 `name [+] msg`。

计划完整，可交付执行。


