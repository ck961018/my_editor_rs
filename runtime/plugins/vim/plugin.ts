import { editingStrategyFor } from "./editing.ts";

type VimEditorState = "normal" | "insert" | "visual" | "visual-line";
type VimRegister = "internal" | "system";

interface VimSearchState {
  [key: string]: ScriptData;
  pattern: EditorSearchPattern;
  direction: "forward" | "backward";
  caseSensitive: boolean;
}

interface VimContentState {
  [key: string]: ScriptData;
  search: VimSearchState | null;
}

type KeyInput = EditorKeyEvent;

type VimPending =
  | { kind: "count"; count: number }
  | { kind: "find"; direction: "forward" | "backward"; count: number }
  | { kind: "goto"; count: number }
  | { kind: "indent"; direction: "indent" | "outdent"; count: number }
  | { kind: "register" }
  | { kind: "search"; direction: "forward" | "backward"; value: string }
  | { kind: "command"; value: string }
  | { kind: "viewport"; line?: number }
  | { kind: "window" }
  | {
    kind: "operator";
    operator: "delete" | "change" | "yank";
    operatorCount: number;
    motionCount?: number;
  };

interface VimViewState {
  state: VimEditorState;
  pending: VimPending | null;
  register: VimRegister;
  indentWidth: number;
  insertSpaces: boolean;
  viewPolicy: {
    cursorStyle: "block" | "bar";
    cursorDomain: "character" | "insertion-point";
    selectionShape: "character" | "character-inclusive" | "line";
    tabWidth?: number;
    statusBar?: StatusBarPresentation;
  };
}

type VimContext = BufferCommandContext<VimContentState, VimViewState, KeyInput>;
type Effect = (context: VimContext) => void;

function isVisual(state: VimViewState): boolean {
  return state.state === "visual" || state.state === "visual-line";
}

function setEditorState(state: VimViewState, next: VimEditorState): void {
  state.state = next;
  state.pending = null;
  state.viewPolicy = {
    cursorStyle: next === "insert" ? "bar" : "block",
    cursorDomain: next === "insert" ? "insertion-point" : "character",
    selectionShape: next === "visual"
      ? "character-inclusive"
      : next === "visual-line"
      ? "line"
      : "character",
    tabWidth: state.viewPolicy.tabWidth,
  };
}

function showPrompt(state: VimViewState, prefix: string, value: string): void {
  state.viewPolicy.statusBar = { left: [{ text: prefix + value }] };
}

function clearPrompt(state: VimViewState): void {
  state.viewPolicy.statusBar = undefined;
}

function takeRegister(state: VimViewState): VimRegister {
  const register = state.register;
  state.register = "internal";
  return register;
}

function indentation(state: VimViewState): EditorIndentationConfig {
  return {
    indentWidth: state.indentWidth,
    insertSpaces: state.insertSpaces,
  };
}

function takeCount(state: VimViewState): number {
  if (state.pending?.kind !== "count") return 1;
  const count = state.pending.count;
  state.pending = null;
  return count;
}

function beginViewport(state: VimViewState): void {
  const line = state.pending?.kind === "count"
    ? Math.max(1, state.pending.count) - 1
    : undefined;
  state.pending = { kind: "viewport", line };
}

function isPlain(key: KeyInput): boolean {
  return !key.modifiers.alt && !key.modifiers.ctrl && !key.modifiers.shift;
}

function isCtrl(key: KeyInput, character: string): boolean {
  return key.code === "character" && key.character === character &&
    key.modifiers.ctrl && !key.modifiers.alt;
}

function operatorEffect(
  state: VimViewState,
  operator: "delete" | "change" | "yank",
  kind: EditorClipboardKind,
  edit: EditorClipboardEdit,
  count: number,
  apply: Effect,
): Effect {
  const destination = takeRegister(state);
  return (context) => {
    context.clipboard.copyForEdit(kind, edit, count, destination);
    if (operator !== "yank") apply(context);
  };
}

function completeOperator(
  state: VimViewState,
  operator: "delete" | "change" | "yank",
  effect: Effect,
): Effect[] {
  if (operator !== "change") {
    state.pending = null;
    return [effect];
  }
  setEditorState(state, "insert");
  return [
    (context) => context.history.begin(),
    effect,
  ];
}

