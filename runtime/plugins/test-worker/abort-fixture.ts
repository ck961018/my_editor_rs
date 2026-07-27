// abort-fixture.ts — worker that stays alive until abort.
// Registers self.onmessage and waits. When the AbortSignal
// fires, the cancellation token cancels and the worker
// thread's recv loop breaks.
self.onmessage = (_e) => {
    // Does nothing — just keeps the worker alive.
};
