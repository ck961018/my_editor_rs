# Scene 驱动前端层替换 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 用 `Scene`/`Space`/`ResolvedScene`/Taffy 驱动的渲染管线替换 v0.1 的 `Frontend`/`TuiFrontend`/旧 `Renderer`，`Editor`/`Buffer` 核心保留并薄包装进 `ContentStore`。

**Architecture:** 新增 `src/layout/` 子系统（ids/space/scene/content/resolved/taffy_engine），全程 `i32` 整数单位，Taffy 作为内部布局引擎（`i32↔f32` adapter）。`App<I, W>` 泛型化输入与输出以支持测试。Phase 1 自底向上建模块（TDD，独立可测），Phase 2 big bang 切换 `tui/renderer.rs`/`app.rs`/`main.rs` 并删 `frontend.rs`/`tui_frontend.rs`。

**Tech Stack:** Rust 2021, MSRV 1.75, ropey 1, crossterm 0.28, tokio 1, futures 0.3, taffy 0.5（新增）, tempfile 3 (dev)。

---

## File Structure

**新增：**
- `src/layout/mod.rs` — 模块注册
- `src/layout/ids.rs` — `SceneId`/`SpaceId`/`ContentId`
- `src/layout/space.rs` — `Space`/`SpaceKind`/`Arrangement`/`Axis`/`Align`/`Sizing`/`Layer`
- `src/layout/scene.rs` — `Size`/`Rect`/`Point`/`SpaceNode`/`Scene`/`SceneBuilder`/`build_editor_scene`
- `src/layout/content.rs` — `Content`/`ContentKind`/`ContentState`/`EditorState`/`ContentStore`
- `src/layout/resolved.rs` — `RenderItem`/`ResolvedScene`/`Renderer`/`render()`
- `src/layout/taffy_engine.rs` — `TaffyEngine`

**修改：**
- `Cargo.toml` — 加 `taffy = "0.5"`
- `src/terminal/input.rs` — 加 `InputSource` trait
- `src/tui/renderer.rs` — 重写为 `TuiRenderer`
- `src/app.rs` — 重写为 `App<I, W>`
- `src/main.rs` — 接线新 `App`
- `src/tui/mod.rs` — 移除 `tui_frontend`

**删除：**
- `src/frontend.rs`
- `src/tui/tui_frontend.rs`

**保留不动：** `src/core/*`、`src/protocol/*`、`src/terminal/output.rs`、`src/terminal/lifecycle.rs`、`src/tui/viewport.rs`。

---

## Phase 1: 新增 layout/ 模块（TDD，每 task 独立可编译可测）

### Task 1: 添加 taffy 依赖

**Files:**
- Modify: `Cargo.toml`

- [ ] **Step 1: 加 taffy 依赖**

修改 `Cargo.toml` 的 `[dependencies]` 段，在 `futures = "0.3"` 后加一行：

```toml
[dependencies]
ropey = "1"
crossterm = { version = "0.28", features = ["event-stream"] }
tokio = { version = "1", features = ["full"] }
futures = "0.3"
taffy = "0.5"
```

- [ ] **Step 2: 验证依赖可拉取并编译**

Run: `cargo build`
Expected: 成功拉取 taffy 并编译通过（无错误）。若 taffy 0.5 与 MSRV 1.75 不兼容，改用 `taffy = "0.4"`（API 相同）。

- [ ] **Step 3: Commit**

```bash
git add Cargo.toml Cargo.lock
git commit -m "build: 添加 taffy 0.5 布局引擎依赖"
```

---

### Task 2: layout/ids.rs + mod 注册

**Files:**
- Create: `src/layout/ids.rs`
- Create: `src/layout/mod.rs`
- Modify: `src/main.rs`（加 `mod layout;`）

- [ ] **Step 1: 写 layout/ids.rs**

```rust
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct SceneId(pub u64);

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct SpaceId(pub u64);

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct ContentId(pub u64);
```

- [ ] **Step 2: 写 layout/mod.rs**

```rust
pub mod ids;
```

- [ ] **Step 3: 在 main.rs 加 mod layout 声明**

修改 `src/main.rs` 第 1-6 行的 mod 声明段，加 `mod layout;`：

```rust
mod app;
mod core;
mod layout;
mod protocol;
mod terminal;
mod tui;
```

- [ ] **Step 4: 写失败测试（在 ids.rs 末尾加 tests 模块）**

在 `src/layout/ids.rs` 末尾追加：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_copy_eq_hash() {
        let a = SpaceId(1);
        let b = a; // Copy
        assert_eq!(a, b);
        let mut set = std::collections::HashSet::new();
        set.insert(ContentId(2));
        assert!(set.contains(&ContentId(2)));
    }

    #[test]
    fn ids_distinct_by_value() {
        assert_ne!(SpaceId(1), SpaceId(2));
        assert_ne!(ContentId(0), SceneId(0).0); // 不同类型，仅值对比示意
    }
}
```

- [ ] **Step 5: 跑测试验证通过**

Run: `cargo test layout::ids`
Expected: 2 passed.

- [ ] **Step 6: Commit**

```bash
git add src/layout/ids.rs src/layout/mod.rs src/main.rs
git commit -m "feat(layout): ids 模块 - SceneId/SpaceId/ContentId"
```

---

### Task 3: layout/space.rs

**Files:**
- Create: `src/layout/space.rs`
- Modify: `src/layout/mod.rs`（加 `pub mod space;`）

- [ ] **Step 1: 写 layout/space.rs**

```rust
use crate::layout::ids::{ContentId, SpaceId};

pub struct Space {
    pub id: SpaceId,
    pub name: Option<String>,
    pub kind: SpaceKind,
    pub sizing: Sizing,
    pub layer: Layer,
}

pub enum SpaceKind {
    Container {
        arrangement: Arrangement,
        children: Vec<SpaceId>,
    },
    Host {
        content: ContentId,
    },
}

