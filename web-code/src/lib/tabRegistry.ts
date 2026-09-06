// V70-A6 — cross-tab awareness (§P7, research §3.5), the small version.
//
// TWO jobs, both strictly optional enhancements:
//
//   1. **Hand a trail to a tab that does not have it.** A trail-linked tab
//      opens with `?trail=<id>&step=<n>` but only the tab that MINTED the
//      trail has its body (v7.0 trails are per-tab `sessionStorage`; server
//      trails are v7.4). The newcomer broadcasts `trail.request`; whoever
//      holds it answers `trail.offer`. `sessionStorage` is also inherited by
//      a `window.open`-ed tab, so this is the second line rather than the
//      only one — and where neither works the origin chip simply does not
//      render (an honest absence, never a dead chip).
//   2. **Count the tabs opened off one trail**, so the ORIGIN tab's chip can
//      say "opened in 2 other tabs". Purely informational.
//
// WHY BroadcastChannel + Web Locks AND NOT A SharedWorker
// kb's own SPA hosts its SSE connection in a SharedWorker (root CLAUDE.md
// #24) because it needs ONE network connection per browser. Nothing here is a
// network connection: this is coordination between same-origin documents, and
// the research's recommendation for exactly that is BroadcastChannel for the
// bus plus Web Locks for leader election ("SharedWorker only where centralised
// computation is genuinely needed and its patchy mobile support is
// acceptable"). kb-code's per-tab `EventSource` is a recorded divergence from
// kb #24 already; this does not touch it.
//
// ORDERING: BroadcastChannel does NOT guarantee message order, so every
// message carries a monotonic `seq` from its sender and receivers ignore a
// message older than the last one they saw FROM THAT SENDER.
//
// DEGRADATION: if `BroadcastChannel` is missing, every export becomes a
// no-op and `tabCount()` reports 0 others. If `navigator.locks` is missing,
// there is no leader and the count falls back to counting live `hello`s —
// which is the same number in practice, just recomputed per query.

import type { Trail } from "./trail";
import { adoptTrail, loadTrail } from "./trail";

const CHANNEL = "kbc-tabs";
const LEADER_LOCK = "kbc-tabs-leader";
/// How long a `trail.request` waits for an offer before giving up. Short: a
/// same-origin BroadcastChannel round trip is sub-millisecond, and the
/// consequence of giving up is one missing chip.
export const TRAIL_REQUEST_TIMEOUT_MS = 600;
/// How long `tabCount` collects `pong`s.
export const PING_WINDOW_MS = 250;

export interface TabPresence {
  tabId: string;
  trailId: string | null;
  at: number;
}

type Msg =
  | { v: 1; seq: number; from: string; type: "hello"; trailId: string | null }
  | { v: 1; seq: number; from: string; type: "bye" }
  | { v: 1; seq: number; from: string; type: "ping" }
  | { v: 1; seq: number; from: string; type: "pong"; trailId: string | null }
  | { v: 1; seq: number; from: string; type: "trail.request"; id: string }
  | { v: 1; seq: number; from: string; type: "trail.offer"; trail: Trail }
  | { v: 1; seq: number; from: string; type: "tabs.state"; tabs: TabPresence[] };

/// `Omit` over a UNION collapses to the shared keys, so `Omit<Msg, …>` would
/// lose every payload field. Distribute it explicitly — the body of one
/// message, with the envelope removed.
type MsgBody = Msg extends infer M ? (M extends Msg ? Omit<M, "v" | "seq" | "from"> : never) : never;

function makeTabId(): string {
  return `t${Math.floor(Math.random() * 0xffffffff).toString(16)}`;
}

export const TAB_ID = makeTabId();

let channel: BroadcastChannel | null = null;
let seq = 0;
let myTrailId: string | null = null;
let isLeader = false;
const lastSeqFrom = new Map<string, number>();
const known = new Map<string, TabPresence>();
const subs = new Set<(msg: Msg) => void>();

function open(): BroadcastChannel | null {
  if (channel) return channel;
  if (typeof BroadcastChannel === "undefined") return null;
  try {
    channel = new BroadcastChannel(CHANNEL);
  } catch {
    return null;
  }
  channel.addEventListener("message", (ev) => {
    const m = ev.data as Msg | undefined;
    if (!m || m.v !== 1 || m.from === TAB_ID) return;
    // Out-of-order guard (BroadcastChannel gives no ordering guarantee).
    const last = lastSeqFrom.get(m.from) ?? -1;
    if (m.seq <= last) return;
    lastSeqFrom.set(m.from, m.seq);
    handle(m);
    for (const cb of subs) cb(m);
  });
  return channel;
}

