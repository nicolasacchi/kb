// PHIN1 - cursor-follow geometry for the phone's direct typing, DOM-free.
//
// With the soft keyboard up, the terminal cursor cell must stay inside the part
// of the Watch area the operator can actually see. Two browser families get
// there differently and both reduce to one box:
//  - layout-resize (Android Firefox): `innerHeight` and `visualViewport.height`
//    shrink together, `offsetTop` is 0, so the Watch box itself gets shorter;
//  - visual-resize (Android Chrome): the layout viewport keeps its size,
//    `visualViewport.height` shrinks and `offsetTop` may be above 0, so the
//    keyboard covers the bottom of an unchanged Watch box.
// `visibleBand` intersects the Watch box with the visual viewport (both in
// client coordinates) and `planFollow` says how far to move to bring the cursor
// cell, plus a margin, inside that band.

export interface Rect {
  left: number;
  top: number;
  right: number;
  bottom: number;
}

/** The fields of `window.visualViewport` the geometry reads. */
export interface VV {
  offsetTop: number;
  offsetLeft: number;
  width: number;
  height: number;
}

const finiteRect = (r: Rect): boolean =>
  Number.isFinite(r.left) && Number.isFinite(r.top) && Number.isFinite(r.right) && Number.isFinite(r.bottom);

/**
 * The Watch box seen through the visual viewport. A null viewport (the API is
 * absent) is the Watch box itself. An empty intersection is a zero-height rect at
 * the Watch top: never NaN, never a negative size.
 */
export function visibleBand(watch: Rect, vv: VV | null): Rect {
  if (vv === null) return watch;
  const left = Math.max(watch.left, vv.offsetLeft);
  const top = Math.max(watch.top, vv.offsetTop);
  const right = Math.min(watch.right, vv.offsetLeft + vv.width);
  const bottom = Math.min(watch.bottom, vv.offsetTop + vv.height);
  if (![left, top, right, bottom].every(Number.isFinite) || right <= left || bottom <= top) {
    return { left: watch.left, top: watch.top, right: Math.max(watch.left, watch.right), bottom: watch.top };
  }
  return { left, top, right, bottom };
}

/** One axis: the signed shift (cursor coordinate minus band coordinate) that brings `[c0 - m, c1 + m]` inside `[b0, b1]`. */
function axisShift(c0: number, c1: number, b0: number, b1: number, m: number): number {
  if (b1 - b0 <= 0) return 0;
  const lo = c0 - m;
  const hi = c1 + m;
  if (lo >= b0 && hi <= b1) return 0;
  if (hi - lo > b1 - b0) {
    // The band cannot hold the cell and its margins: the cell's far edge (its
    // bottom, its right) wins, and a cell that is already inside stays put.
    if (c0 >= b0 && c1 <= b1) return 0;
    return c1 - b1;
  }
  return lo < b0 ? lo - b0 : hi - b1;
}

export interface FollowPlanArgs {
  cursor: Rect;
  band: Rect;
  cellW: number;
  cellH: number;
  /** Columns kept visible beside the cursor (default 3). */
  marginCols?: number;
  /** Rows kept visible above and below the cursor (default 1). */
  marginRows?: number;
}

/**
 * The minimal shift, in px, that puts the cursor cell and its margins inside the
 * band. Both numbers follow the `scrollLeft` / `scrollTop` convention: a positive
 * `dx` is added to the pan wrapper's `scrollLeft` (the grid moves left), a positive
 * `dy` reveals lower content (the grid moves UP, which is what a cursor hidden
 * under the keyboard needs) and a negative `dy` moves the grid down. Zeros when
 * the cursor is already inside or when any input is not a usable number.
 */
export function planFollow(a: FollowPlanArgs): { dx: number; dy: number } {
  const { cursor, band, cellW, cellH } = a;
  if (!(Number.isFinite(cellW) && cellW > 0 && Number.isFinite(cellH) && cellH > 0)) return { dx: 0, dy: 0 };
  if (!finiteRect(cursor) || !finiteRect(band)) return { dx: 0, dy: 0 };
  const mc = (a.marginCols ?? 3) * cellW;
  const mr = (a.marginRows ?? 1) * cellH;
  return {
    dx: axisShift(cursor.left, cursor.right, band.left, band.right, mc),
    dy: axisShift(cursor.top, cursor.bottom, band.top, band.bottom, mr),
  };
}

/** How long a manual pan or scroll keeps auto-follow quiet. */
export const SUSPEND_MS = 2000;

export interface FollowGate {
  /** The operator just panned, scrolled or pinched. */
  noteManual(): void;
  /** True again exactly `suspendMs` after the LAST `noteManual`. */
  allowed(): boolean;
  /** Milliseconds until `allowed()` turns true again (0 when it already is). */
  remainingMs(): number;
  reset(): void;
}

export function createFollowGate(opts: { now: () => number; suspendMs?: number }): FollowGate {
  const suspend = opts.suspendMs ?? SUSPEND_MS;
  let last = -Infinity;
  return {
    noteManual() {
      last = opts.now();
    },
    allowed() {
      return opts.now() - last >= suspend;
    },
    remainingMs() {
      return Math.max(0, suspend - (opts.now() - last));
    },
    reset() {
      last = -Infinity;
    },
  };
}
