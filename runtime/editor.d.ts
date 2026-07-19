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

interface ContentEditBatch {
  revision: number;
  edits: ContentEdit[];
}

interface ModeActionResult {
  flow?: "continue" | "stop";
  insertText?: string;
  contentEdits?: ContentEditBatch;
}

interface ModeContext<ContentState extends ScriptData, ViewState extends ScriptData> {
  readonly contentId: number;
  readonly viewId: number;
  readonly revision?: number;
  contentState: ContentState;
  viewState: ViewState;
}

interface ModeDefinition<
  ContentState extends ScriptData,
  ViewState extends ScriptData,
> {
  name: string;
  before?: string;
  content?: {
    create(): ContentState;
  };
  view?: {
    create(contentState: ContentState): ViewState;
  };
  actions: Record<
    string,
    (context: ModeContext<ContentState, ViewState>) => ModeActionResult | void
  >;
  keys?: Record<string, string>;
}

declare const editor: {
  readonly modes: {
    define<ContentState extends ScriptData, ViewState extends ScriptData>(
      definition: ModeDefinition<ContentState, ViewState>,
    ): void;
  };
};
