// V76-R2b — persisted width of the review-diff file-map pane.
//
// Same posture as `desk/deskState.ts`: this module reads/writes NO ambient
// (`window`, `localStorage`) directly. `load`/`save` take an explicit
// `StorageLike` so the unit suite (node environment) can exercise every
// branch, including a corrupt blob, without a DOM. `react-resizable-panels`
// is the drag mechanism; this file is the source of truth. No `autoSaveId`.

export interface StorageLike {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
}

export const REVIEW_MAP_WIDTH_KEY = "kbc:review-map-width";
/// Percent of the mapped body. Matches the pre-V76 CSS `22vw` stop.
export const REVIEW_MAP_WIDTH_DEFAULT = 22;
export const REVIEW_MAP_WIDTH_MIN = 12;
export const REVIEW_MAP_WIDTH_MAX = 48;

export function clampMapWidth(n: number): number {
  if (!Number.isFinite(n)) return REVIEW_MAP_WIDTH_DEFAULT;
  return Math.min(REVIEW_MAP_WIDTH_MAX, Math.max(REVIEW_MAP_WIDTH_MIN, Math.round(n)));
}

export function loadMapWidth(storage: StorageLike): number {
  const raw = storage.getItem(REVIEW_MAP_WIDTH_KEY);
  if (raw == null || raw === "") return REVIEW_MAP_WIDTH_DEFAULT;
  try {
    const parsed: unknown = JSON.parse(raw);
    if (typeof parsed === "number") return clampMapWidth(parsed);
    if (parsed && typeof parsed === "object" && "width" in parsed) {
      const w = (parsed as { width: unknown }).width;
      if (typeof w === "number") return clampMapWidth(w);
    }
  } catch {
    // corrupt blob → default, same recovery deskState uses
  }
  return REVIEW_MAP_WIDTH_DEFAULT;
}

export function saveMapWidth(storage: StorageLike, width: number): void {
  storage.setItem(REVIEW_MAP_WIDTH_KEY, JSON.stringify({ width: clampMapWidth(width) }));
}
