// BA11Y — the browser-local screen-reader preference for the terminal grid.
// Pure: a blocked or throwing storage degrades to the default (off).

export const SCREEN_READER_KEY = "kb-term-screen-reader";

export interface StorageLike {
  getItem(k: string): string | null;
  setItem(k: string, v: string): void;
}

function defaultStore(): StorageLike | null {
  try {
    return window.localStorage;
  } catch {
    return null;
  }
}

/** True only for a stored "1"; absent, foreign or unreadable storage is off. */
export function readScreenReader(store: StorageLike | null = defaultStore()): boolean {
  try {
    return store?.getItem(SCREEN_READER_KEY) === "1";
  } catch {
    return false;
  }
}

/** Persist the choice; a blocked storage (private mode) just does not persist. */
export function writeScreenReader(on: boolean, store: StorageLike | null = defaultStore()): void {
  try {
    store?.setItem(SCREEN_READER_KEY, on ? "1" : "0");
  } catch {
    // private mode: the choice lasts for this mount only
  }
}
