/// Pure keyboard-cursor reducer for the full-page review diff (V4.D3).
/// Hunk counts are INJECTED per call (from parsed diffs). Unfetched files
/// count as 0 hunks, so hunk-stepping becomes file-level stepping.
///
/// Collapse does NOT skip files: `collapsed` only affects rendering. The
/// cursor still lands on a collapsed file (nextFile / nextHunk / gotoFile)
/// so `v` / `x` / viewed-advance keep addressing the file the operator
/// stepped onto.
///
/// PRR-U3 adds `t`/`T` thread-or-finding stepping (design-ui.md §6) as a
/// SEPARATE pure stop-list walk (`buildThreadStops` / `stepThreadStop`) —
/// deliberately not folded into `DiffKeysState`'s file/hunk cursor, since
/// thread stops are keyed by thread id (not a file/hunk index pair) and
/// come from async-loaded comment/finding data, the same "counts injected
/// per call" posture `hunkCounts` already uses for hunks.

import { severityRank, threadVisibleInOverlay, type OverlayMode } from "./diffFindings";

export interface DiffKeysCursor {
  fileIdx: number;
  hunkIdx: number;
}

export interface DiffKeysState {
  files: string[];
  cursor: DiffKeysCursor;
  collapsed: Set<string>;
}

export type DiffKeysAction =
  | { type: "nextHunk" }
  | { type: "prevHunk" }
  | { type: "nextFile" }
  | { type: "prevFile" }
  | { type: "firstFile" }
  | { type: "lastFile" }
  | { type: "toggleCollapse" }
  | { type: "gotoFile"; fileIdx: number }
  | { type: "setFiles"; files: string[] };

export function initialDiffKeysState(files: string[] = []): DiffKeysState {
  return {
    files,
    cursor: { fileIdx: 0, hunkIdx: 0 },
    collapsed: new Set(),
  };
}

function hunkCountAt(hunkCounts: readonly number[], idx: number): number {
  return hunkCounts[idx] ?? 0;
}

function lastHunkIdx(hunkCounts: readonly number[], fileIdx: number): number {
  const n = hunkCountAt(hunkCounts, fileIdx);
  return n > 0 ? n - 1 : 0;
}

function clampFileIdx(files: readonly string[], idx: number): number {
  if (files.length === 0) return 0;
  if (idx < 0) return 0;
  if (idx > files.length - 1) return files.length - 1;
  return idx;
}

/// Wrap-at-ends CLAMPS — next/prev never wrap to the other end.
export function reduceDiffKeys(
  state: DiffKeysState,
  action: DiffKeysAction,
  hunkCounts: readonly number[] = [],
): DiffKeysState {
  const { files, cursor } = state;
  const fileIdx = clampFileIdx(files, cursor.fileIdx);
  const hunkIdx = Math.max(0, cursor.hunkIdx);

  switch (action.type) {
    case "setFiles": {
      const prevPath = files[fileIdx];
      const nextFiles = action.files;
      const kept = prevPath !== undefined ? nextFiles.indexOf(prevPath) : -1;
      return {
        ...state,
        files: nextFiles,
        cursor: { fileIdx: kept >= 0 ? kept : 0, hunkIdx: 0 },
      };
    }
    case "nextHunk": {
      if (files.length === 0) return state;
      const n = hunkCountAt(hunkCounts, fileIdx);
      if (n > 0 && hunkIdx < n - 1) {
        return { ...state, cursor: { fileIdx, hunkIdx: hunkIdx + 1 } };
      }
      // Current file's hunks exhausted (or unfetched / 0) → next file.
      if (fileIdx < files.length - 1) {
        return { ...state, cursor: { fileIdx: fileIdx + 1, hunkIdx: 0 } };
      }
      return state;
    }
    case "prevHunk": {
      if (files.length === 0) return state;
      if (hunkIdx > 0) {
        return { ...state, cursor: { fileIdx, hunkIdx: hunkIdx - 1 } };
      }
      if (fileIdx > 0) {
        const prev = fileIdx - 1;
        return { ...state, cursor: { fileIdx: prev, hunkIdx: lastHunkIdx(hunkCounts, prev) } };
      }
      return state;
    }
    case "nextFile": {
      if (files.length === 0 || fileIdx >= files.length - 1) return state;
      return { ...state, cursor: { fileIdx: fileIdx + 1, hunkIdx: 0 } };
    }
    case "prevFile": {
      if (files.length === 0 || fileIdx <= 0) return state;
      return { ...state, cursor: { fileIdx: fileIdx - 1, hunkIdx: 0 } };
    }
    case "firstFile": {
      if (files.length === 0) return state;
      return { ...state, cursor: { fileIdx: 0, hunkIdx: 0 } };
    }
    case "lastFile": {
      if (files.length === 0) return state;
      return { ...state, cursor: { fileIdx: files.length - 1, hunkIdx: 0 } };
    }
    case "gotoFile": {
      if (files.length === 0) return state;
      const next = clampFileIdx(files, action.fileIdx);
      if (next === fileIdx && hunkIdx === 0) return state;
      return { ...state, cursor: { fileIdx: next, hunkIdx: 0 } };
    }
    case "toggleCollapse": {
      const path = files[fileIdx];
      if (!path) return state;
      const collapsed = new Set(state.collapsed);
      if (collapsed.has(path)) collapsed.delete(path);
      else collapsed.add(path);
      return { ...state, collapsed };
    }
    default:
      return state;
  }
}

