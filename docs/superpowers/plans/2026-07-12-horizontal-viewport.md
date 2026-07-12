# Horizontal Viewport Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make long unwrapped editor lines horizontally scroll with the focused cursor and render only visible character columns.

**Architecture:** `SceneRenderer` remains the only owner of per-`SpaceId` viewport state. It will update `Viewport.left_col` for the focused item and pass that offset plus the resolved content width into its private text painter. The painter clips whole rows obtained from the unchanged `ContentQuery::TextRows` API and clips selection highlighting to the same interval.

**Tech Stack:** Rust 2024, `crossterm` VT output, existing `SceneRenderer` unit tests using `Output<Vec<u8>>`.

## Global Constraints

- Modify only `src/tui/scene_renderer.rs` and its in-module tests.
- Keep `ContentQuery::TextRows(RowRange)` and all backend APIs unchanged.
- Do not modify `App`, `Content`, `ContentStore`, `Buffer`, selection types, or protocol APIs.
- Treat `CursorPos.col` as a character column; do not add display-width, tab, grapheme, soft-wrap, or user-controlled horizontal-scroll behavior.
- Do not add dependencies.
- Run `cargo test` and `cargo clippy --all-targets --all-features` before completing implementation.

---

### Task 1: Follow And Clip The Horizontal Viewport

**Files:**
- Modify: `src/tui/scene_renderer.rs:31-224`
- Test: `src/tui/scene_renderer.rs:328-557`

**Interfaces:**
- Consumes: `SceneRenderer.viewports: HashMap<SpaceId, Viewport>`, `CursorPos { row, col, .. }`, and `RenderItem.rect`.
- Produces: private `follow_viewport(viewport: &mut Viewport, head: CursorPos, width: usize, height: usize)` and an updated private `paint_line_with_highlight(canvas, line, left_col, width, hi)`.
- Compatibility: `paint_item` continues to obtain whole logical rows through `ContentQuery::TextRows(RowRange)` and selects `Viewport` by `SpaceId`.

- [ ] **Step 1: Write failing tests for horizontal following and clipped output**

Append these tests to the existing `#[cfg(test)] mod tests` in `src/tui/scene_renderer.rs`. A scene created with width 5 has a five-column editor; a cursor at character column 7 must view columns 3 through 7 and end in terminal column 4.

```rust
#[test]
fn viewport_follows_cursor_right_and_clips_long_line() {
    let mut builder = SceneBuilder::new();
    let (scene, editor) =
        build_editor_scene(&mut builder, 5, 2, ContentId(0), ContentId(1)).unwrap();
    let query = StubQuery {
        editor_cid: ContentId(0),
        lines: vec!["abcdefgh".to_string()],
        selections: Selections::single(Selection::collapsed(CursorPos {
            char_index: 7,
            row: 0,
            col: 7,
        })),
    };
    let mut renderer = SceneRenderer::new();
    let mut out = Output::new(Vec::new());

    renderer.render(&scene, &query, editor, &mut out as &mut dyn Canvas).unwrap();

    let output = String::from_utf8(out.into_inner()).unwrap();
    assert!(output.contains("defgh"), "output: {output}");
    assert!(!output.contains("abc"), "output: {output}");
    assert!(output.contains("1;5H"), "cursor should be at column 4: {output}");
}

#[test]
fn horizontal_viewport_moves_back_when_cursor_returns_left() {
    let mut builder = SceneBuilder::new();
    let (scene, editor) =
        build_editor_scene(&mut builder, 5, 2, ContentId(0), ContentId(1)).unwrap();
    let mut renderer = SceneRenderer::new();
    let right_query = StubQuery {
        editor_cid: ContentId(0),
        lines: vec!["abcdefgh".to_string()],
        selections: Selections::single(Selection::collapsed(CursorPos {
            char_index: 7,
            row: 0,
            col: 7,
        })),
    };
    let mut first = Output::new(Vec::new());
    renderer.render(&scene, &right_query, editor, &mut first as &mut dyn Canvas).unwrap();

    let left_query = StubQuery {
        editor_cid: ContentId(0),
        lines: vec!["abcdefgh".to_string()],
        selections: Selections::single(Selection::collapsed(CursorPos {
            char_index: 1,
            row: 0,
            col: 1,
        })),
    };
    let mut second = Output::new(Vec::new());
    renderer.render(&scene, &left_query, editor, &mut second as &mut dyn Canvas).unwrap();

    let output = String::from_utf8(second.into_inner()).unwrap();
    assert!(output.contains("bcdef"), "output: {output}");
    assert!(!output.contains("abcdef"), "output: {output}");
    assert!(output.contains("1;1H"), "cursor should be at column 0: {output}");
}
```

