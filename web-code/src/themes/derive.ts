// kbc-theme/1 — the pure derivation engine.
//
// ONE authored palette per theme (26 anchors in the Rosé-Pine role
// vocabulary) → the full ~80-role token set the SPA's CSS already consumes.
// Deterministic, LLM-free, dependency-free and side-effect-free: the same
// registry always produces byte-identical CSS, which is what lets
// `registry.gen.ts` / `themes.gen.css` be CHECKED IN and pinned by a drift
// test that regenerates them in memory.
//
// Deliberately ONE self-contained file with no relative imports: the two
// dev-time node scripts (`scripts/gen-themes.mjs`, `scripts/theme-lint.mjs`)
// import this module directly by its `.ts` specifier and rely on node's
// built-in type stripping, which has no `.js`→`.ts` resolution rewrite. A
// second module would need an extension the bundler and node disagree about.
// For the same reason there are no TS-only runtime constructs here (no
// `enum`, no `namespace`, no parameter properties) — type stripping erases
// types, it does not compile them.
//
// Direction of derivation is ONE-WAY and never circular:
//
//     ANCHORS (authored, 26)
//        ↓ pure OKLCH math (mix / lightness nudge / gamut map)
//     ROLES (~80 CSS custom properties)
//        ↓ 1:1 binding, no math
//     SURFACES (chrome vars · the 15 `.kbc-hl-*` syntax classes · diff · lanes)
//
// The AA REPAIR pass is the one place a vendor value moves. Anchors stay
// verbatim in `registry.json` (the receipt); a role whose contrast against
// any background it can actually sit on falls under its floor is nudged in
// OKLCH lightness ONLY — hue and chroma are preserved, so a repaired Nord
// comment is still Nord's hue, just legible. Every repair is recorded and
// emitted as a comment in the generated CSS, so a moved vendor value is
// never presented as an authored one.

/* ---------------------------------------------------------------------- */
/* Types                                                                    */
/* ---------------------------------------------------------------------- */

export type Appearance = "light" | "dark";

/// The 26 authored anchors. Names are Rosé Pine's role vocabulary (the
/// smallest published palette grammar that covers three surface depths,
/// three highlight levels and three ink weights) plus a ten-hue accent
/// wheel, the UI accent pair, the three diff hues and the scrim.
export interface ThemeAnchors {
  base: string;
  surface: string;
  sunken: string;
  overlay: string;
  highlightLow: string;
  highlightMed: string;
  highlightHigh: string;
  muted: string;
  subtle: string;
  text: string;
  red: string;
  orange: string;
  yellow: string;
  green: string;
  cyan: string;
  blue: string;
  violet: string;
  magenta: string;
  pink: string;
  teal: string;
  accent: string;
  accentFg: string;
  diffAdd: string;
  diffDel: string;
  diffMod: string;
  scrim: string;
}

export const ANCHOR_KEYS: readonly (keyof ThemeAnchors)[] = [
  "base",
  "surface",
  "sunken",
  "overlay",
  "highlightLow",
  "highlightMed",
  "highlightHigh",
  "muted",
  "subtle",
  "text",
  "red",
  "orange",
  "yellow",
  "green",
  "cyan",
  "blue",
  "violet",
  "magenta",
  "pink",
  "teal",
  "accent",
  "accentFg",
  "diffAdd",
  "diffDel",
  "diffMod",
  "scrim",
];

export interface ThemeDef {
  id: string;
  family: string;
  family_name: string;
  name: string;
  appearance: Appearance;
  license: string;
  source_url: string;
  anchors: ThemeAnchors;
  /// Authored, verbatim role values applied AFTER derivation and AFTER the
  /// repair pass — the escape hatch for a vendor step this engine's math
  /// cannot reach, and the mechanism the two built-in `kbc-*` themes use to
  /// reproduce `tokens.css` byte-for-byte.
  overrides?: Record<string, string>;
  notes?: string;
}

export interface Registry {
  schema: string;
  themes: ThemeDef[];
  skipped?: { family: string; reason: string }[];
}

export type Rgb = { r: number; g: number; b: number };
export type Oklch = { l: number; c: number; h: number };

export interface Repair {
  role: string;
  from: string;
  to: string;
  floor: number;
  before: number;
  after: number;
  /// The background the worst pre-repair ratio was measured against.
  against: string;
}

export interface DerivedTheme {
  id: string;
  appearance: Appearance;
  /// role name (without the leading `--`) → the CSS value to emit.
  roles: Record<string, string>;
  /// The subset of `roles` that resolve to an opaque sRGB colour, for the
  /// lint's arithmetic. Non-colour roles (shadows, `age-tint`) are absent.
  colors: Record<string, Rgb>;
  repairs: Repair[];
  overridden: string[];
}

/* ---------------------------------------------------------------------- */
/* Colour primitives                                                        */
/* ---------------------------------------------------------------------- */

const HEX_RE = /^#([0-9a-fA-F]{6})$/;

export function parseHex(hex: string): Rgb {
  const m = hex.trim().match(HEX_RE);
  if (!m) throw new Error(`not a #rrggbb hex colour: ${JSON.stringify(hex)}`);
  const n = parseInt(m[1], 16);
  return { r: (n >> 16) & 0xff, g: (n >> 8) & 0xff, b: n & 0xff };
}

function clamp255(v: number): number {
  return v < 0 ? 0 : v > 255 ? 255 : v;
}

export function toHex(c: Rgb): string {
  const h = (v: number) => Math.round(clamp255(v)).toString(16).padStart(2, "0");
  return `#${h(c.r)}${h(c.g)}${h(c.b)}`;
}

function srgbToLinear(v: number): number {
  const s = v / 255;
  return s <= 0.04045 ? s / 12.92 : Math.pow((s + 0.055) / 1.055, 2.4);
}

function linearToSrgb(v: number): number {
  const s = v <= 0.0031308 ? v * 12.92 : 1.055 * Math.pow(v, 1 / 2.4) - 0.055;
  return s * 255;
}

