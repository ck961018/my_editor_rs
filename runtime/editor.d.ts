/// <reference path="./commands.generated.d.ts" />

type ScriptData =
	| null
	| boolean
	| number
	| string
	| ScriptData[]
	| { [key: string]: ScriptData };

interface MessageEvent<T = unknown> {
	readonly type: "message";
	readonly data: T;
}

interface ErrorEvent {
	readonly type: "error";
	readonly message: string;
	readonly name: string;
	readonly filename: string;
	readonly lineno: number;
	readonly colno: number;
}

interface WorkerEventMap {
	message: MessageEvent<unknown>;
	error: ErrorEvent;
}

interface WorkerOptions {
	type?: "module";
	signal?: AbortSignal;
}

interface Worker {
	onmessage: ((this: Worker, event: MessageEvent<any>) => void) | null;
	onerror: ((this: Worker, event: ErrorEvent) => void) | null;
	postMessage(message: unknown): void;
	terminate(): void;
	addEventListener<K extends keyof WorkerEventMap>(
		type: K,
		listener: (this: Worker, event: WorkerEventMap[K]) => void,
	): void;
	removeEventListener<K extends keyof WorkerEventMap>(
		type: K,
		listener: (this: Worker, event: WorkerEventMap[K]) => void,
	): void;
}

declare const Worker: {
	new (url: URL, options?: WorkerOptions): Worker;
};

interface WorkerGlobalScope {
	onmessage: ((event: MessageEvent<any>) => void) | null;
	postMessage(message: unknown): void;
	close(): void;
}

declare const self: WorkerGlobalScope;

interface AbortSignal {
	readonly aborted: boolean;
}

interface AbortController {
	readonly signal: AbortSignal;
	abort(): void;
}

declare const AbortController: {
	new (): AbortController;
};

interface URL {
	readonly href: string;
	readonly pathname: string;
	toString(): string;
}

declare const URL: {
	new (url: string | URL, base?: string | URL): URL;
};

interface ImportMeta {
	readonly url: string;
}

type DeepReadonly<T> = T extends readonly (infer Item)[]
	? readonly DeepReadonly<Item>[]
	: T extends object
		? { readonly [Key in keyof T]: DeepReadonly<T[Key]> }
		: T;

interface EditorPosition {
	line: number;
	character: number;
}

interface EditorSelection {
	anchor: EditorPosition;
	head: EditorPosition;
}

interface EditorRange {
	start: EditorPosition;
	end: EditorPosition;
}

interface ContentEdit {
	range: EditorRange;
	text: string;
}

interface IndentationDecision {
	indent: string;
	closingIndent?: string;
}

interface LineCommentStrategy {
	delimiter: string;
}

interface BlockCommentStrategy {
	open: string;
	close: string;
}

interface OpenClosePair {
	open: string;
	close: string;
}

interface EditorIndentationConfig {
	indentWidth: number;
	insertSpaces: boolean;
}

type EditorSearchPattern =
	| { kind: "literal"; value: string }
	| { kind: "regex"; value: string };

interface EditorSearchOptions {
	caseSensitive?: boolean;
	direction?: "forward" | "backward";
	wrap?: boolean;
}

interface EditorReplaceAllOptions {
	caseSensitive?: boolean;
}

interface TextDecorationSpan {
	range: EditorRange;
	face: string;
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
		| "tab"
		| "backtab"
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
	tabWidth?: number;
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
	insertNewline(decision: IndentationDecision): void;
	toggleLineComment(strategy: LineCommentStrategy): void;
	toggleBlockComment(strategy: BlockCommentStrategy): void;
	insertPair(pair: OpenClosePair): void;
	insertClosingPair(pair: OpenClosePair): void;
	deletePairBackward(pair: OpenClosePair): void;
	indentLines(config: EditorIndentationConfig): void;
	outdentLines(config: EditorIndentationConfig): void;
	duplicateLines(): void;
	moveLinesUp(): void;
	moveLinesDown(): void;
	applyEdits(edits: ContentEdit[]): void;
}

