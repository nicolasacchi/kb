// T14 — grid fitting: the panel renders EXACTLY the session grid by
// `fontSize` only, never CSS `transform`/`zoom` (xterm issue 6023).
//
// The pure helpers below are unit-tested; the DOM measuring loop (a probe
// of the LOADED font, ≤4 verification iterations on `.xterm-screen`, then
// `overflow: auto` on the cropped axis) lives in `fitHost.ts`.

/**
 * `fontSize = clamp(floor(containerWidth / (cols * advanceRatio)), 10, 16)`
 * where `advanceRatio = measureText("W").width / fontSize`.
 */
export function computeFontSize(
  containerWidth: number,
  cols: number,
  advanceRatio: number,
  min = 10,
  max = 16,
): number {
  if (!(containerWidth > 0) || !(cols > 0) || !(advanceRatio > 0)) return min;
  const size = Math.floor(containerWidth / (cols * advanceRatio));
  return Math.min(max, Math.max(min, size));
}

/** Inline panel bounds (px of font size). FUTERMFULL passes its own, lower, floor. */
export const FIT_MIN_INLINE = 9;
export const FIT_MAX_INLINE = 20;

export interface FitInput {
  /** The host's content box, in CSS px. Zero or NaN means "not laid out yet". */
  width: number;
  /** The host's content box height; zero, NaN or negative means "unbounded" (width fit only). */
  height: number;
  cols: number;
  rows: number;
  /** One cell, per px of font size, measured from the LOADED font (`cellProbe.ts`). */
  cellW: number;
  cellH: number;
  min: number;
  max: number;
}

export interface FitResult {
  fontSize: number;
  /** An axis whose grid is larger than the host even at `fontSize` (the host scrolls there). */
  cropped: { x: boolean; y: boolean };
}

/**
 * The largest whole font size at which the whole grid fits BOTH axes, clamped
 * to `[min, max]`. Below the floor the grid is cropped on the axis that does
 * not fit. Total: bad geometry returns the floor and no crop, never throws.
 */
export function computeFit(i: FitInput): FitResult {
  const floor: FitResult = { fontSize: i.min, cropped: { x: false, y: false } };
  if (!(i.width > 0) || !(i.cols > 0) || !(i.cellW > 0)) return floor;
  const heightKnown = i.height > 0 && i.rows > 0 && i.cellH > 0;
  const byWidth = Math.floor(i.width / (i.cols * i.cellW));
  const byHeight = heightKnown ? Math.floor(i.height / (i.rows * i.cellH)) : Number.POSITIVE_INFINITY;
  const fontSize = Math.min(i.max, Math.max(i.min, Math.min(byWidth, byHeight)));
  // Half a pixel of slack: sub-pixel rounding is not a crop.
  return {
    fontSize,
    cropped: {
      x: i.cols * i.cellW * fontSize > i.width + 0.5,
      y: heightKnown && i.rows * i.cellH * fontSize > i.height + 0.5,
    },
  };
}
