// Navigation memory for the kb-code reader: a ring of recent locations,
// a derived recent-files list, and a vim-style jump list (Ctrl-o / Ctrl-i).
//
// Pure logic lives in the exported functions (vitest-covered). The module-
// level `recordJump` / `goBack` / `goForward` / `getRecent*` wrappers own a
// single browser-local snapshot + `localStorage` persistence so call sites
// stay one-liners. No React, no CM6.
//
// V70-A6 changed three things the recon called out (navigation-history.md):
//
//   * **PER PANE** (G6). The ring used to be pane-1-only: the recorder keyed
//     on `activeFile`, so every file opened into pane 2 was invisible to
//     `Ctrl-o` and to `g.`. The store now holds one `NavHistoryState` per
//     pane, and every entry point takes the pane it is acting for. The
//     RECENT-LOCATIONS view (`g.`, the reader start card) reads the two rings
//     MERGED, newest-first — "where have I been" is a question about the
//     reader, not about a pane.
//   * **A REAL SNIPPET AND A TYPED `via`** (G7/R2). Five of the six host-side
//     `recordJump` call sites passed `snippet: ""`, which made `g.` a bare
//     path list — the feature's whole differentiator (recognise the place by
//     its code) almost never fired. Callers now pass the cursor line's text,
//     and every jump carries the typed edge it was traversed by (`via`), so
//     Recent Locations can be filtered by kind ("definition hops only") and
//     the trail can be read as a sentence.
//   * **CROSS-TAB CONVERGENCE** (G8). There was no `storage` listener: two
//     tabs each held a divergent in-memory ring and clobbered the key on
//     every jump, last writer wins, silently. A storage event now MERGES the
//     other tab's entries into ours by timestamp (`mergeEntries`) instead of
//     replacing them — and only while our pointer is at the tip, because
//     merging under a live `Ctrl-o` cursor would move the ground the operator
//     is standing on. The pointer itself is never shared: it is a per-tab
//     reading position, not a fact about the corpus.

import type { TrailVia } from "./codeUrl";
import type { SpeedFilterHit } from "./speedSearch";
import { speedFilterItems } from "./speedSearch";

export const LOCATIONS_CAP = 100;
export const RECENT_FILES_CAP = 50;
export const DEDUPE_LINE_DELTA = 3;
export const SNIPPET_MAX = 120;

export const NAV_HISTORY_STORAGE_KEY = "kbc:navHistory";

/// Which pane a jump belongs to. Not `1 | 2 | …`: the reader has exactly two
/// panes by construction (`?pane2=`, root CLAUDE.md #30 — "only an ARTIFACT
/// may occupy a pane" and there is no pane 3).
export type PaneId = 1 | 2;

export const PANE_IDS: readonly PaneId[] = [1, 2];

export interface NavLocation {
  repo: string;
  path: string;
  line: number;
  /** Trimmed current-line text, ≤ SNIPPET_MAX chars. */
  snippet: string;
  ts: number;
  /// The typed edge traversed to get here (`lib/codeUrl.ts`'s `TrailVia`).
  /// Optional because a pre-A6 persisted entry has none, and because a
  /// caller that genuinely does not know must not invent one — an absent
  /// `via` renders as no chip, never as `manual`.
  via?: TrailVia;
  /// Which pane the jump happened in. Carried on the ENTRY as well as the
  /// ring so the merged Recent-Locations view can still say where a row
  /// came from.
  pane?: PaneId;
}

export interface RecentFile {
  repo: string;
  path: string;
  ts: number;
}

/// Jump-list state. `entries` is newest-first (index 0 = tip). `pointer`
/// indexes into `entries`: 0 is the newest tip; higher values are older.
/// After a fresh `pushEntry` the pointer is always 0.
export interface NavHistoryState {
  entries: NavLocation[];
  pointer: number;
}

/// The whole browser-local store: one ring per pane.
export interface NavHistoryStore {
  panes: Record<PaneId, NavHistoryState>;
}

export function emptyNavHistory(): NavHistoryState {
  return { entries: [], pointer: 0 };
}

export function emptyNavHistoryStore(): NavHistoryStore {
  return { panes: { 1: emptyNavHistory(), 2: emptyNavHistory() } };
}

/// Trim + cap a line of source for the location entry's snippet field.
export function makeSnippet(lineText: string): string {
  return lineText.trim().slice(0, SNIPPET_MAX);
}

export function makeLocation(
  repo: string,
  path: string,
  line: number,
  snippet: string,
  ts: number = Date.now(),
  extra?: { via?: TrailVia; pane?: PaneId },
): NavLocation {
  return {
    repo,
    path,
    line: Math.max(1, Math.floor(line) || 1),
    snippet: makeSnippet(snippet),
    ts,
    ...(extra?.via ? { via: extra.via } : {}),
    ...(extra?.pane ? { pane: extra.pane } : {}),
  };
}

