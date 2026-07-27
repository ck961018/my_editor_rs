// import-worker.ts — statically imports a sibling module.
import { value } from "./helper.ts";

self.onmessage = () => {
  self.postMessage(value);
};
