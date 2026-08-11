interface EditorCommandSeeds {
	readonly "content.create": () => number;
	readonly "content.open": (path: string) => void;
	readonly "content.list": () => void;
	readonly "content.close": (contentId?: number, force?: boolean) => void;
	readonly "content.save": (
		contentId?: number,
		force?: boolean,
	) => Promise<void>;
	readonly "content.saveAs": (path: string, force?: boolean) => void;
	readonly "content.reload": (contentId?: number, force?: boolean) => void;
	readonly "view.focus": (viewId: number) => void;
	readonly "view.switch": (spec: ViewSpec) => void;
	readonly "diff.setRightContent": (contentId: number) => void;
	readonly undo: () => void;
	readonly redo: () => void;
	readonly quit: () => void;
	readonly forceQuit: () => void;
	readonly closePane: () => void;
	readonly splitHorizontal: () => void;
	readonly splitVertical: () => void;
	readonly focusLeft: () => void;
	readonly focusDown: () => void;
	readonly focusUp: () => void;
	readonly focusRight: () => void;
	readonly invokeMode: (
		command: `${string}.${string}`,
		arguments?: ScriptData,
	) => void;
}

interface EditorCommands {
	readonly content: {
		readonly create: EditorCommandSeeds["content.create"];
		readonly open: EditorCommandSeeds["content.open"];
		readonly list: EditorCommandSeeds["content.list"];
		readonly close: EditorCommandSeeds["content.close"];
		readonly save: EditorCommandSeeds["content.save"];
		readonly saveAs: EditorCommandSeeds["content.saveAs"];
		readonly reload: EditorCommandSeeds["content.reload"];
	};
	readonly view: {
		readonly focus: EditorCommandSeeds["view.focus"];
		readonly switch: EditorCommandSeeds["view.switch"];
	};
	readonly diff: {
		readonly setRightContent: EditorCommandSeeds["diff.setRightContent"];
	};
	readonly undo: EditorCommandSeeds["undo"];
	readonly redo: EditorCommandSeeds["redo"];
	readonly quit: EditorCommandSeeds["quit"];
	readonly forceQuit: EditorCommandSeeds["forceQuit"];
	readonly closePane: EditorCommandSeeds["closePane"];
	readonly splitHorizontal: EditorCommandSeeds["splitHorizontal"];
	readonly splitVertical: EditorCommandSeeds["splitVertical"];
	readonly focusLeft: EditorCommandSeeds["focusLeft"];
	readonly focusDown: EditorCommandSeeds["focusDown"];
	readonly focusUp: EditorCommandSeeds["focusUp"];
	readonly focusRight: EditorCommandSeeds["focusRight"];
	readonly invokeMode: EditorCommandSeeds["invokeMode"];
}

declare const content: EditorCommands["content"];
declare const view: EditorCommands["view"];
declare const diff: EditorCommands["diff"];
declare const undo: EditorCommands["undo"];
declare const redo: EditorCommands["redo"];
declare const quit: EditorCommands["quit"];
declare const forceQuit: EditorCommands["forceQuit"];
declare const closePane: EditorCommands["closePane"];
declare const splitHorizontal: EditorCommands["splitHorizontal"];
declare const splitVertical: EditorCommands["splitVertical"];
declare const focusLeft: EditorCommands["focusLeft"];
declare const focusDown: EditorCommands["focusDown"];
declare const focusUp: EditorCommands["focusUp"];
declare const focusRight: EditorCommands["focusRight"];
declare const invokeMode: EditorCommands["invokeMode"];
