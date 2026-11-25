// TERMNOTIFY - the browser-local opt-in for OS notifications. Default off;
// every storage failure degrades to off.

import type { StorageLike } from "../hub/termPrefs";

export const NOTIFY_KEY = "kb-term-notify";

function defaultStore(): StorageLike | null {
  try {
    return globalThis.localStorage ?? null;
  } catch {
    return null;
  }
}

export function getNotifyPref(store: StorageLike | null = defaultStore()): boolean {
  if (store === null) return false;
  try {
    return store.getItem(NOTIFY_KEY) === "1";
  } catch {
    return false;
  }
}

export function setNotifyPref(on: boolean, store: StorageLike | null = defaultStore()): void {
  if (store === null) return;
  try {
    if (on) store.setItem(NOTIFY_KEY, "1");
    else store.removeItem(NOTIFY_KEY);
  } catch {
    // quota / private mode: the pref is a convenience, never an error.
  }
}
