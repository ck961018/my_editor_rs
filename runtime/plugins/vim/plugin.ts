type VimEditorState = "normal" | "insert" | "visual" | "visual-line";

interface KeyInput {
  code: "character" | "arrow" | "backspace" | "enter" | "escape" |
    "function" | "unknown";
  character?: string;
  direction?: "up" | "down" | "left" | "right";
  number?: number;
  modifiers: {
    alt: boolean;
    ctrl: boolean;
    shift: boolean;
  };
}

type VimPending =
  | { kind: "count"; count: number }
  | { kind: "find"; direction: "forward" | "backward"; count: number }
  | { kind: "goto"; count: number }
  | { kind: "viewport"; line?: number }
  | {
    kind: "operator";
    operator: "delete" | "change";
    operatorCount: number;
    motionCount?: number;
  };

interface VimViewState {
  state: VimEditorState;
  pending: VimPending | null;
  viewPolicy: {
    cursorStyle: "block" | "bar";
    cursorDomain: "character" | "insertion-point";
    selectionShape: "character" | "character-inclusive" | "line";
  };
}

type VimContext = ModeContext<null, VimViewState, KeyInput>;
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

function completeOperator(
  state: VimViewState,
  operator: "delete" | "change",
  effect: Effect,
): Effect[] {
  if (operator === "delete") {
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
    return [];
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
    if (key.code !== "character" || key.character !== "g" || !isPlain(key)) {
      return [];
    }
    const line = Math.max(1, pending.count) - 1;
    return [(context) => isVisual(state)
      ? context.cursor.extendToLine(line)
      : context.cursor.moveToLine(line)];
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
          (context) =>
            context.text.deleteToLineStartMotion(pending.operatorCount),
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
        (context) => pending.operator === "change"
          ? context.text.changeLines(count)
          : context.text.deleteLines(count),
      );
    }
    if (key.character === "w") {
      return completeOperator(
        state,
        pending.operator,
        (context) => pending.operator === "change"
          ? context.text.changeWordMotion(count)
          : context.text.deleteWordMotion(count),
      );
    }
    if (key.character === "e") {
      return completeOperator(
        state,
        pending.operator,
        (context) => context.text.deleteWordEndMotion(count),
      );
    }
    if (key.character === "$") {
      return completeOperator(
        state,
        pending.operator,
        (context) => context.text.deleteToLineEndMotion(count),
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
    return [(context) => context.text.insert(key.character!)];
  }
  if (key.code === "enter" && isPlain(key) || isCtrl(key, "j") || isCtrl(key, "m")) {
    return [(context) => context.text.insert("\n")];
  }
  if (key.code === "backspace" && isPlain(key) || isCtrl(key, "h")) {
    return [(context) => context.text.deleteBackward()];
  }
  if (isCtrl(key, "w")) return [(context) => context.text.deleteWordBackward()];
  if (isCtrl(key, "u")) return [(context) => context.text.deleteToLineStart()];
  if (isCtrl(key, "k")) return [(context) => context.text.deleteToLineEnd()];
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
  if (["d", "x", "D", "X"].includes(key.character)) {
    const linewise = state.state === "visual-line";
    setEditorState(state, "normal");
    return [(context) => linewise
      ? context.text.deleteSelectedLines()
      : context.text.deleteSelectionInclusive()];
  }
  if (key.character === "c" || key.character === "s") {
    const linewise = state.state === "visual-line";
    setEditorState(state, "insert");
    return [
      (context) => context.history.begin(),
      (context) => linewise
        ? context.text.deleteSelectedLines()
        : context.text.deleteSelectionInclusive(),
    ];
  }
  if (key.character >= "1" && key.character <= "9") {
    state.pending = { kind: "count", count: Number(key.character) };
    return [];
  }
  return null;
}

function handleNormal(state: VimViewState, key: KeyInput): Effect[] | null {
  if (key.code === "escape" && isPlain(key)) return [];
  if (isCtrl(key, "r")) return [(context) => context.history.redo()];
  if (isCtrl(key, "u")) return [(context) => context.viewport.halfPageUp()];
  if (isCtrl(key, "d")) return [(context) => context.viewport.halfPageDown()];
  if (isCtrl(key, "b")) return [(context) => context.viewport.fullPageUp()];
  if (isCtrl(key, "f")) return [(context) => context.viewport.fullPageDown()];
  if (key.code !== "character" || !isPlain(key) || !key.character) return null;

  const motion = handleMotion(state, key.character);
  if (motion) return motion;
  if (key.character >= "1" && key.character <= "9") {
    state.pending = { kind: "count", count: Number(key.character) };
    return [];
  }
  if (key.character === "u") return [(context) => context.history.undo()];
  if (key.character === "x") return [(context) => context.text.deleteForward()];
  if (key.character === "X") return [(context) => context.text.deleteBackward()];
  if (key.character === "J") return [(context) => context.text.joinLines()];
  if (key.character === "D") return [(context) => context.text.deleteToLineEnd()];
  if (key.character === "~") return [(context) => context.text.toggleCase()];
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
      (context) => below
        ? context.text.insertLineBelow()
        : context.text.insertLineAbove(),
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
    setEditorState(state, "insert");
    return [
      (context) => context.history.begin(),
      (context) => context.text.deleteForward(),
    ];
  }
  if (key.character === "C") {
    setEditorState(state, "insert");
    return [
      (context) => context.history.begin(),
      (context) => context.text.deleteToLineEnd(),
    ];
  }
  if (key.character === "S") {
    setEditorState(state, "insert");
    return [
      (context) => context.history.begin(),
      (context) => context.text.deleteLineContent(),
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
  if (key.character === "d" || key.character === "c") {
    state.pending = {
      kind: "operator",
      operator: key.character === "d" ? "delete" : "change",
      operatorCount: takeCount(state),
    };
    return [];
  }
  return null;
}

editor.modes.define({
  name: "vim",
  view: {
    create: (): VimViewState => ({
      state: "normal",
      pending: null,
      viewPolicy: {
        cursorStyle: "block",
        cursorDomain: "character",
        selectionShape: "character",
      },
    }),
  },
  input: "input",
  actions: {
    input(context) {
      const state = context.viewState as VimViewState;
      const key = context.arguments as KeyInput;
      const pending = handlePending(state, key);
      const effects = pending ?? (state.state === "insert"
        ? handleInsert(state, key)
        : isVisual(state)
        ? handleVisual(state, key)
        : handleNormal(state, key));
      if (effects === null) return context.forward();
      for (const effect of effects) effect(context as VimContext);
      return context.handled();
    },
  },
});
