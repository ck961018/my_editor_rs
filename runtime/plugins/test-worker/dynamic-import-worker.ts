// dynamic-import-worker.ts — uses dynamic import() to load a module.
self.onmessage = async () => {
  const mod = await import("./helper.ts");
  self.postMessage(mod.value);
};
