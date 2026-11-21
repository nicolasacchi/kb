// FUTERMPOLISH — the fit driver of the desktop panel: one place that decides
// WHEN a font fit is trusted.
//
// xterm paints its screen after the grid or the font changes, not at the call:
// a measurement taken straight after `term.resize()` / `fontSize = n` can still
// show the PREVIOUS screen. A fit that believed that stale screen stepped the
// font down for a grid that no longer existed and reported Cropped for it, and
// nothing later repaired it. So:
//   * a fit of a NEW situation (another grid, another box, other bounds) is
//     planned from scratch (`fresh`: the no-ratchet guard does not apply), and
//   * every fit is followed by verification passes after the renderer had time
//     to paint, repeated while the font still changes (at most `maxPasses`), so
//     the last result is read from a settled screen.
// A refit of the SAME grid in the SAME box keeps a font that already fits
// (no ratchet down to the floor).

import type { FitResult } from "./fit";

export interface FitterDeps<T, H> {
  /** One fit pass; null when the host cannot be measured (closed, zero width). */
  run(term: T, host: H, fresh: boolean): FitResult | null;
  /** What makes a situation new: grid, box and bounds, as one string. */
  key(host: H): string;
  /** Run `fn` after the renderer has painted. Returns a handle for `cancel`. */
  schedule(fn: () => void): unknown;
  cancel(handle: unknown): void;
  /** Every pass result (the panel's chip and Cropped hint follow it). */
  onResult(r: FitResult): void;
  maxPasses?: number;
}

export interface Fitter<T, H> {
  refit(term: T, host: H): void;
  dispose(): void;
}

export function createFitter<T, H>(deps: FitterDeps<T, H>): Fitter<T, H> {
  const maxPasses = deps.maxPasses ?? 4;
  let lastKey: string | null = null;
  let pending: unknown = null;
  let generation = 0;

  const clearPending = (): void => {
    if (pending !== null) deps.cancel(pending);
    pending = null;
  };

  const pass = (term: T, host: H, fresh: boolean, gen: number, n: number, prevFont: number | null): void => {
    if (gen !== generation) return;
    const r = deps.run(term, host, fresh);
    if (r) deps.onResult(r);
    // Verify after the paint: always once, then while the font keeps moving.
    const changed = r !== null && prevFont !== null && r.fontSize !== prevFont;
    if (n >= maxPasses || (n > 1 && !changed)) return;
    pending = deps.schedule(() => {
      pending = null;
      pass(term, host, false, gen, n + 1, r ? r.fontSize : prevFont);
    });
  };

  return {
    refit(term, host) {
      clearPending();
      generation += 1;
      const k = deps.key(host);
      const fresh = k !== lastKey;
      lastKey = k;
      pass(term, host, fresh, generation, 1, null);
    },
    dispose() {
      clearPending();
      generation += 1;
    },
  };
}
