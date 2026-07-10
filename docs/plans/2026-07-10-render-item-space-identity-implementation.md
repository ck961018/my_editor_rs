# RenderItem Space Identity Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让同一 `ContentId` 在多个 `SpaceId` 中显示时，渲染、selection 和 viewport 按各自的 space 隔离。

**Architecture:** 保留现有 `Space` 场景树及 `SpaceKind` 的内部节点/内容叶子判别。将歧义的 `Host` 术语统一为 `Content`，再由 `TaffyEngine` 将来源 `SpaceId` 写入 `RenderItem`；`SceneRenderer` 只消费该身份，不按 `ContentId` 反查场景树。

**Tech Stack:** Rust 2024 (MSRV 1.85), `taffy` 0.11, `crossterm` 0.29, `cargo test`, `cargo clippy`.

## Global Constraints

- 保持依赖方向：`tui -> frontend + terminal + protocol`，`app -> frontend + core + protocol`，`protocol -> std`。
- `Space` 仍是全部 UI scene 节点的统一类型；不引入 `Pane`、`Window`、第二棵布局树或新的 ID 类型。
- 保留 `SpaceKind`；将 `Host` 改名为 `Content`，不更改 `Container` 的数据模型。
- 本计划不处理 `children` 去重、动态 split、tab、浮层或 View 生命周期。
- `RenderItem` 必须同时包含 `space_id: SpaceId` 与 `content_id: ContentId`。
- selection 和 TUI viewport 以 `SpaceId` 为键；内容行和状态栏数据以 `ContentId` 查询。
- 修改 Rust 代码后运行 `cargo test` 和 `cargo clippy --all-targets --all-features`。

---

## File Structure

- `src/protocol/space.rs`: `SpaceKind` 的公开节点判别类型。
- `src/protocol/scene.rs`: SceneBuilder 的内容节点创建 API、标准场景和协议层测试。
- `src/tui/resolved.rs`: layout 解析后、renderer 消费的 `RenderItem` 数据契约。
- `src/tui/taffy_engine.rs`: 从内容 space 生成 `RenderItem`，并验证相同内容的不同 space 身份。
- `src/tui/scene_renderer.rs`: 按 `RenderItem.space_id` 读取 selection、viewport 和聚焦矩形。
- `src/app/dispatcher.rs`、`src/app/mod.rs`: 更新 `SpaceKind::Host` 的模式匹配与测试辅助调用。

### Task 1: 统一 Content 术语并保留 RenderItem 来源 SpaceId

**Files:**
- Modify: `src/protocol/space.rs`
- Modify: `src/protocol/scene.rs`
- Modify: `src/tui/resolved.rs`
- Modify: `src/tui/taffy_engine.rs`
- Modify: `src/app/dispatcher.rs`
- Modify: `src/app/mod.rs`
- Test: `src/protocol/scene.rs`
- Test: `src/tui/resolved.rs`
- Test: `src/tui/taffy_engine.rs`

**Interfaces:**
- Produces: `SpaceKind::Content { content: ContentId }`.
- Produces: `SceneBuilder::{content, content_grow, content_fixed}` with the same allocation and sizing semantics as the former `host*` APIs.
- Produces: `RenderItem { space_id: SpaceId, content_id: ContentId, .. }`.
- Consumes: Existing `SpaceId`, `ContentId`, `Arrangement`, `Sizing` and `ResolvedScene` contracts.

- [ ] **Step 1: Write the failing resolved-layout identity test**

  In `src/tui/taffy_engine.rs` test module, import `SpaceId`, `Arrangement`, `Axis`, `Align`, and `Size`. Add this test before changing production code:

  ```rust
  #[test]
  fn shared_content_items_keep_their_source_space_ids() {
      let mut builder = SceneBuilder::new();
      let left = builder.content_grow(ContentId(0), 1);
      let right = builder.content_grow(ContentId(0), 1);
      let root = builder.container_grow(
          Arrangement::Flex {
              direction: Axis::Horizontal,
              gap: 0,
              align: Align::Stretch,
          },
          vec![left, right],
          1,
      );
      let scene = builder
          .snapshot(root, Size { width: 20, height: 1 })
          .unwrap();

      let mut engine = TaffyEngine::new();
      let resolved = engine.layout(&scene);

      assert_eq!(resolved.items.len(), 2);
      assert_eq!(resolved.items[0].content_id, ContentId(0));
      assert_eq!(resolved.items[1].content_id, ContentId(0));
      assert_eq!(resolved.items[0].space_id, left);
      assert_eq!(resolved.items[1].space_id, right);
  }
  ```

- [ ] **Step 2: Run the new test and verify it fails to compile**

  Run: `cargo test shared_content_items_keep_their_source_space_ids`

  Expected: compilation failure because `SceneBuilder::content_grow` and `RenderItem::space_id` do not exist yet.

