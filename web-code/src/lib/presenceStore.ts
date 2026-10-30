// OL PRES3 - the tab-local half of presence: the touch ring fed by
// `kbc:mirror.updated`, and the seen-set. In memory, per tab, never sent
// anywhere; a reload starts empty (the ring is about what changed while this
// tab watched, the `annotate watch` seed-then-diff shape: whatever is on
// disk at load is the baseline, only later changes light up).

import { BURST_PATHS, RING_SECS, type MirrorTouch } from "./presence";

const MAX_TOUCHES = 2000;

const touches = new Map<string, MirrorTouch>();
const opened = new Map<string, number>();
const bursts = new Map<string, { count: number; at: number }>();
let version = 0;
const listeners = new Set<() => void>();

function key(repo: string, path: string): string {
  return `${repo}\0${path}`;
}

function bump(): void {
  version += 1;
  for (const l of listeners) l();
}

function nowPerf(): number {
  return typeof performance !== "undefined" ? performance.now() : Date.now();
}

/// A `mirror.updated` frame. A batch over `BURST_PATHS` is a checkout or
/// rebase: remembered as a burst (so the tree can say so), never marked
/// per path.
export function recordMirror(repo: string, paths: readonly string[], at: number = nowPerf()): void {
  if (!repo || paths.length === 0) return;
  if (paths.length > BURST_PATHS) {
    bursts.set(repo, { count: paths.length, at });
    bump();
    return;
  }
  for (const p of paths) touches.set(key(repo, p), { repo, path: p, at });
  const horizon = at - RING_SECS * 1000;
  for (const [k, t] of touches) if (t.at < horizon) touches.delete(k);
  while (touches.size > MAX_TOUCHES) {
    const first = touches.keys().next().value;
    if (first === undefined) break;
    touches.delete(first);
  }
  bump();
}

/// The operator opened `path`: touches at or before `at` stop lighting.
export function markOpened(repo: string, path: string, at: number = nowPerf()): void {
  if (!repo || !path) return;
  const k = key(repo, path);
  if ((opened.get(k) ?? -Infinity) >= at) return;
  opened.set(k, at);
  bump();
}

export function openedAtFor(repo: string): (path: string) => number | undefined {
  return (path) => opened.get(key(repo, path));
}

export function touchesFor(repo: string): MirrorTouch[] {
  const out: MirrorTouch[] = [];
  for (const t of touches.values()) if (t.repo === repo) out.push(t);
  return out;
}

export function burstFor(repo: string): { count: number; at: number } | undefined {
  return bursts.get(repo);
}

export function dismissBurst(repo: string): void {
  if (bursts.delete(repo)) bump();
}

export function subscribePresence(fn: () => void): () => void {
  listeners.add(fn);
  return () => {
    listeners.delete(fn);
  };
}

export function presenceVersion(): number {
  return version;
}

/// Test seam.
export function resetPresenceStore(): void {
  touches.clear();
  opened.clear();
  bursts.clear();
  bump();
}

let started = false;

/// One window listener for the tab's lifetime (idempotent), called from
/// main.tsx beside the SSE bridge so the ring fills even while no tree is
/// mounted.
export function startPresenceListener(): void {
  if (started || typeof window === "undefined") return;
  started = true;
  window.addEventListener("kbc:mirror.updated", (e: Event) => {
    const d = (e as CustomEvent<{ repo?: string; paths?: string[] } | null>).detail;
    if (d?.repo && Array.isArray(d.paths)) recordMirror(d.repo, d.paths);
  });
}
