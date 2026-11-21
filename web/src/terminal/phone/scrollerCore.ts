// T17 — the pure core of the phone scroller (no DOM, no xterm).
//
// xterm.js 6.0.0 has no touch handling, so the phone view drives
// `term.scrollLines` itself from pointer events. Everything that can be
// decided without a DOM lives here so it is unit-testable in node. The
// constants are the values measured in the phone spike (a 10-line drag
// scrolled exactly 10 lines; a 240 px fling scrolled 68-69 lines).

/** Axis lock distance: no gesture axis until the finger has travelled this far. */
export const LOCK_PX = 8;
/** Momentum decay time constant, ms. */
export const TAU_MS = 325;
/** px/ms: start momentum above this release velocity. */
export const FLING_V = 0.15;
/** The finger must have moved within this window at release for a fling. */
export const FLING_IDLE_MS = 80;
/** Samples kept for the release velocity. */
export const VELOCITY_WINDOW_MS = 100;
/** px/ms: stop momentum below this. */
export const STOP_V = 0.02;
export const FONT_STEPS = [8, 9, 10, 11, 12, 13, 14, 16, 18, 20, 22, 24] as const;
export const DEFAULT_FONT = 12;
export const LONG_PRESS_MS = 500;
/** A tap travels under this many px (FUPHONELINKS)... */
export const TAP_MAX_PX = 10;
/** ...and lasts under this many ms (past LONG_PRESS_MS, so a long press is never a tap). */
export const TAP_MAX_MS = 550;

export type Axis = "h" | "v";
export interface VelocitySample {
  t: number;
  y: number;
}

/** null while the travel is below LOCK_PX; ties between axes go to "v". */
export function lockAxis(dx: number, dy: number): Axis | null {
  if (Math.hypot(dx, dy) < LOCK_PX) return null;
  return Math.abs(dx) > Math.abs(dy) ? "h" : "v";
}

/** Whole lines from an accumulated pixel travel; the remainder is carried. */
export function accumulateLines(
  acc: number,
  dy: number,
  cellH: number,
): { lines: number; acc: number } {
  if (!(cellH > 0)) return { lines: 0, acc: acc + dy };
  const total = acc + dy;
  const lines = Math.trunc(total / cellH);
  return { lines, acc: total - lines * cellH };
}

export function releaseVelocity(
  samples: VelocitySample[],
  now: number,
): { v: number; idle: number; fling: boolean } {
  if (samples.length === 0) return { v: 0, idle: 0, fling: false };
  const first = samples[0];
  const last = samples[samples.length - 1];
  const v = last.t > first.t ? (last.y - first.y) / (last.t - first.t) : 0;
  const idle = now - last.t;
  return { v, idle, fling: Math.abs(v) > FLING_V && idle < FLING_IDLE_MS };
}

/** Drop samples older than the window, but never below two. */
export function trimSamples(samples: VelocitySample[], now: number): void {
  while (samples.length > 2 && now - samples[0].t > VELOCITY_WINDOW_MS) samples.shift();
}

export function momentumStep(v: number, dt: number): number {
  return v * Math.exp(-dt / TAU_MS);
}

/** Nearest of FONT_STEPS; the first one wins a tie. */
export function nearestFontStep(px: number): number {
  let best: number = FONT_STEPS[0];
  for (const s of FONT_STEPS) if (Math.abs(s - px) < Math.abs(best - px)) best = s;
  return best;
}

export function pinchFont(startFont: number, scale: number): number {
  return nearestFontStep(startFont * scale);
}

/** One step through FONT_STEPS (clamped at the ends) from a current size. */
export function stepFont(current: number, dir: 1 | -1): number {
  const idx = FONT_STEPS.indexOf(nearestFontStep(current) as (typeof FONT_STEPS)[number]);
  const next = Math.min(FONT_STEPS.length - 1, Math.max(0, idx + dir));
  return FONT_STEPS[next];
}

/** A still tap: under TAP_MAX_PX of travel and under TAP_MAX_MS of contact. */
export function isStillTap(dx: number, dy: number, ms: number): boolean {
  return Math.hypot(dx, dy) < TAP_MAX_PX && ms < TAP_MAX_MS;
}

export interface ScreenRect {
  left: number;
  top: number;
  width: number;
  height: number;
}

/**
 * The cell under a point: a 1-based column and a 0-based screen row, or null
 * when the point is outside the rendered screen (the rect already carries any
 * horizontal pan, since it is the live bounding box).
 */
export function cellAt(
  x: number,
  y: number,
  rect: ScreenRect,
  cols: number,
  rows: number,
): { x: number; row: number } | null {
  if (rect.width <= 0 || rect.height <= 0 || cols <= 0 || rows <= 0) return null;
  const cx = Math.floor((x - rect.left) / (rect.width / cols));
  const cy = Math.floor((y - rect.top) / (rect.height / rows));
  if (cx < 0 || cx >= cols || cy < 0 || cy >= rows) return null;
  return { x: cx + 1, row: cy };
}
