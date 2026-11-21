// FUTERMFIT — one terminal cell per px of font size, measured from the font
// the browser will actually paint (a hidden DOM probe, `line-height: normal`
// like xterm's own measure element), never from a constant ratio. The font
// may still be loading when this runs; callers re-measure after the webfont
// arrives (`fonts.ts`).

export interface CellRatio {
  cellW: number;
  cellH: number;
}

/** What a monospace face roughly measures; used only when nothing can be measured. */
export const FALLBACK_CELL: CellRatio = { cellW: 0.6, cellH: 1.3 };

const PROBE_PX = 100;
const PROBE_CHARS = 20;

/** Measure `family` in `doc`. A zero box (jsdom, a hidden tree) answers the fallback. */
export function measureCellRatio(doc: Document, family: string): CellRatio {
  const body = doc.body;
  if (!body) return FALLBACK_CELL;
  const probe = doc.createElement("span");
  probe.setAttribute("aria-hidden", "true");
  const s = probe.style;
  s.position = "absolute";
  s.visibility = "hidden";
  s.pointerEvents = "none";
  s.whiteSpace = "pre";
  s.letterSpacing = "normal";
  s.lineHeight = "normal";
  s.fontFamily = family || "monospace";
  s.fontSize = `${PROBE_PX}px`;
  probe.textContent = "W".repeat(PROBE_CHARS);
  body.appendChild(probe);
  try {
    const box = probe.getBoundingClientRect();
    const cellW = box.width / PROBE_CHARS / PROBE_PX;
    const cellH = box.height / PROBE_PX;
    if (!(cellW > 0) || !(cellH > 0) || !Number.isFinite(cellW) || !Number.isFinite(cellH)) return FALLBACK_CELL;
    return { cellW, cellH };
  } finally {
    probe.remove();
  }
}
