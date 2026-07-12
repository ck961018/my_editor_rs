Exit code: 0
Wall time: 0.3 seconds
Output:
# ViewData and Vim Cursor Style Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the selection-only render getter with a complete `ViewData` snapshot and show a block cursor for focused Vim Normal-mode views.

**Architecture:** `RenderQuery::view(SpaceId)` returns the complete common render state for one View. `AppQuery` assembles it from View-owned selections and a cursor style derived statically through `ContentStore -> Content -> Buffer -> ModeSet`. The TUI consumes only `ViewData`, while the terminal maps its neutral cursor style to crossterm commands.

**Tech Stack:** Rust 2024 (MSRV 1.85), crossterm 0.29, existing static `Content` enum, Taffy-backed TUI renderer.

## Global Constraints

- Preserve `frontend -> protocol`, `app -> frontend + core + protocol`, `tui -> frontend + terminal + protocol`, `terminal -> protocol`, and `core -> protocol/std` dependency directions.
- Keep `App<F: Frontend>` statically dispatched; do not introduce trait-object frontends or an app dependency on `tui`.
- Keep `Content` as the closed static enum and `ContentStore` as the only content table; app must not match on Buffer or StatusBar.
- Keep layout, viewport state, and follow policy in `SceneRenderer`.
- Render data remains pull-based and owned: content rows by `ContentId`, complete per-view state by `SpaceId`.
- Do not add cursor blink, underline, bar, or configuration behavior in this change.
- Run `cargo test` after every Rust task and `cargo clippy --all-targets --all-features` after the final cross-layer task.

---

## File Structure

| File | Responsibility |
|---|---|
| `src/protocol/content_query.rs` | Define `CursorStyle` and full `ViewData`; change `RenderQuery` to return a View snapshot. |
| `src/core/mode.rs` | Derive cursor style from each mode runtime. |
| `src/core/buffer.rs` | Forward the style query to `ModeSet`. |
| `src/core/content.rs` | Statically dispatch the style query for all Content variants. |
| `src/core/content_store.rs` | Route a View runtime style query through the sole content table. |
| `src/app/mod.rs` | Assemble `ViewData` from View selections and ContentStore runtime-derived style. |
| `src/tui/scene_renderer.rs` | Consume complete View snapshots and set the focused physical cursor style. |
| `src/terminal/output.rs` | Add the neutral cursor-style operation to `Canvas` and map it to crossterm. |
| `src/terminal/lifecycle.rs` | Restore default cursor style on terminal guard drop. |

### Task 1: Introduce Complete ViewData Rendering Snapshot

**Files:**
- Modify: `src/protocol/content_query.rs`
- Modify: `src/app/mod.rs`
- Modify: `src/tui/scene_renderer.rs`

