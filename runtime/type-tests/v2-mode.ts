/// <reference path="../editor.d.ts" />

editor.modes.define({
  name: "pairs",
  on: {
    buffer: {
      state: () => ({ enabled: true }),
      viewState: () => ({ insertedPairs: 0 }),
      commands: {
        quote(ctx) {
          if (!ctx.state.enabled) return ctx.pass();
          // @ts-expect-error Buffer adapters do not expose StatusBar data.
          void ctx.status;
          ctx.edit.insert('""');
          ctx.cursor.moveLeft();
          ctx.viewState.insertedPairs++;
        },
      },
      keys: { '"': "quote" },
      input(ctx) {
        void ctx.arguments.code;
        // @ts-expect-error Raw input uses the closed EditorKeyEvent shape.
        void ctx.arguments.missing;
        return ctx.pass();
      },
    },
    statusBar: {
      state: () => ({ updates: 0 }),
      viewState: () => null,
      commands: {
        update(ctx) {
          ctx.state.updates++;
          // @ts-expect-error StatusBar adapters cannot edit Buffer text.
          ctx.edit.insert("forbidden");
          // @ts-expect-error StatusBar adapters do not expose a cursor.
          ctx.cursor.moveLeft();
          // @ts-expect-error StatusBar adapters do not expose history.
          ctx.history.undo();
          // @ts-expect-error StatusBar adapters do not expose a viewport.
          ctx.viewport.scroll(1);
          // @ts-expect-error StatusBar adapters do not expose app commands.
          ctx.app.quit();
        },
      },
    },
  },
});

editor.modes.define<
  {
    count: number;
    nested: { language: string };
    items: string[];
  },
  null,
  { spans: TextDecorationSpan[] }
>({
  name: "analysis-types",
  on: {
    buffer: {
      state: () => ({
        count: 0,
        nested: { language: "rust" },
        items: [],
      }),
      analysis: {
        syntax: {
          worker: "worker.ts",
          snapshot: "text",
          input(ctx) {
            // @ts-expect-error Analysis input receives read-only Mode state.
            ctx.state.count++;
            // @ts-expect-error Analysis input state is deeply read-only.
            ctx.state.nested.language = "markdown";
            // @ts-expect-error Analysis input arrays are read-only.
            ctx.state.items.push("new");
            return { revision: ctx.revision };
          },
          apply(ctx) {
            ctx.state.count++;
            return {
              contentDecorations: {
                revision: ctx.revision,
                spans: ctx.arguments.spans,
              },
            };
          },
        },
      },
    },
  },
});

// @ts-expect-error Text snapshot analysis input must return an object.
const invalidTextMessage: BackgroundAnalysisDefinition<null, null> = {
  worker: "worker.ts",
  snapshot: "text",
  input() { return 1; },
  apply() {},
};
void invalidTextMessage;

// @ts-expect-error The host owns the text snapshot message field.
const reservedTextMessage: BackgroundAnalysisDefinition<null, null> = {
  worker: "worker.ts",
  snapshot: "text",
  input() { return { text: "owned-by-host" }; },
  apply() {},
};
void reservedTextMessage;

// @ts-expect-error Raw worker lifecycle moved to named analysis.
editor.modes.define({
  name: "raw-analysis-lifecycle",
  on: { buffer: { job() {} } },
});

// @ts-expect-error StatusBar adapters cannot declare analysis.
editor.modes.define({
  name: "status-analysis",
  on: { statusBar: { analysis: {} } },
});

// @ts-expect-error V2 commands return only void or ctx.pass().
editor.modes.define({
  name: "invalid-return",
  on: {
    buffer: {
      commands: { invalidReturn() { return true; } },
    },
  },
});