/// Oklab (Björn Ottosson) from linear sRGB.
function linearToOklab(lr: number, lg: number, lb: number): [number, number, number] {
  const l = 0.4122214708 * lr + 0.5363325363 * lg + 0.0514459929 * lb;
  const m = 0.2119034982 * lr + 0.6806995451 * lg + 0.1073969566 * lb;
  const s = 0.0883024619 * lr + 0.2817188376 * lg + 0.6299787005 * lb;
  const l2 = Math.cbrt(l);
  const m2 = Math.cbrt(m);
  const s2 = Math.cbrt(s);
  return [
    0.2104542553 * l2 + 0.793617785 * m2 - 0.0040720468 * s2,
    1.9779984951 * l2 - 2.428592205 * m2 + 0.4505937099 * s2,
    0.0259040371 * l2 + 0.7827717662 * m2 - 0.808675766 * s2,
  ];
}

function oklabToLinear(L: number, a: number, b: number): [number, number, number] {
  const l2 = L + 0.3963377774 * a + 0.2158037573 * b;
  const m2 = L - 0.1055613458 * a - 0.0638541728 * b;
  const s2 = L - 0.0894841775 * a - 1.291485548 * b;
  const l = l2 * l2 * l2;
  const m = m2 * m2 * m2;
  const s = s2 * s2 * s2;
  return [
    4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s,
    -1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s,
    -0.0041960863 * l - 0.7034186147 * m + 1.707614701 * s,
  ];
}

export function toOklab(c: Rgb): [number, number, number] {
  return linearToOklab(srgbToLinear(c.r), srgbToLinear(c.g), srgbToLinear(c.b));
}

export function toOklch(c: Rgb): Oklch {
  const [L, a, b] = toOklab(c);
  const chroma = Math.sqrt(a * a + b * b);
  let hue = (Math.atan2(b, a) * 180) / Math.PI;
  if (hue < 0) hue += 360;
  return { l: L, c: chroma, h: hue };
}

function oklchToLinearTriple(v: Oklch): [number, number, number] {
  const rad = (v.h * Math.PI) / 180;
  return oklabToLinear(v.l, v.c * Math.cos(rad), v.c * Math.sin(rad));
}

function inGamut(t: [number, number, number]): boolean {
  return t.every((v) => v >= -0.0001 && v <= 1.0001);
}

/// OKLCH → sRGB with chroma-reduction gamut mapping (the CSS Color 4
/// approach: keep L and H, bisect C down until the colour is representable).
/// Deterministic: a fixed 24-step bisection, never a float-equality loop.
export function fromOklch(v: Oklch): Rgb {
  const l = Math.max(0, Math.min(1, v.l));
  let lo = 0;
  let hi = Math.max(0, v.c);
  let best = oklchToLinearTriple({ l, c: 0, h: v.h });
  if (inGamut(oklchToLinearTriple({ l, c: hi, h: v.h }))) {
    best = oklchToLinearTriple({ l, c: hi, h: v.h });
  } else {
    for (let i = 0; i < 24; i++) {
      const mid = (lo + hi) / 2;
      const t = oklchToLinearTriple({ l, c: mid, h: v.h });
      if (inGamut(t)) {
        best = t;
        lo = mid;
      } else {
        hi = mid;
      }
    }
  }
  return {
    r: clamp255(linearToSrgb(Math.max(0, Math.min(1, best[0])))),
    g: clamp255(linearToSrgb(Math.max(0, Math.min(1, best[1])))),
    b: clamp255(linearToSrgb(Math.max(0, Math.min(1, best[2])))),
  };
}

/// `color-mix(in oklch, b <t*100>%, a)` — polar interpolation of L/C/H with
/// the SHORTER hue arc, matching the CSS default (`shorter hue`). A hue is
/// meaningless at zero chroma, so an achromatic endpoint inherits the
/// other's hue rather than dragging the mix through grey.
export function mixOklch(a: Rgb, b: Rgb, t: number): Rgb {
  const A = toOklch(a);
  const B = toOklch(b);
  const ha = A.c < 1e-6 ? B.h : A.h;
  const hb = B.c < 1e-6 ? A.h : B.h;
  let dh = hb - ha;
  if (dh > 180) dh -= 360;
  if (dh < -180) dh += 360;
  return fromOklch({
    l: A.l + (B.l - A.l) * t,
    c: A.c + (B.c - A.c) * t,
    h: ha + dh * t,
  });
}

/// WCAG 2.x relative luminance.
export function luminance(c: Rgb): number {
  return 0.2126 * srgbToLinear(c.r) + 0.7152 * srgbToLinear(c.g) + 0.0722 * srgbToLinear(c.b);
}

/// WCAG 2.x contrast ratio, 1..21.
export function contrastRatio(a: Rgb, b: Rgb): number {
  const la = luminance(a);
  const lb = luminance(b);
  const hi = Math.max(la, lb);
  const lo = Math.min(la, lb);
  return (hi + 0.05) / (lo + 0.05);
}

/// Euclidean distance in Oklab — the perceptual "are these two states
/// distinguishable" metric the Lane Budget's state-separation floors use.
/// NOT a contrast ratio: two colours can be far apart in Oklab and still
/// illegible as text on one another, and vice versa.
export function oklabDistance(a: Rgb, b: Rgb): number {
  const A = toOklab(a);
  const B = toOklab(b);
  return Math.hypot(A[0] - B[0], A[1] - B[1], A[2] - B[2]);
}

/// Pick the candidate with the highest contrast against `bg`. Ties resolve
/// to the earliest candidate, so the caller's preference order is honoured.
export function bestContrast(bg: Rgb, candidates: Rgb[]): Rgb {
  let best = candidates[0];
  let bestRatio = -1;
  for (const c of candidates) {
    const r = contrastRatio(bg, c);
    if (r > bestRatio + 1e-9) {
      best = c;
      bestRatio = r;
    }
  }
  return best;
}

