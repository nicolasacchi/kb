// Wave E — the reader's per-repo "working set" strip: an ordered list of
// recently/pinned-open files, rendered by `components/WorkingSetStrip.tsx`
// and cycled by the vim `[f ]f` binding (`onCycleFile`, already plumbed —
// see `editor/vimKeys.ts`/`vimReader.ts`). Pure (no React, no storage I/O in
// the transform functions themselves) so the cap/eviction/cycle logic is
// unit-tested without a DOM — `hooks/useWorkingSet.ts` is the sole caller
// that wires this to React state + `sessionStorage`.
//
// Display order is STABLE insertion order (chips don't reshuffle under the
// operator as files are re-visited) — only `touchedAt` (bumped on every
// `touch`) tracks recency, purely to decide WHICH unpinned entry is the
// least-recently-used one once the unpinned cap is exceeded. `seq` is a
// monotonic counter carried in the persisted state itself (not
// `Date.now()`), so eviction order is deterministic and testable without
// fake timers.

export interface WorkingSetEntry {
  /// Repo-relative path — the working set's own identity key (one entry per
  /// distinct path; re-touching an already-present path never duplicates).
  path: string;
  /// The `seq` stamp of this entry's most recent `touch()` — NOT wall-clock
  /// time. Used only to rank unpinned entries for eviction (lowest = least
  /// recently used).
  touchedAt: number;
  /// `null` = unpinned (subject to the cap's LRU eviction). A non-null value
  /// is the `seq` stamp of the `pin()` call — its magnitude is otherwise
  /// unused (presence is what matters), but stamping it (rather than a bare
  /// boolean) keeps the type shape uniform with `touchedAt`.
  pinnedAt: number | null;
}

export interface WorkingSetState {
  /// Stable insertion order — see the module doc.
  entries: WorkingSetEntry[];
  /// Monotonic counter, bumped by every `touch`/`pin` call.
  seq: number;
}

/// Unpinned entries beyond this count are evicted, oldest-`touchedAt` first
/// — pinned entries are exempt and never counted against the cap.
export const MAX_UNPINNED = 12;

export function emptyWorkingSet(): WorkingSetState {
  return { entries: [], seq: 0 };
}

function evictIfNeeded(state: WorkingSetState): WorkingSetState {
  const unpinned = state.entries.filter((e) => e.pinnedAt === null);
  if (unpinned.length <= MAX_UNPINNED) return state;
  const overflow = unpinned.length - MAX_UNPINNED;
  const lruFirst = [...unpinned].sort((a, b) => a.touchedAt - b.touchedAt);
  const evictPaths = new Set(lruFirst.slice(0, overflow).map((e) => e.path));
  return { ...state, entries: state.entries.filter((e) => !evictPaths.has(e.path)) };
}

/// Record that `path` was just opened — adds it (at the end of the stable
/// order) if new, or just bumps its `touchedAt` in place if already present.
/// Called on EVERY file open (either pane) per the milestone brief. May
/// evict the least-recently-touched unpinned entry if this pushes the
/// unpinned count past `MAX_UNPINNED`.
export function touch(state: WorkingSetState, path: string): WorkingSetState {
  const seq = state.seq + 1;
  const idx = state.entries.findIndex((e) => e.path === path);
  const entries =
    idx === -1
      ? [...state.entries, { path, touchedAt: seq, pinnedAt: null }]
      : state.entries.map((e, i) => (i === idx ? { ...e, touchedAt: seq } : e));
  return evictIfNeeded({ entries, seq });
}

/// Pin `path` — exempts it from the unpinned cap. A no-op if `path` isn't in
/// the working set, or is already pinned.
export function pin(state: WorkingSetState, path: string): WorkingSetState {
  const idx = state.entries.findIndex((e) => e.path === path);
  if (idx === -1 || state.entries[idx].pinnedAt !== null) return state;
  const seq = state.seq + 1;
  const entries = state.entries.map((e, i) => (i === idx ? { ...e, pinnedAt: seq } : e));
  return { entries, seq };
}

/// Unpin `path`. A no-op if `path` isn't in the working set, or isn't
/// pinned. May immediately evict it (and/or other stale unpinned entries) if
/// the unpinned count is already over the cap.
export function unpin(state: WorkingSetState, path: string): WorkingSetState {
  const idx = state.entries.findIndex((e) => e.path === path);
  if (idx === -1 || state.entries[idx].pinnedAt === null) return state;
  const entries = state.entries.map((e, i) => (i === idx ? { ...e, pinnedAt: null } : e));
  return evictIfNeeded({ ...state, entries });
}

/// Explicit removal (the strip's "✕") — drops `path` regardless of pinned
/// state. A no-op if it isn't present.
export function remove(state: WorkingSetState, path: string): WorkingSetState {
  if (!state.entries.some((e) => e.path === path)) return state;
  return { ...state, entries: state.entries.filter((e) => e.path !== path) };
}

