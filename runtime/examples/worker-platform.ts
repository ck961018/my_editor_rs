/// <reference path="../editor.d.ts" />

export {};

interface HighlightRequest {
  contentId: number;
  revision: number;
  text: string;
}

interface HighlightResult {
  contentId: number;
  revision: number;
  spans: TextDecorationSpan[];
}

const controller = new AbortController();
const worker = new Worker(
  new URL("./worker-platform-worker.ts", import.meta.url),
  { type: "module", signal: controller.signal },
);

worker.onmessage = (event: MessageEvent<HighlightResult>) => {
  const { contentId, revision, spans } = event.data;
  editor.writeDecorations(contentId, revision, spans);
};

editor.modes.define({
  name: "worker-platform-example",
  faces: {
    "plugin.worker-platform.result": {
      inherits: ["syntax.comment"],
    },
  },
  on: {
    buffer: {
      changed(context) {
        const { contentId, revision, text } = context;
        if (revision === undefined || text === undefined) return;
        const request: HighlightRequest = { contentId, revision, text };
        worker.postMessage(request);
      },
    },
  },
});

// Call controller.abort() when the plugin no longer needs the worker.