- [ ] **Step 3: Rename Host terminology in protocol and all source consumers**

  In `src/protocol/space.rs`, replace the enum variant only:

  ```rust
  pub enum SpaceKind {
      Container {
          arrangement: Arrangement,
          children: Vec<SpaceId>,
      },
      Content {
          content: ContentId,
      },
  }
  ```

  In `src/protocol/scene.rs`, rename the builder methods and construct the renamed variant:

  ```rust
  pub fn content(&mut self, content: ContentId) -> SpaceHandle {
      SpaceHandle {
          id: self.alloc(SpaceKind::Content { content }),
      }
  }

  pub fn content_grow(&mut self, content: ContentId, weight: u32) -> SpaceId {
      let id = self.content(content).id;
      self.set_sizing(id, Sizing::Grow(weight))
  }

  pub fn content_fixed(&mut self, content: ContentId, size: i32) -> SpaceId {
      let id = self.content(content).id;
      self.set_sizing(id, Sizing::Fixed(size))
  }
  ```

  Update `build_editor_scene`, `SpaceNode` child extraction, source comments, protocol tests, `src/app/mod.rs`, `src/app/dispatcher.rs`, and `src/tui/taffy_engine.rs` so every `SpaceKind::Host` becomes `SpaceKind::Content`, and every `host*` builder call becomes its `content*` counterpart. Rename tests and comments that describe a content leaf as a host.

- [ ] **Step 4: Add `space_id` to the resolved rendering contract**

  In `src/tui/resolved.rs`, import `SpaceId`, add the public field, and update its unit test:

  ```rust
  use crate::protocol::ids::{ContentId, SpaceId};

  pub struct RenderItem {
      pub space_id: SpaceId,
      pub content_id: ContentId,
      // existing geometry and paint-order fields remain unchanged
  }

  let it = RenderItem {
      space_id: SpaceId(0),
      content_id: ContentId(0),
      // existing test fields unchanged
  };
  assert_eq!(it.space_id, SpaceId(0));
  ```

  In `TaffyEngine::collect`, populate that field from the `sid` argument when emitting a `SpaceKind::Content` item:

  ```rust
  out.items.push(RenderItem {
      space_id: sid,
      content_id: cid,
      rect,
      clip,
      layer: node.space.layer,
      z_index: 0,
      order: out.order,
  });
  ```

- [ ] **Step 5: Run targeted protocol and Taffy tests**

  Run: `cargo test shared_content_items_keep_their_source_space_ids`

  Expected: PASS. The two items must share `ContentId(0)` but have their own `SpaceId` values.

  Run: `cargo test tui::taffy_engine`

  Expected: PASS. Existing geometry and DFS-order tests still pass after the terminology change.

- [ ] **Step 6: Check for stale source terminology and commit checkpoint**

  Run: `rg "SpaceKind::Host|\\bhost_grow\\b|\\bhost_fixed\\b|\\bhost\\(" src`

  Expected: no matches.

  When the workspace is recognized by Git, commit the task:

  ```powershell
  git add src/protocol/space.rs src/protocol/scene.rs src/tui/resolved.rs src/tui/taffy_engine.rs src/app/dispatcher.rs src/app/mod.rs
  git commit -m "refactor: retain source space in render items"
  ```

### Task 2: 按 RenderItem SpaceId 渲染 selection、viewport 和焦点

**Files:**
- Modify: `src/tui/scene_renderer.rs`
- Test: `src/tui/scene_renderer.rs`

**Interfaces:**
- Consumes: `RenderItem::space_id` from Task 1 and `ContentQuery::selections(sid: SpaceId)`.
- Produces: `SceneRenderer::render` uses the focused item selected by `item.space_id == focused`.
- Produces: `paint_item(item, query, viewports, canvas)` uses `item.space_id` directly and no longer needs `Scene`.

