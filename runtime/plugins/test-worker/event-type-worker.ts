self.onmessage = (event) => {
  self.postMessage(event.type);
};
