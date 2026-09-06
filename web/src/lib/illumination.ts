// W2.12 — deterministic per-artifact "illumination": a tiny generative SVG
// ornament, seeded from the artifact id so the same artifact always draws
// the same mark (no re-roll on re-render, no drift across machines).
//
// Seed choice: `content_hash` is NOT on the docs wire (confirmed at HEAD —
// `DocSummary`/`DocResponse` carry no such field; it's storage-internal to
// the indexer's dedup cache). The artifact `id` (12-hex, path-derived,
// invariant #27) is the honest available identity: stable per source path,
// present on every doc row, already the seed `tagColor`/`hashPoint`/
// `sessionColorFor` use for their own deterministic hues. An illumination
// therefore marks "this artifact", not "this exact content" — it doesn't
// change on a content edit, only were the file to move/be recreated.
//
// Determinism contract: `generateIllumination`/`illuminationSvg` use only
// integer arithmetic (`Math.imul`, `Math.floor`, `+`/`-`/`*`/`/`) — no
// `Math.sin`/`cos`/`sqrt`, no `Math.random`, no clock, no locale. Angled
// motifs use SVG's own declarative `rotate()` transform (the browser does
// the trigonometry at paint time, not this module at generation time), and
// the "arc" motif uses `pathLength` + `stroke-dasharray` (a ring segment
// expressed as a percentage of the circle's own circumference — no start/
// end point trig needed at all). Same seed ⇒ byte-identical spec/string on
// every machine.
//
// Theming: every motif's stroke/fill is one of `currentColor`,
// `var(--card-accent, var(--accent))`, or `var(--ink-dim)` — never a baked
// hex — so a theme flip (or an accent-preference change) recolors the
// already-rendered SVG for free, the same way `Card`'s existing `::before`/
// `::after` accent chrome does.

export type MotifColor = "dim" | "accent" | "current";

export type ArcMotif = {
  kind: "arc";
  cx: number;
  cy: number;
  r: number;
  /** Percent (0-100) of the circle's circumference the ring segment covers. */
  sweepPct: number;
  /** Percent (0-100) rotation offset of the segment's start point. */
  offsetPct: number;
  color: MotifColor;
};

export type DotsMotif = {
  kind: "dots";
  cx: number;
  cy: number;
  count: number;
  spacing: number;
  r: number;
  /** Declarative SVG rotation (degrees) applied around (cx, cy). */
  rotateDeg: number;
  color: MotifColor;
};

export type CornerMotif = {
  kind: "corner";
  x: number;
  y: number;
  size: number;
  rotateDeg: number;
  color: MotifColor;
};

export type Motif = ArcMotif | DotsMotif | CornerMotif;

export type IlluminationSpec = {
  seed: string;
  motifs: Motif[];
};

/** viewBox is a 24x24 square; `Illumination.tsx` scales it down to fit the
 * card's ornament zone (≤28px — see that component's own size cap). */
export const VIEW_BOX = 24;

const MOTIF_KINDS = ["arc", "dots", "corner"] as const;
// Weighted so "dim" (`var(--ink-dim)`) dominates — the always-on ornament
// must read as quiet texture, not a competing accent (SPEC item 4).
const COLORS: MotifColor[] = ["dim", "dim", "dim", "accent", "accent", "current"];

function fnv1a32(s: string): number {
  let h = 0x811c9dc5;
  for (let i = 0; i < s.length; i++) {
    h ^= s.charCodeAt(i);
    h = Math.imul(h, 0x01000193);
  }
  return h >>> 0;
}

/** A tiny xorshift32 PRNG seeded from the FNV hash. Deterministic, fast,
 * dependency-free — the same family as `AtlasView.hashPoint`, extended
 * into a short sequence instead of one splat. */
function xorshift32(seed: number): () => number {
  let x = seed >>> 0;
  if (x === 0) x = 0x9e3779b9; // a zero seed would lock xorshift at 0 forever
  return () => {
    x ^= x << 13;
    x >>>= 0;
    x ^= x >>> 17;
    x ^= x << 5;
    x >>>= 0;
    return x;
  };
}

function round2(n: number): number {
  return Math.round(n * 100) / 100;
}

