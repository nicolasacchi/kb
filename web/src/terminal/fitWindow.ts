// FUTERMFIT — "Fit to window": the grid (cols x rows) that fills the panel's
// host at a legible reference font. Pure; the caller posts the existing
// `take-size` with the result, only ever from an explicit click.

/** The reference font the window is measured at, in px. */
export const FIT_REF_FONT = 14;

/** The daemon's bounds (`take-size` refuses anything outside them). */
export const GRID_BOUNDS = { minCols: 20, maxCols: 500, minRows: 5, maxRows: 200 } as const;

export interface WindowMeasure {
  width: number;
  height: number;
  /** One cell per px of font size (`cellProbe.ts`). */
  cellW: number;
  cellH: number;
}

const clamp = (n: number, lo: number, hi: number): number => Math.min(hi, Math.max(lo, n));

/** `null` when the window cannot be measured (zero, negative or non-finite box). */
export function fitWindowDims(m: WindowMeasure | null, font: number = FIT_REF_FONT): { cols: number; rows: number } | null {
  if (m === null) return null;
  const ok = (n: number): boolean => Number.isFinite(n) && n > 0;
  if (!ok(m.width) || !ok(m.height) || !ok(m.cellW) || !ok(m.cellH) || !ok(font)) return null;
  return {
    cols: clamp(Math.floor(m.width / (m.cellW * font)), GRID_BOUNDS.minCols, GRID_BOUNDS.maxCols),
    rows: clamp(Math.floor(m.height / (m.cellH * font)), GRID_BOUNDS.minRows, GRID_BOUNDS.maxRows),
  };
}