function handlePending(state: VimViewState, key: KeyInput): Effect[] | null {
  const pending = state.pending;
  if (!pending) return null;
  if (key.code === "escape" && isPlain(key)) {
    state.pending = null;
    clearPrompt(state);
    return [];
  }

  if (pending.kind === "register") {
    state.pending = null;
    if (
      key.code === "character" && !key.modifiers.alt && !key.modifiers.ctrl &&
      (key.character === "+" || key.character === "*")
    ) {
      state.register = "system";
    }
    return [];
  }

  if (pending.kind === "search") {
    if (key.code === "enter" && isPlain(key)) {
      state.pending = null;
      clearPrompt(state);
      if (pending.value.length === 0) return [];
      const search = {
        pattern: { kind: "regex", value: pending.value } as const,
        direction: pending.direction,
        caseSensitive: true,
      };
      return [(context) => {
        context.state.search = search;
        context.search.find(search.pattern, {
          caseSensitive: search.caseSensitive,
          direction: search.direction,
          wrap: true,
        });
      }];
    }
    if (key.code === "backspace" && isPlain(key) || isCtrl(key, "h")) {
      pending.value = pending.value.slice(0, -1);
      showPrompt(state, pending.direction === "forward" ? "/" : "?", pending.value);
      return [];
    }
    if (
      key.code === "character" && !key.modifiers.alt && !key.modifiers.ctrl &&
      key.character !== undefined
    ) {
      pending.value += key.character;
      showPrompt(state, pending.direction === "forward" ? "/" : "?", pending.value);
    }
    return [];
  }

  if (pending.kind === "command") {
    if (key.code === "enter" && isPlain(key)) {
      state.pending = null;
      clearPrompt(state);
      const command = pending.value;
      return [(context) => executeCommand(context, command)];
    }
    if (key.code === "backspace" && isPlain(key) || isCtrl(key, "h")) {
      pending.value = pending.value.slice(0, -1);
      showPrompt(state, ":", pending.value);
      return [];
    }
    if (
      key.code === "character" && !key.modifiers.alt && !key.modifiers.ctrl &&
      key.character !== undefined
    ) {
      pending.value += key.character;
      showPrompt(state, ":", pending.value);
    }
    return [];
  }

  if (pending.kind === "indent") {
    state.pending = null;
    const expected = pending.direction === "indent" ? ">" : "<";
    if (key.code !== "character" || key.character !== expected || !isPlain(key)) {
      return [];
    }
    const config = indentation(state);
    return Array.from({ length: pending.count }, () =>
      pending.direction === "indent"
        ? (context: VimContext) => context.edit.indentLines(config)
        : (context: VimContext) => context.edit.outdentLines(config));
  }

  if (pending.kind === "find") {
    state.pending = null;
    if (key.code !== "character" || !isPlain(key) || !key.character) return [];
    return [(context) => {
      const cursor = context.cursor;
      if (isVisual(state)) {
        if (pending.direction === "forward") {
          cursor.extendToCharForward(key.character!, pending.count);
        } else {
          cursor.extendToCharBackward(key.character!, pending.count);
        }
      } else if (pending.direction === "forward") {
        cursor.moveToCharForward(key.character!, pending.count);
      } else {
        cursor.moveToCharBackward(key.character!, pending.count);
      }
    }];
  }

  if (pending.kind === "goto") {
    state.pending = null;
    if (key.code !== "character" || !key.character || !isPlain(key)) {
      return [];
    }
    if (key.character === "g") {
      const line = Math.max(1, pending.count) - 1;
      return [(context) => isVisual(state)
        ? context.cursor.extendToLine(line)
        : context.cursor.moveToLine(line)];
    }
    if (key.character === "c") {
      return [(context) => {
        const strategy = editingStrategyFor(context);
        if (strategy.lineComment) {
          context.edit.toggleLineComment(strategy.lineComment);
        }
      }];
    }
    if (key.character === "b") {
      return [(context) => {
        const strategy = editingStrategyFor(context);
        if (strategy.blockComment) {
          context.edit.toggleBlockComment(strategy.blockComment);
        }
      }];
    }
    if (key.character === "d") {
      return [(context) => context.edit.duplicateLines()];
    }
    if (key.character === "k" || key.character === "j") {
      return [(context) => key.character === "k"
        ? context.edit.moveLinesUp()
        : context.edit.moveLinesDown()];
    }
    return [];
  }

  if (pending.kind === "viewport") {
    state.pending = null;
    if (key.code !== "character" || !isPlain(key) || !key.character) return [];
    const alignment = key.character === "t"
      ? "top"
      : key.character === "z"
      ? "center"
      : key.character === "b"
      ? "bottom"
      : null;
    if (alignment === null) return [];
    const effects: Effect[] = [];
    if (pending.line !== undefined) {
      effects.push((context) => isVisual(state)
        ? context.cursor.extendToLinePreservingColumn(pending.line!)
        : context.cursor.moveToLinePreservingColumn(pending.line!));
    }
    effects.push((context) => {
      if (alignment === "top") context.viewport.alignTop();
      else if (alignment === "center") context.viewport.alignCenter();
      else context.viewport.alignBottom();
    });
    return effects;
  }

  if (pending.kind === "window") {
    state.pending = null;
    if (key.code !== "character" || !isPlain(key) || !key.character) return [];
    const effects: Record<string, Effect> = {
      s: (context) => context.app.splitHorizontal(),
      v: (context) => context.app.splitVertical(),
      q: (context) => context.app.closePane(),
      h: (context) => context.app.focusLeft(),
      j: (context) => context.app.focusDown(),
      k: (context) => context.app.focusUp(),
      l: (context) => context.app.focusRight(),
    };
    const effect = effects[key.character];
    return effect ? [effect] : [];
  }

  if (pending.kind === "operator") {
    if (key.code !== "character" || !isPlain(key) || !key.character) {
      state.pending = null;
      return [];
    }
    if (key.character >= "0" && key.character <= "9") {
      if (key.character === "0" && pending.motionCount === undefined) {
        return completeOperator(
          state,
          pending.operator,
          operatorEffect(
            state,
            pending.operator,
            "character",
            "line-start",
            pending.operatorCount,
            (context) =>
              context.edit.deleteToLineStartMotion(pending.operatorCount),
          ),
        );
      }
      pending.motionCount = (pending.motionCount ?? 0) * 10 +
        Number(key.character);
      return [];
    }
    const count = pending.operatorCount * (pending.motionCount ?? 1);
    if (key.character === pending.operator[0]) {
      return completeOperator(
        state,
        pending.operator,
        operatorEffect(
          state,
          pending.operator,
          "line",
          "lines",
          count,
          (context) => pending.operator === "change"
            ? context.edit.changeLines(count)
            : context.edit.deleteLines(count),
        ),
      );
    }
    if (key.character === "w") {
      return completeOperator(
        state,
        pending.operator,
        operatorEffect(
          state,
          pending.operator,
          "character",
          pending.operator === "change" ? "change-word" : "word",
          count,
          (context) => pending.operator === "change"
            ? context.edit.changeWordMotion(count)
            : context.edit.deleteWordMotion(count),
        ),
      );
    }
    if (key.character === "e") {
      return completeOperator(
        state,
        pending.operator,
        operatorEffect(
          state,
          pending.operator,
          "character",
          "word-end",
          count,
          (context) => context.edit.deleteWordEndMotion(count),
        ),
      );
    }
    if (key.character === "$") {
      return completeOperator(
        state,
        pending.operator,
        operatorEffect(
          state,
          pending.operator,
          "character",
          "line-end",
          count,
          (context) => context.edit.deleteToLineEndMotion(count),
        ),
      );
    }
    state.pending = null;
    return [];
  }

  if (key.code !== "character" || !isPlain(key) || !key.character) {
    state.pending = null;
    return [];
  }
  if (key.character >= "0" && key.character <= "9") {
    pending.count = pending.count * 10 + Number(key.character);
    return [];
  }
  const allowed = isVisual(state)
    ? "hjklwbefFg dz^$G{}".replaceAll(" ", "")
    : "hjklwbefFgdzc$";
  if (!allowed.includes(key.character)) {
    state.pending = null;
    return [];
  }
  return null;
}

