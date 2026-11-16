/**
 * The three "the network may be back" triggers that skip a pending reconnect
 * backoff: `online`, a bfcache `pageshow`, and the tab turning visible.
 * Pure DOM-event binding (no xterm import) so it is unit-testable.
 */
export interface KickWindow {
  addEventListener(type: "online" | "pageshow", fn: () => void): void;
  removeEventListener(type: "online" | "pageshow", fn: () => void): void;
}
export interface KickDocument {
  readonly visibilityState: string;
  addEventListener(type: "visibilitychange", fn: () => void): void;
  removeEventListener(type: "visibilitychange", fn: () => void): void;
}

/** Bind the triggers; the returned disposer removes every listener. */
export function bindNetworkKicks(win: KickWindow, doc: KickDocument, kick: () => void): () => void {
  const onVisible = (): void => {
    if (doc.visibilityState === "visible") kick();
  };
  win.addEventListener("online", kick);
  win.addEventListener("pageshow", kick);
  doc.addEventListener("visibilitychange", onVisible);
  return () => {
    win.removeEventListener("online", kick);
    win.removeEventListener("pageshow", kick);
    doc.removeEventListener("visibilitychange", onVisible);
  };
}
