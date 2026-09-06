// Reading flow — the "where did I come from" stack.
//
// Following a link out of an artifact is a DESCENT: you were reading A at
// scroll y, you followed a reference into B, and the one thing the reader
// wants afterwards is to come back to A *exactly where they left it*. The
// browser's own Back does the first half (it restores the URL) but not the
// second (the artifact iframe is cross-origin, and the server-side resume
// value is a debounced, possibly-stale snapshot). This module is the second
// half: a small per-tab stack of the artifacts you descended FROM, each
// carrying the live scroll offset + active section at the moment you left.
//
// ── storage tier (the one-container-primitive rule) ───────────────────────
//
// sessionStorage, `{ v: 1 }`-versioned, validated-on-read, capped at 30 —
// the same shape `lib/registers.ts` (localStorage) and
// `hooks/useScrollRestoration.ts` (sessionStorage) already use. Session, not
// local, for the same reason scroll restoration is: a reading descent is a
// property of THIS tab's browsing right now; two tabs reading two different
// corpora must never pop each other's stack, and a week-old descent is
// noise. Nothing here crosses the network — no route, no table, no SSE
// event — so the server-noun→CLI-verb parity rule doesn't apply (a recorded
// exemption, same as `registers.ts`'s).
//
// Pure logic + storage: no React, no fetch (`flowStack.test.ts` covers it
// with a stubbed storage). The React binding is `subscribe`/`snapshot`
// feeding `useSyncExternalStore` in `routes/detail.tsx`.

/// One artifact you descended FROM. `scrollY` / `sec` are the live values
/// the pane reported (its `kb:scroll` / `kb:section` relay) at the instant
/// the reader navigated away — never the server's resume value.
export type FlowEntry = {
  kb: string;
  /// The artifact id, when the doc query had resolved. Empty string is
  /// tolerated (a push that raced the by-path fetch) — nothing here keys on
  /// it, it is carried for future consumers.
  id: string;
  sourceRelative: string;
  title: string;
  scrollY: number;
  /// Active heading id when the reader left, for the popover's "§ section"
  /// label. Deliberately NOT put on the return URL — see `detail.tsx`'s
  /// `goBack`: the seeded scroll offset is strictly more precise than the
  /// section, and a `?sec=` would out-rank it in the pane's probe ladder.
  sec?: string;
  /// `Date.now()` at push time.
  ts: number;
};

/// The consume-once scroll seed a return hands to the pane it is returning
/// to. Written on pop, read by the reader route for the artifact it names.
export type FlowSeed = {
  kb: string;
  sourceRelative: string;
  y: number;
};

const STORAGE_KEY = "kb:flow:v1";
const SEED_KEY = "kb:flow:seed";
/// Deep enough that a real research descent never truncates, shallow enough
/// that the blob stays a few KB. Oldest entries drop first.
export const FLOW_CAP = 30;

// ── storage primitives (every one of them fails soft) ─────────────────────

function readRaw(key: string): string | null {
  if (typeof sessionStorage === "undefined") return null;
  try {
    return sessionStorage.getItem(key);
  } catch {
    return null;
  }
}

function writeRaw(key: string, value: string) {
  if (typeof sessionStorage === "undefined") return;
  try {
    sessionStorage.setItem(key, value);
  } catch {
    // Denied/full (private mode, quota). The write is simply lost: the
    // snapshot below re-reads storage, so the next `flowSnapshot()` sees
    // the pre-write bytes and the chip degrades to "no descent recorded".
    // Deliberate — a half-in-memory stack that disagrees with storage
    // would strand a return the reader can't complete.
  }
}

function removeRaw(key: string) {
  if (typeof sessionStorage === "undefined") return;
  try {
    sessionStorage.removeItem(key);
  } catch {
    // Denied — a stale seed is consumed by the next matching read anyway.
  }
}

// ── validation (a corrupt blob degrades to empty, never throws) ───────────

function isValidEntry(x: unknown): x is FlowEntry {
  if (typeof x !== "object" || x === null) return false;
  const e = x as Record<string, unknown>;
  return (
    typeof e.kb === "string" &&
    e.kb.length > 0 &&
    typeof e.id === "string" &&
    typeof e.sourceRelative === "string" &&
    e.sourceRelative.length > 0 &&
    typeof e.title === "string" &&
    typeof e.scrollY === "number" &&
    Number.isFinite(e.scrollY) &&
    (e.sec === undefined || typeof e.sec === "string") &&
    typeof e.ts === "number" &&
    Number.isFinite(e.ts)
  );
}

function parseStack(raw: string | null): FlowEntry[] {
  if (!raw) return [];
  try {
    const parsed: unknown = JSON.parse(raw);
    if (typeof parsed !== "object" || parsed === null) return [];
    const blob = parsed as Record<string, unknown>;
    if (blob.v !== 1) return [];
    if (!Array.isArray(blob.stack)) return [];
    return blob.stack.filter(isValidEntry).slice(-FLOW_CAP);
  } catch {
    return [];
  }
}

// ── the snapshot cache (useSyncExternalStore needs referential stability) ─

const EMPTY: readonly FlowEntry[] = Object.freeze([]);
let cache: readonly FlowEntry[] = EMPTY;
/// The raw blob `cache` was parsed from. Re-reading storage is cheap; what
/// is NOT cheap is handing `useSyncExternalStore` a fresh array on every
/// render (React would loop). So we re-read every time but only re-PARSE —
/// and hand out a new reference — when the bytes actually changed. This
/// also means a write from anywhere (another module, a test's raw seeding)
/// is picked up without an explicit invalidation.
let cachedRaw: string | null | undefined;
const listeners = new Set<() => void>();

function invalidate() {
  cachedRaw = undefined;
  for (const l of listeners) l();
}