function handleInsert(state: VimViewState, key: KeyInput): Effect[] | null {
  if (key.code === "escape" && isPlain(key)) {
    setEditorState(state, "normal");
    return [
      (context) => context.history.commit(),
      (context) => context.cursor.collapseSelections(),
    ];
  }
  if (key.code === "character" && isPlain(key) && key.character !== undefined) {
    return [(context) => {
      const character = key.character!;
      const strategy = editingStrategyFor(context);
      const opening = strategy.pairs.find((pair) => pair.open === character);
      const closing = strategy.pairs.find((pair) => pair.close === character);
      if (opening && (!closing || characterAtCursor(context) !== character)) {
        context.edit.insertPair(opening);
      } else if (closing) {
        context.edit.insertClosingPair(closing);
      } else {
        context.edit.insert(character);
      }
    }];
  }
  if (key.code === "enter" && isPlain(key) || isCtrl(key, "j") || isCtrl(key, "m")) {
    return [(context) =>
      context.edit.insertNewline(editingStrategyFor(context).newline(context))];
  }
  if (key.code === "backspace" && isPlain(key) || isCtrl(key, "h")) {
    return [(context) => {
      const pair = pairAroundCursor(context, editingStrategyFor(context).pairs);
      if (pair) context.edit.deletePairBackward(pair);
      else context.edit.deleteBackward();
    }];
  }
  if (key.code === "tab" && !key.modifiers.alt && !key.modifiers.ctrl) {
    return [(context) => context.edit.indentLines(indentation(state))];
  }
  if (key.code === "backtab" && !key.modifiers.alt && !key.modifiers.ctrl) {
    return [(context) => context.edit.outdentLines(indentation(state))];
  }
  if (isCtrl(key, "w")) return [(context) => context.edit.deleteWordBackward()];
  if (isCtrl(key, "u")) return [(context) => context.edit.deleteToLineStart()];
  if (isCtrl(key, "k")) return [(context) => context.edit.deleteToLineEnd()];
  if (isCtrl(key, "b")) return [(context) => context.cursor.moveLeft()];
  if (isCtrl(key, "f")) return [(context) => context.cursor.moveRight()];
  if (key.code === "arrow" && key.direction !== undefined) {
    const extend = key.modifiers.shift;
    const direction = key.direction;
    return [(context) => moveArrow(context, direction, extend)];
  }
  return null;
}

