import { useEffect, useRef } from "react";

// Back for the phone terminal sheet where the platform has no CloseWatcher
// (Safari, iOS): while the sheet is open the hook owns ONE history entry, so
// the edge swipe pops that entry and closes the sheet instead of leaving the
// page. Terminal-only: `useCloseRequest` stays a strict no-op without the API
// and the reader's one-entry-per-artifact history is untouched.
//
// - With a CloseWatcher the hook does nothing (Android Back already works).
// - Inactive: nothing is pushed, Back behaves as before.
// - Closed by the sheet's own button: the entry is popped again with
//   `history.back()`, so closing never leaves a stray entry behind.
// - The existing `history.state` is kept (the router stores its index there).
// - StrictMode's effect replay reuses the entry instead of pushing a second.

const KEY = "kbSheet";

function hasCloseWatcher(): boolean {
  return typeof window !== "undefined" && typeof (window as unknown as { CloseWatcher?: unknown }).CloseWatcher === "function";
}

export function useSheetHistory(active: boolean, onClose: () => void): void {
  const onCloseRef = useRef(onClose);
  onCloseRef.current = onClose;
  const entryRef = useRef<string | null>(null);
  const aliveRef = useRef(false);

  useEffect(() => {
    if (!active || typeof window === "undefined" || hasCloseWatcher()) return;
    aliveRef.current = true;
    if (entryRef.current === null) {
      const id = `${Date.now().toString(36)}${Math.random().toString(36).slice(2, 8)}`;
      const base = (window.history.state ?? {}) as Record<string, unknown>;
      window.history.pushState({ ...base, [KEY]: id }, "");
      entryRef.current = id;
    }
    const onPop = (): void => {
      // Our entry is gone (Back): the sheet closes, nothing more to pop.
      const cur = (window.history.state as Record<string, unknown> | null)?.[KEY];
      if (cur === entryRef.current) return;
      entryRef.current = null;
      aliveRef.current = false;
      onCloseRef.current();
    };
    window.addEventListener("popstate", onPop);
    return () => {
      window.removeEventListener("popstate", onPop);
      aliveRef.current = false;
      const id = entryRef.current;
      if (id === null) return;
      // Deferred one microtask so a StrictMode replay (same task) keeps the entry.
      queueMicrotask(() => {
        if (aliveRef.current || entryRef.current !== id) return;
        entryRef.current = null;
        const cur = (window.history.state as Record<string, unknown> | null)?.[KEY];
        // Only pop an entry that is still ours (a route change moved on).
        if (cur === id) window.history.back();
      });
    };
  }, [active]);
}
