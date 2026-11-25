// TERMNOTIFY - per-terminal attention marks for the switcher. A module-level
// in-memory store (browser-local, never persisted); the host writes it, any
// component reads it through useTermAttention().

import { useSyncExternalStore } from "react";

export interface TermMark {
  asks: boolean;
  finishedUnseen: boolean;
}
export type TermMarks = Readonly<Record<string, TermMark>>;

const EMPTY: TermMarks = Object.freeze({});
let marks: TermMarks = EMPTY;
const listeners = new Set<() => void>();

function same(a: TermMarks, b: TermMarks): boolean {
  const ka = Object.keys(a);
  if (ka.length !== Object.keys(b).length) return false;
  return ka.every(
    (k) => b[k] !== undefined && a[k].asks === b[k].asks && a[k].finishedUnseen === b[k].finishedUnseen,
  );
}

/** Replace the marks; a no-change write keeps the snapshot reference. */
export function setMarks(next: TermMarks): void {
  const n = Object.keys(next).length === 0 ? EMPTY : next;
  if (same(marks, n)) return;
  marks = n;
  for (const l of listeners) l();
}

export function peekMarks(): TermMarks {
  return marks;
}

export function __resetMarks(): void {
  setMarks(EMPTY);
}

function subscribe(l: () => void): () => void {
  listeners.add(l);
  return () => {
    listeners.delete(l);
  };
}

/** `{ [termId]: { asks, finishedUnseen } }`; only terminals with a mark appear. */
export function useTermAttention(): TermMarks {
  return useSyncExternalStore(subscribe, peekMarks, peekMarks);
}