pub enum Arrangement {
    Flex {
        direction: Axis,
        gap: i32,
        align: Align,
    },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Axis {
    Horizontal,
    Vertical,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Align {
    Stretch,
    Start,
    Center,
    End,
}

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
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Layer {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (*self as i32).cmp(&(*other as i32))
    }
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

    #[test]
    fn layer_sort_key_stable() {
        let mut v = vec![Layer::Debug, Layer::Base, Layer::Modal, Layer::Overlay];
        v.sort();
        assert_eq!(v, vec![Layer::Base, Layer::Overlay, Layer::Modal, Layer::Debug]);
    }

    #[test]
    fn axis_and_align_copy_eq() {
        assert_eq!(Axis::Vertical, Axis::Vertical);
        assert_ne!(Align::Start, Align::End);
    }
}
```

- [ ] **Step 2: 在 layout/mod.rs 注册**

修改 `src/layout/mod.rs`：

```rust
pub mod ids;
pub mod space;
```

- [ ] **Step 3: 跑测试验证通过**

Run: `cargo test layout::space`
Expected: 3 passed.

- [ ] **Step 4: Commit**

```bash
git add src/layout/space.rs src/layout/mod.rs
git commit -m "feat(layout): space 模块 - Space/SpaceKind/Arrangement/Sizing/Layer"
```

---

### Task 4: layout/scene.rs

**Files:**
- Create: `src/layout/scene.rs`
- Modify: `src/layout/mod.rs`（加 `pub mod scene;`）

- [ ] **Step 1: 写 layout/scene.rs**

```rust
use std::collections::HashMap;

use crate::layout::ids::{ContentId, SpaceId};
use crate::layout::space::{Align, Arrangement, Axis, Layer, Sizing, Space, SpaceKind};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Size {
    pub width: i32,
    pub height: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl Rect {
    pub fn contains(&self, p: Point) -> bool {
        p.x >= self.x && p.x < self.x + self.width && p.y >= self.y && p.y < self.y + self.height
    }

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
pub struct Point {
    pub x: i32,
    pub y: i32,
}

pub struct SpaceNode {
    pub id: SpaceId,
    pub parent: Option<SpaceId>,
    pub children: Vec<SpaceId>,
    pub space: Space,
}

pub struct Scene {
    pub root: SpaceId,
    pub size: Size,
    pub focused: Option<ContentId>,
    nodes: HashMap<SpaceId, SpaceNode>,
}

impl Scene {
    pub fn node(&self, id: SpaceId) -> &SpaceNode {
        self.nodes.get(&id).expect("space id exists")
    }

    pub fn resize(&mut self, width: i32, height: i32) {
        self.size = Size { width, height };
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum BuildError {
    UnknownRoot,
    CycleDetected,
    DanglingChild,
}

pub struct SceneBuilder {
    nodes: HashMap<SpaceId, SpaceNode>,
    next_id: u64,
}

impl SceneBuilder {
    pub fn new() -> Self {
        Self { nodes: HashMap::new(), next_id: 0 }
    }

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
            space: Space { id, name: None, kind, sizing: Sizing::Grow(1), layer: Layer::Base },
        };
        self.nodes.insert(id, node);
        id
    }

    pub fn host(&mut self, content: ContentId) -> SpaceHandle {
        let id = self.alloc(SpaceKind::Host { content });
        SpaceHandle { id }
    }

    pub fn container(&mut self, arrangement: Arrangement, children: Vec<SpaceId>) -> SpaceHandle {
        let id = self.alloc(SpaceKind::Container { arrangement, children });
        SpaceHandle { id }
    }

    pub fn finish(mut self, root: SpaceId, size: Size) -> Result<Scene, BuildError> {
        if !self.nodes.contains_key(&root) {
            return Err(BuildError::UnknownRoot);
        }
        // DFS 回填 parent + 环检测 + 悬空子节点检测
        let mut visited: HashMap<SpaceId, ()> = HashMap::new();
        let mut stack: Vec<SpaceId> = vec![root];
        while let Some(sid) = stack.pop() {
            if visited.contains_key(&sid) {
                return Err(BuildError::CycleDetected);
            }
            visited.insert(sid, ());
            let node = self.nodes.get(&sid).ok_or(BuildError::DanglingChild)?;
            for c in &node.children {
                if !self.nodes.contains_key(c) {
                    return Err(BuildError::DanglingChild);
                }
                if let Some(cnode) = self.nodes.get_mut(c) {
                    cnode.parent = Some(sid);
                }
                stack.push(*c);
            }
        }
        Ok(Scene { root, size, focused: None, nodes: self.nodes })
    }
}

impl Default for SceneBuilder {
    fn default() -> Self {
        Self::new()
    }
}

pub struct SpaceHandle {
    pub id: SpaceId,
}

impl SpaceHandle {
    pub fn fixed(self, builder: &mut SceneBuilder, size: i32) -> SpaceId {
        if let Some(n) = builder.nodes.get_mut(&self.id) {
            n.space.sizing = Sizing::Fixed(size);
        }
        self.id
    }

    pub fn grow(self, builder: &mut SceneBuilder, weight: u32) -> SpaceId {
        if let Some(n) = builder.nodes.get_mut(&self.id) {
            n.space.sizing = Sizing::Grow(weight);
        }
        self.id
    }
}

/// v0.1 标准布局：root Vertical [editor Grow(1), status Fixed(1)]。
pub fn build_editor_scene(width: i32, height: i32, editor: ContentId, status: ContentId) -> Scene {
    let mut b = SceneBuilder::new();
    let ed = b.host(editor).grow(&mut b, 1);
    let st = b.host(status).fixed(&mut b, 1);
    let root = b.container(
        Arrangement::Flex { direction: Axis::Vertical, gap: 0, align: Align::Stretch },
        vec![ed, st],
    );
    b.finish(root.id, Size { width, height }).expect("valid editor scene")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rect_contains_and_intersect() {
        let r = Rect { x: 0, y: 0, width: 10, height: 10 };
        assert!(r.contains(Point { x: 5, y: 5 }));
        assert!(!r.contains(Point { x: 10, y: 0 }));
        let o = Rect { x: 5, y: 5, width: 10, height: 10 };
        assert_eq!(r.intersect(&o), Some(Rect { x: 5, y: 5, width: 5, height: 5 }));
        let far = Rect { x: 20, y: 20, width: 5, height: 5 };
        assert_eq!(r.intersect(&far), None);
    }

    #[test]
    fn build_editor_scene_has_two_hosts() {
        let scene = build_editor_scene(80, 24, ContentId(0), ContentId(1));
        let root = scene.node(scene.root);
        match &root.space.kind {
            SpaceKind::Container { children, .. } => assert_eq!(children.len(), 2),
            _ => panic!("root must be container"),
        }
        assert_eq!(scene.size, Size { width: 80, height: 24 });
        assert!(scene.focused.is_none());
    }

    #[test]
    fn finish_rejects_unknown_root() {
        let b = SceneBuilder::new();
        let err = b.finish(SpaceId(999), Size { width: 10, height: 10 }).unwrap_err();
        assert_eq!(err, BuildError::UnknownRoot);
    }

    #[test]
    fn finish_rejects_dangling_child() {
        let mut b = SceneBuilder::new();
        let ed = b.host(ContentId(0)).grow(&mut b, 1);
        // 构造一个引用不存在子节点的 container：手动改 children
        let id = b.alloc(SpaceKind::Container {
            arrangement: Arrangement::Flex { direction: Axis::Vertical, gap: 0, align: Align::Stretch },
            children: vec![ed, SpaceId(999)],
        });
        let err = b.finish(id, Size { width: 10, height: 10 }).unwrap_err();
        assert_eq!(err, BuildError::DanglingChild);
    }

    #[test]
    fn resize_updates_size() {
        let mut scene = build_editor_scene(80, 24, ContentId(0), ContentId(1));
        scene.resize(100, 40);
        assert_eq!(scene.size, Size { width: 100, height: 40 });
    }
}
```

- [ ] **Step 2: 在 layout/mod.rs 注册**

```rust
pub mod ids;
pub mod scene;
pub mod space;
```

- [ ] **Step 3: 跑测试验证通过**

Run: `cargo test layout::scene`
Expected: 5 passed.

- [ ] **Step 4: Commit**

```bash
git add src/layout/scene.rs src/layout/mod.rs
git commit -m "feat(layout): scene 模块 - Size/Rect/Scene/SceneBuilder + 不变量校验"
```

---

### Task 5: layout/content.rs

**Files:**
- Create: `src/layout/content.rs`
- Modify: `src/layout/mod.rs`（加 `pub mod content;`）

- [ ] **Step 1: 写 layout/content.rs**

```rust
use std::io;

use crate::core::editor::Editor;
use crate::layout::ids::ContentId;
use crate::protocol::core_patch::PatchList;
use crate::protocol::frontend_event::FrontendEvent;
use crate::tui::viewport::Viewport;

pub struct Content {
    pub id: ContentId,
    pub kind: ContentKind,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ContentKind {
    Text,
    StatusBar,
    Terminal,
    Tree,
    Inspector,
    Panel,
    Custom(),
}

pub trait ContentState {
    fn kind(&self) -> ContentKind;
}

pub struct EditorState {
    editor: Editor,
    viewport: Viewport,
}

impl EditorState {
    pub fn new(editor: Editor, viewport: Viewport) -> Self {
        Self { editor, viewport }
    }

    pub fn editor(&self) -> &Editor {
        &self.editor
    }

    pub fn editor_mut(&mut self) -> &mut Editor {
        &mut self.editor
    }

    pub fn viewport(&self) -> &Viewport {
        &self.viewport
    }

    pub fn viewport_mut(&mut self) -> &mut Viewport {
        &mut self.viewport
    }
}

impl ContentState for EditorState {
    fn kind(&self) -> ContentKind {
        ContentKind::Text
    }
}

/// v0.1 务实处理：单个 Editor 状态服务两个 ContentId（Text + StatusBar）。
pub struct ContentStore {
    editor: EditorState,
    editor_content: ContentId,
    status_content: ContentId,
}

impl ContentStore {
    pub fn new(
        editor: Editor,
        viewport: Viewport,
        editor_content: ContentId,
        status_content: ContentId,
    ) -> Self {
        Self { editor: EditorState::new(editor, viewport), editor_content, status_content }
    }

    pub fn editor_state(&self) -> &EditorState {
        &self.editor
    }

    pub fn editor_state_mut(&mut self) -> &mut EditorState {
        &mut self.editor
    }

    pub fn content_kind(&self, id: ContentId) -> ContentKind {
        if id == self.status_content {
            ContentKind::StatusBar
        } else {
            ContentKind::Text
        }
    }

    pub fn handle_event(&mut self, ev: FrontendEvent, patches: &mut PatchList) -> io::Result<()> {
        self.editor.editor.handle_event(ev, patches)?;
        self.editor.viewport.ensure_cursor_visible(self.editor.editor.cursor().row);
        Ok(())
    }

    pub fn resize(&mut self, width: usize, height: usize) {
        self.editor.viewport = Viewport::new(width, height);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::core_patch::PatchList;
    use crate::protocol::key_event::{CtrlKey, KeyEvent};

    #[test]
    fn content_kind_routes_by_id() {
        let store = ContentStore::new(
            Editor::new(),
            Viewport::new(80, 24),
            ContentId(0),
            ContentId(1),
        );
        assert_eq!(store.content_kind(ContentId(0)), ContentKind::Text);
        assert_eq!(store.content_kind(ContentId(1)), ContentKind::StatusBar);
    }

    #[test]
    fn handle_event_delegates_to_editor() {
        let mut store = ContentStore::new(
            Editor::new(),
            Viewport::new(80, 24),
            ContentId(0),
            ContentId(1),
        );
        let mut pl = PatchList::new();
        store
            .handle_event(FrontendEvent::Key(KeyEvent::Char(b'a')), &mut pl)
            .unwrap();
        assert_eq!(store.editor_state().editor().buffer().slice().to_string(), "a");
    }

    #[test]
    fn handle_event_keeps_cursor_visible() {
        // 80x24 -> viewport.height = 23。插入 30 个换行使光标到第 30 行，触发滚动。
        let mut store = ContentStore::new(
            Editor::new(),
            Viewport::new(80, 24),
            ContentId(0),
            ContentId(1),
        );
        for _ in 0..30 {
            let mut pl = PatchList::new();
            store.handle_event(FrontendEvent::Key(KeyEvent::Enter), &mut pl).unwrap();
        }
        let vp = store.editor_state().viewport();
        assert_eq!(store.editor_state().editor().cursor().row, 30);
        assert!(vp.top_row > 0, "应已滚动，top_row={}", vp.top_row);
        // 光标行在视口内
        assert!(store.editor_state().editor().cursor().row >= vp.top_row);
        assert!(store.editor_state().editor().cursor().row < vp.top_row + vp.height);
    }

    #[test]
    fn resize_resets_viewport() {
        let mut store = ContentStore::new(
            Editor::new(),
            Viewport::new(80, 24),
            ContentId(0),
            ContentId(1),
        );
        store.resize(100, 40);
        assert_eq!(store.editor_state().viewport().width, 100);
        assert_eq!(store.editor_state().viewport().height, 39);
    }

    #[test]
    fn ctrl_q_quits() {
        let mut store = ContentStore::new(
            Editor::new(),
            Viewport::new(80, 24),
            ContentId(0),
            ContentId(1),
        );
        let mut pl = PatchList::new();
        store.handle_event(FrontendEvent::Key(KeyEvent::Ctrl(CtrlKey::Q)), &mut pl).unwrap();
        assert!(store.editor_state().editor().should_quit());
    }
}
```

- [ ] **Step 2: 在 layout/mod.rs 注册**

```rust
pub mod content;
pub mod ids;
pub mod scene;
pub mod space;
```

- [ ] **Step 3: 跑测试验证通过**

Run: `cargo test layout::content`
Expected: 5 passed.

- [ ] **Step 4: Commit**

```bash
git add src/layout/content.rs src/layout/mod.rs
git commit -m "feat(layout): content 模块 - ContentStore/EditorState 包 Editor+Viewport"
```

---

### Task 6: layout/resolved.rs

**Files:**
- Create: `src/layout/resolved.rs`
- Modify: `src/layout/mod.rs`（加 `pub mod resolved;`）

> **细化 spec §4.5**：`Renderer::draw_content` 与 `render()` 返回 `io::Result<()>`，以正确传播 IO 错误（spec 原签名无返回，会吞错误）。

- [ ] **Step 1: 写 layout/resolved.rs**

```rust
use std::io;

use crate::layout::content::ContentStore;
use crate::layout::ids::{ContentId, SpaceId};
use crate::layout::scene::Rect;
use crate::layout::space::Layer;

pub struct RenderItem {
    pub space: SpaceId,
    pub parent: Option<SpaceId>,
    pub content: Option<ContentId>,
    pub rect: Rect,
    pub clip: Option<Rect>,
    pub layer: Layer,
    pub z_index: i32,
    pub order: u64,
}

pub struct ResolvedScene {
    pub items: Vec<RenderItem>,
}

pub trait Renderer {
    fn draw_content(
        &mut self,
        content: ContentId,
        store: &ContentStore,
        rect: Rect,
        clip: Option<Rect>,
    ) -> io::Result<()>;

    fn flush(&mut self) -> io::Result<()>;
}

pub fn render(scene: &ResolvedScene, store: &ContentStore, renderer: &mut dyn Renderer) -> io::Result<()> {
    let mut items = scene.items.clone();
    items.sort_by_key(|i| (i.layer, i.z_index, i.order));
    for item in items {
        if let Some(content) = item.content {
            renderer.draw_content(content, store, item.rect, item.clip)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct RecordingRenderer {
        drawn: Vec<(ContentId, Rect)>,
    }
    impl Renderer for RecordingRenderer {
        fn draw_content(&mut self, content: ContentId, _store: &ContentStore, rect: Rect, _clip: Option<Rect>) -> io::Result<()> {
            self.drawn.push((content, rect));
            Ok(())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn item(content: Option<ContentId>, order: u64, rect: Rect) -> RenderItem {
        RenderItem {
            space: SpaceId(order),
            parent: None,
            content,
            rect,
            clip: None,
            layer: Layer::Base,
            z_index: 0,
            order,
        }
    }

    #[test]
    fn render_skips_none_content_and_preserves_order() {
        let scene = ResolvedScene {
            items: vec![
                item(Some(ContentId(0)), 0, Rect { x: 0, y: 0, width: 80, height: 23 }),
                item(None, 1, Rect { x: 0, y: 0, width: 80, height: 24 }),
                item(Some(ContentId(1)), 2, Rect { x: 0, y: 23, width: 80, height: 1 }),
            ],
        };
        // ContentStore 仅用于满足签名；用最小构造
        use crate::core::editor::Editor;
        use crate::layout::content::ContentStore;
        use crate::tui::viewport::Viewport;
        let store = ContentStore::new(Editor::new(), Viewport::new(80, 24), ContentId(0), ContentId(1));
        let mut r = RecordingRenderer { drawn: vec![] };
        render(&scene, &store, &mut r).unwrap();
        assert_eq!(r.drawn.len(), 2);
        assert_eq!(r.drawn[0].0, ContentId(0));
        assert_eq!(r.drawn[1].0, ContentId(1));
    }

    #[test]
    fn render_sorts_by_layer_z_order() {
        // 故意乱序：order 高的应排后
        let scene = ResolvedScene {
            items: vec![
                item(Some(ContentId(1)), 5, Rect { x: 0, y: 23, width: 80, height: 1 }),
                item(Some(ContentId(0)), 0, Rect { x: 0, y: 0, width: 80, height: 23 }),
            ],
        };
        use crate::core::editor::Editor;
        use crate::layout::content::ContentStore;
        use crate::tui::viewport::Viewport;
        let store = ContentStore::new(Editor::new(), Viewport::new(80, 24), ContentId(0), ContentId(1));
        let mut r = RecordingRenderer { drawn: vec![] };
        render(&scene, &store, &mut r).unwrap();
        assert_eq!(r.drawn[0].0, ContentId(0)); // order 0 先画
        assert_eq!(r.drawn[1].0, ContentId(1));
    }
}
```

- [ ] **Step 2: 在 layout/mod.rs 注册**

```rust
pub mod content;
pub mod ids;
pub mod resolved;
pub mod scene;
pub mod space;
```

- [ ] **Step 3: 跑测试验证通过**

Run: `cargo test layout::resolved`
Expected: 2 passed.

- [ ] **Step 4: Commit**

```bash
git add src/layout/resolved.rs src/layout/mod.rs
git commit -m "feat(layout): resolved 模块 - RenderItem/Renderer/render()"
```

---

### Task 7: layout/taffy_engine.rs

**Files:**
- Create: `src/layout/taffy_engine.rs`
- Modify: `src/layout/mod.rs`（加 `pub mod taffy_engine;`）

- [ ] **Step 1: 写 layout/taffy_engine.rs**

```rust
use std::collections::HashMap;

use taffy::prelude::*;

use crate::layout::content::ContentStore;
use crate::layout::ids::SpaceId;
use crate::layout::resolved::{RenderItem, ResolvedScene};
use crate::layout::scene::{Rect, Scene, SpaceNode};
use crate::layout::space::{Align, Axis, Sizing, SpaceKind};

pub struct TaffyEngine {
    tree: TaffyTree,
}

impl TaffyEngine {
    pub fn new() -> Self {
        Self { tree: TaffyTree::new() }
    }

    pub fn layout(&mut self, scene: &Scene, _store: &ContentStore) -> ResolvedScene {
        self.tree = TaffyTree::new();
        let mut map: HashMap<SpaceId, NodeId> = HashMap::new();
        let root_node = self.build_node(scene, scene.root, None, &mut map);
        let available = Size {
            width: AvailableSpace::Definite(scene.size.width as f32),
            height: AvailableSpace::Definite(scene.size.height as f32),
        };
        let _ = self.tree.compute_layout(root_node, available);
        let mut items = Vec::new();
        let mut order: u64 = 0;
        self.collect(scene, scene.root, None, None, &map, &mut items, &mut order);
        ResolvedScene { items }
    }

    fn build_node(
        &mut self,
        scene: &Scene,
        sid: SpaceId,
        parent_axis: Option<Axis>,
        map: &mut HashMap<SpaceId, NodeId>,
    ) -> NodeId {
        let node = scene.node(sid);
        let style = style_for(node, parent_axis);
        let taffy_id = match &node.space.kind {
            SpaceKind::Container { children, .. } => {
                let arrangement = match &node.space.kind {
                    SpaceKind::Container { arrangement, .. } => *arrangement,
                    _ => unreachable!(),
                };
                let axis = arrangement.direction;
                let child_ids: Vec<NodeId> = children
                    .iter()
                    .map(|c| self.build_node(scene, *c, Some(axis), map))
                    .collect();
                self.tree.new_with_children(style, child_ids).unwrap()
            }
            SpaceKind::Host { .. } => self.tree.new_leaf(style).unwrap(),
        };
        map.insert(sid, taffy_id);
        taffy_id
    }

    fn collect(
        &self,
        scene: &Scene,
        sid: SpaceId,
        parent: Option<SpaceId>,
        parent_clip: Option<Rect>,
        map: &HashMap<SpaceId, NodeId>,
        items: &mut Vec<RenderItem>,
        order: &mut u64,
    ) {
        let node = scene.node(sid);
        let taffy_id = map[&sid];
        let layout = self.tree.layout(taffy_id).expect("layout computed");
        let rect = Rect {
            x: layout.location.x.round() as i32,
            y: layout.location.y.round() as i32,
            width: layout.size.width.round() as i32,
            height: layout.size.height.round() as i32,
        };
        let clip = match parent_clip {
            Some(p) => p.intersect(&rect),
            None => Some(rect),
        };
        let content = match &node.space.kind {
            SpaceKind::Host { content } => Some(*content),
            SpaceKind::Container { .. } => None,
        };
        items.push(RenderItem {
            space: sid,
            parent,
            content,
            rect,
            clip,
            layer: node.space.layer,
            z_index: 0,
            order: *order,
        });
        *order += 1;
        if let SpaceKind::Container { children, .. } = &node.space.kind {
            for c in children {
                self.collect(scene, *c, Some(sid), clip, map, items, order);
            }
        }
    }
}

impl Default for TaffyEngine {
    fn default() -> Self {
        Self::new()
    }
}

fn style_for(node: &SpaceNode, parent_axis: Option<Axis>) -> Style {
    let mut style = Style::default();
    match (parent_axis, &node.space.sizing) {
        (Some(Axis::Vertical), Sizing::Fixed(x)) => {
            style.size.height = LengthPercentageAuto::Length(*x as f32);
        }
        (Some(Axis::Horizontal), Sizing::Fixed(x)) => {
            style.size.width = LengthPercentageAuto::Length(*x as f32);
        }
        (_, Sizing::Grow(w)) => {
            style.flex_grow = *w as f32;
        }
        (None, Sizing::Fixed(_)) => {} // root 忽略 sizing
    }
    if let SpaceKind::Container { arrangement, .. } = &node.space.kind {
        style.display = Display::Flex;
        style.flex_direction = match arrangement.direction {
            Axis::Vertical => FlexDirection::Column,
            Axis::Horizontal => FlexDirection::Row,
        };
        let gap_val = LengthPercentage::Length(arrangement.gap as f32);
        style.gap = Size { width: gap_val, height: gap_val };
        style.align_items = match arrangement.align {
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
    use crate::core::editor::Editor;
    use crate::layout::content::ContentStore;
    use crate::layout::ids::ContentId;
    use crate::layout::scene::{build_editor_scene, Size};
    use crate::tui::viewport::Viewport;

    fn store() -> ContentStore {
        ContentStore::new(Editor::new(), Viewport::new(80, 24), ContentId(0), ContentId(1))
    }

    fn item_for(scene: &ResolvedScene, content: ContentId) -> &RenderItem {
        scene.items.iter().find(|i| i.content == Some(content)).unwrap()
    }

    #[test]
    fn editor_grows_and_status_fixed_in_vertical() {
        let scene = build_editor_scene(80, 24, ContentId(0), ContentId(1));
        let mut engine = TaffyEngine::new();
        let resolved = engine.layout(&scene, &store());
        let editor = item_for(&resolved, ContentId(0));
        let status = item_for(&resolved, ContentId(1));
        assert_eq!(editor.rect, Rect { x: 0, y: 0, width: 80, height: 23 });
        assert_eq!(status.rect, Rect { x: 0, y: 23, width: 80, height: 1 });
    }

    #[test]
    fn order_is_dfs_preorder_root_first() {
        let scene = build_editor_scene(80, 24, ContentId(0), ContentId(1));
        let mut engine = TaffyEngine::new();
        let resolved = engine.layout(&scene, &store());
        // root(order 0) -> editor(order 1) -> status(order 2)
        assert_eq!(resolved.items.len(), 3);
        assert_eq!(resolved.items[0].content, None); // root container
        assert_eq!(resolved.items[0].order, 0);
        assert_eq!(resolved.items[1].content, Some(ContentId(0)));
        assert_eq!(resolved.items[2].content, Some(ContentId(1)));
    }

    #[test]
    fn clip_propagates_from_parent() {
        let scene = build_editor_scene(80, 24, ContentId(0), ContentId(1));
        let mut engine = TaffyEngine::new();
        let resolved = engine.layout(&scene, &store());
        let editor = item_for(&resolved, ContentId(0));
        // 根 clip = Some(root.rect)；editor clip = root.clip ∩ editor.rect = editor.rect
        assert_eq!(editor.clip, Some(editor.rect));
    }

    #[test]
    fn resize_changes_geometry() {
        let mut scene = build_editor_scene(80, 24, ContentId(0), ContentId(1));
        scene.resize(100, 40);
        let mut engine = TaffyEngine::new();
        let resolved = engine.layout(&scene, &store());
        let editor = item_for(&resolved, ContentId(0));
        assert_eq!(editor.rect.height, 39);
        assert_eq!(editor.rect.width, 100);
        let _ = Size { width: 0, height: 0 }; // 占位避免未使用 import
    }
}
```

- [ ] **Step 2: 在 layout/mod.rs 注册**

```rust
pub mod content;
pub mod ids;
pub mod resolved;
pub mod scene;
pub mod space;
pub mod taffy_engine;
```

- [ ] **Step 3: 跑测试验证通过**

Run: `cargo test layout::taffy_engine`
Expected: 4 passed。

若 Taffy 对无 measure 的 leaf 不分配尺寸导致 `editor.rect.height != 23`，给 Host leaf 在 `style_for` 中加 `style.min_size.height = LengthPercentageAuto::Length(0.0)` 仍不行——则改为在 `build_node` 的 Host 分支显式设 `style.flex_basis`。若仍不符，回退：在 `style_for` 对 `Sizing::Grow` 同时设 `style.flex_grow` 且对 Host 设 `style.size.cross = auto`。记录实际 Taffy 行为并调整断言为"editor.height == 23 且 status.height == 1"，两值之和必须等于 24。

- [ ] **Step 4: Commit**

```bash
git add src/layout/taffy_engine.rs src/layout/mod.rs
git commit -m "feat(layout): taffy_engine - i32<->f32 adapter + 几何/clip/order 收集"
```

---

## Phase 2: Big bang 切换

> Task 8 可独立编译。Task 9-11 互相耦合，中间状态不可编译；在 Task 11 末尾统一 `cargo build` + `cargo test` 验证。

### Task 8: terminal/input.rs 加 InputSource trait

**Files:**
- Modify: `src/terminal/input.rs`

- [ ] **Step 1: 在 input.rs 顶部加 trait 定义**

在 `src/terminal/input.rs` 第 8 行（`use crate::protocol::key_event::translate_key;` 之后）插入：

```rust
/// 异步输入源抽象。生产用 crossterm `Input`，测试用脚本驱动。
pub trait InputSource {
    async fn next_event(&mut self) -> io::Result<Option<FrontendEvent>>;
}
```

- [ ] **Step 2: 为 Input impl InputSource**

在 `impl Input { ... }` 块之后（`impl Default for Input` 之前）插入：

```rust
impl InputSource for Input {
    async fn next_event(&mut self) -> io::Result<Option<FrontendEvent>> {
        Input::next_event(self).await
    }
}
```

- [ ] **Step 3: 写失败测试（在 input.rs 的 tests 模块末尾追加）**

```rust
    #[test]
    fn input_is_input_source() {
        // 编译期验证 Input: InputSource
        fn assert_input_source<T: crate::terminal::input::InputSource>() {}
        assert_input_source::<crate::terminal::input::Input>();
    }
```

- [ ] **Step 4: 跑测试验证通过**

Run: `cargo test terminal::input`
Expected: 原有 4 + 新 1 = 5 passed.

- [ ] **Step 5: Commit**

```bash
git add src/terminal/input.rs
git commit -m "feat(terminal): InputSource trait 抽象输入源"
```

---

### Task 9: 重写 tui/renderer.rs 为 TuiRenderer

**Files:**
- Modify: `src/tui/renderer.rs`（整体替换）

> ⚠️ 此 task 后 `tui_frontend.rs`（use 旧 `Renderer::draw`）会编译失败，预期行为，Task 11 删除后恢复。

- [ ] **Step 1: 整体替换 src/tui/renderer.rs**

```rust
use std::io;

use crate::core::editor::Editor;
use crate::core::status::StatusMessage;
use crate::layout::content::{ContentKind, ContentStore};
use crate::layout::ids::ContentId;
use crate::layout::resolved::Renderer as LayoutRenderer;
use crate::layout::scene::Rect;
use crate::terminal::output::Output;
use crate::tui::viewport::Viewport;

pub struct TuiRenderer<W: io::Write> {
    output: Output<W>,
}

impl<W: io::Write> TuiRenderer<W> {
    pub fn new(output: Output<W>) -> Self {
        Self { output }
    }

    pub fn into_output(self) -> Output<W> {
        self.output
    }

    fn draw_editor(&mut self, editor: &Editor, viewport: &Viewport, rect: Rect) -> io::Result<()> {
        self.output.hide_cursor()?;
        let buffer = editor.buffer();
        let total_lines = buffer.len_lines();
        for row in 0..rect.height {
            let line_idx = viewport.top_row + row as usize;
            let screen_row = (rect.y + row) as usize;
            self.output.move_cursor(screen_row, rect.x as usize)?;
            self.output.clear_line()?;
            if line_idx < total_lines {
                let line = buffer.line(line_idx).to_string();
                let content = line.trim_end_matches('\n');
                self.output.write_str(content)?;
            }
        }
        let cursor = editor.cursor();
        let screen_row = cursor.row.saturating_sub(viewport.top_row) + rect.y as usize;
        let screen_col = cursor.col.saturating_sub(viewport.left_col) + rect.x as usize;
        self.output.move_cursor(screen_row, screen_col)?;
        self.output.show_cursor()?;
        Ok(())
    }

    fn draw_status(&mut self, editor: &Editor, rect: Rect) -> io::Result<()> {
        self.output.move_cursor(rect.y as usize, rect.x as usize)?;
        self.output.clear_line()?;
        self.output.write_str(&status_line(editor))?;
        Ok(())
    }
}

impl<W: io::Write> LayoutRenderer for TuiRenderer<W> {
    fn draw_content(
        &mut self,
        content: ContentId,
        store: &ContentStore,
        rect: Rect,
        _clip: Option<Rect>,
    ) -> io::Result<()> {
        let st = store.editor_state();
        match store.content_kind(content) {
            ContentKind::Text => self.draw_editor(st.editor(), st.viewport(), rect)?,
            ContentKind::StatusBar => self.draw_status(st.editor(), rect)?,
            _ => {}
        }
        Ok(())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.output.flush()
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
    use crate::core::editor::Editor;
    use crate::layout::content::ContentStore;
    use crate::layout::ids::ContentId;
    use crate::layout::scene::build_editor_scene;
    use crate::layout::taffy_engine::TaffyEngine;
    use crate::protocol::core_patch::PatchList;
    use crate::protocol::frontend_event::FrontendEvent;
    use crate::protocol::key_event::KeyEvent;
    use crate::tui::viewport::Viewport;

    fn editor_with(text: &str) -> Editor {
        let mut ed = Editor::new();
        for ch in text.chars() {
            let mut pl = PatchList::new();
            let key = if ch == '\n' { KeyEvent::Enter } else { KeyEvent::Char(ch as u8) };
            ed.handle_event(FrontendEvent::Key(key), &mut pl).unwrap();
        }
        ed
    }

    fn render_to_bytes(editor: Editor, width: i32, height: i32) -> Vec<u8> {
        let store = ContentStore::new(editor, Viewport::new(width as usize, height as usize), ContentId(0), ContentId(1));
        let scene = build_editor_scene(width, height, ContentId(0), ContentId(1));
        let mut engine = TaffyEngine::new();
        let resolved = engine.layout(&scene, &store);
        let renderer = TuiRenderer::new(Output::new(Vec::new()));
        let mut renderer = renderer;
        crate::layout::resolved::render(&resolved, &store, &mut renderer).unwrap();
        renderer.into_output().into_inner()
    }

    #[test]
    fn draws_text_and_status_line() {
        let ed = editor_with("hi");
        let bytes = render_to_bytes(ed, 40, 5);
        let s = String::from_utf8(bytes).unwrap();
        assert!(s.contains("hi"), "text missing: {s}");
        assert!(s.contains("[No Name]"), "status missing: {s}");
        assert!(s.contains("0:2"), "cursor pos missing: {s}");
    }

    #[test]
    fn draws_modified_marker() {
        let ed = editor_with("x");
        let bytes = render_to_bytes(ed, 40, 5);
        let s = String::from_utf8(bytes).unwrap();
        assert!(s.contains("[+]"), "modified marker missing: {s}");
    }

    #[test]
    fn draws_multiline() {
        let ed = editor_with("ab\ncd");
        let bytes = render_to_bytes(ed, 40, 5);
        let s = String::from_utf8(bytes).unwrap();
        assert!(s.contains("ab"), "{s}");
        assert!(s.contains("cd"), "{s}");
    }
}
```

- [ ] **Step 2: 不跑测试（编译在 Task 11 后验证）**

此步 `cargo build` 会因 `tui_frontend.rs` 引用旧 `Renderer::draw` 失败，属预期。继续 Task 10。

- [ ] **Step 3: 暂不 commit（与 Task 10/11 一起 commit）**

---

### Task 10: 重写 app.rs 为 App<I, W>

**Files:**
- Modify: `src/app.rs`（整体替换）

> ⚠️ 此 task 后 `main.rs`（use `App::new(path, frontend)`）会编译失败，预期行为，Task 11 修复。

- [ ] **Step 1: 整体替换 src/app.rs**

```rust
use std::io;

use crate::core::editor::Editor;
use crate::layout::content::ContentStore;
use crate::layout::ids::ContentId;
use crate::layout::resolved::render;
use crate::layout::scene::{build_editor_scene, Size};
use crate::layout::taffy_engine::TaffyEngine;
use crate::protocol::core_patch::PatchList;
use crate::protocol::frontend_event::FrontendEvent;
use crate::terminal::input::InputSource;
use crate::terminal::output::Output;
use crate::tui::renderer::TuiRenderer;
use crate::tui::viewport::Viewport;

pub struct App<I: InputSource, W: io::Write> {
    store: ContentStore,
    scene: crate::layout::scene::Scene,
    engine: TaffyEngine,
    renderer: TuiRenderer<W>,
    input: I,
}

impl<I: InputSource, W: io::Write> App<I, W> {
    pub fn new(
        path: Option<&str>,
        width: usize,
        height: usize,
        input: I,
        output: Output<W>,
    ) -> io::Result<Self> {
        let mut editor = Editor::new();
        if let Some(p) = path {
            editor.open_path(p)?;
        }
        let viewport = Viewport::new(width, height);
        let store = ContentStore::new(editor, viewport, ContentId(0), ContentId(1));
        let scene = build_editor_scene(width as i32, height as i32, ContentId(0), ContentId(1));
        Ok(Self {
            store,
            scene,
            engine: TaffyEngine::new(),
            renderer: TuiRenderer::new(output),
            input,
        })
    }

    pub async fn run(&mut self) -> io::Result<()> {
        self.render()?;
        while !self.store.editor_state().editor().should_quit() {
            let event = match self.input.next_event().await? {
                Some(e) => e,
                None => continue,
            };
            if let FrontendEvent::Resize(r) = &event {
                self.scene.resize(r.width as i32, r.height as i32);
                self.store.resize(r.width as usize, r.height as usize);
            }
            let mut patches = PatchList::new();
            self.store.handle_event(event, &mut patches)?;
            self.render()?;
        }
        Ok(())
    }

    fn render(&mut self) -> io::Result<()> {
        let resolved = self.engine.layout(&self.scene, &self.store);
        render(&resolved, &self.store, &mut self.renderer)?;
        self.renderer.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    use crate::protocol::frontend_event::ResizeEvent;
    use crate::protocol::key_event::{CtrlKey, KeyEvent};

    struct ScriptedInput {
        events: VecDeque<FrontendEvent>,
    }

    impl ScriptedInput {
        fn new(events: Vec<FrontendEvent>) -> Self {
            Self { events: events.into() }
        }
    }

    impl InputSource for ScriptedInput {
        async fn next_event(&mut self) -> io::Result<Option<FrontendEvent>> {
            Ok(self.events.pop_front())
        }
    }

    #[tokio::test]
    async fn run_inserts_char_then_quits() {
        let input = ScriptedInput::new(vec![
            FrontendEvent::Key(KeyEvent::Char(b'a')),
            FrontendEvent::Key(KeyEvent::Ctrl(CtrlKey::Q)),
        ]);
        let mut app = App::new(None, 40, 5, input, Output::new(Vec::new())).unwrap();
        app.run().await.unwrap();
        assert_eq!(app.store.editor_state().editor().buffer().slice().to_string(), "a");
        assert!(app.store.editor_state().editor().should_quit());
    }

    #[tokio::test]
    async fn run_forwards_resize_to_scene_and_viewport() {
        let input = ScriptedInput::new(vec![
            FrontendEvent::Resize(ResizeEvent { width: 100, height: 40 }),
            FrontendEvent::Key(KeyEvent::Ctrl(CtrlKey::Q)),
        ]);
        let mut app = App::new(None, 40, 5, input, Output::new(Vec::new())).unwrap();
        app.run().await.unwrap();
        assert_eq!(app.scene.size, Size { width: 100, height: 40 });
        assert_eq!(app.store.editor_state().viewport().width, 100);
        assert_eq!(app.store.editor_state().viewport().height, 39);
    }

    #[tokio::test]
    async fn run_supports_backspace_and_arrows() {
        let input = ScriptedInput::new(vec![
            FrontendEvent::Key(KeyEvent::Char(b'a')),
            FrontendEvent::Key(KeyEvent::Char(b'b')),
            FrontendEvent::Key(KeyEvent::Backspace),
            FrontendEvent::Key(KeyEvent::Arrow(crate::protocol::key_event::ArrowKey::Left)),
            FrontendEvent::Key(KeyEvent::Ctrl(CtrlKey::Q)),
        ]);
        let mut app = App::new(None, 40, 5, input, Output::new(Vec::new())).unwrap();
        app.run().await.unwrap();
        assert_eq!(app.store.editor_state().editor().buffer().slice().to_string(), "a");
        // 'a' 后退格删 'b'，再左移：光标 col=0
        assert_eq!(app.store.editor_state().editor().cursor().col, 0);
    }
}
```

- [ ] **Step 2: 不跑测试（编译在 Task 11 后验证）**

- [ ] **Step 3: 暂不 commit（与 Task 11 一起 commit）**

---

### Task 11: 重写 main.rs + 删旧文件 + 验证

**Files:**
- Modify: `src/main.rs`（整体替换）
- Modify: `src/tui/mod.rs`（移除 `tui_frontend`）
- Delete: `src/frontend.rs`
- Delete: `src/tui/tui_frontend.rs`

- [ ] **Step 1: 整体替换 src/main.rs**

```rust
mod app;
mod core;
mod layout;
mod protocol;
mod terminal;
mod tui;

use std::io::{self, Stdout};

use app::App;
use crossterm::terminal::size as term_size;
use terminal::input::Input;
use terminal::lifecycle::TerminalGuard;
use terminal::output::Output;

#[tokio::main]
async fn main() -> io::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let path = args.get(1).map(|s| s.as_str());

    let _guard = TerminalGuard::enter()?;

    let (width, height) = term_size().unwrap_or((80, 24));
    let mut app: App<Input, Stdout> =
        App::new(path, width as usize, height as usize, Input::new(), Output::new(io::stdout()))?;
    app.run().await?;
    Ok(())
}
```

- [ ] **Step 2: 更新 src/tui/mod.rs（移除 tui_frontend）**

```rust
pub mod renderer;
pub mod viewport;
```

- [ ] **Step 3: 删除 src/frontend.rs**

Run: `git rm src/frontend.rs`

- [ ] **Step 4: 删除 src/tui/tui_frontend.rs**

Run: `git rm src/tui/tui_frontend.rs`

- [ ] **Step 5: cargo build 验证编译通过**

Run: `cargo build`
Expected: 编译通过，零错误。若有 unused import 警告，按提示删除。

常见编译问题与修复：
- `Output` 未导入：确认 `main.rs` 有 `use terminal::output::Output;`。
- `ContentKind::Custom()` 缺参数：`content.rs` 的 `Custom` 变体无字段，写 `Custom()` 正确。
- taffy API 差异：若 `AlignItems::FlexStart` 等不存在，查 `taffy::style::AlignItems` 实际变体名调整。

- [ ] **Step 6: cargo test 验证全部测试通过**

Run: `cargo test`
Expected: 全部通过。应包含：
- `core::*` 原有全部测试（editor/buffer/cursor/status）
- `protocol::*` 原有全部测试（key_event/frontend_event/core_patch/input map_event）
- `layout::ids` (2) + `layout::space` (3) + `layout::scene` (5) + `layout::content` (5) + `layout::resolved` (2) + `layout::taffy_engine` (4)
- `tui::renderer` (3) + `tui::viewport` 原有 (4)
- `terminal::input` (5)
- `app` (3)

若 `tui::taffy_engine::editor_grows_and_status_fixed_in_vertical` 的 `editor.rect.height` 不是 23：检查 Taffy leaf 默认尺寸行为，按 Task 7 Step 3 注记调整 `style_for`（可能需给 Host leaf 显式 `style.flex_basis = LengthPercentageAuto::Length(0.0)` 或移除 leaf 的 size 约束），保持"editor.height + status.height == total"。

- [ ] **Step 7: cargo build --release 验证零警告**

Run: `cargo build --release`
Expected: 零警告零错误。

- [ ] **Step 8: Commit（Phase 2 整体）**

```bash
git add -A
git commit -m @'
refactor: big bang 替换前端层为 Scene 驱动架构

- 删 Frontend trait / TuiFrontend / 旧 Renderer
- 新增 layout/ 子系统驱动渲染（Taffy i32<->f32 adapter）
- App<I,W> 泛型化输入输出，Editor/Buffer 包进 ContentStore
- main 接线新 App，protocol 保留（CorePatch 保留但 App 全量重算）
- v0.1 功能不回退，core/* 测试全保留
'@
```

---

## Self-Review（plan 作者自检，已完成）

**1. Spec 覆盖：**
- §2 架构总览 → Task 5 (ContentStore) + Task 9 (TuiRenderer) + Task 10 (App) ✓
- §3 模块结构 → Task 2-7 + Task 9-11 ✓
- §4 核心类型 → Task 2 (ids) / Task 3 (space) / Task 4 (scene) / Task 5 (content) / Task 6 (resolved) ✓
- §5 Taffy adapter → Task 7 ✓
- §6 TuiRenderer → Task 9 ✓
- §7 App 主循环 → Task 10 ✓
- §8 不变量 → Task 4 (SceneBuilder.finish) ✓
- §9 不回退 → Task 10 测试覆盖 insert/backspace/arrow/resize/quit ✓
- §10 测试 → 各 task 内嵌测试 ✓
- §11 对齐 layout_design → Task 7 注记 MeasureFunc 不实现 ✓
- §12 Non-goals → 均未实现 ✓
- §13 风险 → Task 7 Step 3 + Task 11 Step 5/6 注记 Taffy leaf 行为 ✓

**2. Placeholder 扫描：** 无 TBD/TODO；Task 9-11 中间不编译有明确注记（非 placeholder）。✓

**3. 类型一致性：**
- `ContentStore::new(editor, viewport, editor_content, status_content)` — Task 5 定义，Task 9/10/11 一致 ✓
- `EditorState::editor()/viewport()` — Task 5 定义，Task 9/10 一致 ✓
- `TaffyEngine::new()/layout(&scene, &store)` — Task 7 定义，Task 9/10 一致 ✓
- `TuiRenderer::new(output)/into_output()` — Task 9 定义，Task 10 测试不用 into_output（App 不暴露），Task 9 自测用 ✓
- `Renderer::draw_content(...)->io::Result<()>` + `flush()` — Task 6 定义，Task 9 impl 一致 ✓
- `build_editor_scene(width, height, editor, status)` — Task 4 定义，Task 9/10 一致 ✓
- `App::new(path, width, height, input, output)` — Task 10 定义，Task 11 main 一致 ✓
- `InputSource::next_event` — Task 8 定义，Task 10 ScriptedInput/Input 一致 ✓
- `Scene::resize(width, height)` — Task 4 定义，Task 10 一致 ✓
- `ContentStore::resize(width, height)` — Task 5 定义，Task 10 一致 ✓

无类型不一致。

---

## Execution Handoff

Plan complete and saved to `docs/superpowers/plans/2026-06-30-scene-driven-frontend.md`. Two execution options:

**1. Subagent-Driven (recommended)** - I dispatch a fresh subagent per task, review between tasks, fast iteration.

**2. Inline Execution** - Execute tasks in this session using executing-plans, batch execution with checkpoints.

Which approach?
