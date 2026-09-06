// MI-W4.2b — the salience × days-since-last-recall quadrant scatter's pure
// bucketing/layout core.
//
// A sorted table buries the two outlier shapes that actually matter:
// - "high salience, never/rarely recalled" — DEAD WEIGHT: the operator
//   marked it important, but nothing ever pulls it back in.
// - "low salience, constantly recalled" — MIS-SCORED: it keeps getting
//   used despite a low salience score, so the score is probably wrong.
// A scatter plot makes both pop as points in the "wrong" quadrant; this
// module turns raw census-shaped rows into plot-ready points + the
// quadrant classification, with no chart-library dependency.

export interface QuadrantInput {
  kb: string;
  id: string;
  title: string;
  /** For the focus panel's artifact link (`artifactHref` needs a path, not
   * an id). */
  sourceRelative: string;
  /** Raw `kb-salience` meta, `[0,1]`. */
  salience: number;
  recallCount: number;
  /** Unix seconds, or `null`/`undefined` when never recalled. */
  lastRecalledAt: number | null | undefined;
}

export type Quadrant = "dead-weight" | "mis-scored" | "healthy-active" | "healthy-idle";

export interface QuadrantPoint extends QuadrantInput {
  /** Days since last recall; `null` when never recalled (plotted at the
   * far/"never" edge, not zero — zero would misleadingly read as "just
   * recalled"). */
  daysSinceRecall: number | null;
  quadrant: Quadrant;
}

/**
 * FIX2 — `highSalienceThreshold`/`dormantDays` used to be hand-duplicated
 * TS constants (0.7 / 60) tied to `kb_core::triage::HIGH_SALIENCE_
 * THRESHOLD`/`DORMANT_DAYS` only by a doc comment — the same drift risk as
 * hand-duplicating a decay formula. They are now REQUIRED parameters,
 * wire-supplied from `GET /api/memory/triage`'s `high_salience_threshold`/
 * `dormant_days` fields (mirrors how `decay_k`/`drop_threshold` are already
 * wire-supplied rather than duplicated) — there is no module-level default
 * to silently drift out of sync with the server's own constants.
 */
export interface QuadrantThresholds {
  /** Salience at/above this is "high" for quadrant classification purposes
   * — must agree with the hygiene queue's high-salience-dormant reason on
   * what counts as high (both read the SAME wire value). */
  highSalienceThreshold: number;
  /** A memory idle this many days (or never recalled) counts as "dormant"
   * for the scatter's x-axis split. */
  dormantDays: number;
}

function classify(
  salience: number,
  daysSinceRecall: number | null,
  thresholds: QuadrantThresholds,
): Quadrant {
  const highSalience = salience >= thresholds.highSalienceThreshold;
  const dormant = daysSinceRecall === null || daysSinceRecall >= thresholds.dormantDays;
  if (highSalience && dormant) return "dead-weight";
  if (!highSalience && !dormant) return "mis-scored";
  return dormant ? "healthy-idle" : "healthy-active";
}

/** Build plot-ready points from raw rows. `nowUnix` is caller-supplied
 * (mirrors `kb_core::memory::rerank`'s explicit-clock convention) so this
 * stays pure and unit-testable without mocking the system clock. */
export function buildQuadrantPoints(
  rows: QuadrantInput[],
  nowUnix: number,
  thresholds: QuadrantThresholds,
): QuadrantPoint[] {
  return rows.map((r) => {
    const daysSinceRecall =
      r.lastRecalledAt == null ? null : Math.max(0, (nowUnix - r.lastRecalledAt) / 86_400);
    return { ...r, daysSinceRecall, quadrant: classify(r.salience, daysSinceRecall, thresholds) };
  });
}

/** Count points per quadrant — the scatter's corner labels/legend. */
export function quadrantCounts(points: QuadrantPoint[]): Record<Quadrant, number> {
  const out: Record<Quadrant, number> = {
    "dead-weight": 0,
    "mis-scored": 0,
    "healthy-active": 0,
    "healthy-idle": 0,
  };
  for (const p of points) out[p.quadrant]++;
  return out;
}