function moveArrow(
  context: VimContext,
  direction: "up" | "down" | "left" | "right",
  extend: boolean,
): void {
  if (direction === "up") {
    extend ? context.cursor.extendUp() : context.cursor.moveUp();
  } else if (direction === "down") {
    extend ? context.cursor.extendDown() : context.cursor.moveDown();
  } else if (direction === "left") {
    extend ? context.cursor.extendLeft() : context.cursor.moveLeft();
  } else {
    extend ? context.cursor.extendRight() : context.cursor.moveRight();
  }
}

function cursorOffset(context: VimContext): number {
  const position = context.primarySelection.head;
  let offset = 0;
  for (let line = 0; line < position.line; line++) {
    const next = context.text.indexOf("\n", offset);
    if (next < 0) return context.text.length;
    offset = next + 1;
  }
  return Math.min(context.text.length, offset + position.character);
}

function characterAtCursor(context: VimContext): string | undefined {
  return context.text[cursorOffset(context)];
}

function pairAroundCursor(
  context: VimContext,
  pairs: readonly OpenClosePair[],
): OpenClosePair | undefined {
  const offset = cursorOffset(context);
  return pairs.find((pair) =>
    context.text.slice(0, offset).endsWith(pair.open) &&
    context.text.slice(offset).startsWith(pair.close)
  );
}

function wordAtCursor(context: VimContext): string | undefined {
  const offset = cursorOffset(context);
  for (const match of context.text.matchAll(/[\p{Alphabetic}\p{Number}_]+/gu)) {
    const start = match.index;
    const end = start + match[0].length;
    if (start <= offset && offset < end) return match[0];
  }
  return undefined;
}

function handleMotion(state: VimViewState, character: string): Effect[] | null {
  const visual = isVisual(state);
  const motions: Record<string, (context: VimContext, count: number) => void> = {
    h: (context, count) => visual
      ? context.cursor.extendWithinLineLeft(count)
      : context.cursor.moveWithinLineLeft(count),
    j: (context, count) => visual
      ? context.cursor.extendDown(count)
      : context.cursor.moveDown(count),
    k: (context, count) => visual
      ? context.cursor.extendUp(count)
      : context.cursor.moveUp(count),
    l: (context, count) => visual
      ? context.cursor.extendWithinLineRight(count)
      : context.cursor.moveWithinLineRight(count),
    w: (context, count) => visual
      ? context.cursor.extendWordForward(count)
      : context.cursor.moveWordForward(count),
    b: (context, count) => visual
      ? context.cursor.extendWordBackward(count)
      : context.cursor.moveWordBackward(count),
    e: (context, count) => visual
      ? context.cursor.extendWordEnd(count)
      : context.cursor.moveWordEnd(count),
    "0": (context) => visual
      ? context.cursor.extendToLineStart()
      : context.cursor.moveToLineStart(),
    "^": (context) => visual
      ? context.cursor.extendToFirstNonBlank()
      : context.cursor.moveToFirstNonBlank(),
    "$": (context, count) => {
      if (count > 1) {
        if (visual) context.cursor.extendDown(count - 1);
        else context.cursor.moveDown(count - 1);
      }
      if (visual) context.cursor.extendToLineEnd();
      else context.cursor.moveToLineEnd();
    },
    G: (context) => visual
      ? context.cursor.extendToLastLine()
      : context.cursor.moveToLastLine(),
    "{": (context, count) => visual
      ? context.cursor.extendToPrevParagraph(count)
      : context.cursor.moveToPrevParagraph(count),
    "}": (context, count) => visual
      ? context.cursor.extendToNextParagraph(count)
      : context.cursor.moveToNextParagraph(count),
  };
  const resolved = motions[character];
  if (!resolved) return null;
  const count = takeCount(state);
  return [(context) => resolved(context, count)];
}

