// STATSX4 — a small model -> context-window table. It is a CONTEXT table, not a
// price table: it only turns "tokens held now" into a percentage. A model the
// table does not know returns null and the panel shows absolute tokens, never a
// guessed percentage. No transcript carries its window size.

const ONE_M = 1_000_000;
const TWO_HUNDRED_K = 200_000;

/** `[1m]`, `-1m`, `(1M context)` and a trailing `1m` word mark a 1M variant. */
const ONE_M_MARK = /(\[1m\]|-1m(?![a-z0-9])|\(1m context\)|\b1m context\b)/i;

/**
 * The context window for a model string, or null when unknown. Claude models
 * (full ids or the opus/sonnet/haiku aliases) default to 200k, the 1M variant
 * by suffix. omp models are listed only where the window is known.
 */
export function contextWindow(model: string | null | undefined): number | null {
  if (typeof model !== "string") return null;
  const m = model.trim().toLowerCase();
  if (m === "") return null;
  const claude = /^(claude[-\s]|opus\b|sonnet\b|haiku\b|anthropic\/claude)/.test(m);
  if (!claude) return null;
  return ONE_M_MARK.test(m) ? ONE_M : TWO_HUNDRED_K;
}

/** Percent of the window the context holds (0..100, one decimal), or null without a known window. */
export function contextPercent(tokens: number | null | undefined, model: string | null | undefined): number | null {
  if (typeof tokens !== "number" || !Number.isFinite(tokens) || tokens < 0) return null;
  const w = contextWindow(model);
  if (w === null) return null;
  return Math.min(100, Math.round((tokens / w) * 1000) / 10);
}

/**
 * True when the window is an ASSUMPTION: a Claude model id with no 1M marker.
 * Transcript `message.model` ids carry no `[1m]` suffix, so a 1M-window session
 * is indistinguishable from a 200k one; callers must label the percentage
 * approximate and must not show a figure the assumed window cannot hold.
 */
export function contextWindowAssumed(model: string | null | undefined): boolean {
  const w = contextWindow(model);
  return w === TWO_HUNDRED_K;
}
