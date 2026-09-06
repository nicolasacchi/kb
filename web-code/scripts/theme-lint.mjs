#!/usr/bin/env node
// V70-A7 — `theme lint`, the contrast oracle. Deterministic, LLM-free, no
// network, no deps: the Modus posture (a published, MACHINE-CHECKED contrast
// contract) applied to every theme this repo bundles.
//
//   npm run lint:themes              # AA floor (4.5 text / 3.0 non-text)
//   npm run lint:themes -- --strict  # AAA floor (7:1 text / 4.5 non-text)
//   npm run lint:themes -- --json    # machine-readable
//   npm run lint:themes -- --theme catppuccin-mocha
//
// WHAT IT CHECKS, and why each lane has the severity it has:
//
//  1. TEXT LEGIBILITY (fail). Every foreground role against every background
//     it can ACTUALLY co-occur with — not all pairs. The co-occurrence
//     contract is the Lane Budget, exported from `src/themes/derive.ts` as
//     CODE_*/CHROME_*/DECORATIVE_* role lists, so the lint and the repair
//     pass can never disagree about which pairs exist. Diff-tinted and
//     age-band backgrounds are in the sets: a comment inside an added line
//     is a real, reachable pair, and it is exactly the pair a naive
//     "fg on bg" check misses.
//  2. APCA Lc (warn). Reported for every pair checked in (1). WCAG 2.x is
//     known to mis-model light-text-on-dark; APCA is the better predictor,
//     but it is not a normative floor, so a low Lc is surfaced, never fatal.
//  3. STATE SEPARATION (fail). Oklab distance between roles that encode
//     OPPOSED meanings — add vs del, ok vs danger, adjacent age bands,
//     the eight provenance hues pairwise. This is the check nobody runs and
//     the one GitHub shipped broken twice (two semantically opposed states
//     at 1.18:1 against each other, with a dedicated accessibility team).
//  4. CVD SIMULATION (warn). (1) and (3) re-run through protanopia,
//     deuteranopia and tritanopia. WARN, not fail, and deliberately so: the
//     Lane Budget already guarantees every CVD-fragile lane carries a
//     REDUNDANT NON-COLOUR CUE (diff has a permanent sign column, trust is
//     line style, diagnostics have per-severity glyphs), so a hue collapse
//     under deuteranopia degrades a theme's polish, not its information.
//     Failing here would reject every red/green palette in existence.
//  5. SYNTAX DISTINCTIVENESS (warn). Per Flexoki's finding: perfect
//     perceptual uniformity fights the distinctiveness syntax colouring
//     exists for. When they conflict, distinctiveness wins — so this lane
//     reports and never fails.
//
// Exit code is 0 unless a `fail`-severity check failed.
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import {
  deriveTheme,
  toHex,
  contrastRatio,
  oklabDistance,
  SYNTAX_ROLES,
  AGE_BANDS,
  PROV_HUES,
  SURFACE_BG_ROLES,
  CODE_BG_ROLES,
  INK_BG_ROLES,
  CODE_TEXT_ROLES,
  CHROME_TEXT_ROLES,
  DECORATIVE_ROLES,
  EXEMPT_ROLES,
  FILL_PAIRS,
  STATE_PAIRS,
  AGE_ADJACENT_FLOOR,
  PROV_PAIRWISE_FLOOR,
  SYNTAX_DISTINCT_FLOOR,
} from "../src/themes/derive.ts";

const REGISTRY = fileURLToPath(
  new URL("../../crates/kb-code-server/themes/registry.json", import.meta.url),
);

const argv = process.argv.slice(2);
const STRICT = argv.includes("--strict");
const AS_JSON = argv.includes("--json");
const ONLY = (() => {
  const i = argv.indexOf("--theme");
  return i >= 0 ? argv[i + 1] : null;
})();