function sameFile(a: Pick<NavLocation, "repo" | "path">, b: Pick<NavLocation, "repo" | "path">): boolean {
  return a.repo === b.repo && a.path === b.path;
}

function withinDedupeWindow(a: NavLocation, b: NavLocation): boolean {
  return sameFile(a, b) && Math.abs(a.line - b.line) <= DEDUPE_LINE_DELTA;
}

/// Push a new location onto the jump list. If the pointer is mid-list
/// (operator had Ctrl-o'd back), the "future" (newer-than-pointer) entries
/// are dropped first — same as vim's jumplist when you jump from the
/// middle. Consecutive same-file entries within `DEDUPE_LINE_DELTA` lines
/// collapse into the new one (newest wins). Cap is `LOCATIONS_CAP`.
export function pushEntry(state: NavHistoryState, loc: NavLocation): NavHistoryState {
  // Drop the "forward" half when recording a fresh jump from mid-list.
  let base = state.pointer > 0 ? state.entries.slice(state.pointer) : state.entries;

  if (base.length > 0 && withinDedupeWindow(base[0], loc)) {
    // Replace the tip rather than stacking near-duplicates.
    base = [loc, ...base.slice(1)];
  } else {
    base = [loc, ...base];
  }
  return { entries: base.slice(0, LOCATIONS_CAP), pointer: 0 };
}

/// Move the jump pointer older. When already at the tip (`pointer === 0`),
/// records `current` first (vim-style) so the operator can Ctrl-i back to
/// where they just were. Returns the location to land on, or `null` if
/// there is nothing older.
export function jumpBack(
  state: NavHistoryState,
  current: NavLocation,
): { state: NavHistoryState; target: NavLocation | null } {
  let s = state;
  if (s.pointer === 0) {
    // Record current at the tip, then step older. If current dedupes with
    // the existing tip we still want to walk past it to the next older
    // entry — so after pushEntry, always try pointer+1.
    s = pushEntry(s, current);
  }
  if (s.pointer >= s.entries.length - 1) {
    return { state: s, target: null };
  }
  const pointer = s.pointer + 1;
  return { state: { ...s, pointer }, target: s.entries[pointer] };
}

/// Move the jump pointer newer. Returns `null` when already at the tip.
export function jumpForward(
  state: NavHistoryState,
): { state: NavHistoryState; target: NavLocation | null } {
  if (state.pointer <= 0) {
    return { state, target: null };
  }
  const pointer = state.pointer - 1;
  return { state: { entries: state.entries, pointer }, target: state.entries[pointer] };
}

/// How many steps `Ctrl-o` / `Ctrl-i` still have in `state` — the COUNT the
/// pane-header arrows badge (Pane Relief's cheapest high-value idea: "the
/// count badges turn Back from a slot machine into an instrument").
export function backCount(state: NavHistoryState): number {
  return Math.max(0, state.entries.length - 1 - state.pointer);
}

export function forwardCount(state: NavHistoryState): number {
  return Math.max(0, state.pointer);
}

/// What `Ctrl-o` / `Ctrl-i` would land on WITHOUT moving the pointer — the
/// hover preview on each arrow (`path:line · snippet · via`).
export function peekBack(state: NavHistoryState): NavLocation | null {
  return state.entries[state.pointer + 1] ?? null;
}

export function peekForward(state: NavHistoryState): NavLocation | null {
  return state.pointer > 0 ? state.entries[state.pointer - 1] ?? null : null;
}

/// Unique-by-(repo,path) view of the location ring, newest first, capped.
export function recentFilesFrom(entries: readonly NavLocation[]): RecentFile[] {
  const out: RecentFile[] = [];
  const seen = new Set<string>();
  for (const e of entries) {
    const key = `${e.repo}\0${e.path}`;
    if (seen.has(key)) continue;
    seen.add(key);
    out.push({ repo: e.repo, path: e.path, ts: e.ts });
    if (out.length >= RECENT_FILES_CAP) break;
  }
  return out;
}

/// Filter the location ring with the shared speed-search primitive. The
/// haystack now includes the `via` kind, so typing "usage" narrows to usage
/// hops the same way typing a path narrows to a file.
export function filterLocations(
  entries: readonly NavLocation[],
  query: string,
): SpeedFilterHit<NavLocation>[] {
  return speedFilterItems(entries, query, (e) => `${e.path}:${e.line} ${e.snippet} ${e.via ?? ""}`);
}

