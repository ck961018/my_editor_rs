# Static Content Store Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace dynamic `ContentHandler` storage and concrete-type probes with a static `Content` enum, `ContentStore`, message-based content execution, and a render-only query projection.

**Architecture:** `core::ContentStore` owns `HashMap<ContentId, Content>`, where `Content` is a closed enum over `Buffer` and `StatusBar`. Keymaps retain cloneable `ContentCommand` values; App converts them into borrowed `ContentInput` values only at execution time. `ContentStore` serves message-based content queries, while App combines those data with per-Space selections through `RenderQuery` for the frontend.

**Tech Stack:** Rust 2024, std `HashMap`, Ropey, Tokio, Crossterm, Taffy, existing unit and integration tests.

## Global Constraints

- Content types are static and closed: do not introduce `Box<dyn ContentHandler>`, a dynamic content registry, or downcasting.
- `View` remains in `app` and owns selections by `SpaceId`; Content only receives a temporary `&mut Selections` for one edit execution.
- `core` must not depend on `tokio`, terminal IO, Taffy, App, TUI, or frontend rendering concepts.
- Content has one execution entry point: `Content::execute(ContentInput<'_>) -> ContentEffect`.
- Content queries are pull-only and owned by `ContentStore`; do not add rendering methods to `Content`.
- `RenderQuery` is read-only and may expose only Content query data and per-Space rendering state.
- Preserve current editing, mode, save, rendering, and status-bar behavior.
- Do not implement new Content variants, cross-View selection transforms, split/panel lifecycle, or mode/keymap runtime extraction.
- Rust changes require `cargo test` and `cargo clippy --all-targets --all-features` before completion.

---

## File Structure

- `src/protocol/content_query.rs`: Content query/data enums and the frontend-facing `RenderQuery` trait.
- `src/core/content.rs`: `Content`, `ContentInput`, `ContentEvent`, `ContentEffect`, and `SaveSnapshot`.
- `src/core/content_store.rs`: static content collection, dispatch helpers, and content query routing.
- `src/core/edit.rs`: Buffer editing algorithm moved from App, now expressed in `EditCommand` terms.
- `src/core/buffer.rs`: Buffer-side command execution, save snapshot preparation, save completion, and keymap terminology.
- `src/core/status_bar.rs`: StatusBar query of `DocumentStatus` through `ContentStore` rather than type probes.
- `src/app/dispatcher.rs`: command targeting against `ContentStore` rather than `ContentLookup`.
- `src/app/mod.rs`: ContentInput construction, ContentEffect handling, RenderQuery adapter, and removal of dynamic content paths.
- `src/app/content.rs` and `src/app/executor.rs`: removed after their responsibilities move to core.
- `src/frontend/mod.rs`, `src/tui/tui_frontend.rs`, `src/tui/scene_renderer.rs`: RenderQuery API migration.
- `AGENTS.md`: replace the outdated ContentHandler contract with the static Content/ContentStore boundary.

### Task 1: Introduce Message-Based Render Queries

**Files:**
- Modify: `src/protocol/content_query.rs`
- Modify: `src/frontend/mod.rs`
- Modify: `src/tui/tui_frontend.rs`
- Modify: `src/tui/scene_renderer.rs`
- Modify: `src/app/mod.rs`

**Interfaces:**
- Produces `ContentQuery`, `ContentData`, `DocumentStatus`, and `RenderQuery` in `protocol`.
- Keeps the temporary App implementation behaviorally identical while its storage remains dynamic until Task 3.
- Later tasks consume `RenderQuery::content`, `RenderQuery::selections`, and `ContentData::Unsupported`.

- [ ] **Step 1: Write protocol tests for content query messages and render projection**

Add tests in `src/protocol/content_query.rs` that construct every message family required now:

```rust
#[test]
fn content_query_and_data_preserve_owned_status() {
    let status = DocumentStatus {
        file_name: Some("note.txt".to_string()),
        modified: true,
        message: StatusMessage::Saved,
    };
    assert_eq!(
        ContentData::DocumentStatus(status.clone()),
        ContentData::DocumentStatus(status),
    );
    assert_eq!(ContentData::Unsupported, ContentData::Unsupported);
}
```

- [ ] **Step 2: Run the focused protocol test and verify it fails**

