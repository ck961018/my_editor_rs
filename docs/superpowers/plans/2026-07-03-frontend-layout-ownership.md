# 前端布局所有权下放 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 把 `TaffyEngine` + viewport 从后端下放到前端，后端经 `ContentQuery` trait 被 pull 内容，前端自治 layout/渲染；`Scene`/`Space` 纯数据下沉 `protocol/`，`Frame`/`FrameContent`/`layout` 层删除。

**Architecture:** Helix 式 pull 模型。后端（app）持内容权威 + `ContentQuery` impl + `cursors` map，不感知几何/viewport。前端 `SceneRenderer` 持 taffy + per-space viewport 缓存，layout 出 rect 后 pull 可见行 paint 到 `Canvas`。`TuiFrontend`/`HeadlessFrontend` 共用 `SceneRenderer`，只差 output 后端。

**Tech Stack:** Rust 2024, taffy 0.11, crossterm 0.29, tokio, ropey。

**Spec:** `docs/superpowers/specs/2026-07-03-frontend-layout-ownership-design.md`

---

## File Structure

**新建：**
- `protocol/geometry.rs` — `Size`/`Rect`/`Point`（从 layout/scene.rs 拆出）
- `protocol/content_query.rs` — `ContentQuery` trait + `RowRange` + `StatusBarData`
- `protocol/scene.rs` — `Scene`/`SpaceNode`/`SceneBuilder`/`BuildError`（从 layout 移入）
- `protocol/space.rs` — `Space`/`SpaceKind`/`Arrangement`/`Axis`/`Sizing`/`Align`/`Layer`（从 layout 移入，瘦身）
- `tui/scene_renderer.rs` — `SceneRenderer`（layout + viewport + pull + paint）
- `tui/headless.rs` — `HeadlessFrontend`（从 app/frontend.rs 迁出）

**移动：**
- `layout/taffy_engine.rs` → `tui/taffy_engine.rs`
- `layout/resolved.rs` → `tui/resolved.rs`

**删除：**
- `layout/` 整层（mod.rs/scene.rs/space.rs/ids.rs/taffy_engine.rs/resolved.rs）
- `frame/` 整层（mod.rs）
- `protocol/frame.rs`（`Frame`/`FrameItem`/`FrameContent`）
- `protocol/edit_view.rs`（`SpaceState`）

**改写：**
- `core/content.rs` — ContentHandler 瘦身为分发契约 + 类型查询（`as_buffer`/`as_status_bar`）
- `core/buffer.rs` / `core/status_bar.rs` — 删 render impl
- `core/operation.rs` — 删 `ViewportScrollBy`
- `app/mod.rs` — 删 engine、加 cursors map、impl ContentQuery、render 改调前端
- `app/executor.rs` — 签名改 `execute(op, content, &mut Cursors)`
- `app/frontend.rs` — `Frontend::render` 签名改
- `tui/tui_frontend.rs` — 改用 SceneRenderer

---

## Task 1: 拆 protocol/geometry.rs

**Files:**
- Create: `src/protocol/geometry.rs`
- Modify: `src/protocol/mod.rs`
- Modify: `src/layout/scene.rs`

- [ ] **Step 1: 写 protocol/geometry.rs**

```rust
//! 几何原语：Size/Rect/Point。纯数据，前后端共享。

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

#[cfg(test)]
mod tests {
    use super::*;
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

- [ ] **Step 2: protocol/mod.rs 注册模块**

在 `src/protocol/mod.rs` 加 `pub mod geometry;`（放在 `pub mod frontend_event;` 后）：

```rust
pub mod cursor;
pub mod edit_view;
pub mod frame;
pub mod frontend_event;
pub mod geometry;
pub mod ids;
pub mod key_event;
pub mod status;
pub mod viewport;
```

- [ ] **Step 3: layout/scene.rs 改为 re-export**

删除 `src/layout/scene.rs` 顶部的 `Size`/`Rect`/`Point` 定义 + `impl Rect` + `tests::rect_intersect` 测试。在 `use std::collections::HashSet;` 后加：

```rust
pub use crate::protocol::geometry::{Point, Rect, Size};
```

删掉原 `#[derive(Clone, Copy, Debug, PartialEq, Eq)] pub struct Size { ... }`、`pub struct Rect { ... }`、`impl Rect { ... }`、`pub struct Point { ... }` 三段，以及 `tests` 模块里的 `rect_intersect` 测试。

- [ ] **Step 4: 跑测试**

Run: `cargo test`
Expected: 全绿（geometry 测试 + scene 原 build_editor_scene_has_two_hosts 通过，re-export 保持 `crate::layout::scene::Rect` 可用）

- [ ] **Step 5: Commit**

```bash
git add src/protocol/geometry.rs src/protocol/mod.rs src/layout/scene.rs
git commit -m "refactor(protocol): 拆 geometry.rs（Size/Rect/Point 从 scene.rs 移出）"
```

---

## Task 2: 新建 protocol/content_query.rs

**Files:**
- Create: `src/protocol/content_query.rs`
- Modify: `src/protocol/mod.rs`

- [ ] **Step 1: 写 protocol/content_query.rs**

```rust
//! 前端 pull 后端内容的契约。同进程同步调用，返回 owned 数据。

use crate::protocol::cursor::CursorPos;
use crate::protocol::ids::ContentId;
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
    fn cursor(&self, cid: ContentId) -> CursorPos;
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

- [ ] **Step 2: protocol/mod.rs 注册**

加 `pub mod content_query;`（放在 `pub mod cursor;` 后）：

```rust
pub mod content_query;
pub mod cursor;
pub mod edit_view;
pub mod frame;
pub mod frontend_event;
pub mod geometry;
pub mod ids;
pub mod key_event;
pub mod status;
pub mod viewport;
```

- [ ] **Step 3: 跑测试**

Run: `cargo test`
Expected: 全绿（新 tests 通过，无编译错误——trait 暂无 impl）

- [ ] **Step 4: Commit**

```bash
git add src/protocol/content_query.rs src/protocol/mod.rs
git commit -m "feat(protocol): 新增 ContentQuery trait + RowRange + StatusBarData"
```

---

## Task 3: cursor 迁 App 层 + executor 签名改

把 cursor 从 `Space.cursors` 迁到 `App.cursors: HashMap<ContentId, Cursors>`。executor 签名从 `execute(op, content, &mut Space)` 改为 `execute(op, content, &mut Cursors)`。Space.cursors 字段暂保留（下个任务删），但不再被读写。

**Files:**
- Modify: `src/app/mod.rs`
- Modify: `src/app/executor.rs`

- [ ] **Step 1: 改 executor 签名 + 测试**

`src/app/executor.rs` 全文替换为：

```rust
use crate::core::content::{ContentHandler, Cursors};
use crate::core::operation::Operation;

