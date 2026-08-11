/// <reference path="../editor.d.ts" />

function localTypedCommand(value: number, suffix?: string): string {
	return `${value}${suffix ?? ""}`;
}

const registeredLocal = editor.commands.register(localTypedCommand);
const registeredExplicit = editor.commands.register(
	"typed.explicit",
	(value: string): number => value.length,
);

const localResult: string = registeredLocal(42);
const explicitResult: number = registeredExplicit("vell");
const contentId: number = content.create();
view.switch({ type: "core.buffer", content: contentId });
view.switch({ type: "core.diff", left: contentId, right: contentId });
diff.setRightContent(contentId);
view.focus(0);
const saving: Promise<void> = content.save();

// @ts-expect-error A ViewSpec has exactly one source.
view.switch({ type: "core.buffer", content: contentId, create: true });
// @ts-expect-error A DiffView requires both bindings.
view.switch({ type: "core.diff", left: contentId });

// @ts-expect-error Buffer lifecycle commands were removed in favor of Content/View.
newBuffer();
// @ts-expect-error Buffer switching was removed in favor of view.switch.
switchBuffer(contentId);
// @ts-expect-error The old unscoped save command is no longer public.
save();

void localResult;
void explicitResult;
void saving;
