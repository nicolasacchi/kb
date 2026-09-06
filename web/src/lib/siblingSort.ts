// Pure sort + date helpers for the reader Folder sidebar (sibling files).
// Client-side only: the slim docs page is already capped at 500 and the
// inspector re-orders in the browser (PreviewInspector folderRows).

export type SiblingSortKey = "name" | "title" | "updated" | "created";

export type SiblingSortable = {
  filename: string;
  title: string;
  mtime: number | null;
  created?: number | null;
  /// v0.33 Y2 — first time the daemon indexed this row (`first_indexed_unix`).
  /// Stable across edits; used as the mid-chain fallback for "created" sort
  /// so an mtime bump no longer reshuffles sibling order.
  firstIndexed?: number | null;
};

const SORT_KEYS: readonly SiblingSortKey[] = [
  "name",
  "title",
  "updated",
  "created",
];

/// Accept known keys; unknown / missing / legacy orphan values → "updated"
/// (the NEW-user default). Stored "name"/"title" prefs keep working.
export function parseSiblingSort(v: unknown): SiblingSortKey {
  return SORT_KEYS.includes(v as SiblingSortKey)
    ? (v as SiblingSortKey)
    : "updated";
}

function timeDesc(
  a: number | null | undefined,
  b: number | null | undefined,
): number {
  // Missing times sink on desc (match gallery sort.ts timeOf). Equal —
  // including both-missing (-Inf - -Inf would be NaN) — defers to the
  // caller's filename tiebreak.
  const av = a ?? Number.NEGATIVE_INFINITY;
  const bv = b ?? Number.NEGATIVE_INFINITY;
  if (av === bv) return 0;
  return bv - av;
}

/// created_unix with stable first-indexed then mtime fallbacks (btime-less
/// filesystems / pre-v0.15 rows / pre-seed first_indexed). Chain order:
/// `created` (btime/kb-created) → `firstIndexed` → `mtime`.
export function createdOrMtime(row: SiblingSortable): number | null {
  return row.created ?? row.firstIndexed ?? row.mtime ?? null;
}

/// Comparator for the Folder sidebar. updated/created = desc; name/title =
/// asc. Stable tiebreak is always filename localeCompare.
export function siblingComparator(
  sort: SiblingSortKey,
): (a: SiblingSortable, b: SiblingSortable) => number {
  return (a, b) => {
    let primary = 0;
    switch (sort) {
      case "name":
        primary = a.filename.localeCompare(b.filename);
        break;
      case "title":
        primary = (a.title || a.filename).localeCompare(
          b.title || b.filename,
          undefined,
          { sensitivity: "base" },
        );
        break;
      case "updated":
        primary = timeDesc(a.mtime, b.mtime);
        break;
      case "created":
        primary = timeDesc(createdOrMtime(a), createdOrMtime(b));
        break;
    }
    if (primary !== 0) return primary;
    return a.filename.localeCompare(b.filename);
  };
}

/// Unix seconds to show on a row: the active sort key's value when that
/// key is updated/created, else mtime. created falls back to mtime.
export function siblingDateUnix(
  row: SiblingSortable,
  sort: SiblingSortKey,
): number | null {
  if (sort === "created") return createdOrMtime(row);
  return row.mtime ?? null;
}
