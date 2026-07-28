interface HighlightState {
  language: "markdown" | "rust" | null;
}

function languageFor(fileName?: string): HighlightState["language"] {
  const name = fileName?.toLowerCase();
  if (name?.endsWith(".rs")) {
    return "rust";
  }
  return name?.endsWith(".md") || name?.endsWith(".markdown")
    ? "markdown"
    : null;
}

const workers = new Map<number, { worker: Worker; abort: AbortController }>();

editor.modes.define<HighlightState, null>({
  name: "syntax-highlighting",
  on: {
    buffer: {
      state(context) {
        return {
          language: languageFor(context.resourceName),
        };
      },
      changed(context) {
        const language = context.state.language;
        if (language === null) return;
        const contentId = context.contentId;
        const text = context.text;
        const revision = context.revision;
        if (text === undefined || revision === undefined) return;

        const prev = workers.get(contentId);
        if (prev !== undefined) {
          prev.abort.abort();
          prev.worker.terminate();
        }

        const abort = new AbortController();
        const worker = new Worker(
          new URL("./worker.ts", import.meta.url),
          { type: "module", signal: abort.signal },
        );
        worker.onmessage = (e) => {
          const { revision, spans } = e.data;
          editor.writeDecorations(revision, spans);
        };
        workers.set(contentId, { worker, abort });
        worker.postMessage({ contentId, language, revision, text });
      },
    },
  },
});