/// 执行局部 Operation（光标/文本）。全局/多光标变体不进此处（App 分流）。
pub fn execute(op: Operation, content: &mut dyn ContentHandler, cursors: &mut Cursors) {
    let Some(buf) = content.buffer_mut() else { return; };
    match op {
        Operation::CursorMoveBy { chars, lines } => {
            for c in cursors.all_mut() { buf.move_cursor_by(c, chars, lines); }
        }
        Operation::CursorMoveLeftBy(n) => {
            for c in cursors.all_mut() { buf.move_cursor_left(c, n); }
        }
        Operation::CursorMoveRightBy(n) => {
            for c in cursors.all_mut() { buf.move_cursor_right(c, n); }
        }
        Operation::CursorMoveUpBy(n) => {
            for c in cursors.all_mut() { buf.move_cursor_up(c, n); }
        }
        Operation::CursorMoveDownBy(n) => {
            for c in cursors.all_mut() { buf.move_cursor_down(c, n); }
        }
        Operation::CursorMoveTo { char_idx, line_idx } => {
            buf.set_cursor(&mut cursors.primary, char_idx, line_idx);
            cursors.secondaries.clear();
        }
        Operation::CursorInsertText(text) => {
            buf.insert_at_cursors(cursors, &text);
        }
        Operation::CursorDelete(n) => {
            buf.delete_at_cursors(cursors, n);
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
    use crate::protocol::cursor::CursorPos;

    #[test]
    fn insert_text_changes_buffer_and_cursor() {
        let mut buf = Buffer::new();
        let mut c = Cursors::single(CursorPos::origin());
        execute(Operation::CursorInsertText("hi".to_string()), &mut buf, &mut c);
        assert_eq!(buf.slice().to_string(), "hi");
        assert_eq!(c.primary.char_index, 2);
    }

    #[test]
    fn delete_left_removes_char() {
        let mut buf = Buffer::new();
        buf.insert_char(0, 'a');
        buf.insert_char(1, 'b');
        let mut c = Cursors::single(CursorPos::origin());
        c.primary.char_index = 2;
        buf.recompute_cursor(&mut c.primary);
        execute(Operation::CursorDelete(-1), &mut buf, &mut c);
        assert_eq!(buf.slice().to_string(), "a");
        assert_eq!(c.primary.char_index, 1);
    }

    #[test]
    fn move_right_advances_cursor() {
        let mut buf = Buffer::new();
        buf.insert_char(0, 'a');
        let mut c = Cursors::single(CursorPos::origin());
        execute(Operation::CursorMoveRightBy(1), &mut buf, &mut c);
        assert_eq!(c.primary.char_index, 1);
    }

    #[test]
    fn move_to_clears_secondaries() {
        let mut buf = Buffer::new();
        buf.insert_char(0, 'a');
        let mut c = Cursors {
            primary: CursorPos::origin(),
            secondaries: vec![CursorPos::origin()],
        };
        execute(Operation::CursorMoveTo { char_idx: 0, line_idx: 0 }, &mut buf, &mut c);
        assert!(c.secondaries.is_empty());
    }
}
```

注意：删掉原 `viewport_scroll_changes_top_row` 测试（ViewportScrollBy 下个任务删，此任务先删其测试避免编译失败——把 executor 的 `Operation::ViewportScrollBy { lines } => { space.viewport.scroll_by(lines); }` 分支保留但改为 noop 不可能（不接 space 了）。所以此任务同时删 executor 的 ViewportScrollBy 分支 + Operation 变体。)

修正：此任务一并删 `Operation::ViewportScrollBy`。Step 1b 见下。

- [ ] **Step 2: 删 Operation::ViewportScrollBy**

`src/core/operation.rs`：删去 `ViewportScrollBy { lines: isize }` 变体（含其 `#[allow(dead_code)]` 注释行），并删 tests 里的 `let _ = Operation::ViewportScrollBy { lines: 3 };`。变体后的 `Save,` 前那行删除。

删后 operation.rs 的 Operation 枚举尾部应为：

```rust
    CursorDelete(isize),
    Save,
    Quit,
```

tests 中 `operation_variants_construct` 删掉 `let _ = Operation::ViewportScrollBy { lines: 3 };` 这一行。

- [ ] **Step 3: App 加 cursors map + 改 execute_operation/render**

`src/app/mod.rs`：

3a. `App` struct 加字段（在 `scene: Scene,` 后、`engine: TaffyEngine,` 前）：

```rust
pub struct App {
    contents: HashMap<ContentId, Box<dyn ContentHandler>>,
    cursors: HashMap<ContentId, Cursors>,
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
```

3b. `use` 区加 `use crate::core::content::Cursors;`（在 `use crate::core::content::{ContentHandler, ContentLookup};` 改为 `use crate::core::content::{ContentHandler, ContentLookup, Cursors};`）。

3c. `App::new` 初始化 cursors。在 `let (scene, editor_space) = build_editor_scene(...)` 后、`Ok(Self {` 前加：

```rust
        let mut cursors: HashMap<ContentId, Cursors> = HashMap::new();
        cursors.insert(editor_content, Cursors::single(CursorPos::origin()));
        cursors.insert(status_content, Cursors::single(CursorPos::origin()));
```

并在 `Ok(Self { ... })` 的字段列表加 `cursors,`（在 `contents,` 后）。

3d. `use` 区加 `use crate::protocol::cursor::CursorPos;`。

3e. `execute_operation` 改：把 `let space: &mut Space = &mut self.scene.node_mut(self.focused).space; executor::execute(op, content, space);` 改为：

```rust
                let cid = self.focused_content_id();
                let content: &mut dyn ContentHandler = self
                    .contents
                    .get_mut(&cid)
                    .map(|b| b.as_mut())
                    .expect("focused content exists");
                let cursors = self
                    .cursors
                    .get_mut(&cid)
                    .expect("focused cursor exists");
                executor::execute(op, content, cursors);
```

（即把原 `let cid = self.focused_content_id();` 那段替换。注意原代码 `let cid` 在 `_ =>` 分支开头已声明，现在保留声明，删掉 `let space` 行，改 executor 调用。）

完整 `_ =>` 分支：

```rust
            _ => {
                let cid = self.focused_content_id();
                let content: &mut dyn ContentHandler = self
                    .contents
                    .get_mut(&cid)
                    .map(|b| b.as_mut())
                    .expect("focused content exists");
                let cursors = self
                    .cursors
                    .get_mut(&cid)
                    .expect("focused cursor exists");
                executor::execute(op, content, cursors);
            }
```

3f. `render` 改读 cursors map。把 `let focused_cursor = self.scene.node(self.focused).space.cursors.primary;` 改为：

```rust
        let focused_cursor = self
            .cursors
            .get(&focused_cid)
            .map(|c| c.primary)
            .unwrap_or_else(CursorPos::origin);
```

3g. 删 `use crate::layout::space::{Space, SpaceKind};` 改为 `use crate::layout::space::SpaceKind;`（Space 不再被 execute_operation 用）。检查 `focused_content_id` 仍用 `SpaceKind`，保留。

- [ ] **Step 4: 跑测试**

Run: `cargo test`
Expected: 全绿。executor 测试改用 Cursors，app 集成测试通过（cursor 经 cursors map 流转）。若有 `space.cursors` 残留引用，编译错误指向它，按提示修复（render 的 viewport 跟随段仍读 `space.cursors.primary`？已改 focused_cursor；viewport 跟随段 `let space = &mut self.scene.node_mut(self.focused).space; let row = space.cursors.primary.row;` 也要改——见 Step 5）。

- [ ] **Step 5: 修 render 的 viewport 跟随段**

`App::render` 中：

```rust
        let resolved = self.engine.layout(&self.scene);
        let focused_cid = self.focused_content_id();
        if let Some(item) = resolved.items.iter().find(|i| i.content_id == focused_cid) {
            let space = &mut self.scene.node_mut(self.focused).space;
            let row = space.cursors.primary.row;
            space.viewport.ensure_cursor_visible(row, item.rect.height as usize);
        }
```

改为（cursor 从 cursors map 读，viewport 仍在 Space——本任务暂保留 Space.viewport，下个任务随 Space 瘦身删）：

```rust
        let resolved = self.engine.layout(&self.scene);
        let focused_cid = self.focused_content_id();
        if let Some(item) = resolved.items.iter().find(|i| i.content_id == focused_cid) {
            let row = self
                .cursors
                .get(&focused_cid)
                .map(|c| c.primary.row)
                .unwrap_or(0);
            let space = &mut self.scene.node_mut(self.focused).space;
            space.viewport.ensure_cursor_visible(row, item.rect.height as usize);
        }
```

- [ ] **Step 6: 跑测试全绿**

Run: `cargo test`
Expected: 全绿

- [ ] **Step 7: Commit**

```bash
git add src/app/mod.rs src/app/executor.rs src/core/operation.rs
git commit -m "refactor(app): cursor 迁 App 层 cursors map + executor 签名改 + 删 ViewportScrollBy"
```

---

## Task 4: Space 瘦身 + space/scene 下沉 protocol + 删 layout 层

`Space` 删 `viewport`/`cursors`/`wrap_mode` 字段。`layout/space.rs` → `protocol/space.rs`，`layout/scene.rs` → `protocol/scene.rs`，`layout/ids.rs` 合入 `protocol/ids.rs`，删 `layout/` 层。所有 `crate::layout::scene`/`crate::layout::space`/`crate::layout::ids` import 改 `crate::protocol::scene`/`crate::protocol::space`/`crate::protocol::ids`。

**Files:**
- Create: `src/protocol/space.rs`
- Create: `src/protocol/scene.rs`
- Delete: `src/layout/space.rs`, `src/layout/scene.rs`, `src/layout/taffy_engine.rs`, `src/layout/resolved.rs`, `src/layout/mod.rs`, `src/layout/ids.rs`
- Modify: `src/protocol/mod.rs`, `src/protocol/ids.rs`, `src/main.rs`, `src/app/mod.rs`, `src/app/executor.rs`, `src/app/dispatcher.rs`, `src/frame/mod.rs`, `src/tui/tui_frontend.rs`, `src/core/content.rs`（仅 import 修正）

- [ ] **Step 1: 写 protocol/space.rs（瘦身版）**

```rust
//! 空间节点：布局意图。纯数据，前后端共享。不含 viewport/cursor（前端持）。

use crate::protocol::ids::{ContentId, SpaceId};

pub struct Space {
    #[allow(dead_code)] // 结构性 identity 字段
    pub id: SpaceId,
    pub kind: SpaceKind,
    pub sizing: Sizing,
    pub layer: Layer,
}

pub enum SpaceKind {
    Container { arrangement: Arrangement, children: Vec<SpaceId> },
    Host { content: ContentId },
}

pub enum Arrangement {
    Flex { direction: Axis, gap: i32, align: Align },
}

#[allow(dead_code)] // Vertical 现用，Horizontal 预留
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Axis { Horizontal, Vertical }

#[allow(dead_code)] // Stretch 现用，Start/Center/End 预留
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Align { Stretch, Start, Center, End }

pub enum Sizing {
    Fixed(i32),
    Grow(u32),
}

#[repr(i32)]
#[allow(dead_code)] // Base 现用，Overlay/Modal/Debug 预留
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

- [ ] **Step 2: 写 protocol/scene.rs**

从 `src/layout/scene.rs` 复制全部内容，改动：
- 顶部 `use` 改：`use crate::protocol::geometry::{Rect, Size as SceneSize};` + `use crate::protocol::space::{Align, Arrangement, Axis, Layer, Sizing, Space, SpaceKind};`（删 `use crate::layout::space::*` 和 `use crate::layout::ids::*`，改为 `use crate::protocol::ids::{ContentId, SpaceId};`）
- 删 `use crate::core::content::{Cursors, WrapMode};`
- 删 `use crate::protocol::cursor::CursorPos;` 和 `use crate::protocol::viewport::Viewport;`（不再用）
- `SpaceNode` 不变
- `Scene` 不变
- `SceneBuilder::alloc` 里构造 `Space { id, kind, sizing, layer }`——删 `viewport: Viewport::origin(),`、`cursors: Cursors::single(...)`、`wrap_mode: WrapMode::None,` 三行。注意 `Space` 瘦身后无这些字段。

完整 alloc 方法：

```rust
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
            },
        };
        self.nodes.insert(id, node);
        id
    }