- [ ] **Step 2: Run the two tests and verify they fail**

Run:

```powershell
cargo test tui::scene_renderer::tests::viewport_follows_cursor_right_and_clips_long_line
cargo test tui::scene_renderer::tests::horizontal_viewport_moves_back_when_cursor_returns_left
```

Expected: both tests fail because the current renderer writes `abcdefgh` and positions the cursor at terminal column 7 without updating `Viewport.left_col`.

- [ ] **Step 3: Write failing tests for newline suppression and clipped selection**

Append these tests after Step 1. The selection is `[1, 7)`; at cursor column 7 the visible interval is `[3, 8)`, so only `defg` must be reversed.

```rust
#[test]
fn long_row_is_clipped_without_emitting_its_newline() {
    let mut builder = SceneBuilder::new();
    let (scene, editor) =
        build_editor_scene(&mut builder, 5, 2, ContentId(0), ContentId(1)).unwrap();
    let query = StubQuery {
        editor_cid: ContentId(0),
        lines: vec!["abcdefgh\n".to_string()],
        selections: Selections::single(Selection::collapsed(CursorPos::origin())),
    };
    let mut renderer = SceneRenderer::new();
    let mut out = Output::new(Vec::new());

    renderer.render(&scene, &query, editor, &mut out as &mut dyn Canvas).unwrap();

    let output = String::from_utf8(out.into_inner()).unwrap();
    assert!(output.contains("abcde"), "output: {output}");
    assert!(!output.contains("abcdef"), "output: {output}");
    assert!(!output.contains('\n'), "output: {output:?}");
}

#[test]
fn selection_highlight_is_clipped_to_horizontal_viewport() {
    let mut builder = SceneBuilder::new();
    let (scene, editor) =
        build_editor_scene(&mut builder, 5, 2, ContentId(0), ContentId(1)).unwrap();
    let query = StubQuery {
        editor_cid: ContentId(0),
        lines: vec!["abcdefgh".to_string()],
        selections: Selections::single(Selection {
            anchor: CursorPos {
                char_index: 1,
                row: 0,
                col: 1,
            },
            head: CursorPos {
                char_index: 7,
                row: 0,
                col: 7,
            },
        }),
    };
    let mut renderer = SceneRenderer::new();
    let mut out = Output::new(Vec::new());

    renderer.render(&scene, &query, editor, &mut out as &mut dyn Canvas).unwrap();

    let output = String::from_utf8(out.into_inner()).unwrap();
    assert!(output.contains("\x1b[7mdefg\x1b[27mh"), "output: {output}");
    assert!(!output.contains("\x1b[7mabc"), "output: {output}");
}
```

- [ ] **Step 4: Run the two tests and verify they fail**

Run:

```powershell
cargo test tui::scene_renderer::tests::long_row_is_clipped_without_emitting_its_newline
cargo test tui::scene_renderer::tests::selection_highlight_is_clipped_to_horizontal_viewport
```

Expected: the newline test fails because the current painter appends the stripped newline; the selection test fails because it reverses the full-row selection rather than its visible intersection.

- [ ] **Step 5: Implement focused horizontal following**

At the module imports, add `use crate::protocol::selection::CursorPos;`. Above `paint_item`, define this private helper:

