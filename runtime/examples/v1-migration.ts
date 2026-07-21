/// <reference path="../editor.d.ts" />

editor.modes.define<{ count: number }, Record<string, never>>({
  name: "migration-v1",
  content: {
    create: () => ({ count: 0 }),
  },
  view: {
    create: () => ({}),
  },
  actions: {
    increment(context) {
      context.contentState.count++;
    },
  },
  keys: { "x": "increment" },
});

editor.modes.define({
  name: "migration-v2",
  on: {
    buffer: {
      state: () => ({ count: 0 }),
      viewState: () => ({}),
      commands: {
        increment(context) {
          context.state.count++;
        },
      },
      keys: { "x": "increment" },
    },
  },
});