```

tests 模块 `build_editor_scene_has_two_hosts` 不变（不用 viewport/cursor）。`rect_intersect` 测试已移到 geometry.rs，这里不重复。

注意：原 scene.rs 顶部 `pub use crate::protocol::geometry::{Point, Rect, Size};` 这行 re-export 要保留吗？其他文件经 `crate::layout::scene::Rect` 引用——但本任务删 layout 层，所有引用改 protocol。所以 scene.rs 不需 re-export geometry。但 scene.rs 自身用 `Rect`（`SpaceNode`? 不，scene.rs 的 Rect 用在 build_editor_scene? 不用。Rect 用在 resolved.rs/taffy_engine.rs）。scene.rs 实际不用 Rect/Point，只用 Size（build_editor_scene 参数 `Size { width, height }`）。所以 `use crate::protocol::geometry::Size;` 即可。

简化 scene.rs use：
```rust
use std::collections::HashSet;
use std::collections::HashMap;

use crate::protocol::geometry::Size;
use crate::protocol::ids::{ContentId, SpaceId};
use crate::protocol::space::{Align, Arrangement, Axis, Sizing, Space, SpaceKind};
```

（删掉 `use crate::layout::ids::*` 注释行、`use crate::core::content::*`、cursor、viewport。）

- [ ] **Step 3: protocol/mod.rs 注册 + 删 edit_view/frame 暂留**

加 `pub mod scene;` 和 `pub mod space;`（按字母序插入）：

```rust
pub mod content_query;
pub mod cursor;
pub mod edit_view;
pub mod frame;
pub mod frontend_event;
pub mod geometry;
pub mod ids;
pub mod key_event;
pub mod scene;
pub mod space;
pub mod status;
pub mod viewport;
```

- [ ] **Step 4: 删 layout/ 层 + main.rs 改**

删除文件：`src/layout/mod.rs`、`src/layout/scene.rs`、`src/layout/space.rs`、`src/layout/ids.rs`、`src/layout/taffy_engine.rs`、`src/layout/resolved.rs`。

`src/main.rs` 删 `mod layout;` 行。

- [ ] **Step 5: 移 taffy_engine.rs / resolved.rs 到 tui/**

`src/layout/taffy_engine.rs` 移到 `src/tui/taffy_engine.rs`，改 use：
- `use crate::layout::ids::SpaceId;` → `use crate::protocol::ids::SpaceId;`
- `use crate::layout::resolved::{RenderItem, ResolvedScene};` → `use crate::tui::resolved::{RenderItem, ResolvedScene};`
- `use crate::layout::scene::{Rect, Scene, Size as SceneSize, SpaceNode};` → `use crate::protocol::geometry::Rect; use crate::protocol::scene::{Scene, SpaceNode}; use crate::protocol::geometry::Size as SceneSize;`
- `use crate::layout::space::{Align, Arrangement, Axis, Sizing, SpaceKind};` → `use crate::protocol::space::{Align, Arrangement, Axis, Sizing, SpaceKind};`
- `use crate::protocol::edit_view::SpaceState;` → 暂留（resolved.rs 的 RenderItem 用 SpaceState，下个任务删。本任务保留 SpaceState 字段，但 SpaceState 在 protocol/edit_view.rs 仍在）
- tests 里 `use crate::layout::scene::build_editor_scene;` → `use crate::protocol::scene::build_editor_scene;`

`src/layout/resolved.rs` 移到 `src/tui/resolved.rs`，改 use：
- `use crate::layout::scene::Rect;` → `use crate::protocol::geometry::Rect;`
- 其余 use 不变（Layer/SpaceState/ContentId 已在 protocol）
- tests 不变

`tui/mod.rs` 加 `pub mod resolved; pub mod taffy_engine;`（在 `pub mod tui_frontend;` 前）。

- [ ] **Step 6: 全局 import 修正**

搜索所有 `crate::layout::` 引用，替换：
- `crate::layout::scene` → `crate::protocol::scene`
- `crate::layout::space` → `crate::protocol::space`
- `crate::layout::ids` → `crate::protocol::ids`
- `crate::layout::taffy_engine` → `crate::tui::taffy_engine`
- `crate::layout::resolved` → `crate::tui::resolved`

受影响文件（基于 grep）：
- `src/app/mod.rs`：`use crate::layout::scene::{build_editor_scene, Scene};` → `use crate::protocol::scene::{build_editor_scene, Scene};`；`use crate::layout::taffy_engine::TaffyEngine;` → `use crate::tui::taffy_engine::TaffyEngine;`
- `src/app/dispatcher.rs`：`use crate::layout::scene::Scene;` → `use crate::protocol::scene::Scene;`；`use crate::layout::space::SpaceKind;` → `use crate::protocol::space::SpaceKind;`；tests `use crate::layout::scene::build_editor_scene;` → `use crate::protocol::scene::build_editor_scene;`
- `src/app/executor.rs`：tests 里 `use crate::layout::space::{Layer, Sizing, Space, SpaceKind};` → `use crate::protocol::space::{Layer, Sizing, Space, SpaceKind};`；`use crate::protocol::viewport::Viewport;` 删（Space 不再含 viewport，space_with 测试已删——见 Step 7）
- `src/frame/mod.rs`：`use crate::layout::resolved::ResolvedScene;` → `use crate::tui::resolved::ResolvedScene;`；tests `use crate::layout::scene::build_editor_scene;` → `use crate::protocol::scene::build_editor_scene;`；`use crate::layout::taffy_engine::TaffyEngine;` → `use crate::tui::taffy_engine::TaffyEngine;`
- `src/tui/tui_frontend.rs`：无 layout 引用（用 protocol::frame），不改
- `src/core/content.rs`：无 layout 引用（已用 protocol），不改

- [ ] **Step 7: executor tests 修复（space_with 已删）**

Task 3 已把 executor tests 改为不构造 Space。但若 executor.rs tests 仍残留 `use crate::layout::space::*` 或 `Viewport`，删掉这些未用 import。Task 3 的 executor.rs 全文已不含 space_with，确认无残留。

- [ ] **Step 8: 跑测试**

Run: `cargo test`
Expected: 全绿。若有编译错误，多半是遗漏的 `crate::layout::` 引用或 Space 字段残留——按错误提示修复（grep `layout::` 和 `space.viewport`/`space.cursors`/`space.wrap_mode` 确认清零）。

- [ ] **Step 9: Commit**

```bash
git add -A
git commit -m "refactor: Space 瘦身 + space/scene 下沉 protocol + 删 layout 层 + taffy_engine/resolved 移 tui"
```

---

## Task 5: App impl ContentQuery

> **执行说明：本任务只执行 Step 7-10（下方）。Step 1-3 是 ContentHandler 瘦身的代码全文，预置在此供 Task 8 Step 1-3 引用，本任务不要执行 Step 1-6。**

后端建查询能力。ContentHandler 暂保留 render（Task 8 删）。`status_bar` 临时经 render 路径取数据，Task 8 改用 `status_bar_data`。

**Files:**
- Modify: `src/app/mod.rs`

- [ ] **Step 1: 写 content.rs（瘦身版）**

`src/core/content.rs` 全文替换为：

```rust
use std::borrow::Cow;