Run: `cargo test protocol::content_query::tests::content_query_and_data_preserve_owned_status`

Expected: compilation failure because `ContentData` and `DocumentStatus` do not exist.

- [ ] **Step 3: Replace the old typed ContentQuery trait with query/data messages and RenderQuery**

In `src/protocol/content_query.rs`, retain `RowRange` and add the new contract:

```rust
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocumentStatus {
    pub file_name: Option<String>,
    pub modified: bool,
    pub message: StatusMessage,
}

pub type StatusBarData = DocumentStatus;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContentQuery {
    TextRows(RowRange),
    TextLineCount,
    DocumentStatus,
    StatusBarData,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContentData {
    TextRows(Vec<String>),
    TextLineCount(usize),
    DocumentStatus(DocumentStatus),
    StatusBarData(StatusBarData),
    Unsupported,
}

pub trait RenderQuery {
    fn content(&self, id: ContentId, query: ContentQuery) -> ContentData;
    fn selections(&self, id: SpaceId) -> Selections;
}
```

Update `Frontend::render`, `TuiFrontend::render`, and SceneRenderer signatures to take
`&dyn RenderQuery`. In `SceneRenderer`, request text rows, line count, and status data with
`RenderQuery::content`; match the expected `ContentData` variant and retain current empty/default
fallbacks for `Unsupported` or a mismatched variant. Convert `StubQuery` and `MultiSpaceQuery`
tests to implement `RenderQuery`.

Temporarily implement `RenderQuery` for `AppQuery` in `src/app/mod.rs` by translating the new
messages to its existing `as_buffer` and `as_status_bar` behavior. This bridge is removed in
Task 3; it preserves a compiling application while the protocol and TUI migrate first.

- [ ] **Step 4: Run protocol, TUI, and App query tests**

Run:

```text
cargo test protocol::content_query
cargo test tui::scene_renderer
cargo test app::tests::content_query_reads_buffer_and_view
```

Expected: PASS. The renderer still receives rows/status/selections through one read-only query
object, and the multi-Space renderer test still observes distinct selections.

- [ ] **Step 5: Commit the render query contract migration**

```text
git add src/protocol/content_query.rs src/frontend/mod.rs src/tui/tui_frontend.rs src/tui/scene_renderer.rs src/app/mod.rs
git commit -m "refactor: introduce render query messages"
```

### Task 2: Add the Static Content Model in Core

**Files:**
- Create: `src/core/content_store.rs`
- Create: `src/core/edit.rs`
- Modify: `src/core/mod.rs`
- Modify: `src/core/content.rs`
- Modify: `src/core/command.rs`
- Modify: `src/core/keymap.rs`
- Modify: `src/core/buffer.rs`
- Modify: `src/core/status_bar.rs`

**Interfaces:**
- Consumes `ContentQuery`, `ContentData`, `DocumentStatus`, `StatusBarData`, `ContentCommand`,
  `EditCommand`, and `Selections`.
- Produces `Content`, `ContentStore`, `ContentInput<'a>`, `ContentEvent`, `ContentEffect`,
  `SaveSnapshot`, and `ContentStore::{insert,keymap,resolve_key,execute,query}`.
- Task 3 replaces App's dynamic content map with these APIs and removes the temporary legacy
  bridge that remains in this task for compilation.

- [ ] **Step 1: Write failing core tests for static dispatch and execution**

Add tests adjacent to the new core types. Cover these exact cases:

```rust
#[test]
fn buffer_edit_updates_only_the_borrowed_selections() {
    let mut store = ContentStore::new();
    store.insert(ContentId(0), Content::Buffer(Buffer::new()));
    let mut selections = Selections::single(Selection::collapsed(CursorPos::origin()));

    let effect = store.execute(
        ContentId(0),
        ContentInput::WithSelections {
            command: ContentCommand::Edit(EditCommand::InsertText("x".to_string())),
            selections: &mut selections,
        },
    );

    assert_eq!(effect, ContentEffect::None);
    assert_eq!(
        store.query(ContentId(0), ContentQuery::TextRows(RowRange { start: 0, end: 1 })),
        ContentData::TextRows(vec!["x".to_string()]),
    );
    assert_eq!(selections.primary().head().char_index, 1);
}

#[test]
fn status_bar_queries_document_status_without_type_probe() {
    let mut store = ContentStore::new();
    store.insert(ContentId(0), Content::Buffer(Buffer::new()));
    store.insert(ContentId(1), Content::StatusBar(StatusBar::new(ContentId(0))));

    assert!(matches!(
        store.query(ContentId(1), ContentQuery::StatusBarData),
        ContentData::StatusBarData(_),
    ));
}
```