type EditorClipboardKind = "character" | "line";
type EditorClipboardEndpoint = "internal" | "system";
type EditorClipboardEdit =
	| "delete-forward"
	| "delete-backward"
	| "word"
	| "word-end"
	| "change-word"
	| "line-start"
	| "line-end"
	| "lines"
	| "change-lines"
	| "line-content"
	| "selection-inclusive"
	| "selected-lines";

interface ClipboardPrimitives {
	copy(kind: EditorClipboardKind, destination?: EditorClipboardEndpoint): void;
	copyForEdit(
		kind: EditorClipboardKind,
		edit: EditorClipboardEdit,
		count?: number,
		destination?: EditorClipboardEndpoint,
	): void;
	cut(kind: EditorClipboardKind, destination?: EditorClipboardEndpoint): void;
	paste(source?: EditorClipboardEndpoint, placement?: "before" | "after"): void;
}

interface HistoryPrimitives {
	begin(): void;
	commit(): void;
	rollback(): void;
	undo(): void;
	redo(): void;
}

interface SearchPrimitives {
	find(pattern: EditorSearchPattern, options?: EditorSearchOptions): void;
	replaceNext(
		pattern: EditorSearchPattern,
		replacement: string,
		options?: EditorSearchOptions,
	): void;
	replaceAll(
		pattern: EditorSearchPattern,
		replacement: string,
		options?: EditorReplaceAllOptions,
	): void;
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
	executeLine(source: string): void;
}

type EditorCommand = (...arguments: any[]) => unknown;

interface EditorCommands {
	register<Command extends EditorCommand>(callback: Command): Command;
	register<Command extends EditorCommand>(
		id: string,
		callback: Command,
	): Command;
	shortcut(name: string, callback: (tail?: string) => unknown): void;
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
	quit(): void;
	closePane(): void;
	splitHorizontal(): void;
	splitVertical(): void;
	focusLeft(): void;
	focusDown(): void;
	focusUp(): void;
	focusRight(): void;
}

interface ContentPrimitives {
	create(): void;
	open(path: string): void;
	list(): void;
	close(contentId?: number, force?: boolean): void;
	save(contentId?: number, force?: boolean): void;
	saveAs(path: string, force?: boolean): void;
	reload(contentId?: number, force?: boolean): void;
}

type BufferViewSpec =
	| {
		readonly type: "core.buffer";
		readonly content: number;
		readonly create?: never;
		readonly path?: never;
	}
	| {
		readonly type: "core.buffer";
		readonly content?: never;
		readonly create: true;
		readonly path?: never;
	}
	| {
		readonly type: "core.buffer";
		readonly content?: never;
		readonly create?: never;
		readonly path: string;
	};

type DiffViewSpec = {
	readonly type: "core.diff";
	readonly left: number;
	readonly right: number;
};

type DefinedViewSpec = {
	readonly type: "defined";
	readonly definition: string;
	readonly bindings: Readonly<Record<string, number>>;
};

type ViewSpec = BufferViewSpec | DiffViewSpec | DefinedViewSpec;

interface ViewPrimitives {
	focus(viewId: number): void;
	switch(spec: ViewSpec): void;
}