use crate::core::buffer::Buffer;
use crate::core::keymap::Keymap;
use crate::core::operation::Operation;
use crate::core::status_bar::StatusBar;
use crate::protocol::content_query::StatusBarData;
use crate::protocol::cursor::CursorPos;
use crate::protocol::ids::ContentId;
use crate::protocol::key_event::KeyEvent;
use crate::protocol::status::StatusMessage;

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
}
```

注意：删 `WrapMode`、`RenderCtx`、`ContentHandler::{line,len_lines,file_name,modified,status,render}`。`StatusBar` 的 import 加（as_status_bar 返回类型）。`StatusBarData` import 加（status_bar_data 用）。

- [ ] **Step 2: buffer.rs 删 render impl + ContentHandler impl 修正**

`src/core/buffer.rs`：
- 删 `use crate::core::content::{ContentHandler, RenderCtx};` 改为 `use crate::core::content::ContentHandler;`
- 删 `use crate::protocol::frame::FrameContent;`
- 删 `impl ContentHandler for Buffer` 里的 `line`/`len_lines`/`file_name`/`modified`/`status`/`render` 方法（保留 `keymap`/`keymap_mut`/`default_binding`/`buffer_mut`），加 `as_buffer`：

```rust
impl ContentHandler for Buffer {
    fn keymap(&self) -> &Keymap { &self.keymap }
    fn keymap_mut(&mut self) -> &mut Keymap { &mut self.keymap }
    fn default_binding(&self, key: KeyEvent) -> Option<Operation> {
        match key {
            KeyEvent::Char(ch) => Some(Operation::CursorInsertText((ch as char).to_string())),
            _ => None,
        }
    }
    fn buffer_mut(&mut self) -> Option<&mut Buffer> { Some(self) }
    fn as_buffer(&self) -> Option<&Buffer> { Some(self) }
}
```

- Buffer 固有方法 `line`/`len_lines`/`file_name`/`modified`/`status` 保留（ContentQuery impl 用）。注意原 `line` 固有方法不存在——原 `line` 是 ContentHandler trait 方法。加固有方法：

在 `impl Buffer` 固有方法区（`slice`/`path` 后）加：

```rust
    pub fn line(&self, idx: usize) -> Cow<str> {
        Cow::Owned(self.slice().line(idx).to_string())
    }
```

（`len_lines`/`file_name`/`modified`/`status` 固有方法已存在。）

- 删 tests 里 `render` 相关测试（无）。确认 `default_binding_char_to_insert`/`default_binding_non_char_is_none`/`buffer_mut_returns_self` 通过。

- [ ] **Step 3: status_bar.rs 删 render + 加 status_bar_data**

`src/core/status_bar.rs` 全文替换为：

```rust
use crate::core::content::{ContentHandler, ContentLookup};
use crate::core::keymap::Keymap;
use crate::protocol::content_query::StatusBarData;
use crate::protocol::ids::ContentId;
use crate::protocol::status::StatusMessage;

/// 状态栏 content：观察 target_content_id 指向的 content，查询时主动查其
/// file_name/modified/status。自身不持显示数据，只持指针 + 空 keymap。
pub struct StatusBar {
    target_content_id: ContentId,
    keymap: Keymap,
}

impl StatusBar {
    pub fn new(target_content_id: ContentId) -> Self {
        Self { target_content_id, keymap: Keymap::new() }
    }
    #[allow(dead_code)] // 测试用
    pub fn target_content_id(&self) -> ContentId {
        self.target_content_id
    }
    /// 产状态栏显示数据：查 target content 的 file_name/modified/status。
    pub fn status_bar_data(&self, lookup: &dyn ContentLookup) -> StatusBarData {
        let target = lookup.get(self.target_content_id);
        StatusBarData {
            file_name: target.and_then(|c| c.as_buffer()).and_then(|b| b.file_name().map(|s| s.to_string())),
            modified: target.and_then(|c| c.as_buffer()).map(|b| b.modified()).unwrap_or(false),
            message: target.and_then(|c| c.as_buffer()).map(|b| b.status()).unwrap_or(StatusMessage::None),
        }
    }
}

impl ContentHandler for StatusBar {
    fn keymap(&self) -> &Keymap { &self.keymap }
    fn keymap_mut(&mut self) -> &mut Keymap { &mut self.keymap }
    fn as_status_bar(&self) -> Option<&StatusBar> { Some(self) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::buffer::Buffer;
    use crate::protocol::ids::ContentId;

    fn lookup_with(buf: &Buffer, target: ContentId) -> impl ContentLookup {
        struct L<'a> { buf: &'a Buffer, target: ContentId }
        impl<'a> ContentLookup for L<'a> {
            fn get(&self, id: ContentId) -> Option<&dyn ContentHandler> {
                if id == self.target { Some(self.buf) } else { None }
            }
        }
        L { buf, target }
    }

    #[test]
    fn status_bar_data_target_missing_defaults() {
        let sb = StatusBar::new(ContentId(0));
        let buf = Buffer::new();
        let data = sb.status_bar_data(&lookup_with(&buf, ContentId(9)));
        assert!(data.file_name.is_none());
        assert!(!data.modified);
        assert_eq!(data.message, StatusMessage::None);
    }

    #[test]
    fn target_content_id_stored() {
        let sb = StatusBar::new(ContentId(7));
        assert_eq!(sb.target_content_id(), ContentId(7));
    }
}
```

注意：`status_bar_data` 经 `as_buffer` 查 target 的 file_name/modified/status。这要求 target content 是 Buffer（as_buffer Some）。若 target 是其他类型，返回 default。

- [ ] **Step 4: 跑测试**

Run: `cargo test`
Expected: 编译错误——`frame/mod.rs` 的 `build_frame` 用 `ContentHandler::render`/`RenderCtx`，已删。这是预期，下个任务删 build_frame。暂时让 frame/mod.rs 编译过：注释掉 build_frame 体？不行。直接在 Step 5 删 frame 层。

修正：本任务必须同时处理 frame/mod.rs（它依赖删掉的 render）。把"删 frame 层"合到本任务。

- [ ] **Step 5: 删 frame/ 层**

删除 `src/frame/mod.rs` + `src/frame/` 目录。`src/main.rs` 删 `mod frame;` 行。

`src/app/mod.rs` 删 `use crate::frame::build_frame;`。`App::render` 当前调 `build_frame`——但 render 在下个任务（Task 6）重写。本任务先把 render 里的 build_frame 调用注释/临时改为返回 Ok？不行，要编译。

实际上 Task 3 后 `App::render` 仍是：

```rust
        let frame = build_frame(&resolved, &self.contents as &dyn ContentLookup, focused_cid, Some(focused_cursor));
        self.frontend.render(&frame)