function enterVisual(state: VimViewState, next: "visual" | "visual-line"): Effect[] {
  if (state.state === next) {
    setEditorState(state, "normal");
    return [(context) => context.cursor.collapseSelections()];
  }
  setEditorState(state, next);
  return [];
}

function handleVisual(state: VimViewState, key: KeyInput): Effect[] | null {
  if (key.code === "escape" && isPlain(key)) {
    setEditorState(state, "normal");
    return [(context) => context.cursor.collapseSelections()];
  }
  if (
    key.code === "arrow" &&
    key.direction !== undefined &&
    !key.modifiers.alt &&
    !key.modifiers.ctrl
  ) {
    const direction = key.direction;
    return [(context) => moveArrow(context, direction, true)];
  }
  if (isCtrl(key, "u")) {
    return [(context) => context.viewport.halfPageUp(true)];
  }
  if (isCtrl(key, "d")) {
    return [(context) => context.viewport.halfPageDown(true)];
  }
  if (isCtrl(key, "b")) {
    return [(context) => context.viewport.fullPageUp(true)];
  }
  if (isCtrl(key, "f")) {
    return [(context) => context.viewport.fullPageDown(true)];
  }
  if (key.code !== "character" || !isPlain(key) || !key.character) return null;
  const motion = handleMotion(state, key.character);
  if (motion) return motion;
  if (key.character === '"') {
    state.pending = { kind: "register" };
    return [];
  }
  if (key.character === "v") return enterVisual(state, "visual");
  if (key.character === "V") return enterVisual(state, "visual-line");
  if (key.character === "f" || key.character === "F") {
    const count = takeCount(state);
    state.pending = {
      kind: "find",
      direction: key.character === "f" ? "forward" : "backward",
      count,
    };
    return [];
  }
  if (key.character === "g") {
    state.pending = { kind: "goto", count: takeCount(state) };
    return [];
  }
  if (key.character === "z") {
    beginViewport(state);
    return [];
  }
  if (key.character === "y") {
    const linewise = state.state === "visual-line";
    const destination = takeRegister(state);
    setEditorState(state, "normal");
    return [
      (context) => context.clipboard.copyForEdit(
        linewise ? "line" : "character",
        linewise ? "selected-lines" : "selection-inclusive",
        1,
        destination,
      ),
      (context) => context.cursor.collapseSelections(),
    ];
  }
  if (["d", "x", "D", "X"].includes(key.character)) {
    const linewise = state.state === "visual-line";
    const destination = takeRegister(state);
    setEditorState(state, "normal");
    return [(context) => {
      context.clipboard.copyForEdit(
        linewise ? "line" : "character",
        linewise ? "selected-lines" : "selection-inclusive",
        1,
        destination,
      );
      if (linewise) context.edit.deleteSelectedLines();
      else context.edit.deleteSelectionInclusive();
    }];
  }
  if (key.character === "c" || key.character === "s") {
    const linewise = state.state === "visual-line";
    const destination = takeRegister(state);
    setEditorState(state, "insert");
    return [
      (context) => context.history.begin(),
      (context) => {
        context.clipboard.copyForEdit(
          linewise ? "line" : "character",
          linewise ? "selected-lines" : "selection-inclusive",
          1,
          destination,
        );
        if (linewise) context.edit.deleteSelectedLines();
        else context.edit.deleteSelectionInclusive();
      },
    ];
  }
  if (key.character === ">" || key.character === "<") {
    const config = indentation(state);
    return [(context) => key.character === ">"
      ? context.edit.indentLines(config)
      : context.edit.outdentLines(config)];
  }
  if (key.character >= "1" && key.character <= "9") {
    state.pending = { kind: "count", count: Number(key.character) };
    return [];
  }
  return null;
}