/* ---------------------------------------------------------------------- */
/* The AA repair pass                                                       */
/* ---------------------------------------------------------------------- */

/// Text floor: WCAG AA body text.
export const FLOOR_TEXT = 4.5;
/// Non-text / decorative floor: WCAG AA for UI components and graphical
/// objects. `--ink-dim` / `--ink-faint` live here BY DESIGN — tokens.css's
/// own SH.C1 note already records that `--ink-faint` is "decorative
/// rules/disabled glyphs only, never running text".
export const FLOOR_NONTEXT = 3.0;

const REPAIR_STEP = 0.004;
const REPAIR_MAX_STEPS = 220;

function worstAgainst(fg: Rgb, bgs: Rgb[]): { ratio: number; bg: Rgb } {
  let ratio = Infinity;
  let bg = bgs[0];
  for (const candidate of bgs) {
    const r = contrastRatio(fg, candidate);
    if (r < ratio) {
      ratio = r;
      bg = candidate;
    }
  }
  return { ratio, bg };
}

/// Nudge `fg`'s OKLCH lightness away from the backgrounds until it clears
/// `floor` against ALL of them. Hue and chroma are preserved (the gamut
/// mapper may reduce chroma when a lightness step pushes the colour out of
/// sRGB — that is the only chroma change, and it is forced, not chosen).
/// Returns the input unchanged when it already passes.
export function repairContrast(fg: Rgb, bgs: Rgb[], floor: number, appearance: Appearance): Rgb {
  if (worstAgainst(fg, bgs).ratio >= floor) return fg;
  const start = toOklch(fg);
  // Dark theme → backgrounds are dark → the foreground must go LIGHTER.
  const dir = appearance === "dark" ? 1 : -1;
  for (let i = 1; i <= REPAIR_MAX_STEPS; i++) {
    const l = start.l + dir * REPAIR_STEP * i;
    if (l < 0 || l > 1) break;
    const cur = fromOklch({ l, c: start.c, h: start.h });
    if (worstAgainst(cur, bgs).ratio >= floor) return cur;
  }
  // Unreachable in-hue: fall back to the extreme the direction points at.
  return fromOklch({ l: dir > 0 ? 1 : 0, c: 0, h: start.h });
}

/* ---------------------------------------------------------------------- */
/* Role derivation                                                          */
/* ---------------------------------------------------------------------- */

/// The 15 `.kbc-hl-*` classes are EXACTLY `HighlightClass`'s fixed set in
/// `crates/kb-code-server/src/highlight.rs` (Keyword, String, Comment,
/// Function, Type, Number, Variable, Constant, Operator, Punctuation,
/// Property, Attribute, Label, Escape, Other). v7.0 does NOT widen the
/// server vocabulary — that needs the shared-`salt` split (recon §8.2), and
/// is v7.2's job. `other` is the 15th member; there is no `tag` class.
export const SYNTAX_ROLES: readonly string[] = [
  "keyword",
  "string",
  "comment",
  "function",
  "type",
  "number",
  "variable",
  "constant",
  "operator",
  "punctuation",
  "property",
  "attribute",
  "label",
  "escape",
  "other",
];

/// Which accent anchor each syntax class draws from. Chosen against the
/// base16 styling guide's stated intent (base09 constants, base0A classes,
/// base0B strings, base0C escapes, base0D functions, base0E keywords) and
/// the low-colour school's rule that plain identifiers stay plain
/// foreground: `variable` is `text`, not a hue.
const SYNTAX_SOURCE: Record<string, string> = {
  keyword: "violet",
  string: "green",
  comment: "ink-mute",
  function: "blue",
  type: "yellow",
  number: "orange",
  variable: "ink",
  constant: "orange",
  operator: "cyan",
  punctuation: "ink-mute",
  property: "teal",
  attribute: "yellow",
  label: "magenta",
  escape: "pink",
  other: "ink-mute",
};

/// The blame/age ramp's 10 quantized bands, as OPAQUE line backgrounds:
/// band 0 = newest = strongest tint, band 9 = oldest = faintest. One hue
/// (the theme's `blue`), lightness-ramped — a single-hue ramp degrades
/// gracefully under every CVD, which a warm→cool hue ramp does not.
export const AGE_BANDS = 10;
/// Band 0 (newest) tints with 20% of the age hue; each older band drops 1.6
/// points, so band 9 sits at 5.6%. Per the Lane Budget the age ramp owns
/// GUTTER A, not the code line background — which is why the bands are not
/// in `codeBgs` below. (The existing five-bucket `.kbc-age-line--N` line
/// overlay is the pre-Lane-Budget implementation and is left untouched by
/// this unit; these tokens are what a later unit moves it onto.)
const AGE_TINT_MAX = 0.2;
const AGE_TINT_STEP = 0.016;

/// The provenance rail's 8 session hues, chosen from the theme's own accent
/// wheel by farthest-point sampling in Oklab (maximise the minimum pairwise
/// distance) — the fix for the classic name-hash failure where two adjacent
/// identifiers get perceptually adjacent colours.
export const PROV_HUES = 8;
const PROV_WHEEL: readonly (keyof ThemeAnchors)[] = [
  "red",
  "orange",
  "yellow",
  "green",
  "cyan",
  "blue",
  "violet",
  "magenta",
  "pink",
  "teal",
];

const DARK_SHADOWS: Record<string, string> = {
  shadow: "0 1px 0 rgba(255, 255, 255, 0.04) inset, 0 8px 24px rgba(0, 0, 0, 0.4)",
  "shadow-card": "0 1px 0 rgba(255, 255, 255, 0.03) inset, 0 1px 2px rgba(0, 0, 0, 0.4)",
  "shadow-card-hover":
    "0 1px 0 rgba(255, 255, 255, 0.06) inset, 0 14px 30px rgba(0, 0, 0, 0.5), 0 2px 6px rgba(0, 0, 0, 0.3)",
  "shadow-lift": "0 18px 50px rgb(0 0 0 / 0.45)",
  "shadow-md": "0 4px 12px rgba(0, 0, 0, 0.4)",
};

