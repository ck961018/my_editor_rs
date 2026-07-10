# Architecture Boundary Cleanup Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Move the frontend abstraction out of `app`, make `SceneBuilder` the long-lived `SpaceId` allocator owned by `App`, and replace the narrow `CtrlKey` model with a general key modifier protocol.

**Architecture:** Add a pure `frontend` layer containing only the `Frontend` trait. Make `App<F: Frontend>` use static dispatch while `TuiFrontend<W>` implements the trait without depending on `app`. Keep one `SceneBuilder` in `App` for all space allocation, and make key bindings use `KeyEvent { code, modifiers }`.

**Tech Stack:** Rust 2024, tokio, crossterm, ropey, taffy, tempfile.

---

## File Structure

- Create `src/frontend/mod.rs`: pure frontend trait depending only on protocol types and `std::io`.
- Modify `src/main.rs`: add `mod frontend;`, stop importing `FrontendImpl`, inject `TuiFrontend` directly into `App`.
- Modify `src/app/mod.rs`: remove `mod frontend`, remove `FrontendImpl` re-export, genericize `App<F: Frontend>`, add `scene_builder: SceneBuilder`, update tests to use local `ScriptedFrontend`.
- Delete `src/app/frontend.rs`: old app-owned frontend abstraction and enum dispatch.
- Modify `src/tui/tui_frontend.rs`: import `crate::frontend::Frontend`.
- Modify `src/tui/mod.rs`: remove `pub mod headless`.
- Delete `src/tui/headless.rs`: remove global headless frontend implementation.
- Modify `src/protocol/scene.rs`: make `SceneBuilder::snapshot` non-consuming, add builder-owned sizing helpers, update `build_editor_scene` to accept `&mut SceneBuilder`.
- Modify `src/protocol/key_event.rs`: replace `CtrlKey` and enum-style `KeyEvent` with `KeyModifiers`, `KeyCode`, and struct `KeyEvent`.
- Modify `src/protocol/frontend_event.rs`: update tests to the new key constructors.
- Modify `src/core/buffer.rs`: update default keymap and default character binding.
- Modify `src/core/keymap.rs`: update tests and sample bindings.
- Modify `src/app/dispatcher.rs`: update global keymap, tests, and scene fixture builder.
- Modify `src/terminal/input.rs`: update input tests for the new key event shape.
- Modify `docs/design/current-architecture.md`: reflect the new frontend layer, static dispatch, long-lived builder, and modifier model.
- Modify `AGENTS.md`: update project guidance to match the new frontend boundary and key event model.

## Task 1: Extract Pure Frontend Layer With Static Dispatch

**Files:**
- Create: `src/frontend/mod.rs`
- Modify: `src/main.rs`
- Modify: `src/app/mod.rs`
- Modify: `src/tui/tui_frontend.rs`
- Modify: `src/tui/mod.rs`
- Delete: `src/app/frontend.rs`
- Delete: `src/tui/headless.rs`

- [ ] **Step 1: Add the frontend module declaration in `src/main.rs`**

Change the module declarations at the top of `src/main.rs` to:

```rust
mod app;
mod core;
mod frontend;
mod protocol;
mod terminal;
mod tui;
```

Run: `cargo test`

Expected: FAIL with a module-not-found error for `frontend`.

- [ ] **Step 2: Create `src/frontend/mod.rs`**

Create `src/frontend/mod.rs` with:

```rust
//! 前端抽象层。App 和具体前端实现都依赖这里，避免 app <-> tui 互相依赖。

use std::io;

use crate::protocol::content_query::ContentQuery;
use crate::protocol::frontend_event::FrontendEvent;
use crate::protocol::ids::SpaceId;
use crate::protocol::scene::Scene;

pub trait Frontend {
    async fn next_event(&mut self) -> io::Result<Option<FrontendEvent>>;

    fn render(
        &mut self,
        scene: &Scene,
        query: &dyn ContentQuery,
        focused: SpaceId,
    ) -> io::Result<()>;
}
```

Run: `cargo test`

Expected: FAIL because `src/app/mod.rs`, `src/tui/tui_frontend.rs`, and tests still import `crate::app::Frontend` / `FrontendImpl`.

- [ ] **Step 3: Update `src/tui/tui_frontend.rs` to depend on the new layer**

Replace:

```rust
use crate::app::Frontend;
```

with:

```rust
use crate::frontend::Frontend;
```

Run: `cargo test`

Expected: FAIL because `src/app/mod.rs` still owns `FrontendImpl` and the tests still use `HeadlessFrontend`.

