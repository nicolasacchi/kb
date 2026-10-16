// Semantic zoom: the Library's density ladder and the pure gesture accumulator
// behind pinch / ctrl-wheel. The lens is still `?view=`; nothing here touches
// the URL, the DOM or a timer, so every rule is unit-tested over plain numbers.

/// Most overview first. Timeline, Canvas and the Front Page are different axes
/// and are deliberately not on the ladder.
export const ZOOM_LADDER = ["atlas", "list", "grid"] as const;
export type ZoomView = (typeof ZOOM_LADDER)[number];

/// "in" = fewer, richer things (toward the Shelf); "out" = more, smaller (toward the Atlas).
export type ZoomDir = "in" | "out";

export const ZOOM_LABEL: Record<ZoomView, string> = { atlas: "Atlas", list: "Index", grid: "Shelf" };

export function ladderIndex(view: string): number {
  return (ZOOM_LADDER as readonly string[]).indexOf(view);
}

/// The view one step along the ladder, or null when `view` is off the ladder
/// or already at that end (so callers can leave the browser's own zoom alone).
export function nextView(view: string, dir: ZoomDir): ZoomView | null {
  const i = ladderIndex(view);
  if (i < 0) return null;
  return ZOOM_LADDER[i + (dir === "in" ? 1 : -1)] ?? null;
}

/// Summed |deltaY| (pixels) that makes one step.
export const ZOOM_THRESHOLD = 40;
/// A gesture is over after this much quiet; also the window in which the
/// tail of a step's own gesture is swallowed.
export const ZOOM_SETTLE_MS = 350;

export type ZoomGesture = {
  /// Signed accumulated deltaY since the gesture started.
  acc: number;
  /// Timestamp of the previous event, for the quiet-gap reset.
  last: number;
  /// Events before this are the tail of a gesture that already stepped.
  settledUntil: number;
};

export const ZOOM_IDLE: ZoomGesture = { acc: 0, last: -Infinity, settledUntil: -Infinity };

export type ZoomResult = {
  state: ZoomGesture;
  /// "step": move one rung; "swallow": preventDefault, no step; "pass": leave the event alone.
  action: "step" | "swallow" | "pass";
  dir?: ZoomDir;
};

/// Feed one ctrl-wheel event. `deltaY < 0` is a pinch-out / ctrl-wheel-up,
/// which zooms IN (toward the Shelf). `canStep(dir)` says whether the ladder
/// has a rung in that direction. At most one step per gesture: after a step,
/// every event until `ZOOM_SETTLE_MS` of quiet is swallowed (even at the end of
/// the ladder, so the tail of a pinch never turns into browser page-zoom).
export function feedWheel(
  s: ZoomGesture,
  deltaY: number,
  now: number,
  canStep: (dir: ZoomDir) => boolean,
): ZoomResult {
  if (now < s.settledUntil) {
    return { state: { acc: 0, last: now, settledUntil: now + ZOOM_SETTLE_MS }, action: "swallow" };
  }
  const dir: ZoomDir = deltaY < 0 ? "in" : "out";
  if (deltaY === 0 || !canStep(dir)) {
    return { state: { acc: 0, last: now, settledUntil: s.settledUntil }, action: "pass" };
  }
  const fresh = now - s.last > ZOOM_SETTLE_MS || Math.sign(s.acc) !== Math.sign(deltaY);
  const acc = (fresh ? 0 : s.acc) + deltaY;
  if (Math.abs(acc) >= ZOOM_THRESHOLD) {
    return { state: { acc: 0, last: now, settledUntil: now + ZOOM_SETTLE_MS }, action: "step", dir };
  }
  return { state: { acc, last: now, settledUntil: s.settledUntil }, action: "swallow", dir };
}
