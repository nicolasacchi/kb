// CONVNEED — a tiny in-memory request channel from the needs-you banner to the
// Conversation rail ("scroll to the pending row"). Never persisted. The request
// expires so a stale one cannot fire when the rail mounts much later.

import { useSyncExternalStore } from "react";

export const JUMP_TTL_MS = 10_000;

let tick = 0;
let requestedAt: number | null = null;
const listeners = new Set<() => void>();

function notify(): void {
  for (const l of [...listeners]) l();
}

export function requestPendingJump(now: number = Date.now()): void {
  tick += 1;
  requestedAt = now;
  notify();
}

/** True while an unexpired, unconsumed request exists. */
export function hasPendingJump(now: number = Date.now()): boolean {
  return requestedAt !== null && now - requestedAt <= JUMP_TTL_MS;
}

export function clearPendingJump(): void {
  requestedAt = null;
}

function subscribe(cb: () => void): () => void {
  listeners.add(cb);
  return () => {
    listeners.delete(cb);
  };
}

export function usePendingJumpTick(): number {
  return useSyncExternalStore(subscribe, () => tick, () => 0);
}

/** Test-only reset. */
export function resetPendingJumpForTests(): void {
  tick = 0;
  requestedAt = null;
  listeners.clear();
}