/// Merge two rings into one newest-first, de-duplicated list. Identity is
/// `(repo, path, line, ts)`: the same jump recorded in two tabs has the same
/// timestamp (it IS the same write), while two genuinely different visits to
/// one line never share a millisecond in practice — and if they did, keeping
/// one of them is the harmless outcome.
export function mergeEntries(
  mine: readonly NavLocation[],
  theirs: readonly NavLocation[],
  cap = LOCATIONS_CAP,
): NavLocation[] {
  const byKey = new Map<string, NavLocation>();
  for (const e of [...mine, ...theirs]) {
    byKey.set(`${e.repo}\0${e.path}\0${e.line}\0${e.ts}`, e);
  }
  return [...byKey.values()].sort((a, b) => b.ts - a.ts).slice(0, cap);
}

// --- persistence + module singleton --------------------------------------

function isLocation(v: unknown): v is NavLocation {
  if (!v || typeof v !== "object") return false;
  const o = v as Record<string, unknown>;
  return (
    typeof o.repo === "string" &&
    typeof o.path === "string" &&
    typeof o.line === "number" &&
    typeof o.snippet === "string" &&
    typeof o.ts === "number"
  );
}

function parseOneRing(o: { entries?: unknown; pointer?: unknown } | null): NavHistoryState {
  if (!o) return emptyNavHistory();
  const entries = Array.isArray(o.entries) ? o.entries.filter(isLocation).slice(0, LOCATIONS_CAP) : [];
  const pointer =
    typeof o.pointer === "number" && o.pointer >= 0 && o.pointer < Math.max(entries.length, 1)
      ? Math.floor(o.pointer)
      : 0;
  // Clamp pointer into range when the list shrank under it.
  return { entries, pointer: entries.length === 0 ? 0 : Math.min(pointer, entries.length - 1) };
}

/// Parse ONE pane's ring. Kept exported (and unchanged in behaviour) because
/// it is the shape the pre-A6 key held and the shape the store's per-pane
/// slots hold.
export function parseNavHistory(raw: string | null): NavHistoryState {
  if (!raw) return emptyNavHistory();
  try {
    const parsed = JSON.parse(raw) as unknown;
    if (!parsed || typeof parsed !== "object") return emptyNavHistory();
    return parseOneRing(parsed as { entries?: unknown; pointer?: unknown });
  } catch {
    return emptyNavHistory();
  }
}

export function serializeNavHistory(state: NavHistoryState): string {
  return JSON.stringify({
    entries: state.entries.slice(0, LOCATIONS_CAP),
    pointer: state.pointer,
  });
}

/// Parse the whole two-pane store. MIGRATES the pre-A6 single-ring blob
/// (`{entries, pointer}` with no `panes`) into pane 1 rather than discarding
/// it — an operator's jump history is theirs, and a silent reset on upgrade
/// is exactly the kind of small betrayal this file is here to avoid.
export function parseNavHistoryStore(raw: string | null): NavHistoryStore {
  if (!raw) return emptyNavHistoryStore();
  try {
    const parsed = JSON.parse(raw) as unknown;
    if (!parsed || typeof parsed !== "object") return emptyNavHistoryStore();
    const o = parsed as { panes?: unknown; entries?: unknown; pointer?: unknown };
    if (!o.panes || typeof o.panes !== "object") {
      return { panes: { 1: parseOneRing(o), 2: emptyNavHistory() } };
    }
    const p = o.panes as Record<string, { entries?: unknown; pointer?: unknown } | undefined>;
    return { panes: { 1: parseOneRing(p["1"] ?? null), 2: parseOneRing(p["2"] ?? null) } };
  } catch {
    return emptyNavHistoryStore();
  }
}

export function serializeNavHistoryStore(store: NavHistoryStore): string {
  return JSON.stringify({
    panes: {
      1: { entries: store.panes[1].entries.slice(0, LOCATIONS_CAP), pointer: store.panes[1].pointer },
      2: { entries: store.panes[2].entries.slice(0, LOCATIONS_CAP), pointer: store.panes[2].pointer },
    },
  });
}

function loadStore(): NavHistoryStore {
  try {
    if (typeof localStorage === "undefined") return emptyNavHistoryStore();
    return parseNavHistoryStore(localStorage.getItem(NAV_HISTORY_STORAGE_KEY));
  } catch {
    return emptyNavHistoryStore();
  }
}

function saveStore(store: NavHistoryStore): void {
  try {
    if (typeof localStorage === "undefined") return;
    localStorage.setItem(NAV_HISTORY_STORAGE_KEY, serializeNavHistoryStore(store));
  } catch {
    // best-effort — private mode / quota must not break navigation.
  }
}

let _store: NavHistoryStore = loadStore();
const listeners = new Set<() => void>();

function emit(): void {
  for (const l of listeners) l();
}

/// Subscribe to ring changes (a jump recorded here, or another tab's merge
/// landing). Returns the unsubscribe. The pane-header arrows use this to
/// keep their count badges honest without polling.
export function subscribeNavHistory(cb: () => void): () => void {
  listeners.add(cb);
  return () => listeners.delete(cb);
}

