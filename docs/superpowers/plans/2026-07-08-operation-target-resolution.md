# Operation Target Resolution Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use
> superpowers:subagent-driven-development (recommended) or
> superpowers:executing-plans to implement this plan task-by-task. Steps use
> checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make key dispatch return a resolved operation with explicit source and
target so App execution no longer guesses from the current focused content.

**Architecture:** Keep `Operation` as the keymap action enum. Add
`ResolvedOperation`, `OperationSource`, and `OperationTarget` in
`src/app/dispatcher.rs`; dispatcher resolves source and target while it still
has capture-chain context. `App::execute_operation` consumes the resolved
target and dispatches to app, content-only, or view-content execution.

**Tech Stack:** Rust 2024, existing `tokio` app tests, existing
`ContentHandler`/`Scene`/`View` model, no new dependencies.

---

## File Structure

- Modify `src/app/dispatcher.rs`
  - Owns `ResolvedOperation`, `OperationSource`, `OperationTarget`,
    `CaptureEntry`, and `PendingKeymap`.
  - Converts capture-chain keymap hits and default bindings into resolved
    operations.
  - Keeps keymap lookup mechanics local to dispatcher.

- Modify `src/app/mod.rs`
  - Imports `ResolvedOperation` and `OperationTarget`.
  - Passes resolved operations from `handle_event` into `execute_operation`.
  - Executes by explicit target instead of using `focused_content_id()` as a
    fallback.

- Do not modify `src/core/operation.rs`
  - `Operation` remains a pure action enum.

- Do not modify `src/frontend/*`, `src/tui/*`, or the event-loop task modules.

---

### Task 1: Add Resolved Types and Red Dispatcher Tests

**Files:**
- Modify: `src/app/dispatcher.rs`

- [ ] **Step 1: Keep dispatcher tests importing the parent module**

Inside `#[cfg(test)] mod tests` in `src/app/dispatcher.rs`, keep this existing
import:

```rust
use super::*;
```

The new tests below can reference `OperationTarget` directly once the
dispatcher implementation adds the type.

- [ ] **Step 2: Add failing test for default binding target**

Add this test near `char_falls_through_to_default_binding`:

```rust
#[test]
fn default_binding_resolves_to_focused_view_content() {
    let (mut d, scene, focused, contents) = fixture();

    let resolved = d
        .dispatch(KeyEvent::char('a'), focused, &scene, &contents)
        .unwrap();

    assert_eq!(resolved.operation, Operation::InsertText("a".to_string()));
    assert_eq!(resolved.source.sid, Some(focused));
    assert_eq!(resolved.source.cid, Some(ContentId(0)));
    assert_eq!(
        resolved.target,
        OperationTarget::ViewContent {
            sid: focused,
            cid: ContentId(0),
        }
    );
}
```

- [ ] **Step 3: Add failing tests for global targets**

Add these tests near `global_quit_when_content_no_bind` and
`global_save_when_content_no_bind`:

```rust
#[test]
fn global_quit_resolves_to_app_target() {
    let (mut d, scene, focused, contents) = fixture();

    let resolved = d
        .dispatch(KeyEvent::ctrl('q'), focused, &scene, &contents)
        .unwrap();

    assert_eq!(resolved.operation, Operation::Quit);
    assert_eq!(resolved.source.sid, None);
    assert_eq!(resolved.source.cid, None);
    assert_eq!(resolved.target, OperationTarget::App);
}

#[test]
fn global_save_resolves_to_focused_content() {
    let (mut d, scene, focused, contents) = fixture();

    let resolved = d
        .dispatch(KeyEvent::ctrl('s'), focused, &scene, &contents)
        .unwrap();

    assert_eq!(resolved.operation, Operation::Save);
    assert_eq!(resolved.source.sid, None);
    assert_eq!(resolved.source.cid, None);
    assert_eq!(resolved.target, OperationTarget::Content(ContentId(0)));
}
```

- [ ] **Step 4: Add failing test for host keymap override source**

Replace the old `content_overrides_global` body with this assertion-rich version:

