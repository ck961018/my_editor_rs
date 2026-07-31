/// <reference path="../editor.d.ts" />

editor.modes.define({
	name: "pairs",
	faces: {
		"plugin.pairs.match": {
			inherits: ["syntax.string"],
			fallback: {
				bold: true,
				dim: false,
				underlineStyle: "curl",
				strikethrough: false,
			},
		},
	},
	on: {
		buffer: {
			state: () => ({ enabled: true }),
			viewState: () => ({
				insertedPairs: 0,
				viewPolicy: { tabWidth: 4 },
			}),
			commands: {
				quote(ctx) {
					if (!ctx.state.enabled) return ctx.pass();
					// @ts-expect-error Buffer adapters do not expose StatusBar targets.
					void ctx.targetContentId;
					ctx.edit.insert('""');
					ctx.cursor.moveLeft();
					const token = ctx.faces.addRelative(
						"plugin.pairs.match",
						["syntax.string", { underline: true }],
						"view",
					);
					void token;
					ctx.faces.setBase("plugin.pairs.match", null, "content");
					ctx.viewState.insertedPairs++;
				},
			},
			keys: { '"': "quote" },
			input(ctx) {
				if (ctx.arguments.code === "tab" || ctx.arguments.code === "backtab") {
					void ctx.arguments.modifiers.shift;
				}
				// @ts-expect-error Raw input uses the closed EditorKeyEvent shape.
				void ctx.arguments.missing;
				return ctx.pass();
			},
		},
		statusBar: {
			state: () => ({ updates: 0 }),
			viewState: () => null,
			commands: {
				update(ctx) {
					ctx.state.updates++;
					void ctx.targetContentId;
					void ctx.resourceName;
					void ctx.resourcePath;
					void ctx.backingState;
					void ctx.dirty;
					void ctx.saveState;
					void ctx.textMetrics;
					ctx.faces.addRelative("ui.status-bar", [{ bold: true }], "view");
					// @ts-expect-error StatusBar adapters cannot edit Buffer text.
					ctx.edit.insert("forbidden");
					// @ts-expect-error StatusBar adapters do not expose a cursor.
					ctx.cursor.moveLeft();
					// @ts-expect-error StatusBar adapters do not expose history.
					ctx.history.undo();
					// @ts-expect-error StatusBar adapters do not expose a viewport.
					ctx.viewport.scroll(1);
					// @ts-expect-error StatusBar adapters do not expose app commands.
					ctx.app.quit();
				},
			},
		},
	},
});

editor.modes.define<{
	count: number;
	nested: { language: string };
	items: string[];
}>({
	name: "typed-state",
	on: {
		buffer: {
			state: () => ({
				count: 0,
				nested: { language: "rust" },
				items: [],
			}),
		},
	},
});

// @ts-expect-error V2 commands return only void or ctx.pass().
editor.modes.define({
	name: "invalid-return",
	on: {
		buffer: {
			commands: {
				invalidReturn() {
					return true;
				},
			},
		},
	},
});

editor.theme.use("catppuccin-mocha");
editor.faces.override("syntax.comment", { italic: false });
editor.faces.override("diagnostic.error", {
	underlineStyle: "curl",
	strikethrough: true,
});
editor.faces.override(
	"ui.editor",
	{ foreground: { reset: true } },
	{ theme: "catppuccin-latte" },
);

// --- Standard Web Worker contract tests ---

const worker: Worker = new Worker(new URL("./parser.ts", import.meta.url), {
	type: "module",
});
worker.postMessage({ text: "" });
const messageListener = (e: MessageEvent) => {
	void e.data;
};
worker.addEventListener("message", messageListener);
worker.removeEventListener("message", messageListener);
worker.onerror = (event) => {
	void event.message;
};
worker.terminate();
// @ts-expect-error vell does not expose unsupported EventTarget events.
worker.addEventListener("messageerror", () => {});
// @ts-expect-error vell exposes only the implemented Worker subset.
worker.dispatchEvent({ type: "message" });

const controller: AbortController = new AbortController();
new Worker(new URL("./x.ts", import.meta.url), {
	type: "module",
	signal: controller.signal,
});
controller.abort();

// --- editor.writeDecorations frame-safe sink ---

editor.writeDecorations(1, 1, [
	{
		range: {
			start: { line: 0, character: 0 },
			end: { line: 0, character: 1 },
		},
		face: "syntax.keyword",
	},
]);

// @ts-expect-error writeDecorations requires content, revision, and spans.
editor.writeDecorations();
// @ts-expect-error writeDecorations spans must be TextDecorationSpan[].
editor.writeDecorations(1, 1, "not-spans");
