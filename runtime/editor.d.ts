type ScriptData =
  | null
  | boolean
  | number
  | string
  | ScriptData[]
  | { [key: string]: ScriptData };

interface EditorPosition {
  line: number;
  character: number;
}

interface EditorRange {
  start: EditorPosition;
  end: EditorPosition;
}

interface ContentEdit {
  range: EditorRange;
  text: string;
}

interface TextDecorationSpan {
  range: EditorRange;
  face: string;
}

interface TextDecorationSnapshot {
  revision: number;
  spans: TextDecorationSpan[];
}

interface EditorFace {
  foreground?: number | `#${string}`;
  background?: number | `#${string}`;
  bold?: boolean;
  italic?: boolean;
  underline?: boolean;
}

interface EditorKeyEvent {
  code:
    | "character"
    | "arrow"
    | "backspace"
    | "enter"
    | "escape"
    | "function"
    | "unknown";
  character?: string;
  direction?: "up" | "down" | "left" | "right";
  number?: number;
  modifiers: {
    alt: boolean;
    ctrl: boolean;
    shift: boolean;
  };
}

interface ViewPolicy {
  cursorStyle?: "default" | "block" | "bar";
  cursorDomain?: "insertion-point" | "character";
  selectionShape?: "character" | "character-inclusive" | "line";
  selectionFace?: string;
}

interface ModeActionResult {
  continue?: boolean;
  contentDecorations?: TextDecorationSnapshot;
  viewDecorations?: TextDecorationSnapshot;
}

interface CursorPrimitives {
  moveLeft(count?: number): void;
  moveRight(count?: number): void;
  moveWithinLineLeft(count?: number): void;
  moveWithinLineRight(count?: number): void;
  moveUp(count?: number): void;
  moveDown(count?: number): void;
  moveToLine(line: number): void;
  moveToLinePreservingColumn(line: number): void;
  moveToCharForward(character: string, count?: number): void;
  moveToCharBackward(character: string, count?: number): void;
  extendLeft(count?: number): void;
  extendRight(count?: number): void;
  extendWithinLineLeft(count?: number): void;
  extendWithinLineRight(count?: number): void;
  extendUp(count?: number): void;
  extendDown(count?: number): void;
  extendToLine(line: number): void;
  extendToLinePreservingColumn(line: number): void;
  extendToCharForward(character: string, count?: number): void;
  extendToCharBackward(character: string, count?: number): void;
  moveWordForward(count?: number): void;
  moveWordBackward(count?: number): void;
  moveWordEnd(count?: number): void;
  extendWordForward(count?: number): void;
  extendWordBackward(count?: number): void;
  extendWordEnd(count?: number): void;
  moveToLineStart(): void;
  moveToFirstNonBlank(): void;
  moveToLineEnd(): void;
  moveToLastLine(): void;
  moveToPrevParagraph(count?: number): void;
  moveToNextParagraph(count?: number): void;
  extendToLineStart(): void;
  extendToFirstNonBlank(): void;
  extendToLineEnd(): void;
  extendToLastLine(): void;
  extendToPrevParagraph(count?: number): void;
  extendToNextParagraph(count?: number): void;
  moveAfterLineEnd(): void;
  collapseSelections(): void;
}

interface TextPrimitives {
  insert(text: string): void;
  deleteBackward(count?: number): void;
  deleteForward(count?: number): void;
  deleteWordBackward(): void;
  deleteToLineStart(): void;
  deleteToLineEnd(): void;
  joinLines(): void;
  toggleCase(): void;
  insertLineBelow(): void;
  insertLineAbove(): void;
  deleteLineContent(): void;
  deleteSelectionInclusive(): void;
  deleteSelectedLines(): void;
  deleteWordMotion(count?: number): void;
  deleteWordEndMotion(count?: number): void;
  changeWordMotion(count?: number): void;
  deleteToLineStartMotion(count?: number): void;
  deleteToLineEndMotion(count?: number): void;
  deleteLines(count?: number): void;
  changeLines(count?: number): void;
  applyEdits(edits: ContentEdit[]): void;
}

interface HistoryPrimitives {
  begin(): void;
  commit(): void;
  rollback(): void;
  undo(): void;
  redo(): void;
}

interface ViewportPrimitives {
  halfPageUp(extendSelection?: boolean): void;
  halfPageDown(extendSelection?: boolean): void;
  fullPageUp(extendSelection?: boolean): void;
  fullPageDown(extendSelection?: boolean): void;
  alignTop(): void;
  alignCenter(): void;
  alignBottom(): void;
}

interface ModePrimitives {
  invoke(mode: string, action: string, arguments?: ScriptData): void;
}

interface AppPrimitives {
  save(): void;
  quit(): void;
}

interface DocumentContext {
  readonly fileName?: string;
  readonly modified: boolean;
}

interface ContentChange {
  readonly startCharacter: number;
  readonly endCharacter: number;
  readonly text: string;
}

interface ContentContext<ContentState, Arguments = ScriptData> {
  readonly contentId: number;
  readonly revision?: number;
  readonly text?: string;
  readonly document?: DocumentContext;
  readonly change?: ContentChange[];
  readonly jobVersion?: number;
  readonly arguments?: Arguments;
  contentState: ContentState;
}

interface ModeContext<ContentState, ViewState, Arguments = ScriptData>
  extends ContentContext<ContentState, Arguments> {
  readonly contentId: number;
  readonly viewId: number;
  readonly arguments: Arguments;
  readonly cursor: CursorPrimitives;
  readonly text: TextPrimitives;
  readonly history: HistoryPrimitives;
  readonly viewport: ViewportPrimitives;
  readonly mode: ModePrimitives;
  readonly app: AppPrimitives;
  viewState: ViewState;
  handled(): false;
  forward(): true;
}

interface ContentJob {
  slot: string;
  version: number;
  includeText?: boolean;
  message: ScriptData;
}

interface ModeDefinition<
  ContentState,
  ViewState,
  WorkerResponse = ScriptData,
> {
  name: string;
  before?: string;
  worker?: string;
  faces?: Record<string, EditorFace>;
  content?: {
    create(
      context: Omit<ContentContext<never>, "contentState" | "arguments">,
    ): ContentState;
    changed?(context: ContentContext<ContentState>): void;
    job?(context: ContentContext<ContentState>): ContentJob | void;
    applyJob?(
      context: ContentContext<ContentState, WorkerResponse> & {
        readonly jobVersion: number;
        readonly arguments: WorkerResponse;
      },
    ): Pick<ModeActionResult, "contentDecorations"> | void;
  };
  view?: {
    create(contentState: ContentState): ViewState & { viewPolicy?: ViewPolicy };
  };
  input?: string;
  actions: Record<
    string,
    (
      context: ModeContext<ContentState, ViewState>,
    ) => ModeActionResult | boolean | void
  >;
  keys?: Record<string, string>;
}

declare const editor: {
  readonly modes: {
    define<ContentState, ViewState, WorkerResponse = ScriptData>(
      definition: ModeDefinition<ContentState, ViewState, WorkerResponse>,
    ): void;
  };
  readonly resources: {
    readText(path: string): string;
    readBinary(path: string): Uint8Array;
  };
  readonly worker: {
    onMessage<Message = ScriptData, Response = ScriptData>(
      callback: (message: Message) => Response | Promise<Response>,
    ): void;
  };
};
