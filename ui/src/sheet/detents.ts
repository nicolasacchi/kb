// Pure detent machine for the shared Sheet. No DOM, no React.

export type Detent = "closed" | "peek" | "full";

/** Velocity (px/ms) beyond which a drag release is a fling and forces direction. */
export const FLING_VELOCITY = 0.5;

/**
 * Sign convention (screen space, y grows downward):
 * - `offset` (px): how far the sheet's top edge has been dragged from the
 *   current detent's resting position. POSITIVE = dragged DOWN (sheet shorter),
 *   NEGATIVE = dragged UP (sheet taller).
 * - `velocity` (px/ms) at release: POSITIVE = moving down, NEGATIVE = moving up.
 * - `peekPx` / `fullPx`: resolved pixel heights of the two open detents.
 */
export type DetentEvent = {
  type: "open" | "close" | "up" | "down" | "dragEnd";
  velocity?: number;
  offset?: number;
  peekPx: number;
  fullPx: number;
};

function up(s: Detent): Detent {
  return s === "closed" ? "peek" : "full";
}
function down(s: Detent): Detent {
  return s === "full" ? "peek" : "closed";
}

export function detentHeight(s: Detent, peekPx: number, fullPx: number): number {
  return s === "closed" ? 0 : s === "peek" ? peekPx : fullPx;
}

export function next(state: Detent, ev: DetentEvent): Detent {
  switch (ev.type) {
    case "open":
      return state === "closed" ? "peek" : state;
    case "close":
      return "closed";
    case "up":
      return up(state);
    case "down":
      return down(state);
    case "dragEnd": {
      const v = ev.velocity ?? 0;
      if (v > FLING_VELOCITY) return down(state);
      if (v < -FLING_VELOCITY) return up(state);
      const h = detentHeight(state, ev.peekPx, ev.fullPx) - (ev.offset ?? 0);
      const cands: Detent[] = ["closed", "peek", "full"];
      let best: Detent = state;
      let bestD = Infinity;
      for (const c of cands) {
        const d = Math.abs(detentHeight(c, ev.peekPx, ev.fullPx) - h);
        if (d < bestD) {
          bestD = d;
          best = c;
        }
      }
      return best;
    }
  }
}