```rust
#[test]
fn content_overrides_global_and_resolves_to_host_source() {
    let (mut d, scene, focused, mut contents) = fixture();
    contents
        .get_mut(&ContentId(0))
        .unwrap()
        .keymap_mut()
        .bind(KeyEvent::ctrl('q'), Operation::InsertText("q".to_string()));

    let resolved = d
        .dispatch(KeyEvent::ctrl('q'), focused, &scene, &contents)
        .unwrap();

    assert_eq!(resolved.operation, Operation::InsertText("q".to_string()));
    assert_eq!(resolved.source.sid, Some(focused));
    assert_eq!(resolved.source.cid, Some(ContentId(0)));
    assert_eq!(
        resolved.target,
        OperationTarget::ViewContent {
            sid: focused,
            cid: ContentId(0),
        }
    );
}
```

- [ ] **Step 5: Add failing test for prefix source retention**

Add this test near `prefix_key_waits_then_completes`:

```rust
#[test]
fn prefix_completion_keeps_original_host_source() {
    let (mut d, scene, focused, mut contents) = fixture();
    let mut sub = Keymap::new();
    sub.bind(KeyEvent::char('s'), Operation::Save);
    contents
        .get_mut(&ContentId(0))
        .unwrap()
        .keymap_mut()
        .bind_prefix(KeyEvent::char('x'), sub);

    assert!(
        d.dispatch(KeyEvent::char('x'), focused, &scene, &contents)
            .is_none()
    );

    let resolved = d
        .dispatch(KeyEvent::char('s'), focused, &scene, &contents)
        .unwrap();

    assert_eq!(resolved.operation, Operation::Save);
    assert_eq!(resolved.source.sid, Some(focused));
    assert_eq!(resolved.source.cid, Some(ContentId(0)));
    assert_eq!(resolved.target, OperationTarget::Content(ContentId(0)));
}
```

- [ ] **Step 6: Run dispatcher tests and verify they fail**

Run:

```powershell
cargo test app::dispatcher
```

Expected: compile failure mentioning unresolved `OperationTarget` or
`ResolvedOperation`, or assertion compile errors because `dispatch` still
returns `Operation`.

Do not commit after this red step.

---

### Task 2: Implement Dispatcher Resolution

**Files:**
- Modify: `src/app/dispatcher.rs`

- [ ] **Step 1: Add resolved-operation types**

Add these definitions after `pub struct Dispatcher`:

```rust
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ResolvedOperation {
    pub operation: Operation,
    pub source: OperationSource,
    pub target: OperationTarget,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct OperationSource {
    pub sid: Option<SpaceId>,
    pub cid: Option<ContentId>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OperationTarget {
    App,
    Content(ContentId),
    ViewContent { sid: SpaceId, cid: ContentId },
}

#[derive(Clone)]
struct PendingKeymap {
    keymap: Keymap,
    source: OperationSource,
}

struct CaptureEntry<'a> {
    keymap: &'a Keymap,
    source: OperationSource,
}
```

- [ ] **Step 2: Change Dispatcher pending field**

Change:

```rust
pending: Option<Keymap>,
```

to:

```rust
pending: Option<PendingKeymap>,
```

Keep `is_pending()` unchanged:

```rust
pub fn is_pending(&self) -> bool {
    self.pending.is_some()
}
```

- [ ] **Step 3: Replace `dispatch` with resolved dispatch**

Replace the whole `dispatch` function with:

```rust
pub fn dispatch(
    &mut self,
    key: KeyEvent,
    focused: SpaceId,
    scene: &Scene,
    contents: &dyn ContentLookup,
) -> Option<ResolvedOperation> {
    if let Some(pending) = self.pending.take() {
        return match lookup_in(&pending.keymap, &key) {
            LookupResult::Hit(op) => {
                resolve_operation(op, pending.source, focused, scene, contents)
            }
            LookupResult::Prefix(sub) => {
                self.pending = Some(PendingKeymap {
                    keymap: sub.clone(),
                    source: pending.source,
                });
                None
            }
            LookupResult::Miss => None,
        };
    }

    for entry in self.capture_chain(focused, scene, contents) {
        match lookup_in(entry.keymap, &key) {
            LookupResult::Hit(op) => {
                return resolve_operation(op, entry.source, focused, scene, contents);
            }
            LookupResult::Prefix(sub) => {
                self.pending = Some(PendingKeymap {
                    keymap: sub.clone(),
                    source: entry.source,
                });
                return None;
            }
            LookupResult::Miss => continue,
        }
    }

    let cid = focused_content_id(scene, focused)?;
    let op = contents.get(cid)?.default_binding(key)?;
    resolve_operation(
        op,
        OperationSource {
            sid: Some(focused),
            cid: Some(cid),
        },
        focused,
        scene,
        contents,
    )
}
```