/// The current stack, OLDEST first (the top — where `u` / the chip returns
/// to — is the LAST element). Referentially stable while the stored bytes
/// are unchanged, so `useSyncExternalStore` doesn't loop.
export function flowSnapshot(): readonly FlowEntry[] {
  const raw = readRaw(STORAGE_KEY);
  if (raw !== cachedRaw) {
    cachedRaw = raw;
    const parsed = parseStack(raw);
    cache = parsed.length === 0 ? EMPTY : parsed;
  }
  return cache;
}

/// `useSyncExternalStore` subscription. Also mirrors cross-tab writes (a
/// duplicated tab shares the session storage), which only ever means
/// "re-read", never "merge".
export function subscribeFlow(onChange: () => void): () => void {
  listeners.add(onChange);
  const onStorage = (e: StorageEvent) => {
    if (e.key === null || e.key === STORAGE_KEY) invalidate();
  };
  if (typeof window !== "undefined") {
    window.addEventListener("storage", onStorage);
  }
  return () => {
    listeners.delete(onChange);
    if (typeof window !== "undefined") {
      window.removeEventListener("storage", onStorage);
    }
  };
}

function writeStack(stack: readonly FlowEntry[]) {
  writeRaw(STORAGE_KEY, JSON.stringify({ v: 1, stack }));
  invalidate();
}

// ── the API ───────────────────────────────────────────────────────────────

/// Every entry, oldest first (see `flowSnapshot`).
export function flowList(): readonly FlowEntry[] {
  return flowSnapshot();
}

/// The artifact a return would go to — the newest entry, or undefined.
export function flowPeek(): FlowEntry | undefined {
  const stack = flowSnapshot();
  return stack.length > 0 ? stack[stack.length - 1] : undefined;
}

/// Push the artifact just left. Consecutive duplicates collapse (re-entering
/// the same artifact twice in a row would otherwise make `u` a no-op loop);
/// over-cap pushes drop the OLDEST entry.
export function flowPush(entry: FlowEntry): readonly FlowEntry[] {
  if (!isValidEntry(entry)) return flowSnapshot();
  const stack = [...flowSnapshot()];
  const top = stack[stack.length - 1];
  if (top && top.kb === entry.kb && top.sourceRelative === entry.sourceRelative) {
    stack.pop();
  }
  stack.push(entry);
  writeStack(stack.slice(-FLOW_CAP));
  return flowSnapshot();
}

/// Pop and return the newest entry (undefined when empty).
export function flowPop(): FlowEntry | undefined {
  const stack = [...flowSnapshot()];
  const top = stack.pop();
  if (!top) return undefined;
  writeStack(stack);
  return top;
}

/// Drop `depth` entries from the TOP without navigating, and return the new
/// top. `flowDropAbove(0)` is a no-op returning the current top — which is
/// exactly what the chip's popover needs: jumping to display row `d` (0 =
/// most recent) drops the `d` entries above it so the target becomes the
/// top, and the route's own return-detection then pops + seeds it like any
/// other return.
export function flowDropAbove(depth: number): FlowEntry | undefined {
  const d = Math.max(0, Math.floor(depth));
  if (d === 0) return flowPeek();
  const stack = [...flowSnapshot()];
  if (d >= stack.length) {
    writeStack([]);
    return undefined;
  }
  const kept = stack.slice(0, stack.length - d);
  writeStack(kept);
  return kept[kept.length - 1];
}

/// Drop everything (no UI calls this today; the tests and a future "clear
/// flow" action do).
export function flowClear(): void {
  writeStack([]);
}

// ── the consume-once scroll seed ──────────────────────────────────────────

/// Park the scroll offset a return should land on. Overwrites any unclaimed
/// seed: only one return can be in flight, and the newest wins.
export function writeFlowSeed(seed: FlowSeed): void {
  if (
    !seed ||
    typeof seed.kb !== "string" ||
    typeof seed.sourceRelative !== "string" ||
    typeof seed.y !== "number" ||
    !Number.isFinite(seed.y)
  ) {
    return;
  }
  writeRaw(SEED_KEY, JSON.stringify({ v: 1, ...seed }));
}

/// Read + CLEAR the seed, but only when it names this artifact. A seed for
/// a different document is left in place (the reader may still be one
/// navigation away from claiming it); a malformed one is dropped.
export function consumeFlowSeed(
  kb: string,
  sourceRelative: string,
): FlowSeed | null {
  const raw = readRaw(SEED_KEY);
  if (!raw) return null;
  let seed: FlowSeed | null = null;
  try {
    const parsed: unknown = JSON.parse(raw);
    if (typeof parsed === "object" && parsed !== null) {
      const s = parsed as Record<string, unknown>;
      if (
        s.v === 1 &&
        typeof s.kb === "string" &&
        typeof s.sourceRelative === "string" &&
        typeof s.y === "number" &&
        Number.isFinite(s.y)
      ) {
        seed = { kb: s.kb, sourceRelative: s.sourceRelative, y: s.y };
      }
    }
  } catch {
    seed = null;
  }
  if (!seed) {
    removeRaw(SEED_KEY);
    return null;
  }
  if (seed.kb !== kb || seed.sourceRelative !== sourceRelative) return null;
  removeRaw(SEED_KEY);
  return seed;
}

/// Display label for a stack row — title, else the filename.
export function flowLabel(entry: FlowEntry): string {
  if (entry.title) return entry.title;
  const rel = entry.sourceRelative;
  const slash = rel.lastIndexOf("/");
  return slash >= 0 ? rel.slice(slash + 1) : rel;
}

/// Truncate a label for the chip (the popover shows the full title).
export function truncateLabel(label: string, max = 24): string {
  if (label.length <= max) return label;
  return `${label.slice(0, Math.max(1, max - 1))}…`;
}
