# Scene Mutation and View Lifecycle Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `SceneBuilder` directly and safely mutate the single App-owned
`Scene`, then make App reconcile View, ContentRuntime and focus after each
successful layout change.

**Architecture:** `Scene` becomes the only node table. `SceneBuilder` retains
only a monotonic `next_space_id` and exposes validated semantic operations over
`&mut Scene`. App remains the sole owner of cross-layer lifecycle: it validates
Content IDs before structural mutation, then preserves or replaces Views by
`SpaceId + ContentId` and resolves focus among focusable Content spaces.

**Tech Stack:** Rust 2024, std `HashMap`, existing `ContentStore`, `crossterm`
independent protocol types, Taffy-based TUI tests.

## Global Constraints

- Keep dependency direction unchanged: `protocol` depends only on std; `app`
  may depend on `core`, `frontend` and `protocol`; `tui` must not depend on
  `app`.
- Do not clone Scene, introduce a Scene draft, mutation log, persistent tree,
  plugin callback or asynchronous layout mutation.
- `SceneBuilder` must not retain a node table, root or snapshot; it owns only
  the monotonic SpaceId allocator.
- Scene structure is mutable only inside `protocol::scene` through Builder
  operations. Do not leave a public `Scene::node_mut` escape hatch.
- Content Space focusability is an instance property. Every Content Space gets
  a View; only focusable Content Spaces can become `App.focused`.
- Do not add user-visible split/close commands, key bindings, pane borders or
  focus-navigation UI in this plan.
- Preserve the static `Content` enum, `ContentStore`, pull-based RenderQuery
  model and App generic frontend dispatch.
- Do not modify the user-maintained `docs/roadmap/` files or unrelated dirty
  files (`AGENTS.md`, `src/core/buffer.rs`) while implementing this plan.
- For Rust changes run `cargo test` and `cargo clippy --all-targets --all-features`.

---

## File Structure

- `src/protocol/space.rs`: Space role data. Remove duplicate Container child
  storage, add Content Space focusability, and add a directional split type.
- `src/protocol/scene.rs`: The sole Scene node table, Builder ID allocation,
  structural validation, initial layout construction and semantic mutations.
- `src/app/mod.rs`: App layout operations, View reconciliation, focus
  resolution, and integration tests. No frontend-specific behavior belongs
  here.
- `src/core/content_store.rs`: Read-only Content existence query used by App
  preflight validation.
- `src/app/dispatcher.rs`: Update `SpaceKind::Content` pattern matches and
  fixture construction to the new protocol API.
- `src/tui/taffy_engine.rs`: Traverse `SpaceNode.children` instead of a
  duplicate list in `SpaceKind::Container`.
- `src/tui/scene_renderer.rs`: Update test scene construction to direct
  Builder mutation.

## Task 1: Make Scene the Only Node Table

**Files:**
- Modify: `src/protocol/space.rs`
- Modify: `src/protocol/scene.rs`
- Modify: `src/app/dispatcher.rs`
- Modify: `src/app/mod.rs`
- Modify: `src/tui/taffy_engine.rs`
- Modify: `src/tui/scene_renderer.rs`
- Test: inline tests in the same files

**Interfaces:**
- Consumes: existing `Scene`, `SpaceNode`, `SpaceKind`, `SceneBuilder`,
  `build_editor_scene`, and all pattern matches over `SpaceKind`.
- Produces: a Scene-owned `HashMap<SpaceId, SpaceNode>`; a Builder with only
  `next_space_id`; `SplitDirection`; `SceneError`; and
  `Scene::contains(SpaceId)` and
  `SceneBuilder::split(&mut Scene, SpaceId, ContentId, bool, SplitDirection)`.
  Task 2 adds the remaining Builder mutations. Task 3 consumes `focusable`
  and the direct Scene mutation API.