/// G8 — converge with other tabs instead of clobbering them. Merge is only
/// applied to a ring whose pointer is AT THE TIP: a tab mid-`Ctrl-o` keeps
/// the exact list its pointer indexes into until it walks back to the tip,
/// at which point the next push re-merges. Deliberately never adopts another
/// tab's POINTER — that is this tab's reading position.
export function _applyForeignStore(foreign: NavHistoryStore): void {
  let changed = false;
  const next: NavHistoryStore = { panes: { ...(_store.panes as Record<PaneId, NavHistoryState>) } };
  for (const pane of PANE_IDS) {
    const mine = _store.panes[pane];
    if (mine.pointer !== 0) continue;
    const merged = mergeEntries(mine.entries, foreign.panes[pane].entries);
    if (merged.length !== mine.entries.length || merged.some((e, i) => e.ts !== mine.entries[i]?.ts)) {
      next.panes[pane] = { entries: merged, pointer: 0 };
      changed = true;
    }
  }
  if (!changed) return;
  _store = next;
  emit();
}

if (typeof window !== "undefined") {
  window.addEventListener("storage", (e) => {
    if (e.key !== NAV_HISTORY_STORAGE_KEY) return;
    _applyForeignStore(parseNavHistoryStore(e.newValue));
  });
}

/// Test-only / host-only: replace the in-memory snapshot (does NOT write
/// localStorage unless `persist` is true).
export function _resetNavHistoryForTests(
  state: NavHistoryState | NavHistoryStore = emptyNavHistoryStore(),
  persist = false,
): void {
  _store =
    "panes" in state ? state : { panes: { 1: state, 2: emptyNavHistory() } };
  if (persist) saveStore(_store);
}

export function getNavHistoryStore(): NavHistoryStore {
  return _store;
}

export function getNavHistoryState(pane: PaneId = 1): NavHistoryState {
  return _store.panes[pane];
}

/// Every pane's ring, merged newest-first — what `g.` and the reader start
/// card show ("where have I been", not "where has this pane been").
export function getRecentLocations(pane?: PaneId): NavLocation[] {
  if (pane) return _store.panes[pane].entries;
  return mergeEntries(_store.panes[1].entries, _store.panes[2].entries);
}

export function getRecentFiles(pane?: PaneId): RecentFile[] {
  return recentFilesFrom(getRecentLocations(pane));
}

/// The single recorder used at every navigation call site (file open,
/// gd/gr landing, search jump, G/gg/mark/omnibox). Always records at the tip
/// (resets that pane's forward half).
export function recordJump(input: {
  repo: string;
  path: string;
  line: number;
  snippet?: string;
  ts?: number;
  via?: TrailVia;
  pane?: PaneId;
}): void {
  if (!input.repo || !input.path) return;
  const pane: PaneId = input.pane ?? 1;
  const loc = makeLocation(input.repo, input.path, input.line, input.snippet ?? "", input.ts, {
    via: input.via,
    pane,
  });
  _store = { panes: { ..._store.panes, [pane]: pushEntry(_store.panes[pane], loc) } };
  saveStore(_store);
  emit();
}

/// Ctrl-o. Pass the operator's *current* cursor location so vim-style
/// "record current at tip first" works. Returns the location to navigate
/// to, or `null` if the list has nothing older.
export function goBack(current: {
  repo: string;
  path: string;
  line: number;
  snippet?: string;
  pane?: PaneId;
}): NavLocation | null {
  const pane: PaneId = current.pane ?? 1;
  const cur = makeLocation(current.repo, current.path, current.line, current.snippet ?? "", undefined, {
    pane,
  });
  const { state, target } = jumpBack(_store.panes[pane], cur);
  _store = { panes: { ..._store.panes, [pane]: state } };
  saveStore(_store);
  emit();
  return target;
}

/// `Ctrl-o` pressed on a surface that HAS NO FILE of its own — a list page,
/// the inbox, the review cockpit. There is nothing to record at the tip (the
/// vim rule assumes you are somewhere in a buffer), and stepping past the tip
/// would skip the very place the operator means: "take me back to what I was
/// reading". So this returns the ring's CURRENT entry and leaves the pointer
/// alone; a subsequent `Ctrl-o` from inside the reader then walks normally.
export function enterRing(pane: PaneId = 1): NavLocation | null {
  return _store.panes[pane].entries[_store.panes[pane].pointer] ?? null;
}

/// Ctrl-i. Returns the newer location, or `null` at the tip.
export function goForward(pane: PaneId = 1): NavLocation | null {
  const { state, target } = jumpForward(_store.panes[pane]);
  _store = { panes: { ..._store.panes, [pane]: state } };
  saveStore(_store);
  emit();
  return target;
}
