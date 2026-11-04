const waiting: Array<(release: () => void) => void> = [];
let busy = false;

function drain() {
  if (busy) return;
  const next = waiting.shift();
  if (!next) return;
  busy = true;
  let released = false;
  next(() => {
    if (released) return;
    released = true;
    busy = false;
    drain();
  });
}

/**
 * Suggestion dialogs the launcher opens on its own take turns: `open` runs when
 * no other one is showing and must call `release` once its dialog is closed.
 */
export function enqueueSuggestionModal(open: (release: () => void) => void) {
  waiting.push(open);
  drain();
}