```rust
fn follow_viewport(viewport: &mut Viewport, head: CursorPos, width: usize, height: usize) {
    viewport.ensure_cursor_visible(head.row, height);

    if width == 0 {
        viewport.left_col = head.col;
    } else if head.col < viewport.left_col {
        viewport.left_col = head.col;
    } else if head.col >= viewport.left_col.saturating_add(width) {
        viewport.left_col = head.col - width + 1;
    }
}
```

In `SceneRenderer::render`, replace the direct vertical call with:

```rust
follow_viewport(
    viewport,
    focused_head,
    item.rect.width as usize,
    item.rect.height as usize,
);
```

Restrict the final cursor emission to a positive-size focused item:

```rust
if let Some(item) = focused_item.filter(|item| item.rect.width > 0 && item.rect.height > 0) {
    let vp = self.viewports.get(&focused).copied().unwrap_or_else(Viewport::origin);
    let screen_row = focused_head.row.saturating_sub(vp.top_row) + item.rect.y as usize;
    let screen_col = focused_head.col.saturating_sub(vp.left_col) + item.rect.x as usize;
    canvas.move_cursor(screen_row, screen_col)?;
    canvas.show_cursor()?;
}
```

- [ ] **Step 6: Clip text and selection to visible columns**

In `paint_item`, calculate `let width = item.rect.width as usize;` beside `height` and call the painter as:

```rust
paint_line_with_highlight(canvas, line, vp.left_col, width, hi)?;
```

Replace the painter with the following. It uses absolute logical character columns and deliberately discards a trailing logical newline.

```rust
fn paint_line_with_highlight(
    canvas: &mut dyn Canvas,
    line: &str,
    left_col: usize,
    width: usize,
    hi: Option<(usize, usize)>,
) -> io::Result<()> {
    let content = line.strip_suffix('\n').unwrap_or(line);
    let bounds: Vec<(usize, char)> = content.char_indices().collect();
    let content_len = bounds.len();
    let visible_start = left_col.min(content_len);
    let visible_end = left_col.saturating_add(width).min(content_len);
    let write_segment =
        |canvas: &mut dyn Canvas, from: usize, to: usize, reverse: bool| -> io::Result<()> {
            if to <= from {
                return Ok(());
            }
            let start_byte = bounds[from].0;
            let end_byte = if to == content_len {
                content.len()
            } else {
                bounds[to].0
            };
            if reverse {
                canvas.set_reverse(true)?;
            }
            canvas.write_str(&content[start_byte..end_byte])?;
            if reverse {
                canvas.set_reverse(false)?;
            }
            Ok(())
        };

    let clipped_hi = hi.and_then(|(start, end)| {
        let start = start.max(visible_start);
        let end = end.min(visible_end);
        (start < end).then_some((start, end))
    });
    match clipped_hi {
        None => write_segment(canvas, visible_start, visible_end, false),
        Some((start, end)) => {
            write_segment(canvas, visible_start, start, false)?;
            write_segment(canvas, start, end, true)?;
            write_segment(canvas, end, visible_end, false)
        }
    }
}
```

- [ ] **Step 7: Run renderer tests**

Run:

```powershell
cargo test tui::scene_renderer::tests
```

Expected: all existing scene-renderer tests and the four new horizontal tests pass.

- [ ] **Step 8: Run full verification and inspect the patch**

Run:

```powershell
cargo test
cargo clippy --all-targets --all-features
git diff --check
git diff -- src/tui/scene_renderer.rs
```

Expected: all tests pass; Clippy has no new warnings; the diff is limited to `src/tui/scene_renderer.rs`; and the whitespace check is silent.

- [ ] **Step 9: Commit the implementation**

Run:

```powershell
git add src/tui/scene_renderer.rs
git commit -m "fix: clip editor lines to horizontal viewport"
```

Expected: one commit containing horizontal viewport following, clipped rendering, and renderer tests.