function handleNormal(state: VimViewState, key: KeyInput): Effect[] | null {
  if (key.code === "escape" && isPlain(key)) return [];
  if (isCtrl(key, "w")) {
    state.pending = { kind: "window" };
    return [];
  }
  if (isCtrl(key, "r")) return [(context) => context.history.redo()];
  if (isCtrl(key, "u")) return [(context) => context.viewport.halfPageUp()];
  if (isCtrl(key, "d")) return [(context) => context.viewport.halfPageDown()];
  if (isCtrl(key, "b")) return [(context) => context.viewport.fullPageUp()];
  if (isCtrl(key, "f")) return [(context) => context.viewport.fullPageDown()];
  if (
    key.code === "arrow" && key.modifiers.alt && !key.modifiers.ctrl &&
    (key.direction === "up" || key.direction === "down")
  ) {
    if (key.modifiers.shift && key.direction === "down") {
      return [(context) => context.edit.duplicateLines()];
    }
    if (!key.modifiers.shift) {
      return [(context) => key.direction === "up"
        ? context.edit.moveLinesUp()
        : context.edit.moveLinesDown()];
    }
  }
  if (key.code !== "character" || !isPlain(key) || !key.character) return null;

  const motion = handleMotion(state, key.character);
  if (motion) return motion;
  if (key.character >= "1" && key.character <= "9") {
    state.pending = { kind: "count", count: Number(key.character) };
    return [];
  }
  if (key.character === '"') {
    state.pending = { kind: "register" };
    return [];
  }
  if (key.character === "/" || key.character === "?") {
    const direction = key.character === "/" ? "forward" : "backward";
    state.pending = { kind: "search", direction, value: "" };
    showPrompt(state, key.character, "");
    return [];
  }
  if (key.character === ":") {
    state.pending = { kind: "command", value: "" };
    showPrompt(state, ":", "");
    return [];
  }
  if (key.character === "n" || key.character === "N") {
    return [(context) => {
      const search = context.state.search;
      if (!search) return;
      const direction = key.character === "n"
        ? search.direction
        : search.direction === "forward"
        ? "backward"
        : "forward";
      context.search.find(search.pattern, {
        caseSensitive: search.caseSensitive,
        direction,
        wrap: true,
      });
    }];
  }
  if (key.character === "*" || key.character === "#") {
    const direction = key.character === "*" ? "forward" : "backward";
    return [(context) => {
      const word = wordAtCursor(context);
      if (!word) return;
      const search: VimSearchState = {
        pattern: { kind: "literal", value: word },
        direction,
        caseSensitive: true,
      };
      context.state.search = search;
      context.search.find(search.pattern, {
        caseSensitive: true,
        direction,
        wrap: true,
      });
    }];
  }
  if (key.character === "p" || key.character === "P") {
    const source = takeRegister(state);
    return [(context) => context.clipboard.paste(
      source,
      key.character === "P" ? "before" : "after",
    )];
  }
  if (key.character === "u") return [(context) => context.history.undo()];
  if (key.character === "x" || key.character === "X") {
    const destination = takeRegister(state);
    const backward = key.character === "X";
    return [(context) => {
      context.clipboard.copyForEdit(
        "character",
        backward ? "delete-backward" : "delete-forward",
        1,
        destination,
      );
      if (backward) context.edit.deleteBackward();
      else context.edit.deleteForward();
    }];
  }
  if (key.character === "J") return [(context) => context.edit.joinLines()];
  if (key.character === "D") {
    const destination = takeRegister(state);
    return [(context) => {
      context.clipboard.copyForEdit(
        "character",
        "line-end",
        1,
        destination,
      );
      context.edit.deleteToLineEnd();
    }];
  }
  if (key.character === "~") return [(context) => context.edit.toggleCase()];
  if (key.character === "i") {
    setEditorState(state, "insert");
    return [(context) => context.history.begin()];
  }
  if (key.character === "a") {
    setEditorState(state, "insert");
    return [
      (context) => context.cursor.moveRight(),
      (context) => context.history.begin(),
    ];
  }
  if (key.character === "o" || key.character === "O") {
    const below = key.character === "o";
    setEditorState(state, "insert");
    return [
      (context) => context.history.begin(),
      (context) => {
        const decision = editingStrategyFor(context).newline(context);
        if (below) context.edit.insertLineBelow();
        else context.edit.insertLineAbove();
        if (decision.indent.length > 0) context.edit.insert(decision.indent);
      },
    ];
  }
  if (key.character === "I") {
    setEditorState(state, "insert");
    return [
      (context) => context.cursor.moveToFirstNonBlank(),
      (context) => context.history.begin(),
    ];
  }
  if (key.character === "A") {
    setEditorState(state, "insert");
    return [
      (context) => context.cursor.moveAfterLineEnd(),
      (context) => context.history.begin(),
    ];
  }
  if (key.character === "s") {
    const destination = takeRegister(state);
    setEditorState(state, "insert");
    return [
      (context) => context.history.begin(),
      (context) => {
        context.clipboard.copyForEdit(
          "character",
          "delete-forward",
          1,
          destination,
        );
        context.edit.deleteForward();
      },
    ];
  }
  if (key.character === "C") {
    const destination = takeRegister(state);
    setEditorState(state, "insert");
    return [
      (context) => context.history.begin(),
      (context) => {
        context.clipboard.copyForEdit(
          "character",
          "line-end",
          1,
          destination,
        );
        context.edit.deleteToLineEnd();
      },
    ];
  }
  if (key.character === "S") {
    const destination = takeRegister(state);
    setEditorState(state, "insert");
    return [
      (context) => context.history.begin(),
      (context) => {
        context.clipboard.copyForEdit(
          "line",
          "line-content",
          1,
          destination,
        );
        context.edit.deleteLineContent();
      },
    ];
  }
  if (key.character === "v") return enterVisual(state, "visual");
  if (key.character === "V") return enterVisual(state, "visual-line");
  if (key.character === "f" || key.character === "F") {
    const count = takeCount(state);
    state.pending = {
      kind: "find",
      direction: key.character === "f" ? "forward" : "backward",
      count,
    };
    return [];
  }
  if (key.character === "g") {
    state.pending = { kind: "goto", count: takeCount(state) };
    return [];
  }
  if (key.character === "z") {
    beginViewport(state);
    return [];
  }
  if (key.character === ">" || key.character === "<") {
    state.pending = {
      kind: "indent",
      direction: key.character === ">" ? "indent" : "outdent",
      count: takeCount(state),
    };
    return [];
  }
  if (["d", "c", "y"].includes(key.character)) {
    state.pending = {
      kind: "operator",
      operator: key.character === "d"
        ? "delete"
        : key.character === "c"
        ? "change"
        : "yank",
      operatorCount: takeCount(state),
    };
    return [];
  }
  return null;
}

