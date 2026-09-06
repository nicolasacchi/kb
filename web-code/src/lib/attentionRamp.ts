// Neutral single-hue attention intensity for behavioral heatmaps.
//
// Design law: no red shame gradient. High attention = stronger accent
// opacity; low attention = near-transparent. Red stays reserved for real
// errors elsewhere in the app. Score range for hotspot/risk is 0..1
// (server-side: `hotspot_score` max 1.0; `review_risk_score` clamped 0..1).

/** Clamp a score into 0..1; non-finite → 0. */
export function clampScore(score: number): number {
  if (!Number.isFinite(score)) return 0;
  if (score <= 0) return 0;
  if (score >= 1) return 1;
  return score;
}

/**
 * CSS background for a row/cell tinted by attention score.
 * Uses the app accent at varying alpha (opacity ramp), never red/green.
 *
 * Alpha maps score∈[0,1] → [minAlpha, maxAlpha] so low scores stay subtle
 * and the top of the list is clearly stronger — still one hue.
 */
export function attentionTintStyle(
  score: number | null | undefined,
  opts?: { minAlpha?: number; maxAlpha?: number },
): { backgroundColor: string } | undefined {
  if (score == null || !Number.isFinite(score)) return undefined;
  const minA = opts?.minAlpha ?? 0.06;
  const maxA = opts?.maxAlpha ?? 0.42;
  const t = clampScore(score);
  const alpha = minA + t * (maxA - minA);
  // color-mix keeps theme tokens; accent is purple, never error-red.
  return {
    backgroundColor: `color-mix(in srgb, var(--accent) ${Math.round(alpha * 100)}%, transparent)`,
  };
}

/** Format a score for display (fixed decimals); never invent 0 for null. */
export function formatAttentionScore(score: number | null | undefined, digits = 2): string {
  if (score == null || !Number.isFinite(score)) return "—";
  return score.toFixed(digits);
}

/** Human label for a missing risk/hotspot input (never "0"). */
export function notAvailableLabel(term: string): string {
  return `not available: ${term}`;
}
