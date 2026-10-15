// OL L5 — the Index view's column model. Pure: which columns exist, which are
// visible by default, how the visible set becomes a CSS grid template, how a
// header click maps onto the EXISTING gallery `sort`/`dir` params (no new URL
// grammar, see galleryUrl.ts), and how the per-kb choice is (de)serialised.
import { defaultDir, type SortDir, type SortKey } from "./sort";

export type ListColumnId =
  | "title"
  | "folder"
  | "category"
  | "words"
  | "links"
  | "refs"
  | "kept"
  | "caps"
  | "read"
  | "age";

export type ListColumn = {
  id: ListColumnId;
  label: string;
  /// CSS grid track. Title is the only flexible one.
  track: string;
  align: "left" | "right";
  /// The gallery sort key a header click drives, or null when the server /
  /// client sort grammar has no such key (the header is then a plain label).
  sort: SortKey | null;
  defaultVisible: boolean;
  /// Title cannot be hidden: it is the row's link target.
  locked?: boolean;
};

export const LIST_COLUMNS: readonly ListColumn[] = [
  { id: "title", label: "Title", track: "minmax(0, 1fr)", align: "left", sort: "title", defaultVisible: true, locked: true },
  { id: "folder", label: "Folder", track: "minmax(80px, 180px)", align: "left", sort: null, defaultVisible: true },
  { id: "category", label: "Category", track: "96px", align: "left", sort: null, defaultVisible: false },
  { id: "words", label: "Words", track: "64px", align: "right", sort: "words", defaultVisible: true },
  { id: "links", label: "Links", track: "52px", align: "right", sort: null, defaultVisible: false },
  { id: "refs", label: "Refs", track: "52px", align: "right", sort: null, defaultVisible: false },
  { id: "kept", label: "Kept", track: "52px", align: "right", sort: null, defaultVisible: false },
  { id: "caps", label: "Capabilities", track: "112px", align: "left", sort: null, defaultVisible: false },
  { id: "read", label: "Read", track: "64px", align: "right", sort: null, defaultVisible: true },
  { id: "age", label: "Age", track: "56px", align: "right", sort: "recent", defaultVisible: true },
];

const IDS = new Set<string>(LIST_COLUMNS.map((c) => c.id));

export function defaultColumns(): ListColumnId[] {
  return LIST_COLUMNS.filter((c) => c.defaultVisible).map((c) => c.id);
}

/// Canonicalise any input (storage payload, toggle result) into the visible
/// set: unknown ids dropped, duplicates dropped, title forced on, and the
/// order always the model's own (the chooser toggles, it never reorders).
export function normalizeColumns(input: unknown): ListColumnId[] {
  const want = new Set<string>(
    Array.isArray(input) ? input.filter((x): x is string => typeof x === "string" && IDS.has(x)) : [],
  );
  return LIST_COLUMNS.filter((c) => c.locked || want.has(c.id)).map((c) => c.id);
}

export function toggleColumn(visible: readonly ListColumnId[], id: ListColumnId): ListColumnId[] {
  const col = LIST_COLUMNS.find((c) => c.id === id);
  if (!col || col.locked) return normalizeColumns(visible);
  return normalizeColumns(
    visible.includes(id) ? visible.filter((v) => v !== id) : [...visible, id],
  );
}

export function columnsStorageKey(kb: string): string {
  return `kb:list-columns:${kb}`;
}

export function serializeColumns(visible: readonly ListColumnId[]): string {
  return JSON.stringify(normalizeColumns(visible));
}

/// Total: absent / corrupt / wrong-shaped storage falls back to the defaults.
export function parseColumns(raw: string | null | undefined): ListColumnId[] {
  if (!raw) return defaultColumns();
  try {
    const v: unknown = JSON.parse(raw);
    return Array.isArray(v) ? normalizeColumns(v) : defaultColumns();
  } catch {
    return defaultColumns();
  }
}

/// Trailing fixed track for the per-row download button.
const ACTION_TRACK = "28px";

export function gridTemplate(visible: readonly ListColumnId[]): string {
  const tracks = LIST_COLUMNS.filter((c) => visible.includes(c.id)).map((c) => c.track);
  return [...tracks, ACTION_TRACK].join(" ");
}

/// What a header click does to `sort`/`dir`. Clicking the active sort flips
/// the direction; clicking another sortable header selects it at its default
/// direction. Returns null for a header with no sort key.
export function nextSort(
  col: ListColumn,
  current: SortKey,
  currentDir: SortDir,
): { sort: SortKey; dir: SortDir } | null {
  if (!col.sort) return null;
  if (col.sort === current) {
    return { sort: col.sort, dir: currentDir === "asc" ? "desc" : "asc" };
  }
  return { sort: col.sort, dir: defaultDir(col.sort) };
}

export function ariaSort(
  col: ListColumn,
  current: SortKey,
  dir: SortDir,
): "ascending" | "descending" | "none" | undefined {
  if (!col.sort) return undefined;
  if (col.sort !== current) return "none";
  return dir === "asc" ? "ascending" : "descending";
}