function executeCommand(context: VimContext, raw: string): void {
  const command = raw.trim();
  const substitution = parseSubstitution(command);
  if (substitution) {
    if (!/^[gi]*$/.test(substitution.flags)) {
      commandError(context, `E488: trailing characters: ${substitution.flags}`);
      return;
    }
    const search: VimSearchState = {
      pattern: { kind: "regex", value: substitution.pattern },
      direction: "forward",
      caseSensitive: !substitution.flags.includes("i"),
    };
    context.state.search = search;
    const options = { caseSensitive: search.caseSensitive };
    if (substitution.all) {
      context.search.replaceAll(
        search.pattern,
        substitution.replacement,
        options,
      );
    } else {
      context.search.replaceNext(search.pattern, substitution.replacement, {
        ...options,
        direction: "forward",
        wrap: false,
      });
    }
    return;
  }

  const match = /^(\S+)(?:\s+(.*))?$/.exec(command);
  if (!match) return;
  const name = match[1];
  const argument = match[2]?.trim();
  const force = name.endsWith("!");
  const base = force ? name.slice(0, -1) : name;
  if (base === "e" || base === "edit") {
    if (force && !argument) context.buffers.reload(undefined, true);
    else if (argument) context.buffers.open(argument);
    else commandError(context, "E471: path required");
  } else if (base === "enew" || base === "new") {
    context.buffers.create();
  } else if (base === "buffers" || base === "ls") {
    context.buffers.list();
  } else if (base === "b" || base === "buffer") {
    const id = parseContentId(argument);
    if (id === undefined) commandError(context, "E86: buffer id required");
    else context.buffers.switch(id);
  } else if (base === "bd" || base === "bdelete") {
    const id = argument ? parseContentId(argument) : undefined;
    if (argument && id === undefined) commandError(context, "E86: invalid buffer id");
    else context.buffers.close(id, force);
  } else if (base === "w" || base === "write") {
    if (argument) context.buffers.saveAs(argument, force);
    else context.buffers.save(undefined, force);
  } else if (base === "saveas") {
    if (argument) context.buffers.saveAs(argument, force);
    else commandError(context, "E471: path required");
  } else if (base === "reload") {
    context.buffers.reload(undefined, force);
  } else if (base === "duplicate") {
    context.edit.duplicateLines();
  } else if (base === "moveup") {
    context.edit.moveLinesUp();
  } else if (base === "movedown") {
    context.edit.moveLinesDown();
  } else if (base === "comment") {
    const strategy = editingStrategyFor(context);
    if (strategy.lineComment) context.edit.toggleLineComment(strategy.lineComment);
  } else if (base === "blockcomment") {
    const strategy = editingStrategyFor(context);
    if (strategy.blockComment) context.edit.toggleBlockComment(strategy.blockComment);
  } else if (base === "set") {
    applySetCommand(context, argument ?? "");
  } else {
    commandError(context, `E492: not an editor command: ${command}`);
  }
}

