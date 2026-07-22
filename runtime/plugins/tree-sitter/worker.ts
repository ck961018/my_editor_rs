interface ParseMessage {
  contentId: number;
  language: "markdown" | "rust";
  revision: number;
  text: string;
}

interface EditorPosition {
  line: number;
  character: number;
}

interface DecorationSpan {
  range: { start: EditorPosition; end: EditorPosition };
  face: string;
}

const global = globalThis as Record<string, unknown>;
global.document = {
  currentScript: { src: "file:///runtime/plugins/tree-sitter/vendor/tree-sitter.js" },
};
global.URL = class {
  href: string;

  constructor(path: string, base: string) {
    this.href = `${base.slice(0, base.lastIndexOf("/") + 1)}${path}`;
  }
};
(0, eval)(editor.resources.readText("vendor/tree-sitter.js"));

const TreeSitter = global.Parser as any;
interface ParserState {
  parser: any;
  tree: any | null;
  text: string;
}

const parsers = new Map<string, ParserState>();

const ready = (async () => {
  await TreeSitter.Parser.init({
    wasmBinary: editor.resources.readBinary("vendor/tree-sitter.wasm"),
  });
  const rust = await TreeSitter.Language.load(
    editor.resources.readBinary("vendor/tree-sitter-rust.wasm"),
  );
  const rustQuery = new TreeSitter.Query(
    rust,
    editor.resources.readText("queries/rust/highlights.scm"),
  );
  return { rust, rustQuery };
})();

function faceForCapture(capture: string): string {
  if (capture.startsWith("markup.")) {
    return `syntax.${capture}`;
  }
  const root = capture.split(".", 1)[0];
  const aliases: Record<string, string> = {
    constructor: "syntax.constructor",
    escape: "syntax.string.escape",
    field: "syntax.property",
    module: "syntax.namespace",
  };
  const alias = aliases[root];
  if (alias !== undefined) {
    return alias;
  }
  switch (root) {
    case "attribute":
    case "comment":
    case "constant":
    case "function":
    case "keyword":
    case "label":
    case "namespace":
    case "number":
    case "operator":
    case "property":
    case "punctuation":
    case "string":
    case "type":
    case "variable":
      return `syntax.${capture}`;
    default:
      return "syntax.variable";
  }
}

function textPositions(text: string): Map<number, EditorPosition> {
  const positions = new Map<number, EditorPosition>();
  let offset = 0;
  let line = 0;
  let character = 0;
  positions.set(0, { line, character });
  for (const value of text) {
    offset += value.length;
    if (value === "\r" && text[offset] === "\n") {
      positions.set(offset, { line, character });
      continue;
    } else if (value === "\n") {
      line += 1;
      character = 0;
    } else {
      character += value.length;
    }
    positions.set(offset, { line, character });
  }
  return positions;
}

function parserFor(contentId: number, name: string, language: any): ParserState {
  const key = `${contentId}:${name}`;
  let state = parsers.get(key);
  if (state === undefined) {
    const parser = new TreeSitter.Parser();
    parser.setLanguage(language);
    state = { parser, tree: null, text: "" };
    parsers.set(key, state);
  }
  return state;
}

function pointAt(text: string, index: number): { row: number; column: number } {
  let row = 0;
  let lineStart = 0;
  for (let offset = 0; offset < index; offset += 1) {
    if (text[offset] === "\n") {
      row += 1;
      lineStart = offset + 1;
    }
  }
  return { row, column: index - lineStart };
}

function editTree(tree: any, previous: string, next: string): void {
  let start = 0;
  const shared = Math.min(previous.length, next.length);
  while (start < shared && previous[start] === next[start]) start += 1;
  if (start > 0 && /[\uDC00-\uDFFF]/.test(previous[start] ?? next[start] ?? "")) {
    start -= 1;
  }

  let suffix = 0;
  while (
    suffix < previous.length - start &&
    suffix < next.length - start &&
    previous[previous.length - suffix - 1] === next[next.length - suffix - 1]
  ) {
    suffix += 1;
  }
  if (
    suffix > 0 &&
    /[\uD800-\uDBFF]/.test(
      previous[previous.length - suffix] ?? next[next.length - suffix] ?? "",
    )
  ) {
    suffix -= 1;
  }

  const oldEnd = previous.length - suffix;
  const newEnd = next.length - suffix;
  tree.edit({
    startIndex: start,
    oldEndIndex: oldEnd,
    newEndIndex: newEnd,
    startPosition: pointAt(previous, start),
    oldEndPosition: pointAt(previous, oldEnd),
    newEndPosition: pointAt(next, newEnd),
  });
}

function captureSpans(
  query: any,
  root: any,
  positions: Map<number, EditorPosition>,
  base = 0,
): DecorationSpan[] {
  return query.captures(root).flatMap((capture: any) => {
    const start = positions.get(base + capture.node.startIndex);
    const end = positions.get(base + capture.node.endIndex);
    if (
      start === undefined ||
      end === undefined ||
      start.line === end.line && start.character === end.character
    ) {
      return [];
    }
    return [{
      range: { start, end },
      face: faceForCapture(capture.name),
    }];
  });
}