**Interfaces:**
- Produces:

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CursorStyle {
    Default,
    Block,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ViewData {
    pub selections: Selections,
    pub cursor_style: CursorStyle,
}

pub trait RenderQuery {
    fn content(&self, id: ContentId, query: ContentQuery) -> ContentData;
    fn view(&self, id: SpaceId) -> ViewData;
}
```

- Consumes: existing `View::selections()` and `RenderQuery` test stubs.
- Temporary behavior: `AppQuery::view` returns `CursorStyle::Default` until Task 2 derives it from ContentRuntime.

- [ ] **Step 1: Write failing protocol tests for the full snapshot**

In `src/protocol/content_query.rs` test module, add a test that constructs and compares:

```rust
let data = ViewData {
    selections: Selections::single(Selection::collapsed(CursorPos::origin())),
    cursor_style: CursorStyle::Block,
};
assert_eq!(data.cursor_style, CursorStyle::Block);
assert_eq!(data.selections.primary().head(), CursorPos::origin());
```

Import `CursorPos` and `Selection` from `crate::protocol::selection`. Add the types and change the trait declaration only after this test has been written, so the first compile reports the missing names/method.

- [ ] **Step 2: Run the focused protocol test and verify it fails**

Run: `cargo test protocol::content_query::tests`

Expected: compilation failure because `ViewData` and `CursorStyle` do not yet exist.

- [ ] **Step 3: Add protocol types and migrate RenderQuery implementations**

In `src/protocol/content_query.rs`, keep `ContentQuery`/`ContentData` intact, add `CursorStyle` and `ViewData` after `StatusBarData`, and replace:

```rust
fn selections(&self, sid: SpaceId) -> Selections;
```

with:

```rust
fn view(&self, id: SpaceId) -> ViewData;
```

In `src/app/mod.rs`, replace `AppQuery::selections` with:

```rust
fn view(&self, sid: SpaceId) -> ViewData {
    let view = self
        .views
        .get(&sid)
        .expect("scene content space has view");
    ViewData {
        selections: view.selections().clone(),
        cursor_style: CursorStyle::Default,
    }
}
```

Update the existing AppQuery test to call `query.view(app.focused)` and inspect `data.selections`. Extend it to assert the temporary `CursorStyle::Default`.

In every `SceneRenderer` test stub, implement `view` instead of `selections`:

```rust
fn view(&self, _sid: SpaceId) -> ViewData {
    ViewData {
        selections: self.selections.clone(),
        cursor_style: CursorStyle::Default,
    }
}
```

For `MultiSpaceQuery`, replace its `HashMap<SpaceId, Selections>` field with `HashMap<SpaceId, ViewData>` and return `self.views[&sid].clone()`. Replace all renderer reads of `query.selections(sid)` with `query.view(sid).selections`; Task 3 will remove repeated reads.

- [ ] **Step 4: Run tests and formatting**

Run: `cargo fmt`

Run: `cargo test`

Expected: all tests pass; the visible behavior is unchanged because every current view supplies `CursorStyle::Default`.

- [ ] **Step 5: Commit the protocol snapshot migration**

```bash
git add src/protocol/content_query.rs src/app/mod.rs src/tui/scene_renderer.rs
git commit -m "feat: expose complete view render data"
```

### Task 2: Derive Cursor Style from Content Runtime

**Files:**
- Modify: `src/core/mode.rs`
- Modify: `src/core/buffer.rs`
- Modify: `src/core/content.rs`
- Modify: `src/core/content_store.rs`
- Modify: `src/app/mod.rs`

**Interfaces:**
- Consumes: `CursorStyle` and `ViewData` from Task 1; existing `ModeRuntime`, `BufferRuntime`, and `ContentRuntime`.
- Produces:

```rust
trait Mode {
    fn cursor_style(&self, state: &dyn ModeState) -> CursorStyle;
}

impl ModeSet {
    fn cursor_style(&self, runtime: &ModeRuntime) -> CursorStyle;
}

impl Buffer {
    fn cursor_style(&self, runtime: &BufferRuntime) -> CursorStyle;
}

impl Content {
    fn cursor_style(&self, runtime: &ContentRuntime) -> CursorStyle;
}

impl ContentStore {
    fn cursor_style(&self, id: ContentId, runtime: &ContentRuntime) -> CursorStyle;
}
```

- [ ] **Step 1: Write failing core tests for Vim and Content style dispatch**

In `src/core/mode.rs`, add a test that creates `ModeSet::vim()`, asserts the new runtime reports `CursorStyle::Block`, executes `enter-insert`, and asserts `CursorStyle::Default`.

In `src/core/content.rs`, add a test that creates `Content::StatusBar`, builds its runtime, and asserts `content.cursor_style(&runtime) == CursorStyle::Default`.

In `src/core/content_store.rs`, add a test that creates a Buffer runtime, asserts `store.cursor_style(id, &runtime) == CursorStyle::Block`, executes `ContentCommand::Mode { mode: ModeId::new("vim"), action: ModeActionId::new("enter-insert") }` through `ContentInput::View`, and then asserts `Default`.

- [ ] **Step 2: Run the focused core tests and verify they fail**

Run: `cargo test cursor_style`

Expected: compilation failure because no cursor-style query methods exist.

- [ ] **Step 3: Implement static runtime dispatch**

Import `CursorStyle` from `crate::protocol::content_query` in the affected core files. Add `cursor_style` to `Mode` and implement it as follows:

```rust
impl Mode for PlainEditMode {
    fn cursor_style(&self, _state: &dyn ModeState) -> CursorStyle {
        CursorStyle::Default
    }
}

impl Mode for VimMode {
    fn cursor_style(&self, state: &dyn ModeState) -> CursorStyle {
        match self.state(state).state {
            VimState::Normal => CursorStyle::Block,
            VimState::Insert => CursorStyle::Default,
        }
    }
}
```

Forward it unchanged through `ModeSet` and `Buffer`:

```rust
pub(crate) fn cursor_style(&self, runtime: &ModeRuntime) -> CursorStyle {
    self.base.cursor_style(runtime.base.as_ref())
}

pub(crate) fn cursor_style(&self, runtime: &BufferRuntime) -> CursorStyle {
    self.modes.cursor_style(runtime.modes())
}
```

In `Content`, statically match the existing runtime pair. Buffer forwards to `buffer.cursor_style(runtime)`; StatusBar returns `CursorStyle::Default`; a Buffer/StatusBar runtime mismatch panics with the existing `"content/runtime mismatch"` invariant.

Add this `ContentStore` method, preserving the store as the sole content table:

```rust
pub fn cursor_style(&self, id: ContentId, runtime: &ContentRuntime) -> CursorStyle {
    self.contents
        .get(&id)
        .expect("view content exists")
        .cursor_style(runtime)
}
```

Finally, replace Task 1's temporary default in `AppQuery::view`:

```rust
ViewData {
    selections: view.selections().clone(),
    cursor_style: self.contents.cursor_style(view.content(), view.runtime()),
}
```

Add an app test using the existing two-view-one-buffer setup: put only one view into Insert mode, build `AppQuery`, and assert the two `ViewData` values have independent `Block` and `Default` styles while retaining their own selections.

- [ ] **Step 4: Run tests and formatting**

Run: `cargo fmt`

Run: `cargo test`

Expected: all tests pass; the App query now exposes the actual per-view Vim mode state without any app-level Buffer/Vim type probe.

- [ ] **Step 5: Commit runtime-derived style dispatch**

```bash
git add src/core/mode.rs src/core/buffer.rs src/core/content.rs src/core/content_store.rs src/app/mod.rs
git commit -m "feat: derive view cursor style from content runtime"
```

### Task 3: Render and Restore Focused Terminal Cursor Style

**Files:**
- Modify: `src/tui/scene_renderer.rs`
- Modify: `src/terminal/output.rs`
- Modify: `src/terminal/lifecycle.rs`

**Interfaces:**
- Consumes: `ViewData { selections, cursor_style }` from Task 1 and the runtime-derived styles from Task 2.
- Produces:

```rust
pub trait Canvas {
    fn set_cursor_style(&mut self, style: CursorStyle) -> io::Result<()>;
}
```

- [ ] **Step 1: Write failing output and renderer tests**

In `src/terminal/output.rs`, add tests that call:

```rust
let mut out = Output::new(Vec::new());
out.set_cursor_style(CursorStyle::Block).unwrap();
assert!(String::from_utf8(out.into_inner()).unwrap().contains("\x1b[2 q"));
```

and, separately, assert `CursorStyle::Default` emits `"\x1b[0 q"`. Add a trait-object dispatch test that invokes `Canvas::set_cursor_style`.

In `src/tui/scene_renderer.rs`, create a two-space query with left `CursorStyle::Default` and right `CursorStyle::Block`. Render with the right space focused and assert its `Output<Vec<u8>>` contains `"\x1b[2 q"`; render with left focused and assert it contains `"\x1b[0 q"` and not `"\x1b[2 q"`. This proves the physical cursor style comes only from the focused view.

- [ ] **Step 2: Run the focused tests and verify they fail**

Run: `cargo test cursor_style`

Expected: compilation failure because `Canvas` and `Output` do not expose `set_cursor_style` yet.

- [ ] **Step 3: Implement Canvas, renderer, and terminal lifecycle changes**

In `src/terminal/output.rs`, import `CursorStyle` and crossterm's `SetCursorStyle`. Extend `Canvas`, its `Output<W>` implementation, and the inherent `Output<W>` API with `set_cursor_style`. The inherent method must be:

```rust
pub fn set_cursor_style(&mut self, style: CursorStyle) -> io::Result<()> {
    let style = match style {
        CursorStyle::Default => cursor::SetCursorStyle::DefaultUserShape,
        CursorStyle::Block => cursor::SetCursorStyle::SteadyBlock,
    };
    queue!(self.out, style)
}
```

In `src/tui/scene_renderer.rs`, obtain one `ViewData` per resolved render item and keep it in a `HashMap<SpaceId, ViewData>`. Obtain the focused snapshot from that map with `expect("focused view has render data")`. Pass each item's snapshot to `paint_item` so selection highlighting reads `view.selections` rather than querying again. Immediately before `canvas.show_cursor()?`, add:

```rust
canvas.set_cursor_style(focused_view.cursor_style)?;
```

Keep the current zero-size focused-item condition: if no physical cursor is shown, no style command is needed for that frame.

In `src/terminal/lifecycle.rs`, import `cursor::SetCursorStyle` and restore it in `Drop` before leaving the alternate screen:

```rust
let _ = execute!(
    io::stdout(),
    SetCursorStyle::DefaultUserShape,
    LeaveAlternateScreen
);
```

Keep `disable_raw_mode()` after the `execute!` call.

- [ ] **Step 4: Run complete verification**

Run: `cargo fmt`

Run: `cargo test`

Run: `cargo clippy --all-targets --all-features`

Expected: tests pass. Clippy reports no new warnings or errors; existing repository `dead_code` warnings may remain unchanged.

- [ ] **Step 5: Commit terminal cursor rendering**

```bash
git add src/tui/scene_renderer.rs src/terminal/output.rs src/terminal/lifecycle.rs
git commit -m "feat: render vim normal cursor as block"
```

## Final Verification

- [ ] Run `git diff --check` and confirm no whitespace errors.
- [ ] Run `git status --short` and confirm the worktree is clean after the three planned commits.
- [ ] Manually run `cargo run -- <path>` in a terminal that supports DECSCUSR: verify Normal uses a stable block, `i` restores the terminal default, `Escape` restores the block, and quitting restores the shell cursor default.


