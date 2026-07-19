# TypeScript scripting

The editor loads one optional `config.ts` before it creates the initial Content
and View. Set `MY_EDITOR_CONFIG` to an explicit file, or use the platform
default:

- Windows: `%APPDATA%\my_editor_rs\config.ts`
- Linux and macOS: `$XDG_CONFIG_HOME/my_editor_rs/config.ts`, falling back to
  `$HOME/.config/my_editor_rs/config.ts`

Use [editor.d.ts](../runtime/editor.d.ts) for editor and TypeScript tooling.
The runtime transpiles TypeScript but does not type-check it.

## Defining a mode

```ts
/// <reference path="../../runtime/editor.d.ts" />

editor.modes.define({
  name: "pairs",
  before: "vim",
  content: { create: () => ({ inserted: 0 }) },
  view: { create: () => ({ enabled: true }) },
  actions: {
    quote(context) {
      if (!context.viewState.enabled) return { flow: "continue" };
      context.contentState.inserted++;
      return { insertText: '""' };
    },
  },
  keys: { '"': "quote" },
});
```

`before` places the mode before an existing initial mode such as `vim` or
`tree-sitter`. Without it, the mode is appended. Input is offered in that
order; `flow: "continue"` forwards it, while `stop` is the default.

Content state exists once per `(Mode, Content)`. View state exists once per
`(Mode, View)`. Both must contain only JSON-compatible structured data. A
callback receives detached V8 values, and validated values are copied back to
the Rust Mode stores after it returns.

## Editing Content

`insertText` edits the current selections through the existing deferred edit
path. An absolute batch uses zero-based UTF-16 positions:

```ts
return {
  contentEdits: {
    revision: context.revision!,
    edits: [{
      range: {
        start: { line: 0, character: 1 },
        end: { line: 0, character: 3 },
      },
      text: "replacement",
    }],
  },
};
```

The adapter rejects stale revisions, overlapping ranges, positions inside a
surrogate pair, and batches outside the current Content. A validated batch is
converted once to `ContentAction::Text`, so selection reconciliation, history,
undo, and failure rollback remain owned by the app executor.

The first runtime supports `.ts` and `.js` static relative imports inside the
configuration directory. It rejects bare packages, URLs, CommonJS, dynamic
imports, top-level await, and imports that escape that directory.

Supported key names are a single Unicode character, `Escape`, `Enter`,
`Backspace`, and the four `Arrow*` names. Modifier and multi-key notation will
be added with the public keymap configuration API.

## Windows build note

The repository pins the generated bindings used by rusty_v8, so Cargo does
not need Windows symlink privileges when its registry and target directory
are on different drives. The first build still downloads rusty_v8's prebuilt
static library.
