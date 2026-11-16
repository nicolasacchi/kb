// T18 — "Fit to phone": the grid size that fills the visible Watch area.
//
// Pure arithmetic over measurements the phone view already has (T17's
// `.xterm-screen` cell measure and the wrapper boxes). The pty is resized only
// by the explicit action that calls this; watching, the composer and the soft
// keyboard never do.

export interface PhoneMeasure {
  /** Width of the horizontally pannable wrapper (what the eye sees). */
  visibleWidth: number;
  /** Height the Watch area is allocated (above the Drive dock). */
  visibleHeight: number;
  /** One cell, in CSS px, from the rendered screen. */
  cellWidth: number;
  cellHeight: number;
}

/** `cols = floor(visibleWidth / cellWidth)`, `rows = floor(visibleHeight / cellHeight)`; null if unmeasurable. */
export function fitDims(m: PhoneMeasure | null): { cols: number; rows: number } | null {
  if (m === null) return null;
  const ok = (n: number): boolean => Number.isFinite(n) && n > 0;
  if (!ok(m.visibleWidth) || !ok(m.visibleHeight) || !ok(m.cellWidth) || !ok(m.cellHeight)) return null;
  const cols = Math.floor(m.visibleWidth / m.cellWidth);
  const rows = Math.floor(m.visibleHeight / m.cellHeight);
  if (cols < 1 || rows < 1) return null;
  return { cols, rows };
}
