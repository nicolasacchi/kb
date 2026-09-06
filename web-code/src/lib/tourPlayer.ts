// Pure step-index logic for Phase E4's tour mode — parsing the `?step=`
// deep-link (1-based on the wire, human-friendly) into a starting 0-based
// step index. Mirrors `lib/storyPlayer.ts`'s own header doc split: kept
// DOM/router-free so it's unit-tested without a component.
//
// `clampStep` is deliberately NOT duplicated here — tour mode's index space
// is the exact same "no negative/out-of-range index" contract `lib/
// storyPlayer.ts`'s own `clampStep` already provides (a plain `[0, length -
// 1]` clamp with no player-specific meaning attached to a negative index,
// unlike `lib/historyStep.ts`'s `-1` "working tree" sentinel) — every caller
// (`routes/Tour.tsx`) imports it from there instead of a second copy.

/// The step index `?step=` resolves to (0-based). `null`/missing, junk,
/// zero, or negative all default to `0` — the tour's FIRST span. Mirrors
/// `lib/storyPlayer.ts`'s `initialStepIndex`'s own "unresolvable input
/// starts at the beginning" default.
export function parseTourStepParam(v: string | null): number {
  if (!v) return 0;
  const n = parseInt(v, 10);
  if (!Number.isFinite(n) || n < 1) return 0;
  return n - 1;
}
