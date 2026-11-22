import { useCallback, useEffect, useRef } from "react";

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
// - An entry that is already current (a switch to another terminal remounts the
//   hook while the old sheet's entry is still on top; a reload keeps the state)
//   is ADOPTED, never stacked: one Back always closes the sheet once. The
//   previous owner's deferred pop checks the registry and stands down.

const KEY = "kbSheet";

// entry id -> the hook instance that currently owns it. A release (cleanup) pops
// the entry only while its instance is still the owner; an adopting instance
// takes over before the deferred pop runs.
const owners = new Map<string, object>();

const currentKey = (): string | undefined => {
  const v = (window.history.state as Record<string, unknown> | null)?.[KEY];
  return typeof v === "string" && v !== "" ? v : undefined;
};

function hasCloseWatcher(): boolean {
  return typeof window !== "undefined" && typeof (window as unknown as { CloseWatcher?: unknown }).CloseWatcher === "function";
}

// Returns `popOwnEntry()`: the sheet's close button calls it FIRST. When the hook
// owns an entry it pops it with `history.back()` (the popstate closes the sheet
// through `onClose`, which for the sessions page is a `replace` of `?term=` on
// the entry below) and returns true; else false and the caller closes directly.
// Closing by replacing the URL on top of our entry would strand it and make
// Back re-open the sheet from the `?term=` entry underneath.
export function useSheetHistory(active: boolean, onClose: () => void): () => boolean {
  const onCloseRef = useRef(onClose);
  onCloseRef.current = onClose;
  const entryRef = useRef<string | null>(null);
  const aliveRef = useRef(false);
  // A pop is in flight (back() is async): a second Close before the popstate
  // must not issue a second back(), which would leave the page.
  const poppingRef = useRef(false);
  const tokenRef = useRef<object>({});

  const popOwnEntry = useCallback((): boolean => {
    if (entryRef.current === null) return false;
    if (poppingRef.current) return true;
    const cur = (window.history.state as Record<string, unknown> | null)?.[KEY];
    if (cur !== entryRef.current) return false;
    poppingRef.current = true;
    window.history.back();
    return true;
  }, []);

  useEffect(() => {
    if (!active || typeof window === "undefined" || hasCloseWatcher()) return;
    aliveRef.current = true;
    const token = tokenRef.current;
    const existing = entryRef.current === null ? currentKey() : undefined;
    if (existing !== undefined) {
      // The entry on top is a sheet entry nobody owns any more: take it over.
      entryRef.current = existing;
      owners.set(existing, token);
    } else if (entryRef.current === null) {
      const id = `${Date.now().toString(36)}${Math.random().toString(36).slice(2, 8)}`;
      const base = (window.history.state ?? {}) as Record<string, unknown>;
      window.history.pushState({ ...base, [KEY]: id }, "");
      entryRef.current = id;
      owners.set(id, token);
    }
    const onPop = (): void => {
      // Our entry is gone (Back): the sheet closes, nothing more to pop.
      const cur = (window.history.state as Record<string, unknown> | null)?.[KEY];
      if (cur === entryRef.current) return;
      if (entryRef.current !== null) owners.delete(entryRef.current);
      entryRef.current = null;
      aliveRef.current = false;
      poppingRef.current = false;
      onCloseRef.current();
    };
    window.addEventListener("popstate", onPop);
    return () => {
      window.removeEventListener("popstate", onPop);
      aliveRef.current = false;
      poppingRef.current = false;
      const id = entryRef.current;
      if (id === null) return;
      // Deferred one microtask so a StrictMode replay (same task) keeps the entry.
      queueMicrotask(() => {
        if (aliveRef.current || entryRef.current !== id) return;
        entryRef.current = null;
        // Another hook instance adopted this entry meanwhile: it is theirs to pop.
        if (owners.get(id) !== token) return;
        owners.delete(id);
        const cur = (window.history.state as Record<string, unknown> | null)?.[KEY];
        // Only pop an entry that is still ours (a route change moved on).
        if (cur === id) window.history.back();
      });
    };
  }, [active]);
  return popOwnEntry;
}
