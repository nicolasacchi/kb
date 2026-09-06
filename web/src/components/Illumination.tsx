import { useMemo } from "react";
import { generateIllumination, type Motif, VIEW_BOX } from "../lib/illumination";

// W2.12 — the deterministic per-artifact ornament. See `lib/illumination.ts`
// for the seed/determinism/theming contract; this component is a thin JSX
// renderer over that pure spec (no raw-HTML injection — every motif is
// built as real SVG elements from typed fields).
//
// Purely decorative: `aria-hidden`, no interactive semantics. Placement +
// subtlety (opacity, size cap, z-order under the card's badges/read-state
// dots) live in `gallery.css`'s `.kb-illum` rule — see `Card.tsx`'s anatomy
// comment for exactly where it sits and why.

type Props = {
  /** The artifact id — a stable per-path seed (see lib/illumination.ts's
   * doc comment for why this, not a content hash). */
  seed: string;
};

function motifColorVar(c: Motif["color"]): string {
  if (c === "accent") return "var(--card-accent, var(--accent))";
  if (c === "dim") return "var(--ink-dim)";
  return "currentColor";
}

function MotifNode({ m }: { m: Motif }) {
  const stroke = motifColorVar(m.color);
  if (m.kind === "arc") {
    return (
      <circle
        cx={m.cx}
        cy={m.cy}
        r={m.r}
        fill="none"
        stroke={stroke}
        strokeWidth={1.2}
        strokeLinecap="round"
        pathLength={100}
        strokeDasharray={`${m.sweepPct} 100`}
        strokeDashoffset={-m.offsetPct}
      />
    );
  }
  if (m.kind === "dots") {
    return (
      <g transform={`rotate(${m.rotateDeg} ${m.cx} ${m.cy})`}>
        {Array.from({ length: m.count }, (_, i) => (
          <circle key={i} cx={m.cx + i * m.spacing} cy={m.cy} r={m.r} fill={stroke} />
        ))}
      </g>
    );
  }
  // corner flourish: a small right-angle bracket, rotated to face one of
  // the 4 corners (the declarative SVG `rotate()` transform does the
  // trigonometry — lib/illumination.ts never computes sin/cos itself).
  return (
    <path
      d={`M ${m.x} ${m.y + m.size} L ${m.x} ${m.y} L ${m.x + m.size} ${m.y}`}
      fill="none"
      stroke={stroke}
      strokeWidth={1.2}
      strokeLinecap="round"
      strokeLinejoin="round"
      transform={`rotate(${m.rotateDeg} ${m.x} ${m.y})`}
    />
  );
}

export default function Illumination({ seed }: Props) {
  const spec = useMemo(() => generateIllumination(seed), [seed]);
  return (
    <svg
      className="kb-illum"
      viewBox={`0 0 ${VIEW_BOX} ${VIEW_BOX}`}
      aria-hidden="true"
      focusable="false"
    >
      {spec.motifs.map((m, i) => (
        <MotifNode key={i} m={m} />
      ))}
    </svg>
  );
}
