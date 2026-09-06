// MI-W4.2b — the salience × days-since-last-recall quadrant scatter. A
// sorted table buries the two outlier shapes that matter ("high salience,
// never recalled" dead weight and "low salience, constantly recalled"
// mis-scoring); a scatter makes both pop as points in the wrong quadrant.
// Pure bucketing lives in `lib/recallQuadrant.ts`; this component only
// projects points into an SVG viewbox and wires click-to-focus.
//
// FIX2 — `highSalienceThreshold`/`dormantDays` are REQUIRED props,
// wire-supplied from `GET /api/memory/triage` (mirrors `decay_k`/
// `drop_threshold`'s existing wire-not-duplicated treatment) rather than
// hand-duplicated module constants that could drift from kb-core's own
// `HIGH_SALIENCE_THRESHOLD`/`DORMANT_DAYS`.
//
// FIX4 — each point is a real interactive control: `role="button"` +
// `tabIndex={0}` + an Enter/Space handler (SVG shapes don't get a native
// click/keyboard binding for free) + an accessible name identifying the
// memory, so keyboard and screen-reader users can reach the same
// click-to-focus interaction mouse users get.

import { useMemo, useState } from "react";
import type { KeyboardEvent } from "react";
import { Link } from "react-router-dom";
import { artifactHref } from "../lib/artifactHref";
import {
  buildQuadrantPoints,
  type QuadrantInput,
  type QuadrantPoint,
  type QuadrantThresholds,
} from "../lib/recallQuadrant";

const WIDTH = 320;
const HEIGHT = 200;
const PAD = 8;
/** X-axis cap — "never recalled" and anything past this many days both
 * plot at the far edge; a linear axis stretched to `Infinity` would
 * squash every real data point into a sliver at the origin. */
const X_CAP_DAYS = 180;

export interface RecallQuadrantScatterProps {
  rows: QuadrantInput[];
  nowUnix: number;
  highSalienceThreshold: number;
  dormantDays: number;
}

function pointDescription(p: QuadrantPoint): string {
  const recency =
    p.daysSinceRecall == null ? "never recalled" : `${Math.round(p.daysSinceRecall)}d since recall`;
  return `${p.title} — salience ${p.salience.toFixed(2)}, ${recency}`;
}

export default function RecallQuadrantScatter({
  rows,
  nowUnix,
  highSalienceThreshold,
  dormantDays,
}: RecallQuadrantScatterProps) {
  const [focused, setFocused] = useState<QuadrantPoint | null>(null);
  const thresholds: QuadrantThresholds = useMemo(
    () => ({ highSalienceThreshold, dormantDays }),
    [highSalienceThreshold, dormantDays],
  );
  const points = useMemo(
    () => buildQuadrantPoints(rows, nowUnix, thresholds),
    [rows, nowUnix, thresholds],
  );

  const xOf = (p: QuadrantPoint) => {
    const days = p.daysSinceRecall ?? X_CAP_DAYS;
    const clamped = Math.min(days, X_CAP_DAYS);
    return PAD + (clamped / X_CAP_DAYS) * (WIDTH - 2 * PAD);
  };
  const yOf = (p: QuadrantPoint) => HEIGHT - PAD - p.salience * (HEIGHT - 2 * PAD);

  const dormantX = PAD + (dormantDays / X_CAP_DAYS) * (WIDTH - 2 * PAD);
  const salienceY = HEIGHT - PAD - highSalienceThreshold * (HEIGHT - 2 * PAD);

  const onPointKeyDown = (e: KeyboardEvent<SVGCircleElement>, p: QuadrantPoint) => {
    if (e.key === "Enter" || e.key === " " || e.key === "Spacebar") {
      e.preventDefault();
      setFocused(p);
    }
  };

  if (rows.length === 0) {
    return (
      <section className="kb-mem__quadrant" data-testid="recall-quadrant">
        <h4>Salience × recall quadrant</h4>
        <p className="kb-mem__quadrant-empty">no memories to plot.</p>
      </section>
    );
  }

  return (
    <section className="kb-mem__quadrant" data-testid="recall-quadrant">
      <h4>Salience × recall quadrant</h4>
      <svg
        className="kb-mem__quadrant-svg"
        viewBox={`0 0 ${WIDTH} ${HEIGHT}`}
        width={WIDTH}
        height={HEIGHT}
        aria-label="salience versus days since last recall, per memory"
      >
        {/* FIX4 — deliberately NOT `role="img"`: that role tells assistive
            tech to treat the whole subtree as one flat, non-interactive
            image, which would swallow the focusable per-point buttons
            below. The `<svg>`'s implicit `graphics-document` role still
            takes the `aria-label` above as its accessible name, without
            pruning descendants. */}
        <line x1={dormantX} x2={dormantX} y1={0} y2={HEIGHT} stroke="var(--rule)" strokeDasharray="3 3" />
        <line x1={0} x2={WIDTH} y1={salienceY} y2={salienceY} stroke="var(--rule)" strokeDasharray="3 3" />
        <text x={WIDTH - PAD} y={12} textAnchor="end" className="kb-mem__quadrant-quad-label">
          dead weight
        </text>
        <text x={PAD} y={HEIGHT - PAD - 4} className="kb-mem__quadrant-quad-label">
          mis-scored
        </text>
        {points.map((p) => (
          <circle
            key={`${p.kb}:${p.id}`}
            className={`kb-mem__quadrant-point kb-mem__quadrant-point--${p.quadrant}${
              focused && focused.id === p.id && focused.kb === p.kb ? " is-focused" : ""
            }`}
            cx={xOf(p)}
            cy={yOf(p)}
            r={4}
            data-testid="recall-quadrant-point"
            data-quadrant={p.quadrant}
            onClick={() => setFocused(p)}
            role="button"
            tabIndex={0}
            aria-label={pointDescription(p)}
            onKeyDown={(e) => onPointKeyDown(e, p)}
          >
            <title>{pointDescription(p)}</title>
          </circle>
        ))}
      </svg>
      <div className="kb-mem__quadrant-legend">
        <span>x: days since last recall (capped {X_CAP_DAYS}d)</span>
        <span>y: salience</span>
      </div>
      {focused && (
        <p className="kb-mem__quadrant-focus" data-testid="recall-quadrant-focus">
          <Link to={artifactHref(focused.kb, focused.sourceRelative)}>{focused.title}</Link> —{" "}
          {focused.quadrant.replace("-", " ")}, salience {focused.salience.toFixed(2)},{" "}
          {focused.daysSinceRecall == null
            ? "never recalled"
            : `${Math.round(focused.daysSinceRecall)}d since recall`}
        </p>
      )}
    </section>
  );
}
