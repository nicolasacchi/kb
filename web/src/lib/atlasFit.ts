// M-b — uniform-scale, letterboxed atlas fit transform.
//
// AtlasView's canvas draw loop used to scale the fixed logical (W × H) space
// independently on each axis (`ax = cssW*dpr/W`, `ay = cssH*dpr/H`) to fill
// whatever container it was given. Whenever the container's aspect ratio
// didn't match the logical space's (600/360), ax != ay: every dot — drawn
// with `ctx.arc` (a circle in the *pre-transform* coordinate space) —
// rendered as an ellipse on screen, AND `findHit`'s circular hit-radius test
// (done in logical space) desynced from the visibly-elliptical dot. This is
// already live at <=860px, where mobile.css overrides the 600/360
// `aspect-ratio` with a fixed `height: 70vh`.
//
// The fix: ONE uniform scale (the min of the two per-axis ratios) plus a
// letterbox offset that centers the logical box in the container — the
// standard "contain" fit. Pure, no DOM/React — colocated vitest
// (atlasFit.test.ts), same shape as atlasSelection.ts / atlasCameras.ts.
// AtlasView.tsx is the only caller.

export type Vec2 = { x: number; y: number };

/** A `fitTransform` result: `scale` device/CSS pixels per logical unit
 * (uniform on both axes), `offsetX`/`offsetY` the letterbox offset (in the
 * same pixel space as `scale`) that centers the logical box. All three are
 * `0` for a degenerate (zero or non-finite) container/logical size so
 * downstream math never divides by zero or produces NaN — callers should
 * treat `scale <= 0` as "nothing to draw / hit-test yet" (mirrors the draw
 * loop's existing `cssW === 0 || cssH === 0` early return). */
export type AtlasFit = {
  scale: number;
  offsetX: number;
  offsetY: number;
};

const DEGENERATE_FIT: AtlasFit = { scale: 0, offsetX: 0, offsetY: 0 };

function positiveFinite(n: number): boolean {
  return Number.isFinite(n) && n > 0;
}

/** Uniform-scale "contain" fit of a `logicalW × logicalH` box into a
 * `cssW × cssH` container at device-pixel-ratio `dpr` (pass `dpr = 1` to get
 * the CSS-pixel-space transform pointer math wants instead of the
 * device-pixel-space one the canvas draw loop wants — the two are exactly
 * proportional, so there's no separate "CSS fit" type). */
export function fitTransform(
  cssW: number,
  cssH: number,
  dpr: number,
  logicalW: number,
  logicalH: number,
): AtlasFit {
  const safeDpr = positiveFinite(dpr) ? dpr : 1;
  if (
    !positiveFinite(cssW) ||
    !positiveFinite(cssH) ||
    !positiveFinite(logicalW) ||
    !positiveFinite(logicalH)
  ) {
    return DEGENERATE_FIT;
  }
  const devW = cssW * safeDpr;
  const devH = cssH * safeDpr;
  const scale = Math.min(devW / logicalW, devH / logicalH);
  const offsetX = (devW - logicalW * scale) / 2;
  const offsetY = (devH - logicalH * scale) / 2;
  return { scale, offsetX, offsetY };
}

/** Logical (W×H) point → screen point, given the current pan/zoom on top of
 * `fit`. Mirrors the draw loop's composed transform: `ctx.setTransform(fit.scale,
 * 0, 0, fit.scale, fit.offsetX + pan.x*fit.scale, fit.offsetY + pan.y*fit.scale)`
 * followed by `ctx.scale(zoom, zoom)`. Pass `pan = {x:0, y:0}` and `zoom = 1`
 * to get the bare fit (used by the pinch-zoom midpoint math below). */
export function logicalToScreen(
  logical: Vec2,
  fit: AtlasFit,
  pan: Vec2,
  zoom: number,
): Vec2 {
  return {
    x: fit.scale * zoom * logical.x + fit.offsetX + pan.x * fit.scale,
    y: fit.scale * zoom * logical.y + fit.offsetY + pan.y * fit.scale,
  };
}

/** Inverse of `logicalToScreen`. Returns `null` for a degenerate fit
 * (`scale <= 0`) instead of dividing by zero — same "nothing to hit-test
 * yet" contract as the draw loop's early return. */
export function screenToLogical(
  screen: Vec2,
  fit: AtlasFit,
  pan: Vec2,
  zoom: number,
): Vec2 | null {
  if (!(fit.scale > 0) || !(zoom !== 0)) return null;
  return {
    x: (screen.x - fit.offsetX - pan.x * fit.scale) / (fit.scale * zoom),
    y: (screen.y - fit.offsetY - pan.y * fit.scale) / (fit.scale * zoom),
  };
}
