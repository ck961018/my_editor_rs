/// <reference path="../editor.d.ts" />

export {};

interface HighlightRequest {
  contentId: number;
  revision: number;
  text: string;
}

self.onmessage = (event: MessageEvent<HighlightRequest>) => {
  const { contentId, revision, text } = event.data;
  const firstLine = text.split(/\r?\n/, 1)[0] ?? "";
  const spans: TextDecorationSpan[] = firstLine.length === 0
    ? []
    : [{
      range: {
        start: { line: 0, character: 0 },
        end: { line: 0, character: firstLine.length },
      },
      face: "plugin.worker-platform.result",
    }];
  self.postMessage({ contentId, revision, spans });
};
