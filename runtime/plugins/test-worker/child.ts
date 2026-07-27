// child.ts — echo worker, spawned by parent.ts for nested spawn test.
self.onmessage = (e: MessageEvent) => {
  self.postMessage(e.data);
};
