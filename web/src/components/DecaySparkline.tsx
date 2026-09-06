// MI-W4.1 — the /memory row's inline health-timeline sparkline: a compact
// (~120px) projection of this memory's decaying SCORE over the next 90
// days, with the ACTIVE DecayPolicy's drop threshold drawn as a horizontal
// reference line. Reused verbatim by the MI-W4.3 lineage viewer's per-node
// sparkline — same props shape, same module — so the two surfaces can't
// visually drift apart.
//
// MI-W4.1(revision) — GROUND TRUTH: the decay-policy floor tests the RAW,
// UNDECAYED salience, never the curve this sparkline plots — so the floor
// line is a CONSTANT reference, not a crossing to predict, and the visible
// label is the score's half-life (`decayHalfLifeLabel`, a true fact about
// the RATE alone) rather than a "drops in Nd" claim the curve can't
// actually back up. The one exception: when the memory's raw salience is
// AT/UNDER the floor right now, that's a real, current, actionable fact —
// the label switches to `floorStateLabel` and the component gets a
// `--below-floor` modifier so it's visually unmistakable (that memory is
// invisible to recall today).
//
// Colour: ONE muted ramp reused from the existing design tokens
// (`--muted`/`--ink-dim` for the curve, `--warn` for the floor —
// already the app-wide "pay attention, not alarm" colour) plus `--danger`
// for the below-floor-now state specifically (a fact about right now, not
// a bespoke alarm palette).

import { useMemo } from "react";
import {
  decayHalfLifeLabel,
  floorState,
  floorStateLabel,
  projectDecayCurve,
  type DecayCurveInput,
} from "../lib/decayProjection";

export interface DecaySparklineProps {
  salience: number | null | undefined;
  ageDays: number | null | undefined;
  decayK: number | null | undefined;
  /** Present only when `[memory] scoring_v2_stability` is on (off by default). */
  stability?: number | null;
  /** The active policy's drop threshold; `null` = Loose (never drops). */
  floor: number | null;
  pinned: boolean;
  width?: number;
  height?: number;
}

const DEFAULT_WIDTH = 120;
const DEFAULT_HEIGHT = 32;
const HORIZON_DAYS = 90;

export default function DecaySparkline({
  salience,
  ageDays,
  decayK,
  stability,
  floor,
  pinned,
  width = DEFAULT_WIDTH,
  height = DEFAULT_HEIGHT,
}: DecaySparklineProps) {
  const haveData = salience != null && ageDays != null && decayK != null;

  const { basePath, stablePath, floorY, belowFloorNow, label, title } = useMemo(() => {
    if (!haveData) {
      return {
        basePath: "",
        stablePath: null as string | null,
        floorY: null as number | null,
        belowFloorNow: false,
        label: "—",
        title: "—",
      };
    }
    const input: DecayCurveInput = {
      salience: salience as number,
      ageDaysNow: ageDays as number,
      decayK: decayK as number,
      horizonDays: HORIZON_DAYS,
      samples: 30,
    };
    const toXY = (t: number, y: number) => {
      const x = (t / HORIZON_DAYS) * width;
      // Fixed [0,1] y-domain — salience is always in [0,1], and keeping the
      // domain fixed (rather than per-row auto-scaling) means sparklines
      // are visually COMPARABLE across rows, not just internally consistent.
      const py = height - Math.min(1, Math.max(0, y)) * height;
      return `${x.toFixed(1)},${py.toFixed(1)}`;
    };
    const basePts = projectDecayCurve(input);
    const basePath = `M${basePts.map((p) => toXY(p.t, p.y)).join(" L")}`;

    let stablePath: string | null = null;
    if (stability != null) {
      const stablePts = projectDecayCurve({ ...input, stability });
      stablePath = `M${stablePts.map((p) => toXY(p.t, p.y)).join(" L")}`;
    }

    const state = floorState(salience as number, floor, pinned);
    const floorY =
      floor != null && Number.isFinite(floor)
        ? height - Math.min(1, Math.max(0, floor)) * height
        : null;
    const belowFloorNow = state.kind === "below";
    const stateLabel = floorStateLabel(state);
    const half = decayHalfLifeLabel(decayK as number);
    // Below-floor-now (excluded today) and pinned (exempt from the floor
    // entirely) are both real, notable STATES worth leading with — for
    // every other state (above the floor, or no floor at all) the more
    // informative primary label is the TRUE half-life stat. The full
    // picture (state + half-life) is always available via the tooltip.
    const label = belowFloorNow || pinned ? stateLabel : half;
    const title = `${stateLabel} — ${half}`;

    return { basePath, stablePath, floorY, belowFloorNow, label, title };
  }, [haveData, salience, ageDays, decayK, stability, floor, pinned, width, height]);

  if (!haveData) {
    return (
      <span className="kb-decayspark kb-decayspark--empty" data-testid="decay-sparkline-empty">
        —
      </span>
    );
  }

  return (
    <span
      className={`kb-decayspark${pinned ? " kb-decayspark--pinned" : ""}${
        belowFloorNow ? " kb-decayspark--below-floor" : ""
      }`}
      data-testid="decay-sparkline"
      title={title}
    >
      <svg
        viewBox={`0 0 ${width} ${height}`}
        width={width}
        height={height}
        role="img"
        aria-label={`decay projection: ${title}`}
      >
        {floorY != null && !pinned && (
          <line
            className="kb-decayspark__floor"
            x1={0}
            x2={width}
            y1={floorY}
            y2={floorY}
            data-testid="decay-sparkline-floor"
          />
        )}
        {stablePath && (
          <path className="kb-decayspark__stable" d={stablePath} data-testid="decay-sparkline-stable" />
        )}
        <path className="kb-decayspark__base" d={basePath} data-testid="decay-sparkline-base" />
      </svg>
      <span className="kb-decayspark__label" data-testid="decay-sparkline-label">
        {label}
      </span>
    </span>
  );
}