- [ ] **Step 4: Replace `capture_chain` with source-aware entries**

Replace the whole `capture_chain` function with:

```rust
fn capture_chain<'a>(
    &'a self,
    focused: SpaceId,
    scene: &'a Scene,
    contents: &'a dyn ContentLookup,
) -> Vec<CaptureEntry<'a>> {
    let mut chain = Vec::new();
    let mut cur = Some(focused);
    while let Some(sid) = cur {
        let node = scene.node(sid);
        if let SpaceKind::Host { content } = &node.space.kind {
            if let Some(c) = contents.get(*content) {
                chain.push(CaptureEntry {
                    keymap: c.keymap(),
                    source: OperationSource {
                        sid: Some(sid),
                        cid: Some(*content),
                    },
                });
            }
        }
        cur = node.parent;
    }
    chain.push(CaptureEntry {
        keymap: &self.global_keymap,
        source: OperationSource {
            sid: None,
            cid: None,
        },
    });
    chain
}
```

- [ ] **Step 5: Add target resolver helpers**

Add these helper functions below `lookup_in`:

```rust
fn resolve_operation(
    operation: Operation,
    source: OperationSource,
    focused: SpaceId,
    scene: &Scene,
    contents: &dyn ContentLookup,
) -> Option<ResolvedOperation> {
    let target = resolve_target(&operation, source, focused, scene, contents)?;
    Some(ResolvedOperation {
        operation,
        source,
        target,
    })
}

fn resolve_target(
    operation: &Operation,
    source: OperationSource,
    focused: SpaceId,
    scene: &Scene,
    contents: &dyn ContentLookup,
) -> Option<OperationTarget> {
    match operation {
        Operation::Quit | Operation::FocusNext | Operation::FocusPrev => Some(OperationTarget::App),
        Operation::Save => {
            let cid = source.cid.or_else(|| focused_content_id(scene, focused))?;
            contents.get(cid)?;
            Some(OperationTarget::Content(cid))
        }
        Operation::AddAtNextMatch(_) | Operation::RemoveSecondary => Some(OperationTarget::App),
        _ => {
            let (sid, cid) = match (source.sid, source.cid) {
                (Some(sid), Some(cid)) => (sid, cid),
                _ => {
                    let cid = focused_content_id(scene, focused)?;
                    (focused, cid)
                }
            };
            contents.get(cid)?;
            Some(OperationTarget::ViewContent { sid, cid })
        }
    }
}
```

This keeps current no-op multi-cursor variants app-level because App already
handles them outside `executor::execute`.

- [ ] **Step 6: Update old dispatcher tests to unwrap `.operation`**

For tests that still compare the whole dispatch result to an `Operation`, change
this shape:

```rust
let op = d
    .dispatch(KeyEvent::plain(KeyCode::Enter), focused, &scene, &contents)
    .unwrap();
assert_eq!(op, Operation::InsertText("\n".to_string()));
```

to:

```rust
let resolved = d
    .dispatch(KeyEvent::plain(KeyCode::Enter), focused, &scene, &contents)
    .unwrap();
assert_eq!(resolved.operation, Operation::InsertText("\n".to_string()));
```

Apply the same pattern to:

- `char_falls_through_to_default_binding`
- `buffer_keymap_enter_inserts_newline`
- `buffer_keymap_arrow_left`
- `global_quit_when_content_no_bind`
- `global_save_when_content_no_bind`
- `prefix_key_waits_then_completes`
- `nested_prefix`

- [ ] **Step 7: Run dispatcher tests and verify they pass**

Run:

```powershell
cargo test app::dispatcher
```

Expected: all dispatcher tests pass.

- [ ] **Step 8: Commit dispatcher resolution**

Run:

