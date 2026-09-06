// PRR-U2 (kb v0.39 "The PR Room," §2 S2 / §10) — the agent-verdict card's
// risk arc: an SVG ring showing `risk_score` out of 10, tabular numerals
// (design-ui-mock.html's `.kbc-riskdial`).

const RADIUS = 26;
const CIRCUMFERENCE = 2 * Math.PI * RADIUS;

export interface RiskDialGeometry {
  /// The clamped, rounded score actually drawn (0..10 integer).
  clamped: number;
  /// `stroke-dasharray` for the filled arc — `"<arc-length> <remainder>"`.
  dashArray: string;
}

/// Pure geometry for the risk arc — clamps `score` into `[0, max]` (a
/// report-authored value is agent-supplied text turned number; an
/// out-of-range or non-finite score must never draw a broken/negative arc)
/// and returns the `stroke-dasharray` pair a full-circle `<circle>` needs to
/// render exactly `clamped/max` of its circumference. Rounds to the nearest
/// integer — the dial's own label is always a whole number (`N/10`), so the
/// arc should agree with what's printed beside it.
export function riskDialGeometry(score: number, max = 10): RiskDialGeometry {
  const safeMax = max > 0 ? max : 10;
  const raw = Number.isFinite(score) ? score : 0;
  const clamped = Math.round(Math.min(Math.max(raw, 0), safeMax));
  const filled = (clamped / safeMax) * CIRCUMFERENCE;
  const remainder = CIRCUMFERENCE - filled;
  return { clamped, dashArray: `${filled.toFixed(2)} ${remainder.toFixed(2)}` };
}

export interface RiskDialProps {
  score: number;
  max?: number;
  /// Stroke color for the filled arc — caller picks per severity
  /// (`var(--red)`/`var(--warn)`/`var(--green)`), so this component stays
  /// severity-agnostic.
  color?: string;
}

export default function RiskDial({ score, max = 10, color = "var(--warn)" }: RiskDialProps) {
  const { clamped, dashArray } = riskDialGeometry(score, max);
  return (
    <div className="kbc-riskdial" data-kbc-riskdial={clamped}>
      <svg width="64" height="64" viewBox="0 0 64 64" role="img" aria-label={`risk ${clamped} of ${max}`}>
        <circle cx="32" cy="32" r={RADIUS} fill="none" stroke="var(--rule-hi)" strokeWidth="5" />
        <circle
          cx="32"
          cy="32"
          r={RADIUS}
          fill="none"
          stroke={color}
          strokeWidth="5"
          strokeDasharray={dashArray}
          strokeLinecap="round"
          transform="rotate(-90 32 32)"
        />
        <text
          x="32"
          y="30"
          textAnchor="middle"
          fill="var(--ink)"
          fontSize="17"
          fontWeight="650"
          className="kbc-riskdial__num"
        >
          {clamped}
        </text>
        <text x="32" y="43" textAnchor="middle" fill="var(--ink-dim)" fontSize="9">
          /{max}
        </text>
      </svg>
      <span className="kbc-riskdial__lab">risk</span>
    </div>
  );
}