- [ ] **Step 1: Write the failing protocol tests for focusability and split**

  In `src/protocol/scene.rs` tests, replace the snapshot allocation tests with
  the following coverage:

  ```rust
  #[test]
  fn standard_scene_marks_editor_focusable_and_status_inert() {
      let mut builder = SceneBuilder::new();
      let (scene, editor) =
          build_editor_scene(&mut builder, 80, 24, ContentId(0), ContentId(1)).unwrap();

      assert!(content_focusable(&scene, editor));
      let status = scene.node(scene.root).children[1];
      assert!(!content_focusable(&scene, status));
  }

  #[test]
  fn split_on_matching_axis_inserts_a_sibling_and_advances_id() {
      let mut builder = SceneBuilder::new();
      let (mut scene, editor) =
          build_editor_scene(&mut builder, 80, 24, ContentId(0), ContentId(1)).unwrap();
      let status = scene.node(scene.root).children[1];

      let result = builder
          .split(
              &mut scene,
              status,
              ContentId(1),
              false,
              SplitDirection::Down,
          )
          .unwrap();

      assert_eq!(result.new_space, SpaceId(3));
      assert_eq!(scene.node(status).parent, scene.node(result.new_space).parent);
      assert_tree_valid(&scene);
  }

  #[test]
  fn split_on_different_axis_wraps_target_in_a_new_container() {
      let mut builder = SceneBuilder::new();
      let (mut scene, editor) =
          build_editor_scene(&mut builder, 80, 24, ContentId(0), ContentId(1)).unwrap();

      let result = builder
          .split(
              &mut scene,
              editor,
              ContentId(0),
              true,
              SplitDirection::Right,
          )
          .unwrap();

      let parent = scene.node(editor).parent.expect("split parent exists");
      assert_eq!(parent, scene.node(result.new_space).parent.unwrap());
      assert!(matches!(
          scene.node(parent).space.kind,
          SpaceKind::Container {
              arrangement: Arrangement::Flex {
                  direction: Axis::Horizontal,
                  ..
              }
          }
      ));
      assert_tree_valid(&scene);
  }
  ```

  Add test-only helpers `content_focusable` and `assert_tree_valid` in the
  module. `assert_tree_valid` must walk from `scene.root`, check the parent
  relation for every child, reject repeated visits, and assert that every
  Content node has no children. It must finally assert that its visited set
  has the same length as `scene.nodes`, so no detached node survives a
  successful mutation.

- [ ] **Step 2: Run the protocol tests to verify they fail**

  Run:

  ```powershell
  cargo test protocol::scene::tests::standard_scene_marks_editor_focusable_and_status_inert
  ```

  Expected: FAIL because `focusable`, `SplitDirection`, and the direct
  `SceneBuilder::split` API do not exist.

- [ ] **Step 3: Replace Builder-owned nodes with Scene-owned nodes**

  In `src/protocol/space.rs`, change the layout role types to exactly:

  ```rust
  #[derive(Clone)]
  pub enum SpaceKind {
      Container {
          arrangement: Arrangement,
      },
      Content {
          content: ContentId,
          focusable: bool,
      },
  }

  #[derive(Clone, Copy, Debug, PartialEq, Eq)]
  pub enum SplitDirection {
      Left,
      Right,
      Up,
      Down,
  }

  impl SplitDirection {
      pub const fn axis(self) -> Axis {
          match self {
              Self::Left | Self::Right => Axis::Horizontal,
              Self::Up | Self::Down => Axis::Vertical,
          }
      }

      pub const fn inserts_before(self) -> bool {
          matches!(self, Self::Left | Self::Up)
      }
  }
  ```

  In `src/protocol/scene.rs`, remove `nodes` from `SceneBuilder`, remove
  `SpaceHandle`, `snapshot`, `finish`, and public `Scene::node_mut`. Keep
  `Scene.nodes` private. Define the Builder and error/result types as:

  ```rust
  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  pub enum SceneError {
      UnknownSpace(SpaceId),
      ExpectedContentLeaf(SpaceId),
      InvalidTree,
  }

  pub struct SceneBuilder {
      next_space_id: u64,
  }

  pub struct SplitResult {
      pub new_space: SpaceId,
  }
  ```

  Keep these read-only Scene APIs for App and frontend consumers:

  ```rust
  pub fn contains(&self, id: SpaceId) -> bool {
      self.nodes.contains_key(&id)
  }

  pub fn node(&self, id: SpaceId) -> &SpaceNode {
      self.nodes.get(&id).expect("space id exists")
  }
  ```

  Keep allocation private and monotonic:

  ```rust
  fn alloc(&mut self) -> SpaceId {
      let id = SpaceId(self.next_space_id);
      self.next_space_id += 1;
      id
  }
  ```

  Add private Builder helpers that insert a node into `scene.nodes`, update
  `SpaceNode.parent` and `SpaceNode.children`, and construct `Space` with the
  allocated ID. `build_editor_scene` must create its local initial Scene with
  editor `focusable: true`, status `focusable: false`, then add the vertical
  root Container. It remains the public initial-scene helper and continues to
  return `(Scene, editor_space)`.

  Implement `split` in this order:

  ```text
  1. Read and validate target as an existing Content leaf.
  2. Read target parent and, if present, its arrangement and target index.
  3. Allocate the new Content Space only after validation succeeds.
  4. If parent axis matches direction axis, insert the new ID before or after
     target and assign its parent to the existing parent.
  5. Otherwise allocate a Container, put target and new ID in directional
     order, replace target in its old parent or replace Scene.root, and set
     both child parent fields.
  6. Assert the resulting tree is valid in debug builds and return SplitResult.
  ```

  Do not retain detached historical nodes: every new node is immediately
  attached to the current tree.