- [ ] **Step 1: Add a failing shared-content rendering regression test**

  In the `src/tui/scene_renderer.rs` test module, add these imports and a small query type that returns one shared text buffer and selections keyed by space:

  ```rust
  use std::collections::HashMap;

  use crate::protocol::geometry::Size;
  use crate::protocol::space::{Align, Arrangement, Axis};
  ```

  ```rust
  struct MultiSpaceQuery {
      lines: Vec<String>,
      selections: HashMap<SpaceId, Selections>,
  }

  impl ContentQuery for MultiSpaceQuery {
      fn lines(&self, cid: ContentId, range: RowRange) -> Vec<String> {
          assert_eq!(cid, ContentId(0));
          self.lines
              .iter()
              .skip(range.start)
              .take(range.end.saturating_sub(range.start))
              .cloned()
              .collect()
      }

      fn status_bar(&self, _cid: ContentId) -> StatusBarData {
          StatusBarData {
              file_name: None,
              modified: false,
              message: StatusMessage::None,
          }
      }

      fn selections(&self, sid: SpaceId) -> Selections {
          self.selections[&sid].clone()
      }

      fn line_count(&self, cid: ContentId) -> usize {
          if cid == ContentId(0) {
              self.lines.len()
          } else {
              0
          }
      }
  }
  ```

  Add this test:

  ```rust
  #[test]
  fn shared_content_spaces_use_their_own_selections() {
      let mut builder = SceneBuilder::new();
      let left = builder.content_grow(ContentId(0), 1);
      let right = builder.content_grow(ContentId(0), 1);
      let root = builder.container_grow(
          Arrangement::Flex {
              direction: Axis::Horizontal,
              gap: 0,
              align: Align::Stretch,
          },
          vec![left, right],
          1,
      );
      let scene = builder
          .snapshot(root, Size { width: 20, height: 1 })
          .unwrap();
      let query = MultiSpaceQuery {
          lines: vec!["abcd".to_string()],
          selections: HashMap::from([
              (
                  left,
                  Selections::single(Selection {
                      anchor: CursorPos { char_index: 0, row: 0, col: 0 },
                      head: CursorPos { char_index: 1, row: 0, col: 1 },
                  }),
              ),
              (
                  right,
                  Selections::single(Selection {
                      anchor: CursorPos { char_index: 2, row: 0, col: 2 },
                      head: CursorPos { char_index: 3, row: 0, col: 3 },
                  }),
              ),
          ]),
      };
      let mut renderer = SceneRenderer::new();
      let mut out = Output::new(Vec::new());
      renderer
          .render(&scene, &query, left, &mut out as &mut dyn Canvas)
          .unwrap();
      let output = String::from_utf8(out.into_inner()).unwrap();

      assert!(output.contains("\x1b[7ma\x1b[27mbcd"), "left: {output}");
      assert!(output.contains("ab\x1b[7mc\x1b[27md"), "right: {output}");
  }
  ```

- [ ] **Step 2: Run the regression test and verify the old renderer fails**

  Run: `cargo test shared_content_spaces_use_their_own_selections`

  Expected: FAIL at the second assertion. The old `find_space_by_content` path selects `left` for both render items, so the right item highlights `a` instead of `c`.

- [ ] **Step 3: Remove reverse scene lookup and use RenderItem identity throughout rendering**

  In `SceneRenderer::render`, replace `focused_content_id` and both `content_id` searches with one focused render-item lookup:

  ```rust
  let focused_item = resolved.items.iter().find(|item| item.space_id == focused);
  let focused_head = query.selections(focused).primary().head();
  if let Some(item) = focused_item {
      let viewport = self
          .viewports
          .entry(focused)
          .or_insert_with(Viewport::origin);
      viewport.ensure_cursor_visible(focused_head.row, item.rect.height as usize);
  }
  ```

  Use the same `focused_item` for cursor placement. Change `paint_item` to receive no `Scene`, and replace its lookup block with:

  ```rust
  let sid = item.space_id;
  let vp = viewports
      .get(&sid)
      .copied()
      .unwrap_or_else(Viewport::origin);
  ```

  Delete `focused_content_id` and `find_space_by_content`. Remove now-unused top-level imports of `ContentId` and `SpaceKind`; retain test-module imports needed by the regression test. Update the rendering comment from “Host item” to “Content item”.

- [ ] **Step 4: Run the regression test and renderer test module**

  Run: `cargo test shared_content_spaces_use_their_own_selections`

  Expected: PASS. The output contains a reverse-video `a` in the left space and a reverse-video `c` in the right space.

  Run: `cargo test tui::scene_renderer`

  Expected: PASS. Existing single-view line rendering, cursor follow and multi-line selection tests remain green.

- [ ] **Step 5: Run full verification and commit checkpoint**

  Run: `cargo test`

  Expected: PASS.

  Run: `cargo clippy --all-targets --all-features`

  Expected: no warnings or errors.

  Run: `rg "find_space_by_content|focused_content_id|SpaceKind::Host|\\bhost_grow\\b|\\bhost_fixed\\b|\\bhost\\(" src`

  Expected: no matches.

  When the workspace is recognized by Git, commit the task:

  ```powershell
  git add src/tui/scene_renderer.rs
  git commit -m "fix: render shared content by space identity"
  ```

## Verification Summary

- Task 1 proves the resolved scene preserves both the shared content identity and each item’s source `SpaceId`.
- Task 2 proves two visible instances of the same content consume different selections.
- The public `ContentQuery` contract and layer dependency direction remain unchanged.
- Full tests and Clippy are mandatory because `RenderItem` is a shared TUI rendering contract.
