// meta-worker.ts — sends back import.meta.url.
self.onmessage = () => {
  self.postMessage(import.meta.url);
};
