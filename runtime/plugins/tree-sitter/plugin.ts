interface HighlightState {
  language: "markdown" | "rust" | null;
}

interface HighlightResult {
  revision: number;
  spans: TextDecorationSpan[];
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

editor.modes.define<HighlightState, null, HighlightResult>({
  name: "syntax-highlighting",
  on: {
    buffer: {
      state(context) {
        return {
          language: languageFor(context.resourceName),
        };
      },
      analysis: {
        syntax: {
          worker: "worker.ts",
          snapshot: "text",
          input(context) {
            if (
              context.state.language === null ||
              context.revision === undefined
            ) {
              return;
            }
            return {
              contentId: context.contentId,
              language: context.state.language,
              revision: context.revision,
            };
          },
          apply(context) {
            const result = context.arguments;
            return {
              contentDecorations: {
                revision: context.revision,
                spans: result.spans,
              },
            };
          },
        },
      },
    },
  },
});
