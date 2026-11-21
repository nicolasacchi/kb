// FUTERMFIT — fit the live xterm into its host by fontSize only (never CSS
// `transform`/`zoom`, xterm issue 6023). The pure choice is `computeFit`; this
// module is the DOM half: measure the host, apply, then VERIFY against the
// rendered `.xterm-screen` (cell sizes are not linear in the font size) and
// step down at most 4 times.

import { computeFit, type FitResult } from "./fit";
import { measureCellRatio } from "./cellProbe";

export interface FitTerm {
  options: { fontSize?: number; fontFamily?: string };
}

export interface HostFitOptions {
  min: number;
  max: number;
  /** Test seam: the document that owns the probe. */
  doc?: Document;
}

/**
 * Fit `term` into `host`. Returns null (and touches nothing) while the host
 * has no width: a closed `<details>` or an unmounted subtree must never fit
 * at zero and leave a minimum-size terminal behind.
 */
export function fitHostNow(
  term: FitTerm,
  host: HTMLElement,
  grid: { cols: number; rows: number },
  opts: HostFitOptions,
): FitResult | null {
  if (!(host.clientWidth > 0)) return null;
  const screen = host.querySelector<HTMLElement>(".xterm-screen");
  const ratio = measureCellRatio(opts.doc ?? document, term.options.fontFamily || "monospace");
  host.style.overflow = "";
  const planned = computeFit({
    width: host.clientWidth,
    height: host.clientHeight,
    cols: grid.cols,
    rows: grid.rows,
    cellW: ratio.cellW,
    cellH: ratio.cellH,
    min: opts.min,
    max: opts.max,
  });
  let fontSize = planned.fontSize;
  // A font at which the rendered screen already fits is never shrunk to a plan
  // that is only an estimate: the cell height the browser paints is rounded down
  // to whole device pixels, so in a host that is as tall as its content (no
  // bounded height) every refit would plan one step smaller than the font that
  // just fitted, and the font would ratchet to the floor, moving every picture
  // and cell under the reader. Growing to a larger plan still happens.
  const current = term.options.fontSize;
  if (
    screen &&
    typeof current === "number" &&
    current > planned.fontSize &&
    current >= opts.min &&
    current <= opts.max &&
    screen.scrollWidth <= host.clientWidth &&
    screen.scrollHeight <= host.clientHeight + 1
  ) {
    fontSize = current;
  }
  for (let i = 0; i < 4; i += 1) {
    term.options.fontSize = fontSize;
    if (!screen) break;
    const fits = screen.scrollWidth <= host.clientWidth && screen.scrollHeight <= host.clientHeight;
    if (fits || fontSize <= opts.min) break;
    fontSize = Math.max(opts.min, fontSize - 1);
  }
  const cropped = screen
    ? { x: screen.scrollWidth > host.clientWidth, y: screen.scrollHeight > host.clientHeight }
    : planned.cropped;
  if (cropped.x || cropped.y) host.style.overflow = "auto";
  return { fontSize, cropped };
}
