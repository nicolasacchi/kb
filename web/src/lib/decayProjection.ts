// MI-W4.1 — the /memory health-timeline sparkline's pure maths.
//
// The daemon already computes and ships every INGREDIENT this needs on the
// wire (`RecallHit`/`MemoryLineageNode`): `salience` (the raw kb-salience
// meta), `age_days` (age at the moment the response was built), `decay_k`
// (the per-day rate for the memory's `kb-decay` bucket —
// `kb_core::memory::decay_k`'s own return value, never a client-side guess
// at the slow/fast constants), and — only when `[memory] scoring_v2_stability`
// is on (MI-W5.R split; off by default, unlike its sibling
// `scoring_v2_relevance`) — `stability` (the FSRS-inspired multiplier, itself
// a fixed scalar for a given memory; see
// `kb_core::memory::stability_multiplier`'s doc comment: it depends only on
// historical timestamps, not on "now", so it doesn't itself vary as we
// project forward).
//
// What this module does NOT do: re-derive `stability_multiplier`'s own
// Bjork-desirable-difficulty arithmetic (recall_count/last_recalled_at →
// stability) — that stays server-side, surfaced as a single number. This
// module only continues kb-core's OWN decay curve (`salience *
// exp(-k*age)`, optionally scaled by that surfaced `stability` and re-capped
// exactly like `rerank_with_policy_scored` does) forward in time, for
// RENDERING the curve.
//
// GROUND TRUTH (MI-W4.1 revision) — read before touching this file: this
// curve is a RANKING signal only. Decay never lowers the value the
// decay-policy floor tests — both enforcement sites (`rerank_with_policy_
// scored`'s filter chain and the recall route's inline per-hit check)
// compare the floor against the RAW, UNDECAYED `salience`. A prior revision
// of this module shipped `findCrossing`/`crossingLabel`, which solved this
// curve for the day it crosses the floor and presented that as "days until
// this memory drops" — an event that cannot happen for any memory whose
// salience already clears the floor. Those functions are DELETED, not
// renamed. What replaces them, mirroring `kb_core::memory` exactly:
//
// - `floorState`/`floorStateLabel` — the TRUE, non-time-varying answer to
//   "is this excluded from recall right now": a STATE derived from the
//   CONSTANT raw salience, never a projected crossing date.
// - `decayHalfLifeDays`/`decayHalfLifeLabel` — the TRUE fact about the
//   SCORE trajectory (the score really does shrink over time, which really
//   does make a memory rank lower and lower): a fixed half-life derived
//   directly from `decayK` alone.

export interface DecayCurvePoint {
  /** Days from today, `0..=horizonDays`. */
  t: number;
  /** Projected effective salience at `t`. */
  y: number;
}

export interface DecayCurveInput {
  /** Raw `kb-salience` meta value, `[0,1]`. */
  salience: number;
  /** Age (in days) the wire response was computed against — "today". */
  ageDaysNow: number;
  /** Per-day decay rate for this memory's `kb-decay` bucket. */
  decayK: number;
  /**
   * The FSRS-inspired stability multiplier, when `[memory]
   * scoring_v2_stability` is on (`undefined`/`null` when it's off — the
   * daemon-wide default — treated as a no-op `1.0`, NOT a different curve).
   */
  stability?: number | null;
  /** Days to project forward. Default 90 (the design brief's window). */
  horizonDays?: number;
  /** Number of sample points for the curve (the sparkline polyline). */
  samples?: number;
}

const DEFAULT_HORIZON_DAYS = 90;
const DEFAULT_SAMPLES = 30;

/**
 * The projected effective-salience value at `t` days from today:
 * `salience * min(1, exp(-k*(ageDaysNow+t)) * stability)` — the exact
 * quantity `rerank_with_policy_scored` folds into `score` (minus the
 * rank/relevance factors, which are about SEARCH ranking, not this
 * artifact's own lifecycle, and have no place on a per-memory timeline).
 */
export function decayValueAt(input: DecayCurveInput, t: number): number {
  const stability = input.stability ?? 1;
  const age = input.ageDaysNow + t;
  const base = Math.exp(-input.decayK * age);
  return input.salience * Math.min(1, base * stability);
}

/** Sample the curve at `samples+1` evenly-spaced points over `[0,
 * horizonDays]` — the sparkline's polyline data. */
export function projectDecayCurve(input: DecayCurveInput): DecayCurvePoint[] {
  const horizon = input.horizonDays ?? DEFAULT_HORIZON_DAYS;
  const samples = input.samples ?? DEFAULT_SAMPLES;
  const points: DecayCurvePoint[] = [];
  for (let i = 0; i <= samples; i++) {
    const t = (i / samples) * horizon;
    points.push({ t, y: decayValueAt(input, t) });
  }
  return points;
}

/**
 * The four TRUE states a memory's RAW salience can be in relative to the
 * active decay-policy floor, mirroring `kb_core::memory::FloorState`
 * EXACTLY (same precedence: pinned checked first, then no-floor, then a
 * plain `salience <= floor` comparison against the CONSTANT raw value —
 * never a decayed/projected one). Because raw salience doesn't change with
 * time, this is a STATE, not a projection: re-evaluating it "later" (absent
 * an explicit salience edit) yields the same answer.
 */
export type FloorState =
  | { kind: "pinned" }
  | { kind: "no-floor" } // Loose policy — nothing is ever excluded on salience grounds.
  | { kind: "above"; salience: number; floor: number }
  | { kind: "below"; salience: number; floor: number };

/** Pure classifier — see {@link FloorState}. `floor === null` represents the
 * `Loose` policy (mirrors the wire's `drop_threshold: null`). */
export function floorState(salience: number, floor: number | null, pinned: boolean): FloorState {
  if (pinned) return { kind: "pinned" };
  if (floor === null || !Number.isFinite(floor)) return { kind: "no-floor" };
  return salience <= floor ? { kind: "below", salience, floor } : { kind: "above", salience, floor };
}

/** A short, render-ready label for a {@link FloorState} — states a FACT
 * about right now, never a predicted future date. Shared by the sparkline's
 * caption and the DecayRail rollup row text so the wording never drifts
 * between the two surfaces. */
export function floorStateLabel(state: FloorState): string {
  switch (state.kind) {
    case "pinned":
      return "pinned — exempt from floor";
    case "no-floor":
      return "no floor (loose policy)";
    case "above":
      return `salience ${state.salience.toFixed(2)} — above the ${state.floor.toFixed(2)} floor`;
    case "below":
      return `salience ${state.salience.toFixed(2)} — below the ${state.floor.toFixed(2)} floor, excluded from recall now`;
  }
}

/**
 * The score's half-life in days — `ln(2) / decayK`, mirroring
 * `kb_core::memory::decay_half_life_days` EXACTLY. A property of the decay
 * RATE alone (independent of salience/age/stability — stability rescales
 * the curve's magnitude but not its exponential rate), so this is the one
 * honest "how fast" fact this rate implies on its own. `Infinity` for a
 * non-positive rate (never decays).
 */
export function decayHalfLifeDays(decayK: number): number {
  return decayK > 0 ? Math.LN2 / decayK : Infinity;
}

/** Render-ready half-life label, e.g. "score halves every ~7d". */
export function decayHalfLifeLabel(decayK: number): string {
  const half = decayHalfLifeDays(decayK);
  return Number.isFinite(half) ? `score halves every ~${Math.round(half)}d` : "score never decays";
}