- [ ] **Step 4: Update all protocol consumers to the single child list**

  Make the following mechanical API updates:

  ```rust
  // Before
  SpaceKind::Container { children, .. } => {
      for child in children { /* ... */ }
  }

  // After
  SpaceKind::Container { .. } => {
      for child in &node.children { /* ... */ }
  }
  ```

  Apply this in:

  - `src/app/mod.rs` current `collect_content_spaces` helper;
  - `src/app/dispatcher.rs` capture-chain and focused-content matches;
  - `src/tui/taffy_engine.rs` recursive Taffy node construction and collection;
  - all affected test matches in `src/protocol/scene.rs`,
    `src/tui/taffy_engine.rs`, and `src/tui/scene_renderer.rs`.

  Content matches must accept the new field:

  ```rust
  SpaceKind::Content { content, .. }
  ```

  Replace custom test layouts that previously called `content_grow`,
  `container_grow`, and `snapshot` with `build_editor_scene` followed by
  `SceneBuilder::split`. In the renderer tests that need two visible spaces
  for one ContentId, split the editor to the right using the same ContentId,
  then locate the two matching `RenderItem`s by ContentId and assert their
  distinct SpaceIds; ignore the persistent status render item. In the App
  tests that need two views, construct the extra space through the Builder
  and retain the existing temporary `build_views` fixture until Task 3
  replaces it with lifecycle operations.

- [ ] **Step 5: Run focused and full tests to verify the new Scene boundary**

  Run:

  ```powershell
  cargo fmt
  cargo test protocol::scene::tests
  cargo test tui::taffy_engine::tests
  cargo test tui::scene_renderer::tests
  cargo test
  ```

  Expected: PASS. Existing tests may use `build_editor_scene` or direct
  `split`, but no test may call `snapshot` or rely on Builder-owned nodes.

- [ ] **Step 6: Commit the protocol ownership migration**

  ```powershell
  git add src/protocol/space.rs src/protocol/scene.rs src/app/dispatcher.rs src/app/mod.rs src/tui/taffy_engine.rs src/tui/scene_renderer.rs
  git commit -m "refactor: make scene own layout nodes"
  ```

## Task 2: Complete Validated Builder Mutations

**Files:**
- Modify: `src/protocol/scene.rs`
- Modify: `src/protocol/space.rs`
- Test: inline tests in `src/protocol/scene.rs`

**Interfaces:**
- Consumes: Task 1 `SceneBuilder::split`, `SceneError`, `SplitResult`,
  `SplitDirection`, Scene-owned nodes and `SpaceNode.children`.
- Produces: `SceneBuilder::close`, `CloseResult`,
  `SceneBuilder::replace_content`, and `SceneBuilder::set_sizing`. Task 3
  consumes these methods from App lifecycle operations.