- [ ] **Step 2: Run the focused core tests and verify they fail**

Run:

```text
cargo test core::content_store::tests::buffer_edit_updates_only_the_borrowed_selections
cargo test core::content_store::tests::status_bar_queries_document_status_without_type_probe
```

Expected: compilation failure because `ContentStore`, `Content`, `ContentInput`, and
`EditCommand` do not exist.

- [ ] **Step 3: Implement the static Content model and move edit execution to core**

Rename `TextCommand` to `EditCommand` and `ContentCommand::Text` to
`ContentCommand::Edit` in `src/core/command.rs`; update `Keymap::bind_text` to
`Keymap::bind_edit` and migrate every Buffer keymap and test use.

Create `src/core/edit.rs`, move the full match body from
`app::executor::execute_text_command`, and rename the entry point:

```rust
pub(crate) fn apply_edit(
    command: EditCommand,
    buffer: &mut Buffer,
    selections: &mut Selections,
) {
    // Preserve every existing match arm and selection invariant.
}
```

In `src/core/content.rs`, introduce:

```rust
pub enum ContentInput<'a> {
    Command(ContentCommand),
    WithSelections {
        command: ContentCommand,
        selections: &'a mut Selections,
    },
    Event(ContentEvent),
}

pub enum ContentEvent {
    SaveFinished(std::io::Result<()>),
}

#[derive(Debug, PartialEq, Eq)]
pub struct SaveSnapshot {
    pub path: PathBuf,
    pub bytes: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ContentEffect {
    None,
    Save(SaveSnapshot),
}

pub enum Content {
    Buffer(Buffer),
    StatusBar(StatusBar),
}
```

Implement inherent `keymap`, `keymap_mut`, `resolve_key`, and `execute` on `Content`. Buffer
handles Edit with `apply_edit`, Mode by forwarding to its mode runtime, Save by producing a
snapshot or setting `SaveFailed`, and `SaveFinished` by setting `Saved` or `SaveFailed`.
StatusBar returns `ContentEffect::None` for every input.

Create `ContentStore` in `src/core/content_store.rs` with `HashMap<ContentId, Content>` and
these public operations:

```rust
pub fn insert(&mut self, id: ContentId, content: Content);
pub fn keymap(&self, id: ContentId) -> Option<&Keymap>;
pub fn resolve_key(&self, id: ContentId, key: KeyEvent) -> Option<Command>;
pub fn execute(&mut self, id: ContentId, input: ContentInput<'_>) -> ContentEffect;
pub fn query(&self, id: ContentId, query: ContentQuery) -> ContentData;
```

`query` must use static matches. Buffer returns text rows, line count, and `DocumentStatus`.
StatusBar handles `StatusBarData` by querying its target for `DocumentStatus`; missing or
unsupported targets produce default `StatusBarData`. All unsupported combinations return
`ContentData::Unsupported`.

Keep the existing dynamic ContentHandler implementations only as a temporary compile bridge for
Task 3. Do not use them from the new ContentStore tests or APIs.

- [ ] **Step 4: Run core tests and verify static behavior**

Run:

```text
cargo test core::content_store
cargo test core::edit
cargo test core::buffer
cargo test core::status_bar
```

Expected: PASS. Existing edit semantics are preserved in `core::edit`, and ContentStore handles
Buffer/StatusBar without type probes.

- [ ] **Step 5: Commit the core static content model**

```text
git add src/core/content.rs src/core/content_store.rs src/core/edit.rs src/core/mod.rs src/core/command.rs src/core/keymap.rs src/core/buffer.rs src/core/status_bar.rs
git commit -m "feat: add static content store"
```

### Task 3: Migrate App and Dispatcher to ContentStore

**Files:**
- Delete: `src/app/content.rs`
- Delete: `src/app/executor.rs`
- Modify: `src/app/mod.rs`
- Modify: `src/app/dispatcher.rs`
- Modify: `src/app/message.rs`
- Modify: `src/app/view.rs`

