type ScriptData =
  | null
  | boolean
  | number
  | string
  | ScriptData[]
  | { [key: string]: ScriptData };

type DeepReadonly<T> = T extends readonly (infer Item)[]
  ? readonly DeepReadonly<Item>[]
  : T extends object
    ? { readonly [Key in keyof T]: DeepReadonly<T[Key]> }
    : T;

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
  dim?: boolean;
  italic?: boolean;
  underline?: boolean;
  underlineStyle?: "line" | "double" | "curl" | "dotted" | "dashed";
  strikethrough?: boolean;
}

interface EditorFaceReset {
  readonly reset: true;
}

interface EditorFacePatch {
  foreground?: number | `#${string}` | EditorFaceReset;
  background?: number | `#${string}` | EditorFaceReset;
  bold?: boolean | EditorFaceReset;
  dim?: boolean | EditorFaceReset;
  italic?: boolean | EditorFaceReset;
  underline?: boolean | EditorFaceReset;
  underlineStyle?:
    | "line"
    | "double"
    | "curl"
    | "dotted"
    | "dashed"
    | EditorFaceReset;
  strikethrough?: boolean | EditorFaceReset;
}

interface EditorFaceDefinition {
  inherits?: string[];
  fallback?: EditorFacePatch;
}

type EditorModeFace = EditorFace | EditorFaceDefinition;

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
  statusBar?: StatusBarPresentation;
}

interface StatusBarSegment {
  text: string;
  face?: string;
}

interface StatusBarPresentation {
  left?: StatusBarSegment[];
  center?: StatusBarSegment[];
  right?: StatusBarSegment[];
}

interface ModeActionResult {
  continue?: boolean;
  contentDecorations?: TextDecorationSnapshot;
  viewDecorations?: TextDecorationSnapshot;
}

declare const editorPass: unique symbol;

