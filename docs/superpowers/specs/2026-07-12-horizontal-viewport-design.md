# Horizontal Viewport Design

## Goal

Fix long unwrapped text lines in the terminal frontend. When the primary cursor
passes the right edge of its content area, the frontend must horizontally scroll
that space and render only its visible columns. The terminal must not receive a
long logical line or the line-ending newline and therefore must not auto-wrap
editor text.

## Scope

This is a frontend-only change.

- Modify `src/tui/scene_renderer.rs` and its unit tests.
- Keep `ContentQuery::TextRows(RowRange)` unchanged. The backend continues to
  provide whole logical rows only; it does not receive a column range.
- Do not change `App`, `Content`, `ContentStore`, `Buffer`, selections, or any
  protocol API. `Viewport` remains a renderer-owned value stored per
  `SpaceId`.

The following are explicitly out of scope:

- Soft wrapping and visual-row navigation.
- User-initiated horizontal scrolling.
- Terminal display-width handling for tabs, full-width characters, emoji, and
  grapheme clusters. A column is the existing `CursorPos.col` character column.
- Multi-cursor rendering.

## Ownership

`SceneRenderer` already owns `HashMap<SpaceId, Viewport>`. A viewport is visual
state for one visible content instance, not content state. The renderer uses
`item.content_id` to query text rows and `item.space_id` to select the viewport
and selections.

No content-side API gains layout or viewport knowledge.

## Focused Viewport Following

On each render, the renderer finds the focused render item and follows its
primary selection head. Vertical following retains its current semantics.
Horizontal following is implemented as a private renderer helper and updates
the existing `Viewport.left_col` field:

```text
width = item.rect.width as usize

if width == 0:
    left_col = cursor.col
else if cursor.col < left_col:
    left_col = cursor.col
else if cursor.col >= left_col + width:
    left_col = cursor.col - width + 1
```

For a positive width, the cursor is therefore always in
`[left_col, left_col + width)`. A cursor at the logical end of a line is shown
in the last terminal column rather than at an out-of-bounds column.

Only the focused space follows its cursor, matching the existing vertical
behavior. Non-focused spaces retain their own viewport. A space without a
stored viewport starts at `(top_row: 0, left_col: 0)`. Expanding the terminal
does not reset `left_col`, so resize does not unexpectedly jump the view.

For a zero-width item, the renderer writes no text and aligns `left_col` with
the cursor column, avoiding subtraction underflow until a later resize restores
width.

## Rendering And Clipping

The renderer continues to query complete visible logical rows using
`TextRows(RowRange { start: top_row, end: top_row + height })`. For every row,
it derives the visible character interval:

```text
visible_columns = [left_col, left_col + width)
```

The renderer strips a trailing logical newline before painting, then writes at
most `width` characters from that interval. It never writes the stripped
newline. This prevents terminal auto-wrap and keeps each logical row anchored
to the row selected by `item.rect`.

Selection highlighting is expressed in the same logical character columns. For
the current row, the renderer intersects the selection interval with
`visible_columns`, then paints ordinary prefix, reversed intersection, and
ordinary suffix. A selection outside the interval produces no reverse segment;
a selection crossing either edge highlights only its visible portion.

The terminal cursor position is:

```text
screen_row = item.rect.y + cursor.row - viewport.top_row
screen_col = item.rect.x + cursor.col - viewport.left_col
```

It is emitted only for the focused item after the viewport-following step has
made the column valid for a positive-width item.

## Errors And Compatibility

This change introduces no public error types and no fallible data-paths beyond
the existing `Canvas` IO operations. Missing or unsupported text content keeps
the current empty-row fallback behavior.

Status-bar truncation, sibling-space line clearing, and display-cell width are
not changed by this specification. They are separate rendering concerns.

## Tests

Add renderer tests covering:

1. A cursor past the right edge advances `left_col`, renders only the right-side
   text interval, and emits its terminal cursor at the final content column.
2. Moving the cursor left of `left_col` moves the viewport back and reveals the
   corresponding left-side text.
3. A long logical row emits neither a trailing newline nor text beyond the
   content width.
4. A horizontally clipped selection reverses only the intersection of the
   selection and visible intervals.
5. Existing vertical-following and per-space selection isolation tests continue
   to pass.

Run `cargo test` and `cargo clippy --all-targets --all-features` when the
subsequent implementation is complete.