interface BufferContentContext {
	readonly contentId: number;
	readonly revision?: number;
	readonly text?: string;
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

interface BufferCommandContext<ContentState, ViewState, Arguments = ScriptData>
	extends BufferContentContext {
	readonly viewId: number;
	readonly text: string;
	readonly selections: readonly EditorSelection[];
	readonly primarySelection: EditorSelection;
	readonly arguments: Arguments;
	readonly cursor: CursorPrimitives;
	readonly edit: TextPrimitives;
	readonly search: SearchPrimitives;
	readonly clipboard: ClipboardPrimitives;
	readonly history: HistoryPrimitives;
	readonly viewport: ViewportPrimitives;
	readonly commands: CommandPrimitives;
	readonly faces: FacePrimitives;
	readonly app: AppPrimitives;
	readonly content: ContentPrimitives;
	readonly view: ViewPrimitives;
	state: ContentState;
	viewState: ViewState;
	pass(): Pass;
}

interface BufferAdapterDefinition<ContentState, ViewState> {
	state?(context: BufferContentContext): ContentState;
	viewState?(state: Readonly<ContentState>): ViewState & {
		viewPolicy?: ViewPolicy;
	};
	commands?: Record<
		string,
		(context: BufferCommandContext<ContentState, ViewState>) => void | Pass
	>;
	keys?: Record<string, string>;
	input?(
		context: BufferCommandContext<ContentState, ViewState, EditorKeyEvent>,
	): void | Pass;
	changed?(
		context: BufferContentContext & {
			readonly change: ContentChange[];
			state: ContentState;
		},
	): void;
}

type ModeAttachmentDefinition = {
	readonly view: string;
} & (
	| {
		readonly binding: string;
		readonly languages?: readonly string[];
	}
	| {
		readonly binding?: never;
		readonly languages?: never;
	}
);

interface ModeDefinition<
	BufferState = ScriptData,
	BufferViewState = ScriptData,
> {
	name: string;
	before?: string;
	attach?: ModeAttachmentDefinition;
	faces?: Record<string, EditorModeFace>;
	on: {
		buffer?: BufferAdapterDefinition<BufferState, BufferViewState>;
	};
}

interface ContentChange {
	readonly startCharacter: number;
	readonly endCharacter: number;
	readonly text: string;
}

interface ViewExtensionSelection {
	readonly anchor: Readonly<EditorPosition>;
	readonly head: Readonly<EditorPosition>;
}

interface ViewExtensionDocument {
	readonly contentId: number;
	readonly revision: number;
	readonly text: string;
	readonly resourceName: string | null;
	readonly selections: readonly ViewExtensionSelection[];
	readonly primarySelection: ViewExtensionSelection;
}

interface ViewExtensionContext {
	readonly viewId: number;
	readonly definition: string;
	readonly revision: number;
	readonly bindings: readonly {
		readonly name: string;
		readonly contentId: number;
	}[];
	readonly document: ViewExtensionDocument | null;
}

interface LinesSegment {
	readonly text: string;
	readonly face?: string;
}

interface LinesPresentation {
	readonly type: "lines";
	readonly baseFace?: string;
	readonly rows: readonly (string | readonly LinesSegment[])[];
}

interface ViewExtensionDefinition {
	readonly id: string;
	readonly panes: Readonly<
		Record<
			string,
			{
				readonly side: "left" | "right" | "above" | "below";
				readonly size: number;
				readonly render: (
					context: DeepReadonly<ViewExtensionContext>,
				) => LinesPresentation;
			}
		>
	>;
}

interface CompoundViewChildDefinition {
	readonly key: string;
	readonly view: "core.buffer";
	readonly bindings: {
		readonly document: string;
	};
}

interface CompoundViewDefinition {
	readonly name: string;
	readonly bindings: readonly string[];
	readonly layout: {
		readonly direction: "horizontal" | "vertical";
		readonly children: readonly [
			CompoundViewChildDefinition,
			CompoundViewChildDefinition,
		];
	};
}

declare const editor: {
	readonly commands: EditorCommands;
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
		define<BufferState = ScriptData, BufferViewState = ScriptData>(
			definition: ModeDefinition<BufferState, BufferViewState>,
		): void;
	};
	readonly views: {
		define(definition: CompoundViewDefinition): void;
		extend(target: string, definition: ViewExtensionDefinition): void;
	};
	readonly resources: {
		readText(path: string): string;
		readBinary(path: string): Uint8Array;
	};
	writeDecorations(
		contentId: number,
		revision: number,
		spans: TextDecorationSpan[],
	): void;
};
