# ViewData and Vim Cursor Style Design

**Date:** 2026-07-12

## Goal

Render a stable block cursor while a Vim buffer view is in Normal mode. When
the view enters Insert mode, restore the terminal user's default cursor style.

This change also makes view-level render data a complete, typed snapshot so
the frontend does not grow one `RenderQuery` method per property.

## Scope

Included:

- Replace `RenderQuery::selections(SpaceId)` with a complete `ViewData`
  snapshot returned by `RenderQuery::view(SpaceId)`.
- Carry a mode-derived cursor style from a view's `ContentRuntime` to the TUI.
- Set the focused terminal cursor style on every render and restore the
  default style when the terminal guard exits.
- Keep the existing content pull queries for viewport-bounded text.

Excluded:

- New Vim modes, key bindings, cursor blinking configuration, underline or
  bar cursor variants.
- A cross-process UI event protocol like Neovim's redraw stream.
- Moving viewport or layout ownership out of the TUI.

## Context

`View` is an app-layer session object keyed by `SpaceId`. It owns selections
and a `ContentRuntime`; multiple views may bind the same `ContentId` while
retaining independent mode runtime. `SceneRenderer` currently reads selections
through a dedicated `RenderQuery::selections` method and has no way to obtain
mode-derived presentation state.

The cursor is a physical terminal resource, so only the focused view can set
its shape. The render path must not know that Vim exists or inspect concrete
`Content` variants.

## Design

### Complete view snapshot

Add the following protocol data and replace the selections getter:

```rust
pub enum CursorStyle {
    Default,
    Block,
}

pub struct ViewData {
    pub selections: Selections,
    pub cursor_style: CursorStyle,
}

pub trait RenderQuery {
    fn content(&self, id: ContentId, query: ContentQuery) -> ContentData;
    fn view(&self, id: SpaceId) -> ViewData;
}
```

`ViewData` is a full snapshot of the common rendering state for one view. It
is intentionally a struct, not a query/result enum. Future general view
rendering data is added as explicit fields to this struct.

`ContentQuery` and `ContentData` remain demand-driven. In particular, the TUI
continues to query only the visible `TextRows` range rather than requesting an
entire buffer. The ownership split is:

| Data | Source |
|---|---|
| Text rows, line count, status-bar content, future content-specific data | `ContentQuery` by `ContentId` |
| Selections, cursor style, future common per-view presentation data | `ViewData` by `SpaceId` |
| Viewport, layout rectangles, follow/scroll policy, canvas caches | TUI-owned state |

Every content space in a valid scene has a reconciled `View`. Therefore
`RenderQuery::view` is total for scene content spaces; it does not encode a
fallback origin selection or `Unsupported` result.

### Runtime-derived presentation

`AppQuery::view` assembles the snapshot without recognizing concrete content:

1. It gets selections and the `ContentId`/`ContentRuntime` from the `View`.
2. It asks `ContentStore` for the cursor style for that content/runtime pair.
3. `ContentStore` delegates through the static `Content` enum.
4. Buffer delegates to `ModeSet`, which delegates to its current mode state.

The core-facing method is limited to the current need: obtaining a
`CursorStyle` from a compatible runtime. It is not necessary to introduce a
second content-view snapshot type before another field requires one.

Vim behavior is:

| Vim state | `CursorStyle` |
|---|---|
| Normal | `Block` |
| Insert | `Default` |

Non-buffer content and non-Vim modes return `Default`. A runtime/content type
mismatch remains a programming invariant violation, consistent with existing
`Content` runtime dispatch.

### Rendering and terminal output

The rendering path is:

```text
SceneRenderer
  -> RenderQuery::view(SpaceId)
  -> AppQuery
  -> View selections + ContentStore runtime lookup
  -> Content -> Buffer -> ModeSet -> Vim mode state
  -> ViewData
  -> Canvas::set_cursor_style(CursorStyle)
  -> terminal output
```

`SceneRenderer` obtains `ViewData` for each rendered content space and uses
`selections` for highlighting. For the focused content space it additionally
uses the primary selection head for viewport following and cursor placement.

Immediately before showing the focused physical terminal cursor, the renderer
calls `Canvas::set_cursor_style` with that focused view's style. It performs
this on every frame rather than caching the prior style, so a terminal state
change outside the renderer is corrected at the next render.

`Canvas` adds `set_cursor_style(CursorStyle)`. `Output` maps `Block` to a
steady block cursor and `Default` to the terminal user's default cursor style
through crossterm. Terminals that do not support cursor-shape control may
ignore the sequence; rendering continues normally.

`TerminalGuard::drop` explicitly restores the default cursor style before it
leaves the alternate screen and disables raw mode. This prevents a Normal-mode
block cursor from leaking into the shell.

## Error Handling

- `RenderQuery::view` is called only for scene content spaces and assumes the
  app's view-reconciliation invariant. A missing view is an internal error,
  not normal rendering input.
- Terminal output failures continue to propagate as `io::Result` through the
  existing renderer/frontend path.
- Lack of terminal cursor-shape support is not detectable through this API and
  is treated as a no-op by the terminal emulator.

## Tests

- Protocol: `CursorStyle` and `ViewData` value semantics.
- Core: Vim Normal reports `Block`; entering Insert reports `Default`; two
  independent runtimes can report different styles.
- App: `AppQuery::view` keeps selections with the correct cursor style, and
  two views of one Buffer remain independent.
- TUI: a query stub supplies distinct `ViewData` values; the renderer uses the
  focused view's style for the physical cursor and each view's selections for
  highlighting.
- Terminal: `Canvas` dispatches cursor-style calls and `Output` emits the
  crossterm commands for both styles.

## Rationale

Helix keeps editor and terminal UI tightly coupled and exposes cursor shape
per editing mode. Neovim instead sends mode metadata and mode changes through
an external UI event protocol. This editor has an in-process, statically
dispatched frontend boundary and already uses pull rendering, so a complete
per-view snapshot is the smallest design that preserves its current model
while allowing the snapshot to grow deliberately.
