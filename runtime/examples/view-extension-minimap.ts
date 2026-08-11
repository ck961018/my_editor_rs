editor.views.extend("core.buffer", {
	id: "example.minimap",
	panes: {
		minimap: {
			side: "right",
			size: 8,
			render(context) {
				const document = context.document;
				if (document === null) {
					return { type: "lines", rows: [] };
				}

				const cursorLine = document.primarySelection.head.line;
				const rows: (string | LinesSegment[])[] = document.text
					.split("\n")
					.map((line, index) => {
						const density = Math.min(
							8,
							Math.ceil(line.trim().length / 10),
						);
						const text = "▏".repeat(density);
						return index === cursorLine
							? [{ text, face: "ui.selection" }]
							: text;
					});
				return {
					type: "lines",
					baseFace: "ui.editor",
					rows,
				};
			},
		},
	},
});