- [ ] **Step 4: Genericize `App` and remove the app frontend module**

In `src/app/mod.rs`, replace:

```rust
mod frontend;
```

with no line. Remove:

```rust
#[allow(unused_imports)]
pub use frontend::{Frontend, FrontendImpl};
```

Add:

```rust
use crate::frontend::Frontend;
```

Change:

```rust
pub struct App {
```

to:

```rust
pub struct App<F: Frontend> {
```

Change the `frontend` field from:

```rust
frontend: FrontendImpl,
```

to:

```rust
frontend: F,
```

Change:

```rust
impl App {
```

to:

```rust
impl<F: Frontend> App<F> {
```

Change `App::new` from:

```rust
pub fn new(
    path: Option<&str>,
    width: usize,
    height: usize,
    frontend: FrontendImpl,
) -> io::Result<Self> {
```

to:

```rust
pub fn new(
    path: Option<&str>,
    width: usize,
    height: usize,
    frontend: F,
) -> io::Result<Self> {
```

Change:

```rust
impl ContentQuery for App {
```

to:

```rust
impl<F: Frontend> ContentQuery for App<F> {
```

Run: `cargo test`

Expected: FAIL because `main.rs` still imports `FrontendImpl` and app tests still construct `FrontendImpl::Headless`.

- [ ] **Step 5: Update `src/main.rs` to inject `TuiFrontend` directly**

Replace:

```rust
use app::{App, FrontendImpl};
```

with:

```rust
use app::App;
```

Replace:

```rust
let frontend = FrontendImpl::Tui(TuiFrontend::new(Output::new(io::stdout())));
```

with:

```rust
let frontend = TuiFrontend::new(Output::new(io::stdout()));
```

Run: `cargo test`

Expected: FAIL only in tests that still refer to `HeadlessFrontend` or `FrontendImpl`.

- [ ] **Step 6: Replace app tests' global headless frontend with local `ScriptedFrontend`**

In `src/app/mod.rs` test module, replace imports:

```rust
use crate::protocol::key_event::{ArrowKey, CtrlKey, KeyEvent};
use crate::protocol::content_query::{ContentQuery, RowRange};
use crate::tui::headless::HeadlessFrontend;
```

with:

```rust
use crate::frontend::Frontend;
use crate::protocol::content_query::{ContentQuery, RowRange};
use crate::protocol::key_event::{ArrowKey, CtrlKey, KeyEvent};
use std::collections::VecDeque;
```

Add this helper in the test module:

```rust
struct ScriptedFrontend {
    events: VecDeque<FrontendEvent>,
    renders: usize,
}

impl ScriptedFrontend {
    fn new(events: Vec<FrontendEvent>) -> Self {
        Self { events: events.into(), renders: 0 }
    }
}

impl Frontend for ScriptedFrontend {
    async fn next_event(&mut self) -> io::Result<Option<FrontendEvent>> {
        Ok(self.events.pop_front())
    }

    fn render(
        &mut self,
        _scene: &Scene,
        _query: &dyn ContentQuery,
        _focused: SpaceId,
    ) -> io::Result<()> {
        self.renders += 1;
        Ok(())
    }
}
```

Replace:

```rust
fn make_app(events: Vec<FrontendEvent>, path: Option<&str>) -> App {
    App::new(path, 40, 5, FrontendImpl::Headless(HeadlessFrontend::new(events)))
        .unwrap()
}
```

with:

```rust
fn make_app(events: Vec<FrontendEvent>, path: Option<&str>) -> App<ScriptedFrontend> {
    App::new(path, 40, 5, ScriptedFrontend::new(events)).unwrap()
}
```

Delete the tests `status_bar_renders_focused_buffer_info` and `selection_renders_reverse_in_frame` from `src/app/mod.rs`; those byte-level rendering concerns remain covered in `src/tui/scene_renderer.rs`.

Add this app-level replacement test:

```rust
#[tokio::test(flavor = "multi_thread")]
async fn run_renders_after_state_changes() {
    let mut app = make_app(
        vec![
            FrontendEvent::Key(KeyEvent::Char(b'a')),
            FrontendEvent::Key(KeyEvent::Ctrl(CtrlKey::Q)),
        ],
        None,
    );
    app.run().await.unwrap();
    assert!(app.frontend.renders >= 1);
    let buf = app.contents.get_mut(&editor_cid()).and_then(|c| c.buffer_mut()).unwrap();
    assert_eq!(buf.slice().to_string(), "a");
}
```

Run: `cargo test`