```powershell
git add src\app\dispatcher.rs
git commit -m "refactor(app): resolve operation targets in dispatcher"
```

---

### Task 3: Migrate App Execution to Resolved Targets

**Files:**
- Modify: `src/app/mod.rs`

- [ ] **Step 1: Write failing app test for explicit save target**

Add this test in `#[cfg(test)] mod tests` near the save tests:

```rust
#[tokio::test(flavor = "multi_thread")]
async fn execute_save_uses_resolved_content_target() {
    let dir = tempfile::tempdir().unwrap();
    let focused_path = dir.path().join("focused.txt");
    let other_path = dir.path().join("other.txt");
    std::fs::write(&focused_path, "focused").unwrap();
    std::fs::write(&other_path, "other").unwrap();
    let focused_path_str = focused_path.to_str().unwrap().to_owned();
    let other_path_str = other_path.to_str().unwrap().to_owned();

    let mut app = make_app(vec![], Some(&focused_path_str));
    let other_cid = ContentId(9);
    let mut other = Buffer::new();
    other.open_path(&other_path_str).unwrap();
    other.insert_char(0, 'X');
    app.contents.insert(other_cid, Box::new(other));

    app.execute_operation(ResolvedOperation {
        operation: Operation::Save,
        source: OperationSource {
            sid: None,
            cid: None,
        },
        target: OperationTarget::Content(other_cid),
    })
    .unwrap();
    app.shutdown_tasks().await.unwrap();

    assert_eq!(std::fs::read_to_string(&focused_path).unwrap(), "focused");
    assert_eq!(std::fs::read_to_string(&other_path).unwrap(), "Xother");
}
```

- [ ] **Step 2: Write failing app test for explicit view-content target**

Add this test near `run_supports_backspace_and_arrows`:

```rust
#[test]
fn execute_edit_uses_resolved_view_content_target() {
    let mut app = make_app(vec![], None);
    let other_cid = ContentId(9);
    let other_sid = app.scene_builder.host_grow(other_cid, 1);
    let scene = app
        .scene_builder
        .snapshot(
            app.scene.root,
            crate::protocol::geometry::Size {
                width: app.scene.size.width,
                height: app.scene.size.height,
            },
        )
        .unwrap();
    app.scene = scene;
    app.contents.insert(other_cid, Box::new(Buffer::new()));
    app.views.insert(other_sid, View::new(other_cid));

    app.execute_operation(ResolvedOperation {
        operation: Operation::InsertText("Z".to_string()),
        source: OperationSource {
            sid: Some(other_sid),
            cid: Some(other_cid),
        },
        target: OperationTarget::ViewContent {
            sid: other_sid,
            cid: other_cid,
        },
    })
    .unwrap();

    let focused_buf = app
        .contents
        .get_mut(&editor_cid())
        .and_then(|c| c.buffer_mut())
        .unwrap();
    assert_eq!(focused_buf.slice().to_string(), "");

    let other_buf = app
        .contents
        .get_mut(&other_cid)
        .and_then(|c| c.buffer_mut())
        .unwrap();
    assert_eq!(other_buf.slice().to_string(), "Z");
    assert_eq!(
        app.views
            .get(&other_sid)
            .unwrap()
            .selections()
            .primary()
            .head()
            .char_index,
        1
    );
}
```

- [ ] **Step 3: Run app tests and verify they fail**

Run:

```powershell
cargo test uses_resolved
```

Expected: compile failure because `ResolvedOperation`, `OperationSource`, and
`OperationTarget` are not imported into `src/app/mod.rs`, or because
`execute_operation` still takes `Operation`.

- [ ] **Step 4: Import resolved dispatcher types**

Change this import:

```rust
use crate::app::dispatcher::{Dispatcher, default_global_keymap};
```

to:

```rust
use crate::app::dispatcher::{
    Dispatcher, OperationSource, OperationTarget, ResolvedOperation, default_global_keymap,
};
```

The test module can then use these names through `use super::*;`.

- [ ] **Step 5: Update `handle_event` variable name**

Change the key branch from:

```rust
if let Some(op) =
    self.dispatcher
        .dispatch(k, self.focused, &self.scene, &self.contents)
{
    self.execute_operation(op)?;
}
```

