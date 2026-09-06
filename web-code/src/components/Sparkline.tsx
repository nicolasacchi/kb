import { useMemo, useState } from "react";
import type { TimeseriesBucket } from "../api/types";
import {
  churnSeries,
  sparkTooltip,
  sparklinePath,
  type SparkBucket,
} from "../lib/sparkline";

export interface SparklineProps {
  buckets: TimeseriesBucket[] | SparkBucket[];
  /** SVG width (default 72). */
  width?: number;
  /** SVG height (default 18). */
  height?: number;
  /** Accessible label. */
  label?: string;
  className?: string;
  /** data- attribute for e2e. */
  "data-kbc-sparkline"?: string;
}

function yAt(series: number[], i: number, height: number, pad = 1.5): number {
  let min = Infinity;
  let max = -Infinity;
  for (const v of series) {
    if (v < min) min = v;
    if (v > max) max = v;
  }
  const lo = pad;
  const hi = Math.max(pad, height - pad);
  const span = hi - lo;
  const range = max - min;
  const raw = series[i] ?? 0;
  const t = range > 0 ? (raw - min) / range : 0.5;
  return hi - t * span;
}

/**
 * Pure SVG sparkline of weekly churn. Neutral single-hue accent stroke —
 * activity framing, never health/verdicts. Axis-free; hover a point for
 * week + counts.
 */
export default function Sparkline({
  buckets,
  width = 72,
  height = 18,
  label = "weekly churn",
  className,
  "data-kbc-sparkline": dataAttr = "1",
}: SparklineProps) {
  const [hover, setHover] = useState<number | null>(null);
  const series = useMemo(() => churnSeries(buckets), [buckets]);
  const d = useMemo(
    () => sparklinePath(series, width, height),
    [series, width, height],
  );

  if (buckets.length === 0 || !d) {
    return (
      <span
        className={"kbc-spark" + (className ? ` ${className}` : "")}
        data-kbc-sparkline={dataAttr}
        data-kbc-sparkline-empty
        title="No weekly activity in window"
        aria-label={`${label}: empty`}
      >
        <svg width={width} height={height} viewBox={`0 0 ${width} ${height}`} aria-hidden>
          <line
            x1={0}
            y1={height / 2}
            x2={width}
            y2={height / 2}
            className="kbc-spark__empty"
          />
        </svg>
      </span>
    );
  }

  const n = buckets.length;
  const tip =
    hover != null && buckets[hover]
      ? sparkTooltip(buckets[hover]!)
      : sparkTooltip(buckets[buckets.length - 1]!);

  return (
    <span
      className={"kbc-spark" + (className ? ` ${className}` : "")}
      data-kbc-sparkline={dataAttr}
      title={tip}
      aria-label={`${label}: ${tip}`}
    >
      <svg
        width={width}
        height={height}
        viewBox={`0 0 ${width} ${height}`}
        role="img"
      >
        <path d={d} className="kbc-spark__line" fill="none" />
        {buckets.map((b, i) => {
          const x = n === 1 ? width / 2 : (i / (n - 1)) * width;
          const half = n === 1 ? width / 2 : width / (2 * Math.max(1, n - 1));
          return (
            <rect
              key={b.week_start_unix}
              x={Math.max(0, x - half)}
              y={0}
              width={Math.min(width, half * 2)}
              height={height}
              fill="transparent"
              onMouseEnter={() => setHover(i)}
              onMouseLeave={() => setHover(null)}
            >
              <title>{sparkTooltip(b)}</title>
            </rect>
          );
        })}
        {hover != null && (
          <circle
            cx={n === 1 ? width / 2 : (hover / (n - 1)) * width}
            cy={yAt(series, hover, height)}
            r={2}
            className="kbc-spark__dot"
          />
        )}
      </svg>
    </span>
  );
}