Expected: FAIL because `src/tui/mod.rs` still exposes `headless` and `src/tui/headless.rs` still imports `crate::app::Frontend`, or PASS if all references were removed before deletion.

- [ ] **Step 7: Delete `src/app/frontend.rs` and `src/tui/headless.rs`, update `src/tui/mod.rs`**

Delete:

```text
src/app/frontend.rs
src/tui/headless.rs
```

In `src/tui/mod.rs`, remove:

```rust
pub mod headless;
```

Run: `cargo test`

Expected: PASS.

- [ ] **Step 8: Verify dependency boundary by search**

Run:

```powershell
rg "crate::app::Frontend|FrontendImpl|crate::tui" src\app src\tui src\frontend
```

Expected: No matches for `crate::app::Frontend` or `FrontendImpl`. Matches for `crate::tui` must not appear under `src/app`.

- [ ] **Step 9: Commit frontend boundary migration**

Run:

```powershell
git add src/frontend/mod.rs src/main.rs src/app/mod.rs src/tui/tui_frontend.rs src/tui/mod.rs src/app/frontend.rs src/tui/headless.rs
git commit -m "refactor: extract frontend trait layer"
```

Expected: Commit succeeds.

## Task 2: Make SceneBuilder the Long-Lived SpaceId Allocator

**Files:**
- Modify: `src/protocol/scene.rs`
- Modify: `src/app/mod.rs`
- Modify: `src/app/dispatcher.rs`
- Modify: `src/tui/taffy_engine.rs`
- Modify: `src/tui/scene_renderer.rs`

- [ ] **Step 1: Add failing SceneBuilder allocation tests**

In `src/protocol/scene.rs` tests, add:

```rust
#[test]
fn snapshot_does_not_reset_next_space_id() {
    let mut builder = SceneBuilder::new();
    let (scene, editor_space) = build_editor_scene(
        &mut builder,
        80,
        24,
        ContentId(0),
        ContentId(1),
    )
    .unwrap();
    assert_eq!(editor_space, SpaceId(0));
    let extra = builder.host_grow(ContentId(2), 1);
    assert_eq!(extra, SpaceId(3));
    assert!(scene.node(editor_space).space.id == SpaceId(0));
}

#[test]
fn repeated_snapshot_keeps_allocating_after_existing_nodes() {
    let mut builder = SceneBuilder::new();
    let (scene, _) = build_editor_scene(&mut builder, 80, 24, ContentId(0), ContentId(1)).unwrap();
    let root = scene.root;
    let _second = builder.snapshot(root, Size { width: 100, height: 40 }).unwrap();
    let extra = builder.host_fixed(ContentId(2), 1);
    assert_eq!(extra, SpaceId(3));
}
```

Run: `cargo test protocol::scene`

Expected: FAIL because `build_editor_scene` does not accept a builder and `host_grow`, `host_fixed`, and `snapshot` do not exist.

- [ ] **Step 2: Change `SpaceNode` and `Scene` to cloneable snapshots**

In `src/protocol/scene.rs`, derive clone for snapshot data:

```rust
#[derive(Clone)]
pub struct SpaceNode {
    #[allow(dead_code)]
    pub id: SpaceId,
    pub parent: Option<SpaceId>,
    pub children: Vec<SpaceId>,
    pub space: Space,
}

#[derive(Clone)]
pub struct Scene {
    pub root: SpaceId,
    pub size: Size,
    nodes: HashMap<SpaceId, SpaceNode>,
}
```

In `src/protocol/space.rs`, add `Clone` derives so `SceneBuilder::snapshot`
can clone its node map:

```rust
#[derive(Clone)]
pub struct Space {
    #[allow(dead_code)]
    pub id: SpaceId,
    pub kind: SpaceKind,
    pub sizing: Sizing,
    pub layer: Layer,
}

#[derive(Clone)]
pub enum SpaceKind {
    Container { arrangement: Arrangement, children: Vec<SpaceId> },
    Host { content: ContentId },
}

#[derive(Clone)]
pub enum Arrangement {
    Flex { direction: Axis, gap: i32, align: Align },
}

#[allow(dead_code)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Axis { Horizontal, Vertical }

#[allow(dead_code)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Align { Stretch, Start, Center, End }

#[derive(Clone)]
pub enum Sizing {
    Fixed(i32),
    Grow(u32),
}

#[repr(i32)]
#[allow(dead_code)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Layer {
    Base = 0,
    Overlay = 10,
    Modal = 20,
    Debug = 100,
}
```

Run: `cargo test protocol::scene`

Expected: FAIL because builder APIs are still missing.

