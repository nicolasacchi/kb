// F1 — minimal global feedback surface. A tiny pub/sub store the <Toasts>
// viewport (mounted once in App) renders. Replaces the silent `.catch(() => {})`
// sites so a failed mutation — e.g. a stale-ETag 409 on "resolve comment", which
// used to do nothing yet look successful — is no longer invisible.
//
// No dependency: a module-level array + a listener set, bridged to React via
// useSyncExternalStore. The snapshot array ref is stable between emits, which
// useSyncExternalStore requires.
import { useSyncExternalStore } from "react";

export type ToastKind = "ok" | "err" | "info";
/// U4 — an optional single action button rendered before the dismiss ✕
/// (e.g. "Open" on a capture-succeeded toast). Clicking it also dismisses
/// the toast (Toasts.tsx wires both in one handler).
export interface ToastAction {
  label: string;
  onClick: () => void;
}
export interface Toast {
  id: number;
  kind: ToastKind;
  msg: string;
  action?: ToastAction;
}

let toasts: Toast[] = [];
let nextId = 1;
const listeners = new Set<() => void>();

function emit() {
  for (const l of listeners) l();
}

function push(
  kind: ToastKind,
  msg: string,
  ttl: number,
  action?: ToastAction,
): number {
  const id = nextId++;
  toasts = [...toasts, { id, kind, msg, action }];
  emit();
  if (ttl > 0) setTimeout(() => dismissToast(id), ttl);
  return id;
}

export function dismissToast(id: number): void {
  const next = toasts.filter((t) => t.id !== id);
  if (next.length !== toasts.length) {
    toasts = next;
    emit();
  }
}

export const toast = {
  // An action gets a longer TTL (8s vs 3.5s) — the extra tap needs more
  // than the plain confirmation window.
  ok: (msg: string, action?: ToastAction) =>
    push("ok", msg, action ? 8000 : 3500, action),
  // SL4 — `err` accepts the SAME optional action `ok`/`info` already take
  // (the slate's 409 `slate-taken` toast carries a "post anyway" button; an
  // error with a one-tap remedy is the case an action is FOR). Omitting it
  // is byte-identical to before: same kind, same 6s TTL, same undefined
  // action field.
  err: (msg: string, action?: ToastAction) =>
    push("err", msg, action ? 8000 : 6000, action),
  info: (msg: string, action?: ToastAction) =>
    push("info", msg, action ? 8000 : 4000, action),
};

function subscribe(l: () => void): () => void {
  listeners.add(l);
  return () => {
    listeners.delete(l);
  };
}

/** Current toast list (the stable snapshot the viewport renders). */
export function peekToasts(): Toast[] {
  return toasts;
}

/** React binding for the <Toasts> viewport. */
export function useToasts(): Toast[] {
  return useSyncExternalStore(subscribe, peekToasts, peekToasts);
}

/** Test-only reset so suites don't bleed toasts into each other. */
export function __resetToasts(): void {
  toasts = [];
  nextId = 1;
  emit();
}
