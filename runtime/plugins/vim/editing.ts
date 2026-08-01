export interface EditingStrategyState {
  indentWidth?: number;
  insertSpaces?: boolean;
}

export interface EditingStrategyContext {
  readonly resourceName?: string;
  readonly resourcePath?: string;
  readonly text: string;
  readonly primarySelection: EditorSelection;
  readonly state: ScriptData;
  readonly viewState: EditingStrategyState;
}

export interface LanguageEditingStrategy {
  newline(context: EditingStrategyContext): IndentationDecision;
  readonly lineComment?: LineCommentStrategy;
  readonly blockComment?: BlockCommentStrategy;
  readonly pairs: readonly OpenClosePair[];
}

const COMMON_PAIRS: readonly OpenClosePair[] = [
  { open: "(", close: ")" },
  { open: "[", close: "]" },
  { open: "{", close: "}" },
  { open: '"', close: '"' },
  { open: "'", close: "'" },
];

const FALLBACK: LanguageEditingStrategy = {
  newline: inheritedIndentation,
  pairs: [],
};

const RUST: LanguageEditingStrategy = {
  newline: rustIndentation,
  lineComment: { delimiter: "//" },
  blockComment: { open: "/*", close: "*/" },
  pairs: COMMON_PAIRS,
};

export function editingStrategyFor(
  context: EditingStrategyContext,
): LanguageEditingStrategy {
  const resource = context.resourcePath ?? context.resourceName ?? "";
  return resource.toLowerCase().endsWith(".rs") ? RUST : FALLBACK;
}

function inheritedIndentation(
  context: EditingStrategyContext,
): IndentationDecision {
  return { indent: currentLineParts(context).indent };
}

function rustIndentation(
  context: EditingStrategyContext,
): IndentationDecision {
  const line = currentLineParts(context);
  const opensBlock = line.before.trimEnd().endsWith("{");
  const closesBlock = line.after.trimStart().startsWith("}");
  if (!opensBlock) return { indent: line.indent };
  return {
    indent: line.indent + indentUnit(context.viewState),
    closingIndent: closesBlock ? line.indent : undefined,
  };
}

function currentLineParts(context: EditingStrategyContext): {
  indent: string;
  before: string;
  after: string;
} {
  const offset = positionToOffset(
    context.text,
    context.primarySelection.head,
  );
  const lineStart = context.text.lastIndexOf("\n", offset - 1) + 1;
  const nextBreak = context.text.indexOf("\n", offset);
  const lineEnd = nextBreak < 0 ? context.text.length : nextBreak;
  const line = context.text.slice(lineStart, lineEnd).replace(/\r$/, "");
  return {
    indent: /^[\t ]*/.exec(line)?.[0] ?? "",
    before: context.text.slice(lineStart, offset),
    after: context.text.slice(offset, lineEnd),
  };
}

function positionToOffset(text: string, position: EditorPosition): number {
  let offset = 0;
  for (let line = 0; line < position.line; line++) {
    const next = text.indexOf("\n", offset);
    if (next < 0) return text.length;
    offset = next + 1;
  }
  return Math.min(text.length, offset + position.character);
}

function indentUnit(state: EditingStrategyState): string {
  const width = Math.max(1, Math.min(256, state.indentWidth ?? 4));
  return state.insertSpaces === false ? "\t" : " ".repeat(width);
}