**Interfaces:**
- Consumes `ContentStore::{insert,keymap,resolve_key,execute,query}`, `ContentInput`,
  `ContentEvent`, `ContentEffect`, and `RenderQuery`.
- Removes every production use of `ContentHandler`, `ContentLookup`, `buffer_mut`, `as_buffer`,
  `as_status_bar`, and `app::executor`.
- Produces a behaviorally equivalent App with static content dispatch and async save effects.

- [ ] **Step 1: Update App regression tests for content inputs and save effects**

Update the existing multi-Space test in `src/app/mod.rs` to use the static store API:

```rust
app.contents.insert(other_cid, Content::Buffer(Buffer::new()));
app.views.insert(other_sid, View::new(other_cid));

app.execute_command(DispatchCommand::ViewContent {
    command: ContentCommand::Edit(EditCommand::InsertText("Z".to_string())),
    space: other_sid,
    content: other_cid,
})
.unwrap();

assert_eq!(
    app.contents.query(
        other_cid,
        ContentQuery::TextRows(RowRange { start: 0, end: 1 }),
    ),
    ContentData::TextRows(vec!["Z".to_string()]),
);
assert_eq!(app.views[&other_sid].selections().primary().head().char_index, 1);
```

Keep the existing `ctrl_s_saves_file_and_marks_saved` event sequence. Replace concrete Buffer
inspection with this query assertion:

```rust
assert!(matches!(
    app.contents.query(editor_cid(), ContentQuery::DocumentStatus),
    ContentData::DocumentStatus(DocumentStatus {
        modified: false,
        message: StatusMessage::Saved,
        ..
    }),
));
```

- [ ] **Step 2: Run the focused App tests and verify they fail**

Run:

```text
cargo test app::tests::multi_space_edit_targets_only_focused_content
cargo test app::tests::ctrl_s_saves_file_and_marks_saved
```

Expected: compilation failure because App still owns a dynamic `HashMap` and does not construct
`ContentInput` values.

- [ ] **Step 3: Replace dynamic App content access with ContentStore**

Change App's field to:

```rust
contents: ContentStore,
```

During initialization, insert `Content::Buffer` and `Content::StatusBar` through
`ContentStore::insert`. Update Dispatcher to accept `&ContentStore` and use `keymap` and
`resolve_key`; preserve its existing `DispatchCommand::Content` versus
`DispatchCommand::ViewContent` targeting rules, with `ContentCommand::Edit` taking the
ViewContent route.

In `App::execute_command`, construct exactly one input per dispatch:

```rust
let effect = self.contents.execute(
    content,
    ContentInput::WithSelections {
        command,
        selections: self.views.get_mut(&space).unwrap().selections_mut(),
    },
);
```

For `DispatchCommand::Content`, use `ContentInput::Command(command)`. Centralize effect handling
in an App helper that starts the existing Tokio task for `ContentEffect::Save(snapshot)` and keeps
the `pending_saves` duplicate suppression. On `AppMessage::SaveCompleted`, remove the pending id
and call:

```rust
self.contents.execute(
    content,
    ContentInput::Event(ContentEvent::SaveFinished(result)),
);
```

Update `AppQuery` to implement `RenderQuery`: `content` forwards to `ContentStore::query`; the
selection method continues to read `views`. Remove all temporary dynamic-query branches.

Delete `src/app/content.rs`, `src/app/executor.rs`, their module declarations, `ContentHandler`,
`ContentLookup`, and every Buffer/StatusBar implementation of those traits. Update App tests to
observe rows and status through ContentStore queries rather than concrete Buffer references.

- [ ] **Step 4: Run App, dispatcher, and full test suites**

Run:

```text
cargo test app::tests
cargo test app::dispatcher::tests
cargo test
```

Expected: PASS. Edit affects only the targeted View selections, save writes the same bytes and
returns its result through ContentInput, and no App code borrows a Buffer.

- [ ] **Step 5: Commit the App static dispatch migration**

```text
git add src/app/mod.rs src/app/dispatcher.rs src/app/message.rs src/app/view.rs src/core/content.rs src/core/buffer.rs src/core/status_bar.rs
git rm src/app/content.rs src/app/executor.rs
git commit -m "refactor: dispatch through static content store"
```