const LIGHT_SHADOWS: Record<string, string> = {
  shadow: "0 1px 0 rgba(255, 255, 255, 0.5) inset, 0 8px 24px rgba(0, 0, 0, 0.06)",
  "shadow-card":
    "0 1px 0 rgba(255, 255, 255, 0.6) inset, 0 1px 2px rgba(0, 0, 0, 0.05), 0 0 0 1px rgba(0, 0, 0, 0.03)",
  "shadow-card-hover":
    "0 1px 0 rgba(255, 255, 255, 0.8) inset, 0 12px 28px rgba(0, 0, 0, 0.1), 0 2px 6px rgba(0, 0, 0, 0.05)",
  "shadow-lift": "0 18px 50px rgba(0, 0, 0, 0.14), 0 2px 8px rgba(0, 0, 0, 0.06)",
  "shadow-md": "0 4px 12px rgba(0, 0, 0, 0.12)",
};

/// Pick `n` maximally-separated colours from a wheel.
///
/// Two things make this non-trivial in practice. First, most real families
/// have FEWER than ten distinct accents and the catalogue fills the missing
/// slots by reusing a neighbour (Rosé Pine has six accents, Nord and One
/// each ship a single purple) — so the pool must be DEDUPED before sampling,
/// or the sampler happily returns the same hex twice and two sessions get
/// the same provenance colour. Second, after dedupe some families cannot
/// supply `n` distinct hues at all; rather than return a short list (which
/// would silently collapse session 7 onto session 0), the remainder is
/// SYNTHESISED inside the family's own gamut: the largest circular gap in
/// the chosen hue set is bisected, at the wheel's mean lightness and chroma.
/// Deterministic, and never an off-family hue.
function farthestPointSample(pool: Rgb[], n: number): Rgb[] {
  const seen = new Set<string>();
  const uniq: Rgb[] = [];
  for (const c of pool) {
    const k = toHex(c);
    if (seen.has(k)) continue;
    seen.add(k);
    uniq.push(c);
  }

  let chosenColors: Rgb[];
  if (uniq.length <= n) {
    chosenColors = uniq.slice();
  } else {
    // Deterministic seed: the FIRST wheel entry (authored order), then greedy
    // maximin. Fully order-determined — no float tie-breaks, no randomness.
    const chosen: number[] = [0];
    while (chosen.length < n) {
      let bestIdx = -1;
      let bestMin = -1;
      for (let i = 0; i < uniq.length; i++) {
        if (chosen.includes(i)) continue;
        let minD = Infinity;
        for (const j of chosen) minD = Math.min(minD, oklabDistance(uniq[i], uniq[j]));
        if (minD > bestMin + 1e-12) {
          bestMin = minD;
          bestIdx = i;
        }
      }
      chosen.push(bestIdx);
    }
    chosenColors = chosen.map((i) => uniq[i]);
  }

  if (chosenColors.length >= n) return chosenColors.slice(0, n);

  const lch = uniq.map(toOklch);
  const meanL = lch.reduce((acc, v) => acc + v.l, 0) / lch.length;
  const meanC = lch.reduce((acc, v) => acc + v.c, 0) / lch.length;
  while (chosenColors.length < n) {
    const hues = chosenColors.map((c) => toOklch(c).h).sort((a, b) => a - b);
    let bestGap = -1;
    let bestHue = 0;
    for (let i = 0; i < hues.length; i++) {
      const a = hues[i];
      const b = i + 1 < hues.length ? hues[i + 1] : hues[0] + 360;
      const gap = b - a;
      if (gap > bestGap + 1e-9) {
        bestGap = gap;
        bestHue = (a + gap / 2) % 360;
      }
    }
    chosenColors.push(fromOklch({ l: meanL, c: meanC, h: bestHue }));
  }
  return chosenColors;
}

/// After sampling, some families still hand back two provenance hues that
/// are merely ADJACENT rather than distinct (Nord's frost blues, Gruvbox's
/// two greens, Kanagawa's low-chroma restraint). A provenance colour
/// identifies WHO wrote a line, so an adjacent pair is a misattribution
/// waiting to happen — the exact failure mode the 2009 KDevelop / 2014
/// Brooks "rainbow identifiers" work kept hitting with name hashing.
///
/// Repair is a bounded, deterministic LIGHTNESS spread: find the globally
/// closest pair, push the LATER slot away from the background (the same
/// direction `repairContrast` uses, so separation never costs contrast),
/// re-measure, repeat. Hue and chroma are untouched, so every rail stays an
/// in-family colour.
function spreadProvenance(chosen: Rgb[], floor: number, appearance: Appearance): Rgb[] {
  const out = chosen.slice();
  const dir = appearance === "dark" ? 1 : -1;
  for (let pass = 0; pass < 8; pass++) {
    let wi = -1;
    let wj = -1;
    let worst = Infinity;
    for (let i = 0; i < out.length; i++) {
      for (let j = i + 1; j < out.length; j++) {
        const d = oklabDistance(out[i], out[j]);
        if (d < worst) {
          worst = d;
          wi = i;
          wj = j;
        }
      }
    }
    if (worst >= floor || wi < 0) break;
    const b = toOklch(out[wj]);
    for (let step = 1; step <= 60; step++) {
      const l = Math.max(0, Math.min(1, b.l + dir * 0.008 * step));
      const cand = fromOklch({ l, c: b.c, h: b.h });
      if (oklabDistance(out[wi], cand) >= floor) {
        out[wj] = cand;
        break;
      }
      if (l <= 0 || l >= 1) break;
    }
  }
  return out;
}

