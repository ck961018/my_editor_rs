type HighlightLanguage = "markdown" | "rust";

interface HighlightState {
  language: HighlightLanguage;
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

function defineHighlightMode(language: HighlightLanguage): void {
  editor.modes.define<HighlightState, null>({
    name: `syntax-highlighting-${language}`,
    attach: {
      view: "core.buffer",
      binding: "document",
      languages: [language],
    },
    on: {
      buffer: {
        state() {
          return { language };
        },
        changed(context) {
          const contentId = context.contentId;
          const text = context.text;
          const revision = context.revision;
          if (text === undefined || revision === undefined) return;

          worker.postMessage({ contentId, language, revision, text });
        },
      },
    },
  });
}

defineHighlightMode("markdown");
defineHighlightMode("rust");