const FLOOR_TEXT = STRICT ? 7 : 4.5;
const FLOOR_NONTEXT = STRICT ? 4.5 : 3;
/// APCA: Lc 60 is the "body text" bar and Lc 45 the "large / fluent" bar,
/// but APCA is not a normative floor and it systematically scores
/// light-text-on-dark far below what WCAG 2.x reports for the same pair
/// (`--ink-mute` on this app's dark `--bg` measures 4.9:1 but Lc 37) — which
/// is the whole reason to report it. Warning at the WCAG bar would fire on
/// most of every dark theme and drown the table, so the warn bar is Lc 30
/// ("illegible under any model") and the per-theme MINIMUM Lc is reported
/// in the summary instead.
const APCA_WARN = 30;

/* --- APCA (APCA-W3 0.1.9 constants) ------------------------------------ */
const APCA = {
  trc: 2.4,
  Rco: 0.2126729,
  Gco: 0.7151522,
  Bco: 0.072175,
  normBG: 0.56,
  normTXT: 0.57,
  revTXT: 0.62,
  revBG: 0.65,
  blkThrs: 0.022,
  blkClmp: 1.414,
  scale: 1.14,
  loOffset: 0.027,
  loClip: 0.1,
  deltaYmin: 0.0005,
};

function apcaY({ r, g, b }) {
  const s = (c) => Math.pow(c / 255, APCA.trc);
  return APCA.Rco * s(r) + APCA.Gco * s(g) + APCA.Bco * s(b);
}

function apcaSoftClamp(y) {
  return y > APCA.blkThrs ? y : y + Math.pow(APCA.blkThrs - y, APCA.blkClmp);
}

/// Lightness contrast, signed in the spec; we report the absolute value
/// because the sign only encodes polarity (dark-on-light vs light-on-dark).
function apcaLc(txt, bg) {
  const Yt = apcaSoftClamp(apcaY(txt));
  const Yb = apcaSoftClamp(apcaY(bg));
  if (Math.abs(Yb - Yt) < APCA.deltaYmin) return 0;
  let out;
  if (Yb > Yt) {
    const sapc = (Math.pow(Yb, APCA.normBG) - Math.pow(Yt, APCA.normTXT)) * APCA.scale;
    out = sapc < APCA.loClip ? 0 : sapc - APCA.loOffset;
  } else {
    const sapc = (Math.pow(Yb, APCA.revBG) - Math.pow(Yt, APCA.revTXT)) * APCA.scale;
    out = sapc > -APCA.loClip ? 0 : sapc + APCA.loOffset;
  }
  return Math.abs(out * 100);
}

/* --- CVD simulation (Viénot/Brettel dichromat matrices, linear sRGB) ---- */
const CVD = {
  protanopia: [
    [0.152286, 1.052583, -0.204868],
    [0.114503, 0.786281, 0.099216],
    [-0.003882, -0.048116, 1.051998],
  ],
  deuteranopia: [
    [0.367322, 0.860646, -0.227968],
    [0.280085, 0.672501, 0.047413],
    [-0.01182, 0.04294, 0.968881],
  ],
  tritanopia: [
    [1.255528, -0.076749, -0.178779],
    [-0.078411, 0.930809, 0.147602],
    [0.004733, 0.691367, 0.3039],
  ],
};

const toLin = (v) => {
  const s = v / 255;
  return s <= 0.04045 ? s / 12.92 : Math.pow((s + 0.055) / 1.055, 2.4);
};
const fromLin = (v) => {
  const c = Math.max(0, Math.min(1, v));
  const s = c <= 0.0031308 ? c * 12.92 : 1.055 * Math.pow(c, 1 / 2.4) - 0.055;
  return Math.round(s * 255);
};

function simulate(rgb, kind) {
  const m = CVD[kind];
  const l = [toLin(rgb.r), toLin(rgb.g), toLin(rgb.b)];
  return {
    r: fromLin(m[0][0] * l[0] + m[0][1] * l[1] + m[0][2] * l[2]),
    g: fromLin(m[1][0] * l[0] + m[1][1] * l[1] + m[1][2] * l[2]),
    b: fromLin(m[2][0] * l[0] + m[2][1] * l[1] + m[2][2] * l[2]),
  };
}