/// The surface stack's ELEVATION CAP.
///
/// Families do not agree on what their "overlay" tone is for. Nord's nord2
/// (`#434c5e`) is documented as a selection/highlight shade and sits a full
/// 0.10 OKLCH-L above nord0; kb-code's own `--bg-card-hi` is 0.045 above its
/// `--bg`. Mapping the former straight onto the latter turns a selection
/// colour into a PAGE colour, and because `--bg-card-hi` carries body text
/// (the CM6 active line, hover cards), the AA repair pass then has to lift
/// every foreground against it — which is how a faithful Nord import comes
/// out looking washed out.
///
/// So the three non-base surfaces are capped at a fixed OKLCH lightness
/// distance from `base`, preserving hue, chroma and the DIRECTION of the
/// step (light themes elevate upward, dark themes upward too, but a
/// `sunken` goes the other way and must stay that way). Within the cap a
/// colour is returned untouched, so no theme that already sits in kb-code's
/// register is disturbed — including the two built-ins.
const MAX_ELEVATION_L = 0.06;

function clampElevation(c: Rgb, base: Rgb): Rgb {
  const b = toOklch(base);
  const x = toOklch(c);
  const d = x.l - b.l;
  if (Math.abs(d) <= MAX_ELEVATION_L) return c;
  return fromOklch({ l: b.l + Math.sign(d) * MAX_ELEVATION_L, c: x.c, h: x.h });
}

function rgba(c: Rgb, alpha: number): string {
  return `rgba(${Math.round(c.r)}, ${Math.round(c.g)}, ${Math.round(c.b)}, ${alpha})`;
}