/** Pure generator: seed (the artifact id) → a small, typed motif spec.
 * 2-4 motifs, each an arc / dot-row / corner-flourish with seed-derived
 * position, size, rotation, and color-slot — never raw colors. */
export function generateIllumination(seed: string): IlluminationSpec {
  const next = xorshift32(fnv1a32(seed));
  const frac = () => (next() % 1_000_000) / 1_000_000; // [0, 1)
  const pick = <T,>(arr: readonly T[]): T => arr[Math.floor(frac() * arr.length) % arr.length];

  const motifCount = 2 + Math.floor(frac() * 3); // 2..4
  const motifs: Motif[] = [];
  for (let i = 0; i < motifCount; i++) {
    const kind = pick(MOTIF_KINDS);
    const color = pick(COLORS);
    if (kind === "arc") {
      motifs.push({
        kind: "arc",
        cx: round2(4 + frac() * 16),
        cy: round2(4 + frac() * 16),
        r: round2(3 + frac() * 6),
        sweepPct: round2(20 + frac() * 55),
        offsetPct: round2(frac() * 100),
        color,
      });
    } else if (kind === "dots") {
      motifs.push({
        kind: "dots",
        cx: round2(3 + frac() * 10),
        cy: round2(3 + frac() * 10),
        count: 2 + Math.floor(frac() * 3), // 2..4
        spacing: round2(3 + frac() * 3),
        r: round2(0.6 + frac() * 0.7),
        rotateDeg: Math.floor(frac() * 4) * 45, // one of 0/45/90/135
        color,
      });
    } else {
      motifs.push({
        kind: "corner",
        x: round2(frac() * 18),
        y: round2(frac() * 18),
        size: round2(4 + frac() * 6),
        rotateDeg: Math.floor(frac() * 4) * 90, // one of 0/90/180/270
        color,
      });
    }
  }
  return { seed, motifs };
}

function colorVar(c: MotifColor): string {
  if (c === "accent") return "var(--card-accent, var(--accent))";
  if (c === "dim") return "var(--ink-dim)";
  return "currentColor";
}

function renderMotif(m: Motif): string {
  const stroke = colorVar(m.color);
  if (m.kind === "arc") {
    return (
      `<circle cx="${m.cx}" cy="${m.cy}" r="${m.r}" fill="none" ` +
      `stroke="${stroke}" stroke-width="1.2" stroke-linecap="round" ` +
      `pathLength="100" stroke-dasharray="${m.sweepPct} 100" ` +
      `stroke-dashoffset="${-m.offsetPct}" />`
    );
  }
  if (m.kind === "dots") {
    const dots = Array.from({ length: m.count }, (_, i) => {
      const cx = round2(m.cx + i * m.spacing);
      return `<circle cx="${cx}" cy="${m.cy}" r="${m.r}" fill="${stroke}" />`;
    }).join("");
    return `<g transform="rotate(${m.rotateDeg} ${m.cx} ${m.cy})">${dots}</g>`;
  }
  // corner: a small right-angle bracket, rotated to face one of the 4 corners.
  const s = m.size;
  return (
    `<path d="M ${m.x} ${m.y + s} L ${m.x} ${m.y} L ${m.x + s} ${m.y}" ` +
    `fill="none" stroke="${stroke}" stroke-width="1.2" stroke-linecap="round" ` +
    `stroke-linejoin="round" transform="rotate(${m.rotateDeg} ${m.x} ${m.y})" />`
  );
}

/** Full `<svg>…</svg>` markup for `seed` — deterministic, golden-pinned.
 * `Illumination.tsx` renders JSX built from `generateIllumination` directly
 * (no raw-HTML injection into the DOM); this string form exists for the
 * determinism contract's own test coverage and any non-React consumer. */
export function illuminationSvg(seed: string): string {
  const spec = generateIllumination(seed);
  const body = spec.motifs.map(renderMotif).join("");
  return (
    `<svg viewBox="0 0 ${VIEW_BOX} ${VIEW_BOX}" width="${VIEW_BOX}" height="${VIEW_BOX}" ` +
    `aria-hidden="true">${body}</svg>`
  );
}
