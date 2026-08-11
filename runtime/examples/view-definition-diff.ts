editor.views.define({
	name: "example.diff",
	bindings: ["left", "right"],
	layout: {
		direction: "horizontal",
		children: [
			{
				key: "before",
				view: "core.buffer",
				bindings: { document: "left" },
			},
			{
				key: "after",
				view: "core.buffer",
				bindings: { document: "right" },
			},
		],
	},
});