/* --- the gate ----------------------------------------------------------- */

function lintTheme(def) {
  const d = deriveTheme(def);
  const findings = [];
  let minLc = Infinity;
  const at = (role) => {
    const v = d.colors[role];
    if (!v) throw new Error(`${def.id}: role --${role} is not an opaque colour`);
    return v;
  };
  const push = (severity, lane, pair, measured, floor, unit) =>
    findings.push({
      severity,
      lane,
      pair,
      measured: Math.round(measured * 1000) / 1000,
      floor,
      unit,
    });

  // 1 + 2 — text legibility and APCA, over the co-occurrence contract only.
  const textChecks = [
    ...CODE_TEXT_ROLES.map((fg) => [fg, fg === "ink" ? INK_BG_ROLES : CODE_BG_ROLES, FLOOR_TEXT]),
    ...CHROME_TEXT_ROLES.map((fg) => [fg, SURFACE_BG_ROLES, FLOOR_TEXT]),
    ...DECORATIVE_ROLES.map((fg) => [fg, SURFACE_BG_ROLES, FLOOR_NONTEXT]),
    ...EXEMPT_ROLES.map((fg) => [fg, SURFACE_BG_ROLES, null]),
  ];
  for (const [fg, bgRoles, floor] of textChecks) {
    for (const bgRole of bgRoles) {
      const ratio = contrastRatio(at(fg), at(bgRole));
      const pair = `--${fg} on --${bgRole}`;
      if (floor === null) {
        push("exempt", "text", pair, ratio, 0, ":1");
      } else if (ratio < floor) {
        push("fail", "text", pair, ratio, floor, ":1");
      }
      const lc = apcaLc(at(fg), at(bgRole));
      if (floor !== null) {
        minLc = Math.min(minLc, lc);
        if (lc < APCA_WARN) push("warn", "apca", pair, lc, APCA_WARN, " Lc");
      }
      for (const kind of Object.keys(CVD)) {
        if (floor === null) continue;
        const r = contrastRatio(simulate(at(fg), kind), simulate(at(bgRole), kind));
        if (r < floor) push("warn", `cvd:${kind}`, pair, r, floor, ":1");
      }
    }
  }

  // Solid fills carry their own foreground.
  for (const [bg, fg] of FILL_PAIRS) {
    const ratio = contrastRatio(at(fg), at(bg));
    if (ratio < FLOOR_TEXT) push("fail", "fill", `--${fg} on --${bg}`, ratio, FLOOR_TEXT, ":1");
  }

  // 3 — state separation (Oklab distance, normal vision = fail).
  const statePairs = [
    ...STATE_PAIRS.map((p) => [p.a, p.b, p.floor, p.why, p.severity]),
    ...Array.from({ length: AGE_BANDS - 1 }, (_, i) => [
      `age-band-${i}`,
      `age-band-${i + 1}`,
      AGE_ADJACENT_FLOOR,
      "adjacent age bands",
      "fail",
    ]),
  ];
  for (let i = 0; i < PROV_HUES; i++) {
    for (let j = i + 1; j < PROV_HUES; j++) {
      statePairs.push([
        `prov-hue-${i}`,
        `prov-hue-${j}`,
        PROV_PAIRWISE_FLOOR,
        "provenance hues",
        "fail",
      ]);
    }
  }
  for (const [a, b, floor, why, severity] of statePairs) {
    const dist = oklabDistance(at(a), at(b));
    const pair = `--${a} vs --${b} (${why})`;
    if (dist < floor) push(severity, "state", pair, dist, floor, " ΔOklab");
    for (const kind of Object.keys(CVD)) {
      const sd = oklabDistance(simulate(at(a), kind), simulate(at(b), kind));
      if (sd < floor) push("warn", `cvd:${kind}`, pair, sd, floor, " ΔOklab");
    }
  }

  // 5 — syntax mutual distinctiveness (warn only, Flexoki's rule).
  const syn = SYNTAX_ROLES.map((r) => `syn-${r}`);
  for (let i = 0; i < syn.length; i++) {
    for (let j = i + 1; j < syn.length; j++) {
      // Roles that deliberately SHARE a source (punctuation/other/comment
      // are all `--ink-mute`; attribute and type are both `yellow`) are not
      // a collision — the mapping says so on purpose.
      if (toHex(at(syn[i])) === toHex(at(syn[j]))) continue;
      const dist = oklabDistance(at(syn[i]), at(syn[j]));
      if (dist < SYNTAX_DISTINCT_FLOOR) {
        push("warn", "syntax", `--${syn[i]} vs --${syn[j]}`, dist, SYNTAX_DISTINCT_FLOOR, " ΔOklab");
      }
    }
  }

  const fails = findings.filter((f) => f.severity === "fail");
  const warns = findings.filter((f) => f.severity === "warn");
  return {
    id: def.id,
    family: def.family,
    name: def.name,
    appearance: def.appearance,
    license: def.license,
    verdict: fails.length ? "FAIL" : warns.length ? "PASS (warn)" : "PASS",
    fails: fails.length,
    warns: warns.length,
    minLc: Math.round(minLc * 10) / 10,
    repaired: d.repairs.map((r) => r.role),
    overridden: d.overridden,
    findings,
  };
}