- [ ] **Step 1: Write failing close, replacement, sizing, and failure tests**

  Add these tests to `src/protocol/scene.rs`:

  ```rust
  #[test]
  fn close_collapses_single_child_container_and_updates_root() {
      let mut builder = SceneBuilder::new();
      let (mut scene, editor) =
          build_editor_scene(&mut builder, 80, 24, ContentId(0), ContentId(1)).unwrap();
      let status = scene.node(scene.root).children[1];

      let closed = builder.close(&mut scene, status).unwrap();

      assert_eq!(closed.removed_space, status);
      assert_eq!(closed.surviving_neighbor, Some(editor));
      assert_eq!(scene.root, editor);
      assert_tree_valid(&scene);
  }

  #[test]
  fn replace_content_keeps_space_id_and_changes_focusability() {
      let mut builder = SceneBuilder::new();
      let (mut scene, editor) =
          build_editor_scene(&mut builder, 80, 24, ContentId(0), ContentId(1)).unwrap();

      builder
          .replace_content(&mut scene, editor, ContentId(9), false)
          .unwrap();

      assert!(matches!(
          scene.node(editor).space.kind,
          SpaceKind::Content {
              content: ContentId(9),
              focusable: false,
          }
      ));
  }

  #[test]
  fn set_sizing_changes_only_the_requested_space() {
      let mut builder = SceneBuilder::new();
      let (mut scene, editor) =
          build_editor_scene(&mut builder, 80, 24, ContentId(0), ContentId(1)).unwrap();
      let status = scene.node(scene.root).children[1];

      builder
          .set_sizing(&mut scene, editor, Sizing::Fixed(12))
          .unwrap();

      assert!(matches!(scene.node(editor).space.sizing, Sizing::Fixed(12)));
      assert!(matches!(scene.node(status).space.sizing, Sizing::Fixed(1)));
  }

  #[test]
  fn failed_split_leaves_tree_and_next_id_unchanged() {
      let mut builder = SceneBuilder::new();
      let (mut scene, editor) =
          build_editor_scene(&mut builder, 80, 24, ContentId(0), ContentId(1)).unwrap();

      assert_eq!(
          builder.split(
              &mut scene,
              SpaceId(999),
              ContentId(0),
              true,
              SplitDirection::Right,
          ),
          Err(SceneError::UnknownSpace(SpaceId(999)))
      );

      let split = builder
          .split(
              &mut scene,
              editor,
              ContentId(0),
              true,
              SplitDirection::Right,
          )
          .unwrap();
      assert_eq!(split.new_space, SpaceId(3));
  }

  #[test]
  fn deleted_space_ids_are_not_reused() {
      let mut builder = SceneBuilder::new();
      let (mut scene, editor) =
          build_editor_scene(&mut builder, 80, 24, ContentId(0), ContentId(1)).unwrap();
      let first = builder
          .split(
              &mut scene,
              editor,
              ContentId(0),
              true,
              SplitDirection::Right,
          )
          .unwrap();
      builder.close(&mut scene, first.new_space).unwrap();

      let second = builder
          .split(
              &mut scene,
              editor,
              ContentId(0),
              true,
              SplitDirection::Right,
          )
          .unwrap();
      assert_eq!(second.new_space, SpaceId(5));
  }
  ```

- [ ] **Step 2: Run the new Builder tests to verify they fail**

  Run:

  ```powershell
  cargo test protocol::scene::tests::close_collapses_single_child_container_and_updates_root
  ```

  Expected: FAIL because `close`, `CloseResult`, and `replace_content` are not
  defined.

