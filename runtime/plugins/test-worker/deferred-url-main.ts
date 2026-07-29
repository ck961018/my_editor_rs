const globals = globalThis as typeof globalThis & {
  workerResult: unknown;
  spawnWorker: () => void;
};

globals.workerResult = null;
globals.spawnWorker = () => {
  const worker = new Worker(
    new URL("./meta-worker.ts", import.meta.url),
    { type: "module" },
  );
  worker.onmessage = (event) => {
    globals.workerResult = event.data;
  };
  worker.postMessage({});
};
