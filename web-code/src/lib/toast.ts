// F3a — minimal global feedback surface, ported from kb's own
// `web/src/lib/toast.ts` (root CLAUDE.md invariant #32). A tiny pub/sub
// store the <ToastHost> viewport (mounted once in app.tsx) renders. Replaces
// the silent `.catch(() => {})`/uncaught-`mutateAsync` sites so a failed
// mutation — a stale annotation ETag, a checkout that got refused, a peek
// resolve that errored — is no longer invisible.
//
// No dependency: a module-level array + a listener set, bridged to React via
// useSyncExternalStore. The snapshot array ref is stable between emits, which
// useSyncExternalStore requires.
import { useSyncExternalStore } from "react";

export type ToastKind = "ok" | "err" | "warn";

/// Phase E4 — an optional in-app navigation attached to a toast (e.g. "Added
/// to \"the ingest path\"" → a link straight to that set's detail page).
/// Additive: every pre-existing `toast.ok/err/warn(msg)` call site is
/// unaffected (`link` defaults to absent, and `ToastHost` simply doesn't
/// render the link span when it's missing).
export interface ToastLink {
  to: string;
  label: string;
}

export interface Toast {
  id: number;
  kind: ToastKind;
  msg: string;
  link?: ToastLink;
}

let toasts: Toast[] = [];
let nextId = 1;
const listeners = new Set<() => void>();

function emit() {
  for (const l of listeners) l();
}

function push(kind: ToastKind, msg: string, ttl: number, link?: ToastLink): number {
  const id = nextId++;
  toasts = [...toasts, { id, kind, msg, ...(link ? { link } : {}) }];
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
  ok: (msg: string, link?: ToastLink) => push("ok", msg, 3500, link),
  err: (msg: string) => push("err", msg, 6000),
  warn: (msg: string) => push("warn", msg, 4500),
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

/** React binding for the <ToastHost> viewport. */
export function useToasts(): Toast[] {
  return useSyncExternalStore(subscribe, peekToasts, peekToasts);
}

/** Test-only reset so suites don't bleed toasts into each other. */
export function __resetToasts(): void {
  toasts = [];
  nextId = 1;
  emit();
}
