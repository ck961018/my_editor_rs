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

interface HighlightResult {
  contentId: number;
  revision: number;
  spans: TextDecorationSpan[];
}

const worker = new Worker(
  new URL("./worker.ts", import.meta.url),
  { type: "module" },
);
worker.onmessage = (event: MessageEvent<HighlightResult>) => {
  const { contentId, revision, spans } = event.data;
  editor.writeDecorations(contentId, revision, spans);
};

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

        worker.postMessage({ contentId, language, revision, text });
      },
    },
  },
});
