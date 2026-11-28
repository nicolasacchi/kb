// FXUI — "Recent": this browser session's uploads. A module-level, in-memory
// store keyed by terminal id (newest first, capped). Nothing is persisted:
// no localStorage, sessionStorage or IndexedDB, so a reload empties it.

import { useSyncExternalStore } from "react";
import type { FileUploadAnswer } from "../../api/generated/FileUploadAnswer";

export const RECENT_CAP = 50;

const EMPTY: readonly FileUploadAnswer[] = [];
let byTerm = new Map<string, readonly FileUploadAnswer[]>();
const listeners = new Set<() => void>();

export function addRecent(termId: string, answer: FileUploadAnswer): void {
  const next = [answer, ...(byTerm.get(termId) ?? EMPTY)].slice(0, RECENT_CAP);
  byTerm = new Map(byTerm).set(termId, next);
  for (const l of listeners) l();
}

export function recentFor(termId: string): readonly FileUploadAnswer[] {
  return byTerm.get(termId) ?? EMPTY;
}

export function resetRecent(): void {
  byTerm = new Map();
  for (const l of listeners) l();
}

function subscribe(l: () => void): () => void {
  listeners.add(l);
  return () => listeners.delete(l);
}

export function useRecent(termId: string): readonly FileUploadAnswer[] {
  return useSyncExternalStore(
    subscribe,
    () => recentFor(termId),
    () => EMPTY,
  );
}