- [ ] **Step 3: Add builder-owned sizing helpers**

In `src/protocol/scene.rs`, add these methods inside `impl SceneBuilder`:

```rust
pub fn set_sizing(&mut self, id: SpaceId, sizing: Sizing) -> SpaceId {
    if let Some(node) = self.nodes.get_mut(&id) {
        node.space.sizing = sizing;
    }
    id
}

pub fn host_grow(&mut self, content: ContentId, weight: u32) -> SpaceId {
    let id = self.host(content).id;
    self.set_sizing(id, Sizing::Grow(weight))
}

pub fn host_fixed(&mut self, content: ContentId, size: i32) -> SpaceId {
    let id = self.host(content).id;
    self.set_sizing(id, Sizing::Fixed(size))
}

pub fn container_grow(
    &mut self,
    arrangement: Arrangement,
    children: Vec<SpaceId>,
    weight: u32,
) -> SpaceId {
    let id = self.container(arrangement, children).id;
    self.set_sizing(id, Sizing::Grow(weight))
}
```

Run: `cargo test protocol::scene`

Expected: FAIL because `snapshot` and the new `build_editor_scene` signature are still missing.

- [ ] **Step 4: Replace consuming finish with non-consuming snapshot**

In `src/protocol/scene.rs`, replace:

```rust
pub fn finish(mut self, root: SpaceId, size: Size) -> Result<Scene, BuildError> {
```

with:

```rust
pub fn snapshot(&mut self, root: SpaceId, size: Size) -> Result<Scene, BuildError> {
```

Inside the method, keep the same validation logic, but return a cloned node map:

```rust
Ok(Scene { root, size, nodes: self.nodes.clone() })
```

Keep this compatibility wrapper temporarily for tests or call sites not yet migrated:

```rust
#[allow(dead_code)]
pub fn finish(mut self, root: SpaceId, size: Size) -> Result<Scene, BuildError> {
    self.snapshot(root, size)
}
```

Run: `cargo test protocol::scene`

Expected: FAIL because `build_editor_scene` still has the old signature.

- [ ] **Step 5: Change `build_editor_scene` to accept the long-lived builder**

Replace the function with:

```rust
pub fn build_editor_scene(
    b: &mut SceneBuilder,
    width: i32,
    height: i32,
    editor: ContentId,
    status: ContentId,
) -> Result<(Scene, SpaceId), BuildError> {
    let ed = b.host_grow(editor, 1);
    let st = b.host_fixed(status, 1);
    let root = b.container_grow(
        Arrangement::Flex { direction: Axis::Vertical, gap: 0, align: Align::Stretch },
        vec![ed, st],
        1,
    );
    let scene = b.snapshot(root, Size { width, height })?;
    Ok((scene, ed))
}
```

Update the existing `build_editor_scene_has_two_hosts` test to:

```rust
#[test]
fn build_editor_scene_has_two_hosts() {
    let mut builder = SceneBuilder::new();
    let (scene, editor_space) = build_editor_scene(&mut builder, 80, 24, ContentId(0), ContentId(1)).unwrap();
    let root = scene.node(scene.root);
    match &root.space.kind {
        SpaceKind::Container { children, .. } => assert_eq!(children.len(), 2),
        _ => panic!("root must be container"),
    }
    assert_eq!(editor_space, SpaceId(0));
}
```

Run: `cargo test protocol::scene`

Expected: PASS for protocol scene tests, while whole-crate tests still fail at call sites using the old signature.

- [ ] **Step 6: Make `App` own the builder**

In `src/app/mod.rs`, update the import:

```rust
use crate::protocol::scene::{build_editor_scene, Scene, SceneBuilder};
```

Add a field to `App<F>`:

```rust
scene_builder: SceneBuilder,
```

In `App::new`, replace:

```rust
let (scene, editor_space) =
    build_editor_scene(width as i32, height as i32, editor_content, status_content);
```

with:

```rust
let mut scene_builder = SceneBuilder::new();
let (scene, editor_space) = build_editor_scene(
    &mut scene_builder,
    width as i32,
    height as i32,
    editor_content,
    status_content,
)
.expect("valid editor scene");
```

Add `scene_builder` to the `Ok(Self { ... })` initializer:

```rust
scene_builder,
```

Run: `cargo test`

Expected: FAIL at remaining old `build_editor_scene` call sites.

- [ ] **Step 7: Update old `build_editor_scene` call sites**

For each remaining call site, create a local builder before calling:

```rust
let mut builder = SceneBuilder::new();
let (scene, ed_space) = build_editor_scene(&mut builder, 40, 5, editor, status).unwrap();
```

