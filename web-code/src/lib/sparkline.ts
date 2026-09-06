// V3.4-C3 — pure sparkline geometry for weekly activity (churn/commits).
// Attention framing only — never a health/verdict colour map.

export interface SparkBucket {
  week_start_unix: number;
  commits: number;
  churn: number;
  authors: number;
}

/**
 * Build an SVG path `d` for a polyline through `values` in a `width×height`
 * box. Values are scaled to [pad, height-pad]; empty/all-zero → flat midline.
 * Coordinates are rounded to 2 decimals for stable snapshots.
 */
export function sparklinePath(
  values: number[],
  width: number,
  height: number,
  pad = 1.5,
): string {
  if (values.length === 0 || width <= 0 || height <= 0) return "";
  const n = values.length;
  const lo = pad;
  const hi = Math.max(pad, height - pad);
  const span = hi - lo;
  let min = Infinity;
  let max = -Infinity;
  for (const v of values) {
    const x = Number.isFinite(v) ? v : 0;
    if (x < min) min = x;
    if (x > max) max = x;
  }
  if (!Number.isFinite(min) || !Number.isFinite(max)) {
    min = 0;
    max = 0;
  }
  // All equal (incl. all zeros) → flat midline so the mark is still visible.
  const range = max - min;
  const pts: string[] = [];
  for (let i = 0; i < n; i++) {
    const raw = Number.isFinite(values[i]) ? values[i]! : 0;
    const t = range > 0 ? (raw - min) / range : 0.5;
    const x = n === 1 ? width / 2 : (i / (n - 1)) * width;
    const y = hi - t * span;
    pts.push(`${round2(x)},${round2(y)}`);
  }
  return `M ${pts.join(" L ")}`;
}

/** Churn series from timeseries buckets (order preserved). */
export function churnSeries(buckets: SparkBucket[]): number[] {
  return buckets.map((b) => (Number.isFinite(b.churn) ? Math.max(0, b.churn) : 0));
}

/** ISO week label for tooltip (UTC Monday start). */
export function weekLabel(weekStartUnix: number): string {
  if (!Number.isFinite(weekStartUnix)) return "—";
  const d = new Date(weekStartUnix * 1000);
  const y = d.getUTCFullYear();
  const m = String(d.getUTCMonth() + 1).padStart(2, "0");
  const day = String(d.getUTCDate()).padStart(2, "0");
  return `${y}-${m}-${day}`;
}

/** Tooltip line for one bucket. */
export function sparkTooltip(bucket: SparkBucket): string {
  return `${weekLabel(bucket.week_start_unix)} · commits ${bucket.commits} · churn ${bucket.churn} · authors ${bucket.authors}`;
}

function round2(n: number): number {
  return Math.round(n * 100) / 100;
}