/// Derive the full role set for one theme. Pure: same input → same output,
/// no clock, no randomness, no environment reads.
export function deriveTheme(def: ThemeDef): DerivedTheme {
  const a = def.anchors;
  for (const k of ANCHOR_KEYS) {
    if (typeof a[k] !== "string") {
      throw new Error(`${def.id}: missing anchor ${String(k)}`);
    }
  }
  const px = (k: keyof ThemeAnchors) => parseHex(a[k]);
  const appearance = def.appearance;
  const dark = appearance === "dark";

  const base = px("base");
  const surface = clampElevation(px("surface"), base);
  const sunken = clampElevation(px("sunken"), base);
  const overlay = clampElevation(px("overlay"), base);

  const diffAdd = px("diffAdd");
  const diffDel = px("diffDel");
  const diffMod = px("diffMod");

  // Diff tints are BACKGROUNDS code text sits on (the diff renderer paints
  // the same `.kbc-hl-*` classes onto tinted lines), so they must exist
  // before the repair pass touches a single code foreground.
  const diffAddBg = mixOklch(base, diffAdd, 0.1);
  const diffDelBg = mixOklch(base, diffDel, 0.1);
  const diffModBg = mixOklch(base, diffMod, 0.1);

  /// TWO background sets, because the Lane Budget says where each role can
  /// appear and the repair pass must not over-correct a role onto a surface
  /// it never touches:
  ///
  ///   surfaceBgs — the four chrome surfaces. Chrome roles (`--accent`,
  ///     `--warn`/`--green`/`--red`/`--blue` as chip/label text, the two
  ///     decorative inks) are measured here ONLY: `--accent` is link/focus
  ///     chrome and never paints a diff line.
  ///   codeBgs — surfaces PLUS the three diff tints. `--ink` and all fifteen
  ///     `--syn-*` roles are measured here, because the diff renderer paints
  ///     the same highlight classes onto tinted lines.
  ///   inkBgs — `codeBgs` PLUS all ten blame-age bands. Only the PRIMARY
  ///     foreground is held to this: whatever a lane paints, the legend /
  ///     date / line number rendered over an age band must stay readable.
  ///     The decorative inks are NOT — `--ink-faint` is documented in
  ///     tokens.css as "decorative rules/disabled glyphs only".
  const surfaceBgs: Rgb[] = [base, surface, sunken, overlay];

  const repairs: Repair[] = [];
  const fix = (role: string, from: Rgb, floor: number, bgs: Rgb[]): Rgb => {
    const before = worstAgainst(from, bgs);
    if (before.ratio >= floor) return from;
    const to = repairContrast(from, bgs, floor, appearance);
    repairs.push({
      role,
      from: toHex(from),
      to: toHex(to),
      floor,
      before: Math.round(before.ratio * 100) / 100,
      after: Math.round(worstAgainst(to, bgs).ratio * 100) / 100,
      against: toHex(before.bg),
    });
    return to;
  };

  const colors: Record<string, Rgb> = {};
  const roles: Record<string, string> = {};
  const put = (role: string, c: Rgb) => {
    colors[role] = c;
    roles[role] = toHex(c);
  };

  // --- surfaces ---------------------------------------------------------
  put("bg", base);
  put("bg-panel", surface);
  put("bg-panel-2", sunken);
  put("bg-card", surface);
  put("bg-card-hi", overlay);
  put("rule", px("highlightLow"));
  put("rule-hi", px("highlightMed"));
  put("hl-low", px("highlightLow"));
  put("hl-med", px("highlightMed"));
  put("hl-high", px("highlightHigh"));

  // --- status hues ------------------------------------------------------
  // Chrome semantics first: `--blue` is also the age ramp's single hue, and
  // the ramp's strongest band is one of the code backgrounds below.
  // `--warn` is the AMBER semantic (today's `#d8a04b`), so it draws from
  // `yellow`, not `orange`. That is not cosmetic: `--warn` and `--red` are
  // an opposed pair (concern vs blocker) and in nearly every real palette
  // the family's orange sits within a hair of its red in Oklab — Modus
  // Vivendi ships `#ff6b55` orange beside `#ff5f5f` red — while its yellow
  // is comfortably clear of both. `orange` stays the source for the numeric
  // literal / constant syntax roles.
  const warn = fix("warn", px("yellow"), FLOOR_TEXT, surfaceBgs);
  const green = fix("green", px("green"), FLOOR_TEXT, surfaceBgs);
  const red = fix("red", px("red"), FLOOR_TEXT, surfaceBgs);
  const blue = fix("blue", px("blue"), FLOOR_TEXT, surfaceBgs);
  put("warn", warn);
  put("green", green);
  put("red", red);
  put("blue", blue);

  const ageBands: Rgb[] = [];
  for (let i = 0; i < AGE_BANDS; i++) {
    ageBands.push(mixOklch(base, blue, AGE_TINT_MAX - i * AGE_TINT_STEP));
  }
  ageBands.forEach((c, i) => put(`age-band-${i}`, c));

  const codeBgs: Rgb[] = [...surfaceBgs, diffAddBg, diffDelBg, diffModBg];
  const inkBgs: Rgb[] = [...codeBgs, ...ageBands];

  // --- ink --------------------------------------------------------------
  const ink = fix("ink", px("text"), FLOOR_TEXT, inkBgs);
  const inkMute = fix("ink-mute", px("subtle"), FLOOR_TEXT, codeBgs);
  put("ink", ink);
  put("ink-mute", inkMute);
  put("ink-dim", fix("ink-dim", mixOklch(inkMute, base, 0.25), FLOOR_NONTEXT, surfaceBgs));
  // `--ink-faint` is exempt by its own contract (decorative rules / disabled
  // glyphs, never running text) — derived, never repaired, never linted.
  put("ink-faint", mixOklch(inkMute, base, 0.45));

  // --- accent -----------------------------------------------------------
  const accent = fix("accent", px("accent"), FLOOR_TEXT, surfaceBgs);
  const accentLch = toOklch(accent);
  const softRaw = fromOklch({
    l: Math.max(0, Math.min(1, accentLch.l + (dark ? 0.1 : -0.08))),
    c: accentLch.c,
    h: accentLch.h,
  });
  put("accent", accent);
  put("accent-soft", fix("accent-soft", softRaw, FLOOR_TEXT, surfaceBgs));
  put("accent-bg", mixOklch(base, accent, 0.12));
  put("accent-bor", mixOklch(base, accent, 0.32));
  // R12 — `--accent-fg` was a theme-invariant white; on a light accent that
  // is white-on-light. Pick by measurement, preferring the authored value.
  const accentFgAuthored = px("accentFg");
  const accentFg =
    contrastRatio(accent, accentFgAuthored) >= FLOOR_TEXT
      ? accentFgAuthored
      : bestContrast(accent, [
          accentFgAuthored,
          base,
          ink,
          { r: 255, g: 255, b: 255 },
          { r: 0, g: 0, b: 0 },
        ]);
  put("accent-fg", accentFg);

  // --- the flag-for-agent chip -------------------------------------------
  put("intent-flag-bg", red);
  put("intent-flag-fg", bestContrast(red, [base, ink, { r: 255, g: 255, b: 255 }, { r: 0, g: 0, b: 0 }]));

  // --- syntax -----------------------------------------------------------
  const hueFor: Record<string, Rgb> = {
    violet: px("violet"),
    green: px("green"),
    blue: px("blue"),
    yellow: px("yellow"),
    orange: px("orange"),
    cyan: px("cyan"),
    teal: px("teal"),
    magenta: px("magenta"),
    pink: px("pink"),
    "ink-mute": inkMute,
    ink,
  };
  for (const cls of SYNTAX_ROLES) {
    const src = SYNTAX_SOURCE[cls];
    const raw = hueFor[src];
    // `ink` / `ink-mute` sources are already repaired — re-running `fix`
    // on them is a no-op by construction, and would otherwise double-count
    // the repair in the receipt.
    const val =
      src === "ink" || src === "ink-mute" ? raw : fix(`syn-${cls}`, raw, FLOOR_TEXT, codeBgs);
    put(`syn-${cls}`, val);
  }

  // --- diff -------------------------------------------------------------
  put("diff-add", diffAdd);
  put("diff-del", diffDel);
  put("diff-mod", diffMod);
  put("diff-add-bg", diffAddBg);
  put("diff-del-bg", diffDelBg);
  put("diff-mod-bg", diffModBg);
  put("diff-add-bg-strong", mixOklch(base, diffAdd, 0.22));
  put("diff-del-bg-strong", mixOklch(base, diffDel, 0.22));
  put("diff-mod-bg-strong", mixOklch(base, diffMod, 0.22));
  put("diff-add-gutter", mixOklch(base, diffAdd, 0.34));
  put("diff-del-gutter", mixOklch(base, diffDel, 0.34));
  put("diff-mod-gutter", mixOklch(base, diffMod, 0.34));
  put("diff-add-fg", fix("diff-add-fg", diffAdd, FLOOR_TEXT, surfaceBgs));
  put("diff-del-fg", fix("diff-del-fg", diffDel, FLOOR_TEXT, surfaceBgs));
  put("diff-mod-fg", fix("diff-mod-fg", diffMod, FLOOR_TEXT, surfaceBgs));

  // --- lane budget (the age bands are emitted above) ----------------------
  const wheel = PROV_WHEEL.map((k) => px(k));
  // A 2px rail is a graphical object, not text — the non-text floor. Contrast
  // first, THEN the pairwise spread: the spread only ever moves lightness
  // further from the background, so it can never undo the contrast repair.
  const provFixed = farthestPointSample(wheel, PROV_HUES).map((c, i) =>
    fix(`prov-hue-${i}`, c, FLOOR_NONTEXT, surfaceBgs),
  );
  spreadProvenance(provFixed, PROV_PAIRWISE_FLOOR, appearance).forEach((c, i) => {
    put(`prov-hue-${i}`, c);
  });
  put("trust-hue", accent);

  // --- non-colour roles -------------------------------------------------
  roles["backdrop"] = rgba(px("scrim"), dark ? 0.6 : 0.4);
  roles["age-tint"] = dark ? "8%" : "16%";
  const shadows = dark ? DARK_SHADOWS : LIGHT_SHADOWS;
  for (const [k, v] of Object.entries(shadows)) roles[k] = v;

  // --- authored overrides (last word) -----------------------------------
  const overridden: string[] = [];
  for (const [role, value] of Object.entries(def.overrides ?? {})) {
    const key = role.startsWith("--") ? role.slice(2) : role;
    roles[key] = value;
    overridden.push(key);
    if (HEX_RE.test(value.trim())) colors[key] = parseHex(value);
    else delete colors[key];
  }

  return { id: def.id, appearance, roles, colors, repairs, overridden };
}

