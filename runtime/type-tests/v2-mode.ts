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

// @ts-expect-error V2 commands return only void or ctx.pass().
editor.modes.define({
  name: "invalid-return",
  on: {
    buffer: {
      commands: { invalidReturn() { return true; } },
    },
  },
});