### Task 4: Remove Legacy Terms, Update Repository Guidance, and Verify Boundaries

**Files:**
- Modify: `AGENTS.md`
- Modify: `docs/roadmap/2026-07-10-architecture-improvements.md`
- Modify: `src/core/content.rs`
- Modify: `src/core/content_store.rs`
- Modify: `src/protocol/content_query.rs`
- Modify: `src/tui/scene_renderer.rs`

**Interfaces:**
- Consumes the completed static Content, ContentStore, ContentInput, ContentEffect, and
  RenderQuery migration.
- Produces repository guidance and roadmap status consistent with the implemented architecture.

- [ ] **Step 1: Add a boundary regression assertion**

Add a focused source-boundary assertion to an existing core or App test module that reads the
relevant source files with `include_str!` and asserts legacy probes are absent:

```rust
#[test]
fn production_content_paths_have_no_dynamic_type_probes() {
    let app = include_str!("mod.rs");
    let content = include_str!("../core/content.rs");
    assert!(!app.contains("Box<dyn ContentHandler>"));
    assert!(!app.contains("buffer_mut("));
    assert!(!content.contains("as_buffer("));
}
```

Place the test where the relative `include_str!` paths resolve correctly; it is a regression guard
for this architectural decision, not a replacement for the repository-wide search in Step 4.

- [ ] **Step 2: Run the boundary test and verify it passes**

Run: `cargo test production_content_paths_have_no_dynamic_type_probes`

Expected: PASS. Task 3 has already removed every legacy path; this test prevents a future
reintroduction.

- [ ] **Step 3: Update guidance and roadmap status**

In `AGENTS.md`, replace the ContentHandler guidance with these constraints:

```text
- `Content` 是静态闭合的内容集合；新增内容类型必须扩展 `Content` 枚举和 `ContentStore` 分派。
- `ContentStore` 是唯一内容表；app 不得借出或识别 `Buffer`、`StatusBar` 等具体内容类型。
- 内容执行通过 `Content::execute(ContentInput)`，渲染数据通过 `ContentStore::query`；不要向 Content 加入渲染方法。
```

In the roadmap, mark item 3 as complete and summarize the static Content enum, ContentStore,
ContentInput, ContentEffect, and RenderQuery boundaries. Do not modify roadmap items 4 or 5.

- [ ] **Step 4: Run exact repository searches and quality gates**

Run:

```text
rg "ContentHandler|ContentLookup|Box<dyn ContentHandler>|buffer_mut|as_buffer|as_status_bar" src
rg "TextCommand|ContentCommand::Text|bind_text|execute_text_command" src
cargo fmt --check
cargo test
cargo clippy --all-targets --all-features
git diff --check
```

Expected: both `rg` commands produce no matches; formatting, all tests, Clippy, and whitespace
checks pass.

- [ ] **Step 5: Commit the boundary documentation and verification cleanup**

```text
git add AGENTS.md docs/roadmap/2026-07-10-architecture-improvements.md src/core/content.rs src/core/content_store.rs src/protocol/content_query.rs src/tui/scene_renderer.rs
git commit -m "docs: record static content boundaries"
```

## Plan Self-Review

### Spec coverage

- Static `Content` and `ContentStore`: Task 2, then App adoption in Task 3.
- Single `Content::execute` with bare commands, borrowed selections, and events: Tasks 2 and 3.
- Save effect and App-owned async execution: Tasks 2 and 3.
- Content query/data messages, StatusBar derived query, and RenderQuery: Tasks 1 and 2, then
  final App forwarding in Task 3.
- View selection ownership and multi-Space render isolation: Tasks 1 and 3 tests.
- Legacy dynamic-probe removal, repository guidance, roadmap status, and required verification:
  Task 4.

### Placeholder scan

The only legacy names in this document occur in explicit rename steps and repository searches.
Every implementation task names its files, interfaces, tests, commands, expected outcomes, and
commit.

### Type consistency

The plan consistently uses `ContentCommand::Edit(EditCommand)`, `ContentInput`, `ContentEvent`,
`ContentEffect`, `ContentStore`, `ContentQuery`, `ContentData`, `DocumentStatus`, and
`RenderQuery`. `StatusBar` remains the existing type name throughout.