- [ ] **Step 3: Implement close, replacement, and sizing without a generic mutation API**

  Add these protocol types and methods:

  ```rust
  pub struct CloseResult {
      pub removed_space: SpaceId,
      pub surviving_neighbor: Option<SpaceId>,
  }

  pub fn close(
      &mut self,
      scene: &mut Scene,
      target: SpaceId,
  ) -> Result<CloseResult, SceneError>;

  pub fn replace_content(
      &mut self,
      scene: &mut Scene,
      target: SpaceId,
      content: ContentId,
      focusable: bool,
  ) -> Result<(), SceneError>;

  pub fn set_sizing(
      &mut self,
      scene: &mut Scene,
      target: SpaceId,
      sizing: Sizing,
  ) -> Result<(), SceneError>;
  ```

  `close` must validate that `target` is a Content leaf with a parent before
  mutating. Remove it from its parent, choose the immediate sibling at the
  same index or preceding index as `surviving_neighbor`, remove the target
  node, and repeatedly collapse any Container with one remaining child. When
  collapsing, splice the remaining child into the grandparent or make it the
  new root, then update its parent. Extend `SceneError` with
  `CannotCloseRoot(SpaceId)` and return it when `target` is a valid root
  Content leaf, because that close would leave no tree root.

  `replace_content` must validate a Content leaf before replacing only its
  `content` and `focusable` fields. `set_sizing` must validate target existence
  before assigning `node.space.sizing`. Neither method allocates a SpaceId.

  After every successful method call, validate the complete reachable tree in
  debug builds. Keep `SceneError` derived with `Debug`, `PartialEq`, and `Eq`
  so tests can assert exact failures.

- [ ] **Step 4: Run the Builder mutation suite**

  Run:

  ```powershell
  cargo fmt
  cargo test protocol::scene::tests
  cargo test
  ```

  Expected: PASS. In particular, the failed split test proves validation occurs
  before ID allocation and mutation.

- [ ] **Step 5: Commit semantic Builder operations**

  ```powershell
  git add src/protocol/scene.rs src/protocol/space.rs
  git commit -m "feat: add scene layout mutations"
  ```

## Task 3: Reconcile Views and Focus in App

**Files:**
- Modify: `src/app/mod.rs`
- Modify: `src/app/view.rs`
- Modify: `src/core/content_store.rs`
- Test: inline tests in `src/app/mod.rs`, `src/app/view.rs`, and
  `src/core/content_store.rs`

**Interfaces:**
- Consumes: Task 1 direct Scene/Builder ownership and `focusable` Content
  Spaces; Task 2 `split`, `close`, `replace_content`, `set_sizing` and their
  result types.
- Produces: App-private layout operations that preflight Content IDs, mutate
  Scene through Builder, reconcile `HashMap<SpaceId, View>`, and resolve a
  valid focused Space. No dispatcher command or UI binding is added.

- [ ] **Step 1: Write failing App lifecycle tests**

  Add the following tests in `src/app/mod.rs` using `make_app` and its local
  `ScriptedFrontend`. Extend the test module imports with `Sizing` and
  `SplitDirection` from `protocol::space`:

  ```rust
  #[tokio::test(flavor = "multi_thread")]
  async fn split_creates_independent_view_runtime_for_shared_content() {
      let mut app = make_app(vec![], None);
      let left = app.focused;
      app.handle_event(FrontendEvent::Key(KeyEvent::char('i')))
          .await
          .unwrap();
      let right = app
          .split_space(left, editor_cid(), true, SplitDirection::Right, true)
          .unwrap()
          .new_space;

      assert_eq!(app.focused, right);
      app.handle_event(FrontendEvent::Key(KeyEvent::char('a')))
          .await
          .unwrap();

      assert_eq!(text_rows(&app, editor_cid()), vec![""]);
  }

  #[tokio::test(flavor = "multi_thread")]
  async fn unchanged_space_binding_preserves_its_view_selection() {
      let mut app = make_app(vec![], None);
      for key in ['i', 'a', 'b', 'c'] {
          app.handle_event(FrontendEvent::Key(KeyEvent::char(key)))
              .await
              .unwrap();
      }

      app.set_space_sizing(app.focused, Sizing::Fixed(12)).unwrap();

      assert_eq!(
          app.views.get(&app.focused).unwrap().selections().primary().head.char_index,
          3
      );
  }

  #[tokio::test(flavor = "multi_thread")]
  async fn replace_content_rebuilds_view_from_origin() {
      let mut app = make_app(vec![], None);
      let other = ContentId(9);
      app.contents.insert(other, Content::Buffer(Buffer::new()));
      for key in ['i', 'a', 'b', 'c'] {
          app.handle_event(FrontendEvent::Key(KeyEvent::char(key)))
              .await
              .unwrap();
      }

      app.replace_space_content(app.focused, other, true).unwrap();

      let view = app.views.get(&app.focused).unwrap();
      assert_eq!(view.content(), other);
      assert_eq!(view.selections().primary().head(), CursorPos::origin());
      app.handle_event(FrontendEvent::Key(KeyEvent::char('a')))
          .await
          .unwrap();
      assert_eq!(text_rows(&app, other), vec![""]);
  }

  #[test]
  fn close_focused_space_prefers_surviving_neighbor_and_drops_its_view() {
      let mut app = make_app(vec![], None);
      let left = app.focused;
      let right = app
          .split_space(left, editor_cid(), true, SplitDirection::Right, true)
          .unwrap()
          .new_space;

      app.close_space(right).unwrap();

      assert_eq!(app.focused, left);
      assert!(!app.views.contains_key(&right));
  }

  #[test]
  fn missing_content_is_rejected_before_scene_mutation() {
      let mut app = make_app(vec![], None);
      let root = app.scene.root;

      assert!(matches!(
          app.split_space(root, ContentId(999), true, SplitDirection::Right, true),
          Err(LayoutError::MissingContent(ContentId(999)))
      ));
      assert_eq!(app.scene.root, root);
  }

  #[test]
  fn preferred_inert_status_space_is_not_selected() {
      let app = make_app(vec![], None);
      let status = app.scene.node(app.scene.root).children[1];

      assert_eq!(resolve_focus(&app.scene, app.focused, Some(status)), Some(app.focused));
  }
  ```

  Adapt the existing `two_views_of_one_buffer_keep_independent_mode_runtime`
  and `multi_space_edit_targets_only_focused_content` tests to use
  `split_space` instead of directly assigning `app.scene`, `app.views`, or
  `app.focused`.