function commandError(context: VimContext, message: string): void {
  context.viewState.viewPolicy.statusBar = { left: [{ text: message }] };
}

function parseContentId(value: string | undefined): number | undefined {
  if (!value || !/^\d+$/.test(value)) return undefined;
  const parsed = Number(value);
  return Number.isSafeInteger(parsed) ? parsed : undefined;
}

function applySetCommand(context: VimContext, argument: string): void {
  if (argument === "expandtab") {
    context.viewState.insertSpaces = true;
    return;
  }
  if (argument === "noexpandtab") {
    context.viewState.insertSpaces = false;
    return;
  }
  const width = /^(?:tabstop|ts|shiftwidth|sw)=(\d+)$/.exec(argument);
  if (!width) {
    commandError(context, `E518: unknown option: ${argument}`);
    return;
  }
  const value = Number(width[1]);
  if (value < 1 || value > 256) {
    commandError(context, "E487: option value out of range");
    return;
  }
  if (argument.startsWith("tabstop") || argument.startsWith("ts=")) {
    context.viewState.viewPolicy.tabWidth = value;
  } else {
    context.viewState.indentWidth = value;
  }
}

interface Substitution {
  pattern: string;
  replacement: string;
  flags: string;
  all: boolean;
}

function parseSubstitution(command: string): Substitution | undefined {
  const all = command.startsWith("%s");
  const start = all ? 2 : command.startsWith("s") ? 1 : -1;
  if (start < 0 || start >= command.length) return undefined;
  const delimiter = command[start];
  if (/[\s\p{Alphabetic}\p{Number}_]/u.test(delimiter)) return undefined;
  const pattern = takeDelimited(command, start + 1, delimiter);
  if (!pattern) return undefined;
  const replacement = takeDelimited(command, pattern.next, delimiter);
  if (!replacement) return undefined;
  return {
    pattern: pattern.value,
    replacement: replacement.value,
    flags: command.slice(replacement.next),
    all,
  };
}

function takeDelimited(
  text: string,
  start: number,
  delimiter: string,
): { value: string; next: number } | undefined {
  let value = "";
  for (let index = start; index < text.length; index++) {
    const character = text[index];
    if (character === delimiter) return { value, next: index + 1 };
    if (character === "\\" && text[index + 1] === delimiter) {
      value += delimiter;
      index++;
    } else {
      value += character;
    }
  }
  return undefined;
}

editor.modes.define({
  name: "vim",
  on: {
    buffer: {
      state: (): VimContentState => ({ search: null }),
      viewState: (): VimViewState => ({
        state: "normal",
        pending: null,
        register: "internal",
        indentWidth: 4,
        insertSpaces: true,
        viewPolicy: {
          cursorStyle: "block",
          cursorDomain: "character",
          selectionShape: "character",
        },
      }),
      input(context) {
        const state = context.viewState;
        const key = context.arguments;
        if (!state.pending && state.viewPolicy.statusBar) clearPrompt(state);
        const pending = handlePending(state, key);
        const effects = pending ?? (state.state === "insert"
          ? handleInsert(state, key)
          : isVisual(state)
          ? handleVisual(state, key)
          : handleNormal(state, key));
        if (effects === null) return context.pass();
        for (const effect of effects) effect(context);
      },
    },
  },
});