/// V71-K2 — lay the strip out in `paths`' order. The ONE operation that may
/// MOVE an existing entry, and its only caller is a workspace RESTORE
/// (`routes/Reader.tsx`'s `?workspace=` effect).
///
/// V70-A10 ("Workspaces v0", D26) said restore "opens every saved entry into
/// the working set, IN ORDER" and implemented it as a `touch()` loop over the
/// workspace's saved spans. That loop cannot do it, by this module's own
/// design: display order is STABLE INSERTION order and `touch` on a path
/// already present deliberately never moves it (the whole point — chips must
/// not reshuffle under the operator as files are re-visited). The restored
/// URL names the saved FOCUSED pane's file, so the reader's own file-open
/// effect inserts THAT path first and the loop can only append the rest
/// behind it. So the saved order was never actually restored whenever the
/// focused pane was not also the first saved entry — not cosmetic, because
/// `lib/workspaceDirty.ts` compares the live order against the saved one:
/// the workspace read DIRTY the instant it opened, having changed nothing.
///
/// Entries carry their existing `touchedAt`/`pinnedAt` across the move (a
/// restore is not a visit and must not re-rank anything for eviction); a path
/// in `paths` not yet present is inserted at its named position; and a live
/// entry `paths` does NOT name keeps its relative order AFTER them rather
/// than being dropped — restoring a workspace adds its files, it never closes
/// yours. Duplicates in `paths` collapse to the first occurrence, so the
/// result is always one entry per path.
export function reorder(state: WorkingSetState, paths: readonly string[]): WorkingSetState {
  const byPath = new Map(state.entries.map((e) => [e.path, e]));
  const placed = new Set<string>();
  const entries: WorkingSetEntry[] = [];
  let seq = state.seq;
  for (const path of paths) {
    if (placed.has(path)) continue;
    placed.add(path);
    const existing = byPath.get(path);
    if (existing) entries.push(existing);
    else entries.push({ path, touchedAt: (seq += 1), pinnedAt: null });
  }
  for (const e of state.entries) if (!placed.has(e.path)) entries.push(e);
  return evictIfNeeded({ entries, seq });
}

/// `[f`/`]f` — the next/previous path in stable display order, wrapping at
/// either end. `current` is the focused pane's own open path; if it isn't
/// (yet) a working-set member — shouldn't normally happen, since `touch` is
/// called on every open, but stay honest about the possibility — lands on
/// the first entry for `dir: 1` or the last for `dir: -1`. Returns `null`
/// when the working set is empty (nothing to cycle to).
export function cycle(state: WorkingSetState, current: string | undefined, dir: -1 | 1): string | null {
  const { entries } = state;
  if (entries.length === 0) return null;
  const idx = current !== undefined ? entries.findIndex((e) => e.path === current) : -1;
  if (idx === -1) return dir === 1 ? entries[0].path : entries[entries.length - 1].path;
  const next = (idx + dir + entries.length) % entries.length;
  return entries[next].path;
}

export interface SplitPath {
  /// The parent directory, no trailing slash (`""` for a root-level file).
  dir: string;
  /// The final path segment (file name).
  base: string;
}

/// Split a repo-relative path into a dimmed "directory" part and the
/// prominent basename — `WorkingSetStrip`'s chip rendering (`dirname
/// dimmed`).
export function splitPath(path: string): SplitPath {
  const slash = path.lastIndexOf("/");
  return slash === -1 ? { dir: "", base: path } : { dir: path.slice(0, slash), base: path.slice(slash + 1) };
}

// --- sessionStorage persistence -------------------------------------------
//
// `kbc:ws:<repo>` — scoped per repo (splits/working sets don't cross repos,
// same constraint `PaneLoc` carries no `repo` field for). `sessionStorage`
// (not `localStorage`, unlike `lib/prefs.ts`'s last-repo pref): a working
// set is a per-tab, per-visit scratch list, not a durable cross-session
// preference.

function storageKey(repo: string): string {
  return `kbc:ws:${repo}`;
}

function isEntry(v: unknown): v is WorkingSetEntry {
  if (!v || typeof v !== "object") return false;
  const e = v as Record<string, unknown>;
  return (
    typeof e.path === "string" &&
    typeof e.touchedAt === "number" &&
    (e.pinnedAt === null || typeof e.pinnedAt === "number")
  );
}

export function loadWorkingSet(repo: string): WorkingSetState {
  try {
    const raw = sessionStorage.getItem(storageKey(repo));
    if (!raw) return emptyWorkingSet();
    const parsed: unknown = JSON.parse(raw);
    if (!parsed || typeof parsed !== "object") return emptyWorkingSet();
    const p = parsed as Record<string, unknown>;
    if (!Array.isArray(p.entries) || typeof p.seq !== "number") return emptyWorkingSet();
    const entries = p.entries.filter(isEntry);
    return { entries, seq: p.seq };
  } catch {
    return emptyWorkingSet();
  }
}

export function saveWorkingSet(repo: string, state: WorkingSetState): void {
  try {
    sessionStorage.setItem(storageKey(repo), JSON.stringify(state));
  } catch {
    // best-effort — a denied/full sessionStorage just doesn't persist.
  }
}