- [ ] **Step 2: Run an App lifecycle test to verify it fails**

  Run:

  ```powershell
  cargo test app::tests::split_creates_independent_view_runtime_for_shared_content
  ```

  Expected: FAIL because `split_space`, `LayoutError`, and lifecycle
  reconciliation do not exist.

- [ ] **Step 3: Add Content preflight and App layout operations**

  In `src/core/content_store.rs`, add the read-only helper:

  ```rust
  pub fn contains(&self, id: ContentId) -> bool {
      self.contents.contains_key(&id)
  }
  ```

  In `src/app/mod.rs`, add a private error type:

  ```rust
  #[derive(Debug, PartialEq, Eq)]
  enum LayoutError {
      MissingContent(ContentId),
      WouldRemoveLastFocusable(SpaceId),
      NoFocusableSpace,
      Scene(SceneError),
  }

  impl From<SceneError> for LayoutError {
      fn from(error: SceneError) -> Self {
          Self::Scene(error)
      }
  }
  ```

  Add these App-private methods with the exact responsibilities:

  ```rust
  fn split_space(
      &mut self,
      target: SpaceId,
      content: ContentId,
      focusable: bool,
      direction: SplitDirection,
      focus_new: bool,
  ) -> Result<SplitResult, LayoutError>;

  fn close_space(&mut self, target: SpaceId) -> Result<CloseResult, LayoutError>;

  fn replace_space_content(
      &mut self,
      target: SpaceId,
      content: ContentId,
      focusable: bool,
  ) -> Result<(), LayoutError>;

  fn set_space_sizing(
      &mut self,
      target: SpaceId,
      sizing: Sizing,
  ) -> Result<(), LayoutError>;
  ```

  `split_space` and `replace_space_content` must call `contents.contains`
  before Builder mutation. `close_space` must reject removing the last
  focusable Content Space with `WouldRemoveLastFocusable` before calling
  Builder. `replace_space_content` must count the prospective focusable
  Content Spaces and return `NoFocusableSpace` before calling Builder if its
  requested `focusable` value would leave none. `set_space_sizing` delegates
  only to Builder and does not rebuild Views.