function pushSpan(
  spans: DecorationSpan[],
  positions: Map<number, EditorPosition>,
  startOffset: number,
  endOffset: number,
  face: string,
): void {
  const start = positions.get(startOffset);
  const end = positions.get(endOffset);
  if (
    start !== undefined &&
    end !== undefined &&
    (start.line !== end.line || start.character !== end.character)
  ) {
    spans.push({ range: { start, end }, face });
  }
}

function inlineMarkdownSpans(
  line: string,
  offset: number,
  positions: Map<number, EditorPosition>,
  spans: DecorationSpan[],
): void {
  const rules: [RegExp, string][] = [
    [/`[^`]+`/g, "syntax.markup.raw"],
    [/\[[^\]]+\]\([^)]+\)/g, "syntax.markup.link"],
    [/\*\*(?=\S)[^*]+?\*\*|__(?=\S)[^_]+?__/g, "syntax.markup.bold"],
    [/\*(?=\S)[^*]+?\*|_(?=\S)[^_]+?_/g, "syntax.markup.italic"],
  ];
  for (const [pattern, face] of rules) {
    for (const match of line.matchAll(pattern)) {
      const start = offset + (match.index ?? 0);
      pushSpan(spans, positions, start, start + match[0].length, face);
    }
  }
}

function highlightRust(
  contentId: number,
  source: string,
  base: number,
  positions: Map<number, EditorPosition>,
  language: any,
  query: any,
): DecorationSpan[] {
  const state = parserFor(
    contentId,
    base === 0 ? "rust" : `rust-injection:${base}`,
    language,
  );
  const oldTree = state.tree?.copy() ?? null;
  if (oldTree !== null) editTree(oldTree, state.text, source);
  state.parser.reset();
  const tree = state.parser.parse(source, oldTree);
  oldTree?.delete();
  if (tree === null) {
    throw new Error("Tree-sitter parse returned no tree");
  }
  const spans = captureSpans(query, tree.rootNode, positions, base);
  state.tree?.delete();
  state.tree = tree;
  state.text = source;
  return spans;
}

function markdownSpans(
  message: ParseMessage,
  positions: Map<number, EditorPosition>,
  language: any,
  query: any,
): DecorationSpan[] {
  const spans: DecorationSpan[] = [];
  const lines = message.text.split("\n");
  let offset = 0;
  let fence: { marker: string; contentStart: number; rust: boolean } | null = null;
  for (const line of lines) {
    const visibleLine = line.endsWith("\r") ? line.slice(0, -1) : line;
    const match = visibleLine.match(/^\s*(`{3,}|~{3,})(.*)$/);
    if (match !== null) {
      if (fence === null) {
        const info = match[2].trim().toLowerCase();
        fence = {
          marker: match[1],
          contentStart: offset + line.length + 1,
          rust: info.split(/[\s,]/, 1)[0] === "rust",
        };
        pushSpan(
          spans,
          positions,
          offset,
          offset + visibleLine.length,
          "syntax.markup.raw",
        );
      } else if (
        match[1][0] === fence.marker[0] &&
        match[1].length >= fence.marker.length &&
        match[2].trim() === ""
      ) {
        if (fence.rust) {
          const source = message.text.slice(fence.contentStart, offset);
          spans.push(...highlightRust(
            message.contentId,
            source,
            fence.contentStart,
            positions,
            language,
            query,
          ));
        }
        pushSpan(
          spans,
          positions,
          offset,
          offset + visibleLine.length,
          "syntax.markup.raw",
        );
        fence = null;
      }
    } else if (fence === null) {
      const heading = visibleLine.match(/^\s*#{1,6}(?:\s+|$)/);
      if (heading !== null) {
        pushSpan(
          spans,
          positions,
          offset,
          offset + visibleLine.length,
          "syntax.markup.heading",
        );
      }
      const quote = visibleLine.match(/^\s*>+/);
      if (quote !== null) {
        pushSpan(
          spans,
          positions,
          offset + quote[0].indexOf(">"),
          offset + quote[0].length,
          "syntax.markup.quote",
        );
      }
      const list = visibleLine.match(/^\s*(?:[-+*]|\d+[.)])(?=\s)/);
      if (list !== null) {
        const marker = list[0].trimStart();
        const start = offset + list[0].length - marker.length;
        pushSpan(
          spans,
          positions,
          start,
          start + marker.length,
          "syntax.markup.list",
        );
      }
      inlineMarkdownSpans(visibleLine, offset, positions, spans);
    }
    offset += line.length + 1;
  }
  return spans;
}

editor.worker.onMessage(async (message: ParseMessage) => {
  const { rust, rustQuery } = await ready;
  const positions = textPositions(message.text);
  const spans = message.language === "rust"
    ? highlightRust(message.contentId, message.text, 0, positions, rust, rustQuery)
    : markdownSpans(message, positions, rust, rustQuery);
  return {
    revision: message.revision,
    spans,
  };
});