function post(msg: MsgBody): void {
  const ch = open();
  if (!ch) return;
  try {
    ch.postMessage({ v: 1, seq: ++seq, from: TAB_ID, ...msg } as Msg);
  } catch {
    // A structured-clone failure (a Trail is plain JSON, so this should not
    // happen) must never break navigation.
  }
}

function handle(m: Msg): void {
  switch (m.type) {
    case "hello":
      known.set(m.from, { tabId: m.from, trailId: m.trailId, at: Date.now() });
      if (isLeader) post({ type: "tabs.state", tabs: [...known.values()] });
      break;
    case "bye":
      known.delete(m.from);
      break;
    case "ping":
      post({ type: "pong", trailId: myTrailId });
      break;
    case "pong":
      known.set(m.from, { tabId: m.from, trailId: m.trailId, at: Date.now() });
      break;
    case "trail.request": {
      const t = loadTrail(m.id);
      if (t) post({ type: "trail.offer", trail: t });
      break;
    }
    case "trail.offer":
      adoptTrail(m.trail);
      break;
    case "tabs.state":
      for (const t of m.tabs) if (t.tabId !== TAB_ID) known.set(t.tabId, t);
      break;
  }
}

/// Join the registry. Idempotent; safe to call from an effect.
export function joinTabRegistry(trailId: string | null): () => void {
  myTrailId = trailId;
  if (!open()) return () => {};
  post({ type: "hello", trailId });
  // Leader election. The lock is held for the tab's whole life, so the FIRST
  // tab to ask owns the registry and hands it over instantly (not after a
  // heartbeat timeout) when it closes — the property the research picks Web
  // Locks for over a timer-based heartbeat, which a backgrounded tab starves.
  const locks = (navigator as Navigator & { locks?: LockManager }).locks;
  if (locks) {
    void locks
      .request(LEADER_LOCK, () => {
        isLeader = true;
        post({ type: "tabs.state", tabs: [...known.values()] });
        return new Promise<void>(() => {
          /* held until this document goes away */
        });
      })
      .catch(() => {
        /* another tab holds it, or locks are unavailable — not an error */
      });
  }
  const onHide = () => post({ type: "bye" });
  window.addEventListener("pagehide", onHide);
  return () => {
    window.removeEventListener("pagehide", onHide);
    post({ type: "bye" });
  };
}

export function setTabTrail(trailId: string | null): void {
  if (myTrailId === trailId) return;
  myTrailId = trailId;
  post({ type: "hello", trailId });
}

/// Ask the other tabs for a trail body we do not have. Resolves with `true`
/// once one arrives (adopted into our own sessionStorage by `handle`), or
/// `false` after `TRAIL_REQUEST_TIMEOUT_MS` — never hangs.
export function requestTrail(id: string, timeoutMs = TRAIL_REQUEST_TIMEOUT_MS): Promise<boolean> {
  if (loadTrail(id)) return Promise.resolve(true);
  if (!open()) return Promise.resolve(false);
  return new Promise((resolve) => {
    let done = false;
    const finish = (ok: boolean) => {
      if (done) return;
      done = true;
      subs.delete(onMsg);
      resolve(ok);
    };
    const onMsg = (m: Msg) => {
      if (m.type === "trail.offer" && m.trail.id === id) finish(true);
    };
    subs.add(onMsg);
    post({ type: "trail.request", id });
    window.setTimeout(() => finish(!!loadTrail(id)), timeoutMs);
  });
}

/// How many OTHER live tabs are on `trailId`. Broadcasts a ping and counts
/// the pongs that come back inside `PING_WINDOW_MS` — a live measurement,
/// never a stored count that could outlive the tab it describes.
export function countTabsOnTrail(trailId: string, windowMs = PING_WINDOW_MS): Promise<number> {
  if (!open()) return Promise.resolve(0);
  known.clear();
  post({ type: "ping" });
  return new Promise((resolve) => {
    window.setTimeout(() => {
      let n = 0;
      for (const t of known.values()) if (t.tabId !== TAB_ID && t.trailId === trailId) n++;
      resolve(n);
    }, windowMs);
  });
}

/// Test-only: drop the channel + caches so a suite can re-open cleanly.
export function _resetTabRegistryForTests(): void {
  try {
    channel?.close();
  } catch {
    /* already closed */
  }
  channel = null;
  seq = 0;
  isLeader = false;
  myTrailId = null;
  lastSeqFrom.clear();
  known.clear();
  subs.clear();
}
