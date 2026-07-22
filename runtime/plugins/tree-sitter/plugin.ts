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
  faces: {
    "syntax.attribute": { foreground: 173 },
    "syntax.comment": { foreground: 244, italic: true },
    "syntax.constant": { foreground: 141 },
    "syntax.function": { foreground: 75 },
    "syntax.keyword": { foreground: 170, bold: true },
    "syntax.label": { foreground: 179 },
    "syntax.markup.bold": { bold: true },
    "syntax.markup.heading": { foreground: 75, bold: true },
    "syntax.markup.italic": { italic: true },
    "syntax.markup.link": { foreground: 75, underline: true },
    "syntax.markup.list": { foreground: 170 },
    "syntax.markup.quote": { foreground: 244 },
    "syntax.markup.raw": { foreground: 114 },
    "syntax.namespace": { foreground: 110 },
    "syntax.number": { foreground: 141 },
    "syntax.operator": { foreground: 250 },
    "syntax.property": { foreground: 110 },
    "syntax.punctuation": { foreground: 245 },
    "syntax.string": { foreground: 114 },
    "syntax.type": { foreground: 109 },
    "syntax.variable": { foreground: 252 },
  },
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
