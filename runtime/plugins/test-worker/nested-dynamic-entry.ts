import { loadAnswer } from "./nested/loader.ts";

self.onmessage = async () => {
  self.postMessage(await loadAnswer());
};