Files to update:

- `src/app/dispatcher.rs`
- `src/tui/taffy_engine.rs`
- `src/tui/scene_renderer.rs`

Run:

```powershell
rg "build_editor_scene\(" src
cargo test
```

Expected: `rg` shows only calls that pass `&mut builder`; `cargo test` PASS.

- [ ] **Step 8: Commit SceneBuilder lifecycle migration**

Run:

```powershell
git add src/protocol/scene.rs src/protocol/space.rs src/app/mod.rs src/app/dispatcher.rs src/tui/taffy_engine.rs src/tui/scene_renderer.rs
git commit -m "refactor(scene): keep SceneBuilder as long-lived allocator"
```

Expected: Commit succeeds.

## Task 3: Replace CtrlKey With General Modifier Key Events

**Files:**
- Modify: `src/protocol/key_event.rs`
- Modify: `src/protocol/frontend_event.rs`
- Modify: `src/terminal/input.rs`
- Modify: `src/core/buffer.rs`
- Modify: `src/core/keymap.rs`
- Modify: `src/app/dispatcher.rs`
- Modify: `src/app/mod.rs`

- [ ] **Step 1: Write failing protocol tests for generic modifiers**

In `src/protocol/key_event.rs` tests, first change the crossterm import near
the top of the file to avoid name collisions:

```rust
use crossterm::event::{
    KeyCode as CrosstermCode,
    KeyEvent as CrosstermKey,
    KeyModifiers as CrosstermModifiers,
};
```

Then update the test helper:

```rust
fn key(code: CrosstermCode, mods: CrosstermModifiers) -> CrosstermKey {
    CrosstermKey::new(code, mods)
}
```

Replace `ctrl_q_and_s` and `ctrl_other_is_unknown` with:

```rust
#[test]
fn ctrl_ascii_chars_keep_ctrl_modifier() {
    assert_eq!(translate_key(key(CrosstermCode::Char('q'), CrosstermModifiers::CONTROL)), KeyEvent::ctrl('q'));
    assert_eq!(translate_key(key(CrosstermCode::Char('S'), CrosstermModifiers::CONTROL)), KeyEvent::ctrl('s'));
    assert_eq!(translate_key(key(CrosstermCode::Char('x'), CrosstermModifiers::CONTROL)), KeyEvent::ctrl('x'));
}

#[test]
fn ctrl_arrow_and_function_keep_ctrl_modifier() {
    assert_eq!(
        translate_key(key(CrosstermCode::Left, CrosstermModifiers::CONTROL)),
        KeyEvent::modified(KeyCode::Arrow(ArrowKey::Left), KeyModifiers::ctrl())
    );
    assert_eq!(
        translate_key(key(CrosstermCode::F(1), CrosstermModifiers::CONTROL)),
        KeyEvent::modified(KeyCode::Function(1), KeyModifiers::ctrl())
    );
}
```

Run: `cargo test protocol::key_event`

Expected: FAIL because the new `KeyEvent::ctrl`, `KeyCode::Arrow`, `KeyCode::Function`, and local `KeyModifiers` model do not exist.

- [ ] **Step 2: Replace key event types**