/* ---------------------------------------------------------------------- */
/* Generators (checked-in artefacts; pinned by a drift test)                */
/* ---------------------------------------------------------------------- */

/// Stable emission order — NOT object insertion order, so a future
/// reordering inside `deriveTheme` can never churn the generated files.
export function roleOrder(roles: Record<string, string>): string[] {
  return Object.keys(roles).sort();
}

const BANNER_LINES: readonly string[] = [
  "GENERATED by web-code/scripts/gen-themes.mjs from",
  "crates/kb-code-server/themes/registry.json — DO NOT EDIT BY HAND.",
  "Regenerate with `npm run gen:themes` in web-code/.",
  "The checked-in copy is pinned by src/themes/registry.gen.test.ts, which",
  "regenerates it in memory and asserts byte equality, so it cannot go stale.",
];

export function generateThemesCss(registry: Registry): string {
  const out: string[] = [];
  out.push("/* " + BANNER_LINES.join("\n * ") + " */");
  out.push("");
  out.push(
    [
      "/* kbc-theme/1 — one `[data-kbc-theme]` block per theme, so switching is a",
      " * single attribute flip on <html>: zero refetch, zero editor rebuild, and",
      " * every open pane / diff / canvas recolours in the same frame.",
      " *",
      " * The selector is `html:root[data-kbc-theme=…]` (0,2,1) deliberately: it",
      " * must out-specify tokens.css's `:root[data-theme=…]` (0,2,0) blocks",
      " * regardless of stylesheet order, because 14 of this SPA's stylesheets are",
      " * imported from lazily-loaded routes and therefore evaluate AFTER main.tsx's",
      " * eager imports (recon R10).",
      " *",
      " * The two `kbc-*` themes reproduce tokens.css's own palette; the attribute",
      " * is never set for them (prefs.applyTheme omits it), so with no theme",
      " * chosen the DOM and every painted colour stay byte-identical to pre-V70-A7.",
      " *",
      " * The `repaired` comments below name every vendor value the AA pass had to",
      " * move, with the before/after ratio — a moved value is never presented as",
      " * an authored one. */",
    ].join("\n"),
  );
  out.push("");
  for (const def of registry.themes) {
    const d = deriveTheme(def);
    out.push(`/* ${def.name} — ${def.appearance} · ${def.license} · ${def.source_url} */`);
    if (def.notes) out.push(`/* ${def.notes.split("*/").join("*\\/")} */`);
    for (const r of d.repairs) {
      out.push(
        `/* repaired --${r.role}: ${r.from} → ${r.to} (${r.before}:1 → ${r.after}:1 vs ${r.against}, floor ${r.floor}) */`,
      );
    }
    if (d.overridden.length) {
      out.push(`/* authored overrides: ${d.overridden.map((r) => "--" + r).join(", ")} */`);
    }
    out.push(`html:root[data-kbc-theme="${def.id}"] {`);
    for (const role of roleOrder(d.roles)) out.push(`  --${role}: ${d.roles[role]};`);
    out.push("}");
    out.push("");
  }
  return out.join("\n");
}

export function generateRegistryTs(registry: Registry): string {
  const lines: string[] = [];
  lines.push("// " + BANNER_LINES.join("\n// "));
  lines.push("");
  lines.push('export type KbcAppearance = "light" | "dark";');
  lines.push("");
  lines.push("export interface KbcThemeEntry {");
  lines.push("  readonly id: string;");
  lines.push("  readonly family: string;");
  lines.push("  readonly familyName: string;");
  lines.push("  readonly name: string;");
  lines.push("  readonly appearance: KbcAppearance;");
  lines.push("  readonly license: string;");
  lines.push("  readonly sourceUrl: string;");
  lines.push("  /// The theme's resolved `--bg`, so `applyTheme` can drive the");
  lines.push("  /// `theme-color` meta without a `getComputedStyle` round trip.");
  lines.push("  readonly bg: string;");
  lines.push("  readonly accent: string;");
  lines.push("  readonly ink: string;");
  lines.push("  /// How many roles the AA repair pass had to move off the vendor");
  lines.push("  /// value — surfaced in the picker as an honest receipt.");
  lines.push("  readonly repaired: number;");
  lines.push("}");
  lines.push("");
  lines.push(`export const KBC_THEME_SCHEMA = ${JSON.stringify(registry.schema)};`);
  lines.push("");
  lines.push("export const KBC_THEMES: readonly KbcThemeEntry[] = [");
  for (const t of registry.themes) {
    const d = deriveTheme(t);
    lines.push("  {");
    lines.push(`    id: ${JSON.stringify(t.id)},`);
    lines.push(`    family: ${JSON.stringify(t.family)},`);
    lines.push(`    familyName: ${JSON.stringify(t.family_name)},`);
    lines.push(`    name: ${JSON.stringify(t.name)},`);
    lines.push(`    appearance: ${JSON.stringify(t.appearance)},`);
    lines.push(`    license: ${JSON.stringify(t.license)},`);
    lines.push(`    sourceUrl: ${JSON.stringify(t.source_url)},`);
    lines.push(`    bg: ${JSON.stringify(d.roles["bg"])},`);
    lines.push(`    accent: ${JSON.stringify(d.roles["accent"])},`);
    lines.push(`    ink: ${JSON.stringify(d.roles["ink"])},`);
    lines.push(`    repaired: ${d.repairs.length},`);
    lines.push("  },");
  }
  lines.push("];");
  lines.push("");
  lines.push("/// Families in catalogue order, each with its light/dark members.");
  lines.push("export interface KbcThemeFamily {");
  lines.push("  readonly family: string;");
  lines.push("  readonly familyName: string;");
  lines.push("  readonly members: readonly KbcThemeEntry[];");
  lines.push("}");
  lines.push("");
  lines.push("export const KBC_THEME_FAMILIES: readonly KbcThemeFamily[] = (() => {");
  lines.push("  const out: KbcThemeFamily[] = [];");
  lines.push("  for (const t of KBC_THEMES) {");
  lines.push("    let fam = out.find((f) => f.family === t.family);");
  lines.push("    if (!fam) {");
  lines.push("      fam = { family: t.family, familyName: t.familyName, members: [] };");
  lines.push("      out.push(fam);");
  lines.push("    }");
  lines.push("    (fam.members as KbcThemeEntry[]).push(t);");
  lines.push("  }");
  lines.push("  return out;");
  lines.push("})();");
  lines.push("");
  return lines.join("\n");
}