const registry = JSON.parse(readFileSync(REGISTRY, "utf8"));
const themes = ONLY ? registry.themes.filter((t) => t.id === ONLY) : registry.themes;
if (!themes.length) {
  console.error(`theme-lint: no theme matched ${JSON.stringify(ONLY)}`);
  process.exit(2);
}
const results = themes.map(lintTheme);
const failed = results.filter((r) => r.fails > 0);

if (AS_JSON) {
  console.log(JSON.stringify({ strict: STRICT, results }, null, 2));
} else {
  const pad = (s, n) => String(s).padEnd(n);
  console.log(
    `theme lint — ${results.length} themes · floors ${FLOOR_TEXT}:1 text / ${FLOOR_NONTEXT}:1 non-text${STRICT ? " (--strict / AAA)" : " (AA)"}`,
  );
  console.log(
    `${pad("theme", 22)}${pad("appearance", 11)}${pad("verdict", 13)}${pad("fail", 6)}${pad("warn", 6)}${pad("minLc", 8)}repaired`,
  );
  console.log("-".repeat(84));
  for (const r of results) {
    console.log(
      `${pad(r.id, 22)}${pad(r.appearance, 11)}${pad(r.verdict, 13)}${pad(r.fails, 6)}${pad(r.warns, 6)}${pad(r.minLc, 8)}${r.repaired.length}`,
    );
  }
  if (failed.length) {
    console.log("");
    console.log("FAILING PAIRS");
    console.log("-".repeat(84));
    for (const r of failed) {
      console.log(`\n${r.id} (${r.name})`);
      for (const f of r.findings.filter((x) => x.severity === "fail")) {
        console.log(
          `  [${f.lane}] ${f.pair} — ${f.measured}${f.unit} < ${f.floor}${f.unit.trim() === ":1" ? ":1" : ""}`,
        );
      }
    }
  }
  const warnByLane = new Map();
  for (const r of results) {
    for (const f of r.findings) {
      if (f.severity !== "warn") continue;
      warnByLane.set(f.lane, (warnByLane.get(f.lane) ?? 0) + 1);
    }
  }
  if (warnByLane.size) {
    console.log("");
    console.log(
      "warnings by lane (non-fatal — see this file's header for why each lane warns):",
    );
    for (const [lane, n] of [...warnByLane].sort()) console.log(`  ${lane}: ${n}`);
  }
}

process.exit(failed.length ? 1 : 0);
