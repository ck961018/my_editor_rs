# TypeScript scripting

The editor loads bundled plugins from `runtime/plugins/*/plugin.json`, in
manifest `order`, before it creates the initial Content and View. Rust only
registers the resulting generic Mode definitions; it does not select plugins
by name or implement their behavior.

One optional user `config.ts` is loaded after the bundled plugins. Set
`MY_EDITOR_CONFIG` to an explicit file, or use the platform default:

- Windows: `%APPDATA%\my_editor_rs\config.ts`
- Linux and macOS: `$XDG_CONFIG_HOME/my_editor_rs/config.ts`
- Home-directory fallback: `$HOME/.config/my_editor_rs/config.ts`

Use [editor.d.ts](../runtime/editor.d.ts) for editor and TypeScript tooling.
The runtime transpiles TypeScript but does not type-check it.

## Defining a mode

```ts
editor.modes.define({
  name: "pairs",
  content: { create: () => ({ inserted: 0 }) },
  view: { create: () => ({ enabled: true }) },
  actions: {
    quote(context) {
      if (!context.viewState.enabled) return context.forward();
      context.contentState.inserted++;
      context.text.insert('""');
      return context.handled();
    },
  },
  keys: { '"': "quote" },
});
```

Content state exists once per `(Mode, Content)`. View state exists once per
`(Mode, View)`. Both contain only JSON-compatible structured data. The host
copies validated values back after a callback returns.

Modes receive input in attachment order. `context.forward()` passes the same
input to the next mode after the current operations execute.
`context.handled()` stops dispatch, which is also the default. `input` names
an action that receives every raw, unmapped key in `context.arguments`.

## Native primitives

Rust exposes typed functions under `context.cursor`, `context.text`,
`context.history`, `context.viewport`, `context.mode`, and `context.app`.
Scripts call these functions directly; operation names are not serialized as
strings. Dynamic mode and action names remain strings because plugins define
that namespace.

Primitive calls append typed Rust operations to the current callback. The app
executes them in order only after the callback and its returned state validate.
If the callback fails, none of its staged operations execute. A retained
context cannot call primitives after its callback ends.

For example:

```ts
context.history.begin();
context.cursor.moveWordForward(2);
context.text.deleteToLineEnd();
context.history.commit();
return context.handled();
```

## Editing Content

`context.text.insert()` and the cursor-relative text functions use the
existing deferred edit path. An absolute edit batch uses zero-based UTF-16
positions:

```ts
context.text.applyEdits([{
  range: {
    start: { line: 0, character: 1 },
    end: { line: 0, character: 3 },
  },
  text: "replacement",
}]);
```

The batch is bound to the Content snapshot captured for the current callback.
The adapter rejects overlapping ranges, positions inside a surrogate pair,
and batches outside that snapshot. The app executor continues to own selection
reconciliation, history, undo, and rollback.

## Faces and decorations

A mode may define named `faces` and publish `contentDecorations` or
`viewDecorations`. Each decoration snapshot carries the Content revision and
UTF-16 ranges. Rendering reads the cached Rust snapshot and never calls V8.

When text changes, cached Content decorations are mapped through the change
until a newer asynchronous snapshot arrives. This avoids a blank highlight
frame while preserving revision safety.

`viewState.viewPolicy` may set cursor style, cursor domain, selection shape,
and the named selection face.

## Background workers

A bundled plugin may name one persistent `worker.ts`:

```ts
editor.worker.onMessage(async (message) => {
  const bytes = editor.resources.readBinary("vendor/parser.wasm");
  return await analyze(bytes, message);
});
```

Worker resources are read-only and restricted to the plugin directory.
Absolute paths, parent traversal, network access, timers, and Node APIs are
not provided.

The mode's `content.job` callback returns a JSON message, slot, and version.
The existing Mode job scheduler runs the worker off the UI thread. One job per
`(Mode, Content, slot)` runs at a time, and only the latest queued request is
kept. `content.applyJob` validates the generation and Content revision before
publishing state or decorations.

Workers may return a Promise. The worker isolate pumps V8 microtasks, observes
editor cancellation, and rejects a request that exceeds its execution budget.
The main ScriptHost remains synchronous for input and command callbacks.

## Modules and trust boundary

User configuration supports `.ts` and `.js` static relative imports inside
its configuration directory. Bare packages, URLs, CommonJS, dynamic imports,
top-level await, and imports escaping that directory are rejected.

Bundled worker scripts and binary resources are embedded at build time.
Filesystem-backed user workers are not supported yet.

## Windows build note

The repository pins the bindings used by rusty_v8, so Cargo does not need
Windows symlink privileges when its registry and target directory are on
different drives. The first build still downloads rusty_v8's prebuilt static
library.
