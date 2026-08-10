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
					void ctx.text;
					void ctx.selections[0]?.anchor.character;
					void ctx.primarySelection.head.line;
					// @ts-expect-error Buffer adapters do not expose StatusBar targets.
					void ctx.targetContentId;
					ctx.edit.insertPair({ open: '"', close: '"' });
					ctx.edit.insertClosingPair({ open: '"', close: '"' });
					ctx.edit.deletePairBackward({ open: '"', close: '"' });
					ctx.edit.insertNewline({ indent: "  ", closingIndent: "" });
					ctx.edit.toggleLineComment({ delimiter: "//" });
					ctx.edit.toggleBlockComment({ open: "/*", close: "*/" });
					ctx.edit.indentLines({ indentWidth: 2, insertSpaces: true });
					ctx.edit.outdentLines({ indentWidth: 2, insertSpaces: true });
					ctx.edit.duplicateLines();
					ctx.edit.moveLinesUp();
					ctx.edit.moveLinesDown();
					ctx.clipboard.copy("character");
					ctx.clipboard.copyForEdit("character", "word", 2);
					ctx.clipboard.cut("line", "system");
					ctx.clipboard.paste("internal", "after");
					ctx.search.find(
						{ kind: "literal", value: "needle" },
						{ caseSensitive: false, direction: "forward", wrap: true },
					);
					ctx.search.replaceNext({ kind: "regex", value: "(needle)" }, "$1");
					ctx.search.replaceAll(
						{ kind: "literal", value: "needle" },
						"replacement",
					);
					ctx.content.create();
					ctx.content.open("file.rs");
					ctx.content.list();
					ctx.content.close(undefined, true);
					ctx.content.save();
					ctx.content.saveAs("other.rs", true);
					ctx.content.reload();
					ctx.view.focus(ctx.viewId);
					ctx.view.switch({ type: "core.buffer", content: 1 });
					ctx.view.switch({ type: "core.buffer", create: true });
					ctx.view.switch({ type: "core.buffer", path: "file.rs" });
					// @ts-expect-error Buffer command primitives were removed.
					ctx.buffers.list();
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

editor.modes.define({
	name: "invalid-return",
	on: {
		buffer: {
			commands: {
				// @ts-expect-error Commands return only void or ctx.pass().
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
