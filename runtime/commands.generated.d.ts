interface EditorCommands {
	readonly newBuffer: () => number;
	readonly switchBuffer: (contentId: number) => void;
	readonly save: () => void;
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
}

declare const newBuffer: () => number;
declare const switchBuffer: (contentId: number) => void;
declare const save: () => void;
declare const undo: () => void;
declare const redo: () => void;
declare const quit: () => void;
declare const forceQuit: () => void;
declare const closePane: () => void;
declare const splitHorizontal: () => void;
declare const splitVertical: () => void;
declare const focusLeft: () => void;
declare const focusDown: () => void;
declare const focusUp: () => void;
declare const focusRight: () => void;