```

`build_frame` 删了 → 编译失败。所以本任务必须同时改 `App::render` 不用 build_frame。但前端还没 SceneRenderer（Task 7）。

解法：本任务临时把 `App::render` 改为调 `self.frontend.render(&frame)` 的 frame 用一个临时空 Frame？但 Frame 也快删。

重新排序：**先建 SceneRenderer（Task 7）再删 frame**。但 SceneRenderer 需要 ContentQuery impl（Task 6）。

正确顺序：Task 5（ContentHandler 瘦身）不能先于 build_frame 删除，因为 build_frame 用 render。所以 Task 5 + 删 frame + App::render 切换要一起。

修正任务依赖：把"删 frame 层 + App::render 切换"提到 ContentHandler 瘦身之前？但 App::render 切换需要 SceneRenderer + ContentQuery。

最终正确顺序（修订）：
- Task 5: App impl ContentQuery（先建查询能力，ContentHandler 暂保留 render）
- Task 6: 建 SceneRenderer + HeadlessFrontend（前端新能力，暂不接 App）
- Task 7: Frontend trait 切换 + App::render 切到 frontend.render(&scene,&query,focused) + TuiFrontend 改用 SceneRenderer
- Task 8: 删 frame 层 + protocol/frame.rs + ContentHandler 瘦身（render/RenderCtx）+ protocol/edit_view.rs

这样删 render 在最后，那时 build_frame 已不在。

回到本计划：**Task 5 改为 App impl ContentQuery**（ContentHandler 保留 render）。ContentHandler 瘦身 + 删 frame 放到最后（Task 8）。

（本 Task 5 的 Step 1-3 内容移到 Task 8。本 Task 5 重写为 ContentQuery impl。）

- [ ] **Step 6: 回滚本任务的 content.rs/buffer.rs/status_bar.rs 改动**

本任务（按修订）不改 content.rs/buffer.rs/status_bar.rs。回滚 Step 1-3 的改动（保持 render impl 不变）。重新执行下面的 Step 7。

- [ ] **Step 7: 写 App impl ContentQuery**

`src/app/mod.rs` 加 `impl ContentQuery for App`。需要 `use crate::protocol::content_query::{ContentQuery, RowRange, StatusBarData};`。

但 ContentQuery::lines 需要 Buffer::line 固有方法——当前 `line` 是 ContentHandler trait 方法（`fn line(&self, idx) -> Cow<str>`），Buffer impl 了它。App 持 `Box<dyn ContentHandler>`，可调 `c.line(idx)`。OK，ContentHandler::line 暂保留（Task 8 删）。同理 `len_lines`/`file_name`/`modified`/`status` 都是 trait 方法，App 经 `&dyn ContentHandler` 调。

但 `as_buffer`/`as_status_bar` 还没加（Task 8 加）。本任务用 ContentHandler 现有 trait 方法（line/len_lines/file_name/modified/status）impl ContentQuery。

`src/app/mod.rs` 加：

```rust
impl ContentQuery for App {
    fn lines(&self, cid: ContentId, range: RowRange) -> Vec<String> {
        let Some(c) = self.contents.get(&cid) else { return Vec::new() };
        let total = c.len_lines();
        let start = range.start.min(total);
        let end = range.end.min(total).max(start);
        (start..end)
            .map(|i| c.line(i).trim_end_matches('\n').to_string())
            .collect()
    }
    fn status_bar(&self, cid: ContentId) -> StatusBarData {
        let Some(c) = self.contents.get(&cid) else {
            return StatusBarData { file_name: None, modified: false, message: StatusMessage::None };
        };
        // StatusBar content 经 render 拿数据（暂复用 RenderCtx 路径）
        // 临时：直接查 target（StatusBar 无 target 字段公开，用 render）
        let ctx = RenderCtx {
            lookup: &self.contents as &dyn ContentLookup,
            focused_content_id: cid,
            state: SpaceState { viewport: Viewport::origin(), cursor: CursorPos::origin() },
            rect_height: 1,
        };
        match c.render(&ctx) {
            FrameContent::StatusBar { file_name, modified, message } => StatusBarData { file_name, modified, message },
            _ => StatusBarData { file_name: None, modified: false, message: StatusMessage::None },
        }
    }
    fn cursor(&self, cid: ContentId) -> CursorPos {
        self.cursors.get(&cid).map(|c| c.primary).unwrap_or_else(CursorPos::origin)
    }
    fn line_count(&self, cid: ContentId) -> usize {
        self.contents.get(&cid).map(|c| c.len_lines()).unwrap_or(0)
    }
}
```

需要的 use（app/mod.rs 顶部）：
```rust
use crate::core::content::ContentLookup;  // 已有
use crate::protocol::content_query::{ContentQuery, RowRange, StatusBarData};
use crate::protocol::edit_view::SpaceState;  // 已有？检查
use crate::protocol::frame::FrameContent;     // 暂用，Task 8 删
use crate::core::content::RenderCtx;          // 暂用，Task 8 删
```

注意：`status_bar` 临时经 render 拿数据（因 StatusBar 的 target_content_id 不公开，且 status_bar_data 方法 Task 8 才加）。Task 8 删 render 后改用 `as_status_bar().status_bar_data()`。

- [ ] **Step 8: 写 ContentQuery 测试**

`src/app/mod.rs` tests 加：

```rust
    #[test]
    fn content_query_lines_and_cursor() {
        let mut app = make_app(vec![], None);
        // 插入一行
        let buf = app.contents.get_mut(&editor_cid()).and_then(|c| c.buffer_mut()).unwrap();
        buf.insert_char(0, 'h');
        buf.insert_char(1, 'i');
        let lines = ContentQuery::lines(&app, editor_cid(), RowRange { start: 0, end: 5 });
        assert_eq!(lines, vec!["hi".to_string()]);
        assert_eq!(ContentQuery::line_count(&app, editor_cid()), 1);
        assert_eq!(ContentQuery::cursor(&app, editor_cid()), CursorPos::origin());
    }
```

加 use：`use crate::protocol::content_query::{ContentQuery, RowRange};`（tests 内）+ `use crate::protocol::cursor::CursorPos;`。

- [ ] **Step 9: 跑测试**

Run: `cargo test`
Expected: 全绿（ContentQuery impl + 原 build_frame 路径并存，render 仍工作）

- [ ] **Step 10: Commit**

```bash
git add -A
git commit -m "feat(app): App impl ContentQuery（lines/cursor/line_count/status_bar 暂经 render）"
```

---

## Task 6: 建 tui/scene_renderer.rs + tui/headless.rs

`SceneRenderer` 持 `TaffyEngine` + per-space `Viewport` 缓存，`render(scene, query, focused, canvas)` = layout → ensure viewport → pull 可见行 → paint。`HeadlessFrontend` = `SceneRenderer` + `Output<Vec<u8>>`。

**Files:**
- Create: `src/tui/scene_renderer.rs`
- Create: `src/tui/headless.rs`
- Modify: `src/tui/mod.rs`

- [ ] **Step 1: 写 scene_renderer.rs**

```rust
//! 前端核心：layout（TaffyEngine）+ viewport 跟随 + pull 可见行 + paint 到 Canvas。
//! TuiFrontend 与 HeadlessFrontend 共用，只差 output 后端。

use std::collections::HashMap;
use std::io;

use crate::protocol::content_query::{ContentQuery, RowRange};
use crate::protocol::cursor::CursorPos;
use crate::protocol::ids::{ContentId, SpaceId};
use crate::protocol::scene::Scene;
use crate::protocol::space::SpaceKind;
use crate::protocol::status::StatusMessage;
use crate::protocol::viewport::Viewport;
use crate::terminal::output::Canvas;
use crate::tui::resolved::ResolvedScene;
use crate::tui::taffy_engine::TaffyEngine;

pub struct SceneRenderer {
    engine: TaffyEngine,
    viewports: HashMap<SpaceId, Viewport>,
}

impl SceneRenderer {
    pub fn new() -> Self {
        Self { engine: TaffyEngine::new(), viewports: HashMap::new() }
    }

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
        if let Some(cid) = focused_cid {
            if let Some(item) = resolved.items.iter().find(|i| i.content_id == cid) {
                let cursor = query.cursor(cid);
                let vp = self.viewports.entry(focused).or_insert_with(Viewport::origin);
                vp.ensure_cursor_visible(cursor.row, item.rect.height as usize);
            }
        }
        // 逐 Host item paint
        for item in &resolved.items {
            paint_item(item, scene, query, &self.viewports, canvas)?;
        }
        // 焦点光标定位
        if let Some(cid) = focused_cid {
            let cursor = query.cursor(cid);
            if let Some(item) = resolved.items.iter().find(|i| i.content_id == cid) {
                let vp = self.viewports.get(&focused).copied().unwrap_or_else(Viewport::origin);
                let screen_row = cursor.row.saturating_sub(vp.top_row) + item.rect.y as usize;
                let screen_col = cursor.col.saturating_sub(vp.left_col) + item.rect.x as usize;
                canvas.move_cursor(screen_row, screen_col)?;
                canvas.show_cursor()?;
            }
        }
        canvas.flush()
    }
}

impl Default for SceneRenderer {
    fn default() -> Self { Self::new() }
}

fn focused_content_id(scene: &Scene, focused: SpaceId) -> Option<ContentId> {
    let node = scene.node(focused);
    match &node.space.kind {
        SpaceKind::Host { content } => Some(*content),
        _ => None,
    }
}

