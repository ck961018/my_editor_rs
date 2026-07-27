// parent.ts — worker that spawns a nested child worker.
// Uses standard `new URL(..., import.meta.url)` form.

const child = new Worker(
    new URL("./child.ts", import.meta.url),
    { type: "module" },
);

child.addEventListener("message", (e) => {
    // Forward child's response to our parent (the main thread).
    self.postMessage(e.data);
});

self.onmessage = (e) => {
    // Forward to child.
    child.postMessage(e.data);
};
