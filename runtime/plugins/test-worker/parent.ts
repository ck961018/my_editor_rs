// parent.ts — worker that spawns a nested child worker.
// Uses a plain string path (no URL global in worker isolate yet).

const child = new Worker("child.ts", { type: "module" });

child.addEventListener("message", (e) => {
    // Forward child's response to our parent (the main thread).
    self.postMessage(e.data);
});

self.onmessage = (e) => {
    // Forward to child.
    child.postMessage(e.data);
};