fn paint_item(
    item: &crate::tui::resolved::RenderItem,
    scene: &Scene,
    query: &dyn ContentQuery,
    viewports: &HashMap<SpaceId, Viewport>,
    canvas: &mut dyn Canvas,
) -> io::Result<()> {
    // 找 item 对应的 space（经 scene 节点匹配 content_id）
    let sid = match find_space_by_content(scene, item.content_id) {
        Some(s) => s,
        None => return Ok(()),
    };
    let vp = viewports.get(&sid).copied().unwrap_or_else(Viewport::origin);
    // 区分 editor vs status_bar：line_count > 0 视为 editor（buffer），否则 status_bar
    let line_count = query.line_count(item.content_id);
    if line_count > 0 {
        // editor：拉可见行
        let height = item.rect.height as usize;
        let start = vp.top_row;
        let lines = query.lines(item.content_id, RowRange { start, end: start + height });
        for (row, line) in lines.iter().enumerate() {
            let screen_row = (item.rect.y + row as i32) as usize;
            canvas.move_cursor(screen_row, item.rect.x as usize)?;
            canvas.clear_line()?;
            canvas.write_str(line)?;
        }
        // 缺位画空行（clear_line 已清）
        for row in lines.len()..height {
            let screen_row = (item.rect.y + row as i32) as usize;
            canvas.move_cursor(screen_row, item.rect.x as usize)?;
            canvas.clear_line()?;
        }
    } else {
        // status_bar：拉状态栏数据
        let data = query.status_bar(item.content_id);
        let screen_row = item.rect.y as usize;
        canvas.move_cursor(screen_row, item.rect.x as usize)?;
        canvas.clear_line()?;
        canvas.write_str(&status_line(data.file_name.as_deref(), data.modified, &data.message))?;
    }
    Ok(())
}