- [ ] **Step 4: Implement centralized View reconciliation and focus resolution**

  Replace one-shot `build_views` with a helper that accepts the previous map:

  ```rust
  fn reconcile_views(
      scene: &Scene,
      contents: &ContentStore,
      old_views: HashMap<SpaceId, View>,
  ) -> HashMap<SpaceId, View>;
  ```

  First DFS-collect `(SpaceId, ContentId)` from every Content Space using
  `SpaceNode.children`. Before consuming `old_views`, assert each collected
  ContentId exists in `contents`; App preflight makes this an internal
  invariant. Then remove each old View by SpaceId:

  ```rust
  match old_views.remove(&space_id) {
      Some(view) if view.content() == content_id => view,
      Some(_) | None => View::new(
          content_id,
          contents.create_runtime(content_id).expect("validated content"),
      ),
  }
  ```

  Discard leftover old Views. This gives same-binding preservation, new-space
  runtime creation, removed-space cleanup, and complete reinitialization on
  Content replacement.

  Add:

  ```rust
  fn resolve_focus(
      scene: &Scene,
      previous: SpaceId,
      preferred: Option<SpaceId>,
  ) -> Option<SpaceId>;
  ```

  It must select, in order: a valid preferred focusable Content Space; the
  still-valid previous focusable Content Space; then the first focusable
  Content Space in DFS order. Return `None` if none exist.

  After every successful `split_space`, `close_space`, and
  `replace_space_content`, replace `self.views` with `reconcile_views` and
  assign `self.focused` from `resolve_focus`. Split uses the new Space when
  `focus_new` is true; close uses `CloseResult.surviving_neighbor`; replacement
  prefers its target. Keep `set_space_sizing` from rebuilding views.

  In `App::new`, create initial Views through `reconcile_views(scene,
  contents, HashMap::new())` and resolve editor focus through the same focus
  helper. Remove stale `#[allow(dead_code)]` annotations from the now-used
  `View::content` and `View::runtime` accessors.

- [ ] **Step 5: Add focusability and ContentStore regression tests**

  Add these narrow tests:

  ```rust
  #[test]
  fn closing_last_focusable_space_is_rejected() {
      let mut app = make_app(vec![], None);
      let status = app.scene.node(app.scene.root).children[1];

      assert!(matches!(
          app.close_space(app.focused),
          Err(LayoutError::WouldRemoveLastFocusable(_))
      ));
      assert_ne!(app.focused, status);
  }

  #[test]
  fn replacing_only_focusable_content_with_inert_space_is_rejected() {
      let mut app = make_app(vec![], None);
      let focused = app.focused;
      let other = ContentId(9);
      app.contents.insert(other, Content::Buffer(Buffer::new()));

      assert_eq!(
          app.replace_space_content(focused, other, false),
          Err(LayoutError::NoFocusableSpace)
      );
      assert_eq!(app.focused, focused);
      assert!(matches!(
          app.scene.node(focused).space.kind,
          SpaceKind::Content { content, .. } if content == editor_cid()
      ));
  }
  ```

  In `src/core/content_store.rs`, add:

  ```rust
  #[test]
  fn contains_reports_inserted_content_ids() {
      let mut store = ContentStore::default();
      store.insert(ContentId(4), Content::Buffer(Buffer::new()));

      assert!(store.contains(ContentId(4)));
      assert!(!store.contains(ContentId(5)));
  }
  ```

- [ ] **Step 6: Run complete verification**

  Run:

  ```powershell
  cargo fmt
  cargo test app::tests
  cargo test core::content_store::tests
  cargo test
  cargo clippy --all-targets --all-features
  git diff --check
  ```

  Expected: all tests pass. Clippy may retain only pre-existing warnings that
  are not introduced by this work. The user-visible keymap and rendering tests
  continue to pass without new layout commands.

- [ ] **Step 7: Commit App lifecycle coordination**

  ```powershell
  git add src/app/mod.rs src/app/view.rs src/core/content_store.rs
  git commit -m "feat: reconcile views after scene mutations"
  ```

## Plan Review Checklist

- Task 1 makes Scene the only node table, removes Builder snapshots, removes
  duplicated children, adds focusability, and updates every protocol consumer.
- Task 2 provides all approved Builder semantic operations and verifies failed
  operations preserve Scene and the ID allocator.
- Task 3 keeps all cross-layer lifecycle work in App: Content preflight, View
  preservation/replacement, focus resolution, and no direct test mutation of
  `scene/views/focused`.
- No task introduces an input binding, split UI, clone/draft, external layout
  state, or a dependency outside the approved scope.
