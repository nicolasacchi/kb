// T14 — grid fitting: the panel renders EXACTLY the session grid by
// `fontSize` only, never CSS `transform`/`zoom` (xterm issue 6023).
//
// The pure helper below is unit-tested; the DOM measuring loop (canvas
// `measureText("W")`, ≤4 iterations on `.xterm-screen` width, then
// `overflow-x: auto`) lives in `TerminalPanel.tsx`.

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