fn find_space_by_content(scene: &Scene, cid: ContentId) -> Option<SpaceId> {
    // 简单遍历（v0.2 节点少）；未来可建索引
    // Scene 无公开迭代器，用 root DFS
    fn dfs(scene: &Scene, sid: SpaceId, cid: ContentId) -> Option<SpaceId> {
        let node = scene.node(sid);
        match &node.space.kind {
            SpaceKind::Host { content } if *content == cid => Some(sid),
            SpaceKind::Container { children, .. } => {
                for c in children {
                    if let Some(found) = dfs(scene, *c, cid) { return Some(found); }
                }
                None
            }
            _ => None,
        }
    }
    dfs(scene, scene.root, cid)
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
```

注意：`Canvas` trait 当前不含 `hide_cursor`/`show_cursor`/`flush`（这些在 `Output<W>` 固有方法）。需扩展 `Canvas` trait 加这三个方法。见 Step 2。

- [ ] **Step 2: 扩展 Canvas trait**

`src/terminal/output.rs` 的 `Canvas` trait 加方法：

```rust
pub trait Canvas {
    fn move_cursor(&mut self, row: usize, col: usize) -> io::Result<()>;
    fn clear_line(&mut self) -> io::Result<()>;
    fn write_str(&mut self, s: &str) -> io::Result<()>;
    fn hide_cursor(&mut self) -> io::Result<()>;
    fn show_cursor(&mut self) -> io::Result<()>;
    fn flush(&mut self) -> io::Result<()>;
}

impl<W: Write> Canvas for Output<W> {
    fn move_cursor(&mut self, row: usize, col: usize) -> io::Result<()> { Output::move_cursor(self, row, col) }
    fn clear_line(&mut self) -> io::Result<()> { Output::clear_line(self) }
    fn write_str(&mut self, s: &str) -> io::Result<()> { Output::write_str(self, s) }
    fn hide_cursor(&mut self) -> io::Result<()> { Output::hide_cursor(self) }
    fn show_cursor(&mut self) -> io::Result<()> { Output::show_cursor(self) }
    fn flush(&mut self) -> io::Result<()> { Output::flush(self) }
}
```

更新 output.rs 顶部注释（Canvas 现含 hide/show/flush）。output.rs tests 不变。

- [ ] **Step 3: 写 headless.rs**

```rust
//! HeadlessFrontend：SceneRenderer + Output<Vec<u8>>。测试/未来 headless 模式用。
//! 捕获每帧 VT 字节快照供测试断言。

use std::io;
use std::collections::VecDeque;

use crate::app::Frontend;
use crate::protocol::frontend_event::FrontendEvent;
use crate::protocol::ids::SpaceId;
use crate::protocol::scene::Scene;
use crate::terminal::output::Output;
use crate::tui::scene_renderer::SceneRenderer;

pub struct HeadlessFrontend {
    events: VecDeque<FrontendEvent>,
    renderer: SceneRenderer,
    out: Output<Vec<u8>>,
    pub frames: Vec<Vec<u8>>,
}

impl HeadlessFrontend {
    pub fn new(events: Vec<FrontendEvent>) -> Self {
        Self {
            events: events.into(),
            renderer: SceneRenderer::new(),
            out: Output::new(Vec::new()),
            frames: Vec::new(),
        }
    }
}

impl Frontend for HeadlessFrontend {
    async fn next_event(&mut self) -> io::Result<Option<FrontendEvent>> {
        Ok(self.events.pop_front())
    }
    fn render(&mut self, scene: &Scene, query: &dyn crate::protocol::content_query::ContentQuery, focused: SpaceId) -> io::Result<()> {
        self.renderer.render(scene, query, focused, &mut self.out as &mut dyn crate::terminal::output::Canvas)?;
        let bytes = std::mem::take(Output::get_mut(&mut self.out));
        self.frames.push(bytes);
        Ok(())
    }
}
```

注意：`Output<W>` 需暴露 `get_mut` 或 `into_inner`。`into_inner` 消耗 self，不适合。加 `pub fn get_mut(&mut self) -> &mut W` 或 `pub fn buffer(&self) -> &[u8]`。Step 4 加。

- [ ] **Step 4: Output 加 get_mut**

`src/terminal/output.rs` 的 `impl<W: Write> Output<W>` 加：

```rust
    pub fn get_mut(&mut self) -> &mut W {
        &mut self.out
    }
```

- [ ] **Step 5: tui/mod.rs 注册**

`src/tui/mod.rs`：

```rust
pub mod headless;
pub mod resolved;
pub mod scene_renderer;
pub mod taffy_engine;
pub mod tui_frontend;
```

- [ ] **Step 6: 写 SceneRenderer 测试**

`src/tui/scene_renderer.rs` 末尾加 tests：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::buffer::Buffer;
    use crate::core::content::{ContentHandler, ContentLookup};
    use crate::protocol::content_query::{ContentQuery, RowRange, StatusBarData};
    use crate::protocol::cursor::CursorPos;
    use crate::protocol::ids::ContentId;
    use crate::protocol::scene::build_editor_scene;
    use crate::protocol::status::StatusMessage;
    use crate::terminal::output::Output;
    use std::collections::HashMap;

    struct StubQuery {
        lines: Vec<String>,
        cursor: CursorPos,
    }
    impl ContentQuery for StubQuery {
        fn lines(&self, _cid: ContentId, range: RowRange) -> Vec<String> {
            self.lines.iter().skip(range.start).take(range.end.saturating_sub(range.start)).cloned().collect()
        }
        fn status_bar(&self, _cid: ContentId) -> StatusBarData {
            StatusBarData { file_name: Some("f.txt".to_string()), modified: false, message: StatusMessage::None }
        }
        fn cursor(&self, _cid: ContentId) -> CursorPos { self.cursor }
        fn line_count(&self, _cid: ContentId) -> usize { self.lines.len() }
    }

    #[test]
    fn renders_editor_lines_and_status() {
        let (scene, ed) = build_editor_scene(40, 5, ContentId(0), ContentId(1));
        let query = StubQuery {
            lines: vec!["hello".to_string(), "world".to_string()],
            cursor: CursorPos::origin(),
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
            lines,
            cursor: CursorPos { char_index: 0, row: 25, col: 0 },
        };
        let mut r = SceneRenderer::new();
        let mut out = Output::new(Vec::new());
        r.render(&scene, &query, ed, &mut out as &mut dyn Canvas).unwrap();
        let s = String::from_utf8(out.get_mut().clone()).unwrap();
        // cursor row 25, height 4 → top_row=22 → 应见 line22..line25
        assert!(s.contains("line25"), "{s}");
        assert!(!s.contains("line0"), "{s}");
    }
}
```

注意：第二个测试用 `out.get_mut().clone()` 取字节（不消耗 out）。或改用 `into_inner` 后断言（消耗 out OK，测试结束）。简化：第二个测试也用 `out.into_inner()`。

- [ ] **Step 7: 跑测试**

Run: `cargo test scene_renderer`
Expected: 两个测试通过。若 `find_space_by_content` 借用问题，调整（Scene::node 返回 &SpaceNode，DFS 借用 scene 不可变 OK）。

- [ ] **Step 8: Commit**

```bash
git add -A
git commit -m "feat(tui): SceneRenderer（layout+viewport+pull+paint）+ HeadlessFrontend + Canvas 扩展"
```

---

## Task 7: Frontend trait 切换 + App::render 改调 + TuiFrontend 改用 SceneRenderer

`Frontend::render` 签名改 `(&mut self, scene: &Scene, query: &dyn ContentQuery, focused: SpaceId)`。`TuiFrontend` 改用 `SceneRenderer`。`App::render` 调 `frontend.render(&scene, &query, focused)`。`HeadlessFrontend` 从 `app/frontend.rs` 删除（已在 `tui/headless.rs`）。

**Files:**
- Modify: `src/app/frontend.rs`
- Modify: `src/app/mod.rs`
- Modify: `src/tui/tui_frontend.rs`

- [ ] **Step 1: 改 Frontend trait + FrontendImpl**

`src/app/frontend.rs` 全文替换为：

```rust
//! 前端抽象：App 经此 trait + FrontendImpl 枚举不感知 tui/gui。
//! 定义在 app 层，由 tui 层实现（依赖倒置）。

use std::io;

use crate::protocol::content_query::ContentQuery;
use crate::protocol::frontend_event::FrontendEvent;
use crate::protocol::ids::SpaceId;
use crate::protocol::scene::Scene;

pub trait Frontend {
    async fn next_event(&mut self) -> io::Result<Option<FrontendEvent>>;
    fn render(&mut self, scene: &Scene, query: &dyn ContentQuery, focused: SpaceId) -> io::Result<()>;
}

/// trait + enum 双重分发。App 持此枚举，只调 trait 方法、从不 match 变体。
pub enum FrontendImpl {
    Tui(crate::tui::tui_frontend::TuiFrontend<std::io::Stdout>),
    #[allow(dead_code)]
    Headless(crate::tui::headless::HeadlessFrontend),
}

impl Frontend for FrontendImpl {
    async fn next_event(&mut self) -> io::Result<Option<FrontendEvent>> {
        match self {
            Self::Tui(f) => f.next_event().await,
            Self::Headless(f) => f.next_event().await,
        }
    }
    fn render(&mut self, scene: &Scene, query: &dyn ContentQuery, focused: SpaceId) -> io::Result<()> {
        match self {
            Self::Tui(f) => f.render(scene, query, focused),
            Self::Headless(f) => f.render(scene, query, focused),
        }
    }
}
```

删掉原 `HeadlessFrontend` 定义 + tests（HeadlessFrontend 已在 tui/headless.rs）。删 `use crate::protocol::frame::Frame;` 等。

- [ ] **Step 2: 改 TuiFrontend**

`src/tui/tui_frontend.rs` 全文替换为：

```rust
//! TUI 前端：SceneRenderer + Output<W>。Frontend::render 委托 SceneRenderer。

use std::io;

use crate::app::Frontend;
use crate::protocol::content_query::ContentQuery;
use crate::protocol::frontend_event::FrontendEvent;
use crate::protocol::ids::SpaceId;
use crate::protocol::scene::Scene;
use crate::terminal::input::Input;
use crate::terminal::output::{Canvas, Output};
use crate::tui::scene_renderer::SceneRenderer;

pub struct TuiFrontend<W: io::Write> {
    input: Input,
    output: Output<W>,
    renderer: SceneRenderer,
}

impl<W: io::Write> TuiFrontend<W> {
    pub fn new(output: Output<W>) -> Self {
        Self { input: Input::new(), output, renderer: SceneRenderer::new() }
    }
}

impl<W: io::Write> Frontend for TuiFrontend<W> {
    async fn next_event(&mut self) -> io::Result<Option<FrontendEvent>> {
        self.input.next_event().await
    }
    fn render(&mut self, scene: &Scene, query: &dyn ContentQuery, focused: SpaceId) -> io::Result<()> {
        self.renderer.render(scene, query, focused, &mut self.output as &mut dyn Canvas)
    }
}
```

删掉原 `paint_item`/`status_line` 函数（已移到 scene_renderer.rs）+ tests（tui_frontend tests 用 Frame，删——SceneRenderer tests 已覆盖）。

- [ ] **Step 3: 改 App::render + 删 engine**

`src/app/mod.rs`：

3a. `App` struct 删 `engine: TaffyEngine,` 字段。

3b. 删 `use crate::tui::taffy_engine::TaffyEngine;`。

3c. `App::new` 删 `engine: TaffyEngine::new(),` 行（Ok(Self{...}) 里）。

3d. `App::render` 全文替换为：

```rust
    fn render(&mut self) -> io::Result<()> {
        self.frontend.render(&self.scene, self as &dyn ContentQuery, self.focused)
    }
```

删掉原 layout/build_frame/viewport 跟随代码。

3e. `App` impl `ContentQuery`（Task 5 Step 7 已加）。确认 `self as &dyn ContentQuery` 可用（App impl ContentQuery）。

3f. 删 `use crate::frame::build_frame;`（若残留）。

- [ ] **Step 4: 修 app/mod.rs tests（HeadlessFrontend 路径）**

tests 里 `FrontendImpl::Headless(HeadlessFrontend::new(events))` 的 `HeadlessFrontend` 现在在 `crate::tui::headless`。改 import：

tests 顶部加 `use crate::tui::headless::HeadlessFrontend;`（或全限定）。`make_app` 里 `FrontendImpl::Headless(HeadlessFrontend::new(events))` 不变（类型名同）。

`status_bar_renders_focused_buffer_info` 测试当前找 `frame.items`——Frame 没了，改用字节断言：

```rust
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
        if let FrontendImpl::Headless(h) = &app.frontend {
            let frame = h.frames.first().expect("frame captured");
            let s = String::from_utf8(frame.clone()).unwrap();
            assert!(s.contains("f.txt"), "{s}");
            assert!(!s.contains("[+]"), "{s}"); // 未修改
        } else {
            panic!("expected headless frontend");
        }
    }
```

其余 app tests（run_inserts_char_then_quits 等）不依赖 Frame.items，保持不变（它们断言 buffer 状态，不查 frame）。

`status_bar_renders_focused_buffer_info` 原断言 `frame.items.iter().find(...).content_id == ContentId(1)` 改为字节断言（如上）。

- [ ] **Step 5: 跑测试**

Run: `cargo test`
Expected: 大部分绿。frame/mod.rs + protocol/frame.rs + protocol/edit_view.rs 仍存在但 build_frame 无消费者（App 不再调）。可能 dead_code warnings。下个任务删。

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "refactor: Frontend trait 切换 pull 模型 + TuiFrontend 改用 SceneRenderer + App::render 改调"
```

---

## Task 8: 删 frame 层 + protocol/frame.rs + protocol/edit_view.rs + ContentHandler 瘦身

删 `frame/` 层、`protocol/frame.rs`（Frame/FrameItem/FrameContent）、`protocol/edit_view.rs`（SpaceState）。ContentHandler 瘦身（删 render/line/len_lines/file_name/modified/status + RenderCtx）。Buffer/StatusBar 删 render impl。StatusBar 加 `status_bar_data` + ContentHandler 加 `as_buffer`/`as_status_bar`。App 的 ContentQuery::status_bar 改用 `as_status_bar().status_bar_data()`。

**Files:**
- Delete: `src/frame/mod.rs`, `src/protocol/frame.rs`, `src/protocol/edit_view.rs`
- Modify: `src/protocol/mod.rs`, `src/main.rs`, `src/core/content.rs`, `src/core/buffer.rs`, `src/core/status_bar.rs`, `src/app/mod.rs`, `src/tui/resolved.rs`, `src/tui/taffy_engine.rs`

- [ ] **Step 1: ContentHandler 瘦身（按 Task 5 Step 1 的 content.rs 全文）**

应用 Task 5 Step 1 的 `src/core/content.rs` 全文（删 RenderCtx/WrapMode/渲染访问器，加 as_buffer/as_status_bar，Cursors 保留）。

- [ ] **Step 2: Buffer 删 render impl（按 Task 5 Step 2）**

应用 Task 5 Step 2 的 buffer.rs 改动：删 `use RenderCtx`/`FrameContent`，ContentHandler impl 只留 keymap/keymap_mut/default_binding/buffer_mut + 加 as_buffer，加固有 `line` 方法。

- [ ] **Step 3: StatusBar 删 render + 加 status_bar_data（按 Task 5 Step 3）**

应用 Task 5 Step 3 的 status_bar.rs 全文。

- [ ] **Step 4: App ContentQuery::status_bar 改用 status_bar_data**

`src/app/mod.rs` 的 `impl ContentQuery for App` 的 `status_bar` 方法替换为：

```rust
    fn status_bar(&self, cid: ContentId) -> StatusBarData {
        let Some(c) = self.contents.get(&cid) else {
            return StatusBarData { file_name: None, modified: false, message: StatusMessage::None };
        };
        match c.as_status_bar() {
            Some(sb) => sb.status_bar_data(&self.contents as &dyn ContentLookup),
            None => StatusBarData { file_name: None, modified: false, message: StatusMessage::None },
        }
    }
```

删 `use crate::protocol::frame::FrameContent;`/`use crate::core::content::RenderCtx;`/`use crate::protocol::edit_view::SpaceState;`（若残留）。

App ContentQuery::lines 改用 as_buffer（不再经 trait line）：

```rust
    fn lines(&self, cid: ContentId, range: RowRange) -> Vec<String> {
        let Some(buf) = self.contents.get(&cid).and_then(|c| c.as_buffer()) else { return Vec::new() };
        let total = buf.len_lines();
        let start = range.start.min(total);
        let end = range.end.min(total).max(start);
        (start..end).map(|i| buf.line(i).trim_end_matches('\n').to_string()).collect()
    }
    fn line_count(&self, cid: ContentId) -> usize {
        self.contents.get(&cid).and_then(|c| c.as_buffer()).map(|b| b.len_lines()).unwrap_or(0)
    }
```

- [ ] **Step 5: resolved.rs 删 SpaceState 字段**

`src/tui/resolved.rs`：`RenderItem.state: SpaceState` 字段删（SpaceState 类型将删）。删 `use crate::protocol::edit_view::SpaceState;`。RenderItem 不再持 state（前端 viewport 自己管，cursor 经 query 拿）。

RenderItem 改为：

```rust
use crate::protocol::geometry::Rect;
use crate::protocol::ids::ContentId;
use crate::protocol::space::Layer;

#[derive(Clone)]
pub struct RenderItem {
    pub content_id: ContentId,
    pub rect: Rect,
    #[allow(dead_code)]
    pub clip: Option<Rect>,
    #[allow(dead_code)]
    pub layer: Layer,
    #[allow(dead_code)]
    pub z_index: i32,
    #[allow(dead_code)]
    pub order: u64,
}
```

tests 里 `render_item_holds_state` 改为不测 state（删 state 字段 + 测试）。

- [ ] **Step 6: taffy_engine.rs 删 SpaceState 赋值**

`src/tui/taffy_engine.rs` 的 `collect` 方法里 `state: SpaceState { viewport: ..., cursor: ... }` 字段赋值删除（RenderItem 不再有 state）。删 `use crate::protocol::edit_view::SpaceState;`。

collect 里 `out.items.push(RenderItem { content_id, rect, clip, layer, z_index, order })`（删 state）。`node.space.viewport`/`node.space.cursors` 引用也删（Space 已无这些字段——Task 4 已删。确认此处无残留）。

taffy_engine.rs tests 不变（不查 state）。

- [ ] **Step 7: 删 frame 层 + protocol/frame.rs + protocol/edit_view.rs**

删除文件：`src/frame/mod.rs`、`src/protocol/frame.rs`、`src/protocol/edit_view.rs`。

`src/protocol/mod.rs` 删 `pub mod frame;` 和 `pub mod edit_view;`：

```rust
pub mod content_query;
pub mod cursor;
pub mod frontend_event;
pub mod geometry;
pub mod ids;
pub mod key_event;
pub mod scene;
pub mod space;
pub mod status;
pub mod viewport;
```

`src/main.rs` 删 `mod frame;` 行。

- [ ] **Step 8: 清理残留引用**

grep `protocol::frame`、`protocol::edit_view`、`SpaceState`、`FrameContent`、`FrameItem`、`RenderCtx`、`build_frame`，确保清零。受影响：
- `src/app/frontend.rs` tests：原用 Frame 的测试已在 Task 7 删。确认无残留。
- `src/app/content.rs`：无 frame 引用。
- `src/tui/tui_frontend.rs`：已重写（Task 7），无 frame。
- `src/core/buffer.rs` tests：`buffer_mut_returns_self` 等不依赖 frame。确认 `use crate::protocol::frame::FrameContent` 已删。

- [ ] **Step 9: 跑测试**

Run: `cargo test`
Expected: 全绿。dead_code warnings 可能仍有（预留变体），不影响。

- [ ] **Step 10: Commit**

```bash
git add -A
git commit -m "refactor: 删 frame 层 + Frame/SpaceState + ContentHandler 瘦身为分发契约 + status_bar_data"
```

---

## Task 9: 最终全绿 + 清理 warnings + 文档更新

跑全量测试，清理 dead_code warnings（预留变体的 `#[allow(dead_code)]` 保留，删真正未用的）。更新 `docs/design/current-architecture.md` 反映新架构。

**Files:**
- Modify: `docs/design/current-architecture.md`

- [ ] **Step 1: 跑全量测试**

Run: `cargo test`
Expected: 全绿（原 89 测试 + 新增 ContentQuery/SceneRenderer 测试）

- [ ] **Step 2: 检查 warnings**

Run: `cargo build 2>&1 | Select-String "warning"`
Expected: 仅预留变体的 `#[allow(dead_code)]` 项（已标注），无新 warning。若有未用 import，删之。

- [ ] **Step 3: 更新 current-architecture.md**

更新 `docs/design/current-architecture.md`：
- §2 分层图：删 `layout/`、`frame/`，`tui/` 加 `scene_renderer.rs`/`taffy_engine.rs`/`resolved.rs`/`headless.rs`，`protocol/` 加 `scene.rs`/`space.rs`/`geometry.rs`/`content_query.rs`，删 `frame.rs`/`edit_view.rs`。
- §3.4 layout 节删除，改 §3.4 tui 节描述 SceneRenderer。
- §3.5 tui 节更新。
- §4 数据流：渲染流改 pull 模型描述。
- §6 债务：§6.1 CorePatch 死协议（已删 frame 通道，CorePatch 仍在？检查——CorePatch 在 protocol? 实际 CorePatch 在 current-architecture 提到但代码里可能已无。确认后更新）、§6.4 Viewport 越权（已清——viewport 在前端，无 height-1）标记已解决。

- [ ] **Step 4: Commit**

```bash
git add docs/design/current-architecture.md
git commit -m "docs: 更新 current-architecture 反映前端布局下放 + pull 模型"
```

- [ ] **Step 5: 最终验证**

Run: `cargo test && cargo build`
Expected: 全绿 + 无新 warning

---

## Self-Review

**Spec coverage：**
- §1 目标 1（后端不感知 tui 几何）：Task 4（Space 瘦身）+ Task 7（App::render 不 layout）+ Task 8（删 ResolvedScene 从后端）✓
- §1 目标 2（后端不感知 viewport）：Task 3（cursor 迁 App）+ Task 6（viewport 在 SceneRenderer）✓
- §1 目标 3（前端 pull）：Task 2（ContentQuery trait）+ Task 5（App impl）+ Task 6（SceneRenderer pull）✓
- §1 目标 4（数据轻量）：Task 6（只拉可见行）✓
- §1 目标 5（Scene 协议化）：Task 4 ✓
- §3 模块布局：Task 1/2/4/6/8 ✓
- §4 ContentQuery：Task 2 ✓；§4.2 Frontend trait：Task 7 ✓；§4.3 SceneRenderer：Task 6 ✓；§4.4 viewport：Task 6 ✓；§4.5 cursor 归 App：Task 3 ✓；§4.6 删 ViewportScrollBy：Task 3 ✓
- §5 数据流：Task 7 ✓
- §6 决策：全部任务覆盖 ✓
- §7 测试：Task 5/6/7/8/9 ✓
- §8 迁移清单：Task 4/6/7/8 ✓

**Placeholder scan：** 无 TBD/TODO。所有代码块完整。

**Type consistency：** `ContentQuery` 方法签名（lines/status_bar/cursor/line_count）在 Task 2/5/6/8 一致。`SceneRenderer::render(scene, query, focused, canvas)` 在 Task 6/7 一致。`Frontend::render(&mut self, scene, query, focused)` 在 Task 6/7 一致。`execute(op, content, &mut Cursors)` 在 Task 3/4 一致。