to:

```rust
if let Some(resolved) =
    self.dispatcher
        .dispatch(k, self.focused, &self.scene, &self.contents)
{
    self.execute_operation(resolved)?;
}
```

- [ ] **Step 6: Replace `execute_operation` implementation**

Replace the whole `execute_operation` function with:

```rust
fn execute_operation(&mut self, resolved: ResolvedOperation) -> io::Result<()> {
    match resolved.target {
        OperationTarget::App => match resolved.operation {
            Operation::Quit => self.tasks.cancel(),
            Operation::FocusNext | Operation::FocusPrev => {}
            Operation::AddAtNextMatch(_) | Operation::RemoveSecondary => {}
            _ => {}
        },
        OperationTarget::Content(cid) => {
            if let Operation::Save = resolved.operation {
                self.spawn_save(cid);
            }
        }
        OperationTarget::ViewContent { sid, cid } => {
            let content: &mut dyn ContentHandler = self
                .contents
                .get_mut(&cid)
                .map(|b| b.as_mut())
                .expect("target content exists");
            let view = self.views.get_mut(&sid).expect("target view exists");
            executor::execute(resolved.operation, content, view.selections_mut());
        }
    }
    Ok(())
}
```

- [ ] **Step 7: Change `focused_content_id` return type**

Replace:

```rust
fn focused_content_id(&self) -> ContentId {
    self.views
        .get(&self.focused)
        .map(|v| v.content())
        .unwrap_or(ContentId(0))
}
```

with:

```rust
#[allow(dead_code)]
fn focused_content_id(&self) -> Option<ContentId> {
    self.views.get(&self.focused).map(|v| v.content())
}
```

This method should no longer be used by execution. Keeping it temporarily is
acceptable only if no production path calls it after this task; the final cleanup
task checks that.

- [ ] **Step 8: Run targeted app tests and verify they pass**

Run:

```powershell
cargo test uses_resolved
```

Expected: both tests pass.

- [ ] **Step 9: Run all app tests**

Run:

```powershell
cargo test app
```

Expected: all app tests pass.

- [ ] **Step 10: Commit app execution migration**

Run:

```powershell
git add src\app\mod.rs
git commit -m "refactor(app): execute resolved operation targets"
```

---

### Task 4: Remove Stale Focus Fallback and Tighten Coverage

**Files:**
- Modify: `src/app/mod.rs`
- Modify: `src/app/dispatcher.rs`

- [ ] **Step 1: Remove `focused_content_id` from App if unused**

Run:

```powershell
rg -n "focused_content_id" src\app
```

Expected before edit: only one match in `src/app/mod.rs` if Task 3 removed all
execution uses.

If that is the only match, delete the whole App method:

```rust
fn focused_content_id(&self) -> Option<ContentId> {
    self.views.get(&self.focused).map(|v| v.content())
}
```

- [ ] **Step 2: Add test for global editing fallback target**

In `src/app/dispatcher.rs`, add this test near the global tests:

```rust
#[test]
fn global_edit_operation_resolves_to_focused_view_content() {
    let editor = ContentId(0);
    let status = ContentId(1);
    let mut builder = SceneBuilder::new();
    let (scene, focused) = build_editor_scene(&mut builder, 40, 5, editor, status).unwrap();
    let mut contents: HashMap<ContentId, Box<dyn ContentHandler>> = HashMap::new();
    contents.insert(editor, Box::new(Buffer::new()));
    contents.insert(status, Box::new(StatusBar::new(editor)));

    let mut global = Keymap::new();
    global.bind(KeyEvent::char('g'), Operation::InsertText("g".to_string()));
    let mut d = Dispatcher::new(global);

    let resolved = d
        .dispatch(KeyEvent::char('g'), focused, &scene, &contents)
        .unwrap();

    assert_eq!(resolved.operation, Operation::InsertText("g".to_string()));
    assert_eq!(resolved.source.sid, None);
    assert_eq!(resolved.source.cid, None);
    assert_eq!(
        resolved.target,
        OperationTarget::ViewContent {
            sid: focused,
            cid: editor,
        }
    );
}
```

- [ ] **Step 3: Add test for app-level focus operations**

