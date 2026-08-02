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
const contentId: number = newBuffer();
switchBuffer(contentId);
const saving: Promise<void> = save();

void localResult;
void explicitResult;
void saving;