/* ---------------------------------------------------------------------- */
/* The Lane Budget's co-occurrence contract (the lint's pair enumeration)   */
/* ---------------------------------------------------------------------- */
//
// `theme-lint.mjs` must check the cross-product of pairs that can ACTUALLY
// co-occur, never all pairs — an all-pairs gate would fail every theme in
// the field on combinations the Lane Budget structurally forbids. These
// lists ARE that contract, and they are the same lists `deriveTheme`'s
// repair pass measures against, so lint and repair can never disagree.

/// Backgrounds any chrome text can sit on.
export const SURFACE_BG_ROLES: readonly string[] = ["bg", "bg-panel", "bg-panel-2", "bg-card-hi"];

/// Backgrounds a CODE line can carry: the surfaces plus the three diff
/// tints plus the strongest blame-age band.
export const CODE_BG_ROLES: readonly string[] = [
  ...SURFACE_BG_ROLES,
  "diff-add-bg",
  "diff-del-bg",
  "diff-mod-bg",
];

/// Backgrounds the PRIMARY foreground must survive: the code backgrounds
/// plus every blame-age band (a legend, date or line number rendered over
/// gutter A's ramp is `--ink`).
export const INK_BG_ROLES: readonly string[] = [
  ...CODE_BG_ROLES,
  ...Array.from({ length: AGE_BANDS }, (_, i) => `age-band-${i}`),
];

/// Body text painted inside the code region (CM6 buffer, unified + split
/// diff, minimap). Floor: AA 4.5.
export const CODE_TEXT_ROLES: readonly string[] = [
  "ink",
  "ink-mute",
  ...SYNTAX_ROLES.map((r) => `syn-${r}`),
];

/// Chrome text: labels, chips, links, counts. Floor: AA 4.5.
export const CHROME_TEXT_ROLES: readonly string[] = [
  "accent",
  "accent-soft",
  "warn",
  "green",
  "red",
  "blue",
  "diff-add-fg",
  "diff-del-fg",
  "diff-mod-fg",
];

/// Graphical objects and decorative ink on the chrome surfaces — gutter
/// line numbers, rules, disabled glyphs, the 2px provenance rail.
/// Floor: AA non-text 3.0.
export const DECORATIVE_ROLES: readonly string[] = [
  "ink-dim",
  ...Array.from({ length: PROV_HUES }, (_, i) => `prov-hue-${i}`),
];

/// Roles the lint MEASURES and REPORTS but never fails on, because their own
/// documented contract puts them outside the WCAG text/non-text scope.
/// `--ink-faint` is tokens.css's SH.C1 carve-out verbatim: "decorative
/// rules/disabled glyphs only, never running text".
export const EXEMPT_ROLES: readonly string[] = ["ink-faint"];

/// Solid fills with their own foreground — checked as a pair, not against
/// the page background.
export const FILL_PAIRS: readonly [string, string][] = [
  ["accent", "accent-fg"],
  ["intent-flag-bg", "intent-flag-fg"],
];

/// State-vs-state separation: two roles that encode OPPOSED meanings and
/// must stay perceptually apart. Floors are Oklab distances, not contrast
/// ratios — the GitHub lesson (two semantically opposed states shipped at
/// 1.18:1 against each other, twice, with a dedicated a11y team).
export const STATE_PAIRS: readonly {
  a: string;
  b: string;
  floor: number;
  why: string;
  /// `warn` where the Lane Budget guarantees a REDUNDANT NON-COLOUR CUE for
  /// the same distinction, so colour alone is not load-bearing.
  severity: "fail" | "warn";
}[] = [
  { a: "diff-add", b: "diff-del", floor: 0.12, why: "addition vs deletion", severity: "fail" },
  {
    a: "diff-add-bg",
    b: "diff-del-bg",
    floor: 0.005,
    why: "add vs del line tint — the permanent +/- sign column is the load-bearing cue, the tint is reinforcement",
    severity: "warn",
  },
  { a: "diff-add", b: "diff-mod", floor: 0.08, why: "addition vs modification", severity: "fail" },
  { a: "green", b: "red", floor: 0.12, why: "ok vs danger", severity: "fail" },
  { a: "warn", b: "red", floor: 0.05, why: "concern vs blocker", severity: "fail" },
];

/// Adjacent blame-age bands. A ramp is deliberately gradual, so the floor is
/// small — it exists to catch a ramp that has COLLAPSED (a palette whose
/// `blue` is within rounding distance of its `base`), not to force ten
/// obviously-different colours.
export const AGE_ADJACENT_FLOOR = 0.004;

/// The provenance rail's eight session hues, pairwise. These identify WHO
/// wrote a line, so a collision is a misattribution, not a cosmetic nit.
export const PROV_PAIRWISE_FLOOR = 0.05;

/// Syntax mutual distinctiveness. Per Flexoki's finding — perfect perceptual
/// uniformity fights the distinctiveness syntax colouring exists for — this
/// is a WARNING lane, never a failure: when uniformity and distinctiveness
/// conflict, distinctiveness wins.
export const SYNTAX_DISTINCT_FLOOR = 0.05;