In `src/app/dispatcher.rs`, add this test near `global_edit_operation...`:

```rust
#[test]
fn global_focus_operation_resolves_to_app_target() {
    let (mut d, scene, focused, contents) = fixture();
    d.global_keymap
        .bind(KeyEvent::char('n'), Operation::FocusNext);

    let resolved = d
        .dispatch(KeyEvent::char('n'), focused, &scene, &contents)
        .unwrap();

    assert_eq!(resolved.operation, Operation::FocusNext);
    assert_eq!(resolved.target, OperationTarget::App);
}
```

This test is inside the same module as `Dispatcher`, so it can access the
private `global_keymap` field.

- [ ] **Step 4: Run focused fallback search**

Run:

```powershell
rg -n "focused_content_id\\(|unwrap_or\\(ContentId\\(0\\)\\)|Option<Operation>|pending: Option<Keymap>" src\app
```

Expected:

- No `App::focused_content_id` match.
- No `unwrap_or(ContentId(0))` match.
- No `Option<Operation>` return in dispatcher dispatch.
- No `pending: Option<Keymap>`.

- [ ] **Step 5: Run dispatcher and app tests**

Run:

```powershell
cargo test app
```

Expected: all selected tests pass.

- [ ] **Step 6: Commit cleanup and coverage**

Run:

```powershell
git add src\app\dispatcher.rs src\app\mod.rs
git commit -m "test(app): cover resolved operation target rules"
```

---

### Task 5: Final Verification and Hygiene

**Files:**
- Verify: `src/app/dispatcher.rs`
- Verify: `src/app/mod.rs`
- Verify: `src/core/operation.rs`
- Verify: `docs/superpowers/specs/2026-07-08-operation-target-resolution-design.md`

- [ ] **Step 1: Verify Operation variants stayed target-free**

Run:

```powershell
rg -n "Operation::.*ContentId|Operation::.*SpaceId|Save \\{|InsertText \\{" src\core src\app
```

Expected: no matches indicating `Operation` variants were changed to carry
`ContentId` or `SpaceId`. Matches in docs or comments are not part of this
command.

- [ ] **Step 2: Verify dispatch returns resolved operations**

Run:

```powershell
rg -n "Option<Operation>|-> Option<ResolvedOperation>|fn execute_operation" src\app
```

Expected:

- `src/app/dispatcher.rs` has `-> Option<ResolvedOperation>`.
- `src/app/mod.rs` has `fn execute_operation(&mut self, resolved: ResolvedOperation)`.
- No `Option<Operation>` match in app dispatch path.

- [ ] **Step 3: Format the code**

Run:

```powershell
cargo fmt
```

Expected: command exits successfully.

- [ ] **Step 4: Run the full test suite**

Run:

```powershell
cargo test
```

Expected: all tests pass.

- [ ] **Step 5: Check diff whitespace**

Run:

```powershell
git diff --check
```

Expected: no output.

- [ ] **Step 6: Inspect final status**

Run:

```powershell
git status --short
```

Expected: only intentional files modified. If `cargo fmt` changed files already
included in prior commits, inspect them and commit the formatting change:

```powershell
git add src\app\dispatcher.rs src\app\mod.rs
git commit -m "style(app): format operation target resolution"
```

If there is no diff after verification, do not create an empty commit.

---

## Self-Review Checklist

- Spec coverage:
  - Source and target types are introduced in Task 2.
  - Dispatcher returns `ResolvedOperation` in Task 2.
  - Pending prefix source retention is tested in Task 1 and implemented in
    Task 2.
  - App execution consumes target explicitly in Task 3.
  - Removal of focused-content fallback is checked in Task 4 and Task 5.
  - Existing `Operation` variants remain target-free in Task 5.

- Type consistency:
  - `ResolvedOperation.operation` uses existing `Operation`.
  - `OperationSource` fields are `Option<SpaceId>` and `Option<ContentId>`.
  - `OperationTarget::ViewContent` contains both `sid` and `cid`.
  - `App::execute_operation` accepts `ResolvedOperation`, matching dispatcher
    output.

- Verification commands:
  - `cargo test app::dispatcher`
  - `cargo test app`
  - `cargo fmt`
  - `cargo test`
  - `git diff --check`