In `src/protocol/key_event.rs`, replace the old `CtrlKey` and `KeyEvent` enum with:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct KeyModifiers {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

impl KeyModifiers {
    pub fn none() -> Self { Self::default() }
    pub fn ctrl() -> Self { Self { ctrl: true, alt: false, shift: false } }
    pub fn alt() -> Self { Self { ctrl: false, alt: true, shift: false } }
    pub fn shift() -> Self { Self { ctrl: false, alt: false, shift: true } }
    pub fn ctrl_shift() -> Self { Self { ctrl: true, alt: false, shift: true } }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ArrowKey {
    Up,
    Down,
    Left,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyCode {
    Char(char),
    Arrow(ArrowKey),
    Backspace,
    Enter,
    Escape,
    Function(u8),
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeyEvent {
    pub code: KeyCode,
    pub modifiers: KeyModifiers,
}

impl KeyEvent {
    pub fn plain(code: KeyCode) -> Self {
        Self { code, modifiers: KeyModifiers::none() }
    }
    pub fn char(c: char) -> Self {
        Self::plain(KeyCode::Char(c))
    }
    pub fn ctrl(c: char) -> Self {
        Self::modified(KeyCode::Char(c.to_ascii_lowercase()), KeyModifiers::ctrl())
    }
    pub fn arrow(arrow: ArrowKey) -> Self {
        Self::plain(KeyCode::Arrow(arrow))
    }
    pub fn shift_arrow(arrow: ArrowKey) -> Self {
        Self::modified(KeyCode::Arrow(arrow), KeyModifiers::shift())
    }
    pub fn modified(code: KeyCode, modifiers: KeyModifiers) -> Self {
        Self { code, modifiers }
    }
    pub fn unknown() -> Self {
        Self::plain(KeyCode::Unknown)
    }
    pub fn is_plain_char(&self) -> Option<char> {
        if self.modifiers == KeyModifiers::none() {
            if let KeyCode::Char(c) = self.code {
                return Some(c);
            }
        }
        None
    }
}
```

Run: `cargo test protocol::key_event`

Expected: FAIL because `translate_key` still returns the old variants.

- [ ] **Step 3: Rewrite `translate_key`**

Add a helper in `src/protocol/key_event.rs`:

```rust
fn translate_modifiers(mods: crossterm::event::KeyModifiers) -> KeyModifiers {
    KeyModifiers {
        ctrl: mods.contains(crossterm::event::KeyModifiers::CONTROL),
        alt: mods.contains(crossterm::event::KeyModifiers::ALT),
        shift: mods.contains(crossterm::event::KeyModifiers::SHIFT),
    }
}
```

Replace `translate_key` with:

```rust
pub fn translate_key(k: CrosstermKey) -> KeyEvent {
    let modifiers = translate_modifiers(k.modifiers);
    match k.code {
        CrosstermCode::Char(c) if c.is_ascii_graphic() || c == ' ' => {
            let ch = if modifiers.ctrl { c.to_ascii_lowercase() } else { c };
            KeyEvent::modified(KeyCode::Char(ch), modifiers)
        }
        CrosstermCode::Backspace => KeyEvent::modified(KeyCode::Backspace, modifiers),
        CrosstermCode::Enter => KeyEvent::modified(KeyCode::Enter, modifiers),
        CrosstermCode::Esc => KeyEvent::modified(KeyCode::Escape, modifiers),
        CrosstermCode::Left => KeyEvent::modified(KeyCode::Arrow(ArrowKey::Left), modifiers),
        CrosstermCode::Right => KeyEvent::modified(KeyCode::Arrow(ArrowKey::Right), modifiers),
        CrosstermCode::Up => KeyEvent::modified(KeyCode::Arrow(ArrowKey::Up), modifiers),
        CrosstermCode::Down => KeyEvent::modified(KeyCode::Arrow(ArrowKey::Down), modifiers),
        CrosstermCode::F(n) => KeyEvent::modified(KeyCode::Function(n), modifiers),
        _ => KeyEvent::modified(KeyCode::Unknown, modifiers),
    }
}
```

The import line was already changed in Step 1. Keep using the crossterm aliases:

```rust
use crossterm::event::{
    KeyCode as CrosstermCode,
    KeyEvent as CrosstermKey,
    KeyModifiers as CrosstermModifiers,
};
```

Run: `cargo test protocol::key_event`

Expected: PASS for protocol key event tests.

- [ ] **Step 4: Update core keymap and buffer bindings**

In `src/core/buffer.rs`, replace `default_binding` with:

```rust
fn default_binding(&self, key: KeyEvent) -> Option<Operation> {
    key.is_plain_char()
        .map(|ch| Operation::InsertText(ch.to_string()))
}
```

Replace default keymap bindings:

```rust
km.bind(KeyEvent::plain(KeyCode::Enter), Operation::InsertText("\n".to_string()));
km.bind(KeyEvent::plain(KeyCode::Backspace), Operation::Delete(-1));
km.bind(KeyEvent::arrow(ArrowKey::Left), Operation::MoveLeftBy(1));
km.bind(KeyEvent::arrow(ArrowKey::Right), Operation::MoveRightBy(1));
km.bind(KeyEvent::arrow(ArrowKey::Up), Operation::MoveUpBy(1));
km.bind(KeyEvent::arrow(ArrowKey::Down), Operation::MoveDownBy(1));
km.bind(KeyEvent::shift_arrow(ArrowKey::Left), Operation::ExtendLeftBy(1));
km.bind(KeyEvent::shift_arrow(ArrowKey::Right), Operation::ExtendRightBy(1));
km.bind(KeyEvent::shift_arrow(ArrowKey::Up), Operation::ExtendUpBy(1));
km.bind(KeyEvent::shift_arrow(ArrowKey::Down), Operation::ExtendDownBy(1));
km.bind(KeyEvent::plain(KeyCode::Escape), Operation::Cancel);
```

Update the import:

```rust
use crate::protocol::key_event::{ArrowKey, KeyCode, KeyEvent};
```

Update buffer tests to use:

```rust
KeyEvent::char('a')
KeyEvent::plain(KeyCode::Escape)
KeyEvent::shift_arrow(ArrowKey::Left)
```

Run: `cargo test core::buffer`

Expected: PASS.

- [ ] **Step 5: Update `src/core/keymap.rs` tests**

Update imports:

```rust
use crate::protocol::key_event::{ArrowKey, KeyCode};
```

Replace old constructors:

```rust
KeyEvent::Enter
KeyEvent::Backspace
KeyEvent::Char(b's')
KeyEvent::Char(b'x')
KeyEvent::Arrow(ArrowKey::Left)
```

with:

```rust
KeyEvent::plain(KeyCode::Enter)
KeyEvent::plain(KeyCode::Backspace)
KeyEvent::char('s')
KeyEvent::char('x')
KeyEvent::arrow(ArrowKey::Left)
```

Run: `cargo test core::keymap`

Expected: PASS.

- [ ] **Step 6: Update dispatcher global keymap and tests**

In `src/app/dispatcher.rs`, remove `CtrlKey` from imports and add `KeyCode` where needed:

```rust
use crate::protocol::key_event::{KeyCode, KeyEvent};
```

Replace global bindings:

```rust
km.bind(KeyEvent::ctrl('q'), Operation::Quit);
km.bind(KeyEvent::ctrl('s'), Operation::Save);
```

Update dispatcher tests:

```rust
KeyEvent::char('a')
KeyEvent::plain(KeyCode::Enter)
KeyEvent::arrow(ArrowKey::Left)
KeyEvent::ctrl('q')
KeyEvent::ctrl('s')
KeyEvent::unknown()
```

Run: `cargo test app::dispatcher`

Expected: PASS.

- [ ] **Step 7: Update app and terminal tests**

In `src/app/mod.rs` tests, remove `CtrlKey` from imports and add `KeyCode` if needed:

```rust
use crate::protocol::key_event::{ArrowKey, KeyCode, KeyEvent};
```

Replace:

```rust
KeyEvent::Char(b'a')
KeyEvent::Ctrl(CtrlKey::Q)
KeyEvent::Ctrl(CtrlKey::S)
KeyEvent::Backspace
KeyEvent::Arrow(ArrowKey::Left)
KeyEvent::Shift(ArrowKey::Left)
KeyEvent::Escape
```

with:

```rust
KeyEvent::char('a')
KeyEvent::ctrl('q')
KeyEvent::ctrl('s')
KeyEvent::plain(KeyCode::Backspace)
KeyEvent::arrow(ArrowKey::Left)
KeyEvent::shift_arrow(ArrowKey::Left)
KeyEvent::plain(KeyCode::Escape)
```

In `src/terminal/input.rs` tests, replace expected `KeyEvent::Char(b'a')` with:

```rust
KeyEvent::char('a')
```

Run: `cargo test app terminal`

Expected: PASS.

- [ ] **Step 8: Update frontend event tests**

In `src/protocol/frontend_event.rs` tests, replace the import:

```rust
use crate::protocol::key_event::CtrlKey;
```

with no `CtrlKey` import, and replace:

```rust
KeyEvent::Ctrl(CtrlKey::Q)
```

with:

```rust
KeyEvent::ctrl('q')
```

Run: `cargo test protocol::frontend_event`

Expected: PASS.

- [ ] **Step 9: Full key event migration verification**

Run:

```powershell
rg "CtrlKey|KeyEvent::Char|KeyEvent::Ctrl|KeyEvent::Arrow|KeyEvent::Shift|KeyEvent::Enter|KeyEvent::Backspace|KeyEvent::Escape|KeyEvent::Unknown" src
cargo test
```

Expected: `rg` has no matches for removed variants or `CtrlKey`; `cargo test` PASS.

- [ ] **Step 10: Commit key event migration**

Run:

```powershell
git add src/protocol/key_event.rs src/protocol/frontend_event.rs src/terminal/input.rs src/core/buffer.rs src/core/keymap.rs src/app/dispatcher.rs src/app/mod.rs
git commit -m "refactor(input): use generic key modifier events"
```

Expected: Commit succeeds.

## Task 4: Update Architecture Documentation and Agent Guidance

**Files:**
- Modify: `docs/design/current-architecture.md`
- Modify: `AGENTS.md`

- [ ] **Step 1: Update `docs/design/current-architecture.md` module tree**

In the architecture tree, add:

```text
  └─ frontend/     Frontend trait（纯抽象层，app/tui 共同依赖）
       └─ mod.rs        Frontend trait：next_event + render
```

Remove references to `app/frontend.rs`, `FrontendImpl`, and `HeadlessFrontend`.

Run:

```powershell
rg "FrontendImpl|HeadlessFrontend|app/frontend|tui/headless|trait\\+enum" docs\design\current-architecture.md
```

Expected: No matches.

- [ ] **Step 2: Update dependency descriptions**

Replace the dependency direction description with:

```text
frontend -> protocol
app      -> frontend + core + protocol
tui      -> frontend + terminal + protocol + core
main     -> app + tui + terminal
```

Add this note near the `tui` section:

```text
`tui` 仍依赖 `core` 读取 content 能力边界；本次边界清理只移除
`tui -> app` 依赖。
```

Run:

```powershell
rg "tui.*app|app.*FrontendImpl|Frontend trait \\+ FrontendImpl" docs\design\current-architecture.md
```

Expected: No stale statements saying TUI depends on app or app owns `FrontendImpl`.

- [ ] **Step 3: Update SceneBuilder and key event documentation**

In `docs/design/current-architecture.md`, update `protocol/scene.rs` description to say:

```text
SceneBuilder 是 App 生命周期内唯一 SpaceId 分配者；snapshot(root, size)
生成当前 Scene 渲染快照但不消耗 builder。
```

Update `protocol/key_event.rs` description to say:

```text
KeyEvent = KeyCode + KeyModifiers，可表达 Ctrl/Alt/Shift 与 Char/Arrow/
Enter/Backspace/Escape/Function 的组合。
```

Run:

```powershell
rg "CtrlKey|局部 SceneBuilder|build_editor_scene 产出标准布局|KeyEvent::Char" docs\design\current-architecture.md
```

Expected: No stale old-model statements.

- [ ] **Step 4: Update `AGENTS.md`**

In `AGENTS.md`, update the architecture boundary section to include:

```text
frontend -> protocol
app      -> frontend + core + protocol
tui      -> frontend + terminal + protocol + core
main     -> app + tui + terminal
```

Add guidance:

```text
- `frontend` 是纯抽象层，只放 `Frontend` trait 等前端行为接缝。
- `tui` 不得依赖 `app`；`app` 不得依赖 `tui`。具体接线只在 `main.rs`。
- `App` 持有唯一 `SceneBuilder`。新增 space 必须通过该 builder 分配。
- 按键协议使用 `KeyEvent { code, modifiers }`，不要重新引入 `CtrlKey`
  或 `Shift(ArrowKey)` 这类特化枚举。
```

Run:

```powershell
rg "FrontendImpl|HeadlessFrontend|CtrlKey|Shift\\(ArrowKey\\)|build_editor_scene\\(width" AGENTS.md
git diff --check
```

Expected: `rg` has no stale references; `git diff --check` PASS.

- [ ] **Step 5: Commit documentation updates**

Run:

```powershell
git add docs/design/current-architecture.md AGENTS.md
git commit -m "docs: update architecture boundary guidance"
```

Expected: Commit succeeds.

## Task 5: Final Verification

**Files:**
- Verify: whole repository

- [ ] **Step 1: Format**

Run:

```powershell
cargo fmt
```

Expected: exit code 0.

- [ ] **Step 2: Test**

Run:

```powershell
cargo test
```

Expected: all tests pass.

- [ ] **Step 3: Clippy**

Run:

```powershell
cargo clippy --all-targets --all-features
```

Expected: exit code 0 with no warnings promoted to errors.

- [ ] **Step 4: Boundary searches**

Run:

```powershell
rg "crate::app::Frontend|FrontendImpl|CtrlKey|KeyEvent::Char|KeyEvent::Ctrl|KeyEvent::Shift|pub mod headless" src docs AGENTS.md
```

Expected: no matches.

Run:

```powershell
rg "crate::tui" src\app
```

Expected: no matches.

- [ ] **Step 5: Whitespace hygiene**

Run:

```powershell
git diff --check
```

Expected: exit code 0.

- [ ] **Step 6: Review final diff**

Run:

```powershell
git status --short
git diff --stat HEAD
```

Expected: worktree only contains intentional changes from the implementation tasks and docs.

- [ ] **Step 7: Commit verification cleanup if formatting changed files**

If `cargo fmt` changed files after the previous commits, run:

```powershell
git add src docs AGENTS.md
git commit -m "style: format architecture boundary cleanup"
```

Expected: commit is created only if formatting changed tracked files.
