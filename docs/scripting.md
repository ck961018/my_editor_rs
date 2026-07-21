# TypeScript scripting

The editor loads bundled plugins from `runtime/plugins/*/plugin.json`, in
manifest `order`, before it creates the initial Content and View. Rust only
registers the resulting generic Mode definitions; it does not select plugins
by name or implement their behavior.

One optional user `config.ts` is loaded after the bundled plugins. Set
`MODELEAF_CONFIG` to an explicit file, or use the platform default:

- Windows: `%APPDATA%\modeleaf\config.ts`
- Linux and macOS: `$XDG_CONFIG_HOME/modeleaf/config.ts`
- Home-directory fallback: `$HOME/.config/modeleaf/config.ts`

`MY_EDITOR_CONFIG` and the old `my_editor_rs` default directory remain as
deprecated fallbacks through version 0.1.x. Modeleaf emits one migration
warning when it uses either fallback; they will be removed in 0.2.0.

Use [editor.d.ts](../runtime/editor.d.ts) for editor and TypeScript tooling.
It is the canonical public schema and is embedded in `modeleaf-plugin-v8` as
`TYPESCRIPT_DECLARATIONS`. CI type-checks the bundled plugins and migration
examples against it. The runtime transpiles TypeScript but does not type-check
it.

Rust tests and headless tools can compile and load a source string without a
terminal:

```rust
let loaded = modeleaf_plugin_v8::load_typescript_modes(
    "file:///test.ts",
    source,
)?;
assert!(loaded.diagnostics.is_empty());
let modes = loaded.modes;
```

The result exposes only generic `Mode` objects and structured diagnostics;
V8 types do not cross the crate boundary. `PLUGIN_API_VERSION` identifies the
current schema version.

## Defining a mode

```ts
editor.modes.define({
  name: "pairs",
  on: {
    buffer: {
      state: () => ({ inserted: 0 }),
      viewState: () => ({ enabled: true }),
      commands: {
        quote(context) {
          if (!context.viewState.enabled) return context.pass();
          context.state.inserted++;
          context.edit.insert('""');
          context.cursor.moveLeft();
        },
      },
      keys: { '"': "quote" },
    },
  },
});
```

Content state exists once per `(Mode, Content)`. View state exists once per
`(Mode, View)`. Both contain only JSON-compatible structured data. The host
copies validated values back after a callback returns.

Modes receive input in attachment order. A command that returns normally has
handled the input. Only `return context.pass()` continues to the next Mode
after the current operations execute. The optional `input(context)` callback
receives every raw, unmapped key as a typed `EditorKeyEvent` in
`context.arguments`; simple keymap Modes do not need that callback.

Commands have stable qualified names such as `pairs.quote`. Another command
can stage one with `context.commands.invoke("pairs.quote")`. The nested command
shares the current transaction, but its return value does not replace the
calling command's `void | Pass` decision.

## Native primitives

Rust exposes typed functions under `context.cursor`, `context.edit`,
`context.history`, `context.viewport`, `context.commands`, and `context.app`.
Scripts call these functions directly; operation names are not serialized as
strings. Dynamic mode and action names remain strings because plugins define
that namespace.

Viewport primitives include pane-sized scrolling and cursor alignment.
`alignTop()`, `alignCenter()`, and `alignBottom()` become delayed viewport
effects; they do not move the text cursor.

Primitive calls append typed Rust operations to the current callback. The app
executes them in order only after the callback and its returned state validate.
If the callback fails, none of its staged operations execute. A retained
context cannot call primitives after its callback ends.

For example:

```ts
context.history.begin();
context.cursor.moveWordForward(2);
context.edit.deleteToLineEnd();
context.history.commit();
```

## Editing Content

`context.edit.insert()` and the cursor-relative text functions use the
existing deferred edit path. An absolute edit batch uses zero-based UTF-16
positions:

```ts
context.edit.applyEdits([{
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

An advanced Buffer adapter declares named background analysis separately from
ordinary commands and input:

```ts
analysis: {
  syntax: {
    worker: "worker.ts",
    snapshot: "text",
    input(ctx) {
      if (ctx.state.language === null) return;
      return { language: ctx.state.language, revision: ctx.revision };
    },
    apply(ctx) {
      return {
        contentDecorations: {
          revision: ctx.revision,
          spans: ctx.arguments.spans,
        },
      };
    },
  },
}
```

The analysis name is its stable task identity. `input` must be pure; its return
value is also the dependency signature. The host polls every named analysis
before publishing any replacement, assigns monotonic generations, and captures
the Content revision and input epoch. A changed message or `void` cancels
superseded work, stale results never enter `apply`, and `apply` runs against
transactional Mode state. The current analysis accepts its own post-apply
signature, preventing a state update from self-triggering forever; other
analyses rerun only when their messages change.

`snapshot: "text"` adds the current document text to the worker message off the
UI thread; `input` must return an object without a `text` field. Multiple named
analyses keep independent cached decoration layers.

Workers may return a Promise. The worker isolate pumps V8 microtasks, observes
editor cancellation, and rejects a request that exceeds its execution budget.
The main ScriptHost remains synchronous for input and command callbacks, but a
watchdog bounds each invocation and terminates V8 on timeout or heap pressure.
Validated state, operations, and presentation data are published only after the
invocation succeeds.

Standalone commands remain intentionally deferred. There is no command palette
or non-Mode invocation entry yet, so `context.commands.invoke()` resolves only
registered Mode-local qualified commands instead of maintaining a second global
script action table.

## Migrating a v1 mode

The v1 `content/view/actions/keys` schema remains accepted for user
configuration during the migration window. A configured host emits one
deprecation warning even when several v1 Modes are defined. The parser adapts
them to the same registered Mode and execution frame used by v2.

Migration is mechanical for ordinary Buffer Modes:

- move `content.create` to `on.buffer.state`;
- move `view.create` to `on.buffer.viewState`;
- rename `actions` to `on.buffer.commands` and move `keys` beside it;
- rename `contentState` to `state` and `text` primitives to `edit`;
- replace `forward()` with `pass()` and remove `handled()` returns.

The bundled Vim and Tree-sitter plugins use v2 and therefore do not exercise
the compatibility parser. The
[checked migration example](../runtime/examples/v1-migration.ts) is compiled
by TypeScript and executed by the Rust host test.

v1 is deprecated in 0.1.x, remains available with one structured warning in
0.2.x, and will be removed in 0.3.0. Removal requires the checked migration
example and all bundled plugins to remain on v2. The public
`V1_REMOVAL_VERSION` constant and contract test keep the warning, declaration,
and release policy aligned.

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