interface Pass {
  readonly [editorPass]: true;
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

interface CommandPrimitives {
  invoke(command: `${string}.${string}`, arguments?: ScriptData): void;
}

type FaceRemapScope = "session" | "content" | "view";
type EditorFaceExpression = string | EditorFacePatch;

interface FacePrimitives {
  setBase(
    face: string,
    expressions: readonly EditorFaceExpression[] | null,
    scope?: FaceRemapScope,
  ): void;
  addRelative(
    face: string,
    expressions: readonly EditorFaceExpression[],
    scope?: FaceRemapScope,
  ): number;
  removeRelative(token: number): void;
}

interface AppPrimitives {
  save(): void;
  quit(): void;
  closePane(): void;
  splitHorizontal(): void;
  splitVertical(): void;
  focusLeft(): void;
  focusDown(): void;
  focusUp(): void;
  focusRight(): void;
}

interface BufferContentContext {
  readonly contentId: number;
  readonly revision?: number;
  readonly resourceName?: string;
  readonly resourcePath?: string;
  readonly backingState?: "untitled" | "unmaterialized" | "materialized";
  readonly dirty?: boolean;
  readonly saveState?: "idle" | "saved" | "failed";
  readonly textMetrics?: {
    readonly lineCount: number;
    readonly characterCount: number;
  };
}

interface StatusBarContentContext {
  readonly contentId: number;
  readonly revision?: number;
}

interface BufferCommandContext<ContentState, ViewState, Arguments = ScriptData>
  extends BufferContentContext {
  readonly viewId: number;
  readonly arguments: Arguments;
  readonly cursor: CursorPrimitives;
  readonly edit: TextPrimitives;
  readonly history: HistoryPrimitives;
  readonly viewport: ViewportPrimitives;
  readonly commands: CommandPrimitives;
  readonly faces: FacePrimitives;
  readonly app: AppPrimitives;
  state: ContentState;
  viewState: ViewState;
  pass(): Pass;
}

interface StatusBarCommandContext<
  ContentState,
  ViewState,
  Arguments = ScriptData,
> extends StatusBarContentContext {
  readonly viewId: number;
  readonly targetViewId: number;
  readonly targetContentId: number;
  readonly resourceName?: string;
  readonly resourcePath?: string;
  readonly backingState?: "untitled" | "unmaterialized" | "materialized";
  readonly dirty?: boolean;
  readonly saveState?: "idle" | "saved" | "failed";
  readonly textMetrics?: {
    readonly lineCount: number;
    readonly characterCount: number;
  };
  readonly arguments: Arguments;
  readonly commands: CommandPrimitives;
  readonly faces: FacePrimitives;
  state: ContentState;
  viewState: ViewState;
  pass(): Pass;
}

interface BufferAdapterDefinition<
  ContentState,
  ViewState,
  WorkerResponse = ScriptData,
> {
  state?(context: BufferContentContext): ContentState;
  viewState?(state: Readonly<ContentState>): ViewState & {
    viewPolicy?: ViewPolicy;
  };
  commands?: Record<
    string,
    (
      context: BufferCommandContext<ContentState, ViewState>,
    ) => void | Pass
  >;
  keys?: Record<string, string>;
  input?(
    context: BufferCommandContext<
      ContentState,
      ViewState,
      EditorKeyEvent
    >,
  ): void | Pass;
  changed?(
    context: BufferContentContext & {
      readonly change: ContentChange[];
      state: ContentState;
    },
  ): void;
  analysis?: Record<
    string,
    BackgroundAnalysisDefinition<ContentState, WorkerResponse>
  >;
}

type BackgroundAnalysisInputContext<ContentState> = Omit<
  BufferContentContext,
  "revision"
> & {
  readonly revision: number;
  readonly state: DeepReadonly<ContentState>;
};

type BackgroundAnalysisApplyContext<ContentState, WorkerResponse> = Omit<
  BufferContentContext,
  "revision"
> & {
  readonly revision: number;
  readonly arguments: WorkerResponse;
  state: ContentState;
};

interface BackgroundAnalysisBase<ContentState, WorkerResponse> {
  worker: string;
  apply(
    context: BackgroundAnalysisApplyContext<ContentState, WorkerResponse>,
  ): Pick<ModeActionResult, "contentDecorations"> | void;
}

type TextSnapshotAnalysisMessage = Record<string, ScriptData> & {
  readonly text?: never;
};

type BackgroundAnalysisDefinition<ContentState, WorkerResponse> =
  | (BackgroundAnalysisBase<ContentState, WorkerResponse> & {
      snapshot: "text";
      input(
        context: BackgroundAnalysisInputContext<ContentState>,
      ): TextSnapshotAnalysisMessage | void;
    })
  | (BackgroundAnalysisBase<ContentState, WorkerResponse> & {
      snapshot?: never;
      input(
        context: BackgroundAnalysisInputContext<ContentState>,
      ): ScriptData | void;
    });

interface StatusBarAdapterDefinition<ContentState, ViewState> {
  state?(context: StatusBarContentContext): ContentState;
  viewState?(state: Readonly<ContentState>): ViewState;
  commands?: Record<
    string,
    (
      context: StatusBarCommandContext<ContentState, ViewState>,
    ) => void | Pass
  >;
  keys?: Record<string, string>;
  input?(
    context: StatusBarCommandContext<
      ContentState,
      ViewState,
      EditorKeyEvent
    >,
  ): void | Pass;
}

interface ModeDefinitionV2<
  BufferState = ScriptData,
  BufferViewState = ScriptData,
  BufferWorkerResponse = ScriptData,
  StatusBarState = ScriptData,
  StatusBarViewState = ScriptData,
> {
  name: string;
  before?: string;
  faces?: Record<string, EditorModeFace>;
  on: {
    buffer?: BufferAdapterDefinition<
      BufferState,
      BufferViewState,
      BufferWorkerResponse
    >;
    statusBar?: StatusBarAdapterDefinition<
      StatusBarState,
      StatusBarViewState
    >;
  };
}

interface ContentChange {
  readonly startCharacter: number;
  readonly endCharacter: number;
  readonly text: string;
}

/** @deprecated Removed in Vell 0.3.0 with the v1 Mode API. */
interface DocumentContext {
  readonly fileName?: string;
  readonly modified: boolean;
}

interface ContentContext<ContentState, Arguments = ScriptData> {
  readonly contentId: number;
  readonly revision?: number;
  readonly text?: string;
  /** @deprecated Use resourceName and dirty on v2 Buffer contexts. */
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

/** @deprecated Removed in Vell 0.3.0. Use ModeDefinitionV2. */
interface ModeDefinition<
  ContentState,
  ViewState,
  WorkerResponse = ScriptData,
> {
  name: string;
  before?: string;
  worker?: string;
  faces?: Record<string, EditorModeFace>;
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
  readonly theme: {
    use(name: string): void;
  };
  readonly faces: {
    override(
      name: string,
      patch: EditorFacePatch,
      options?: { readonly theme?: string },
    ): void;
  };
  readonly modes: {
    define<
      BufferState = ScriptData,
      BufferViewState = ScriptData,
      BufferWorkerResponse = ScriptData,
      StatusBarState = ScriptData,
      StatusBarViewState = ScriptData,
    >(
      definition: ModeDefinitionV2<
        BufferState,
        BufferViewState,
        BufferWorkerResponse,
        StatusBarState,
        StatusBarViewState
      >,
    ): void;
    /** @deprecated Removed in Vell 0.3.0. Use the `on` adapter schema. */
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