export function nextUnviewedFileIdx(
  files: readonly string[],
  viewed: ReadonlySet<string>,
  fromIdx: number,
): number | null {
  for (let i = fromIdx + 1; i < files.length; i++) {
    if (!viewed.has(files[i])) return i;
  }
  return null;
}

// --- PRR-U3 — t/T "thread-or-finding" stepping ----------------------------

export interface ThreadStop {
  /// Thread id (== `ReviewFinding.annotation_id` when the stop is a finding).
  id: string;
  fileIdx: number;
  path: string;
}

/// Build the ordered t/T stop list: files in `paths` order; within a file,
/// findings first (most severe first, stable for ties) then plain comments,
/// each group in the server's own return order. Overlay-filtered via
/// `threadVisibleInOverlay` — a stop invisible under the active overlay is
/// never a landing target.
export function buildThreadStops(
  paths: readonly string[],
  threadsByPath: ReadonlyMap<string, readonly { id: string }[]>,
  findingSeverityByThreadId: ReadonlyMap<string, string>,
  overlay: OverlayMode,
): ThreadStop[] {
  const stops: ThreadStop[] = [];
  paths.forEach((path, fileIdx) => {
    const threads = threadsByPath.get(path) ?? [];
    const visible = threads.filter((t) =>
      threadVisibleInOverlay(findingSeverityByThreadId.has(t.id), overlay),
    );
    const findingThreads = visible.filter((t) => findingSeverityByThreadId.has(t.id));
    const commentThreads = visible.filter((t) => !findingSeverityByThreadId.has(t.id));
    findingThreads.sort(
      (a, b) =>
        severityRank(findingSeverityByThreadId.get(a.id)) -
        severityRank(findingSeverityByThreadId.get(b.id)),
    );
    for (const t of [...findingThreads, ...commentThreads]) {
      stops.push({ id: t.id, fileIdx, path });
    }
  });
  return stops;
}

/// `t` (dir 1) / `T` (dir -1) over `stops`. No `currentId` (or a stale one
/// not in `stops`) lands on the first stop going forward / the last stop
/// going backward. Wrap-at-ends CLAMPS (matches `reduceDiffKeys`'s own
/// convention) — stepping past either end is a no-op landing on the same
/// stop, never a wrap to the other side.
export function stepThreadStop(
  stops: readonly ThreadStop[],
  currentId: string | null,
  dir: 1 | -1,
): ThreadStop | null {
  if (stops.length === 0) return null;
  if (currentId == null) return dir === 1 ? stops[0] : stops[stops.length - 1];
  const idx = stops.findIndex((s) => s.id === currentId);
  if (idx === -1) return dir === 1 ? stops[0] : stops[stops.length - 1];
  const next = idx + dir;
  if (next < 0 || next >= stops.length) return stops[idx];
  return stops[next];
}
