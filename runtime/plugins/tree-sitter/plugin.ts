interface HighlightState {
  language: "markdown" | "rust" | null;
  generation: number;
  scheduledGeneration: number | null;
}

interface HighlightResult {
  generation: number;
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
  worker: "worker.ts",
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
  content: {
    create(context) {
      return {
        language: languageFor(context.document?.fileName),
        generation: 0,
        scheduledGeneration: null,
      };
    },
    changed(context) {
      context.contentState.generation += 1;
    },
    job(context) {
      const state = context.contentState;
      if (
        state.language === null ||
        state.scheduledGeneration === state.generation ||
        context.revision === undefined ||
        context.text === undefined
      ) {
        return;
      }
      state.scheduledGeneration = state.generation;
      return {
        slot: "parse",
        version: state.generation,
        message: {
          contentId: context.contentId,
          generation: state.generation,
          language: state.language,
          revision: context.revision,
          text: context.text,
        },
      };
    },
    applyJob(context) {
      const result = context.arguments;
      if (
        context.jobVersion !== context.contentState.generation ||
        result.generation !== context.contentState.generation ||
        result.revision !== context.revision
      ) {
        return;
      }
      return {
        contentDecorations: {
          revision: result.revision,
          spans: result.spans,
        },
      };
    },
  },
  actions: {},
});
