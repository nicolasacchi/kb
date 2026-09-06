// V70-A7 — the derivation engine's unit pins.
//
// The whole point of kbc-theme/1 is that a theme is DERIVED, not authored
// token by token: 26 anchors in, ~80 roles out, deterministically. That only
// holds up if the arithmetic is pinned, because a silent drift in the OKLCH
// round trip would move every colour in every theme at once and nothing
// would visibly break until someone measured contrast again.
import { describe, expect, it } from "vitest";
import {
  ANCHOR_KEYS,
  AGE_BANDS,
  CODE_TEXT_ROLES,
  CHROME_TEXT_ROLES,
  CODE_BG_ROLES,
  FLOOR_NONTEXT,
  FLOOR_TEXT,
  PROV_HUES,
  PROV_PAIRWISE_FLOOR,
  SURFACE_BG_ROLES,
  SYNTAX_ROLES,
  contrastRatio,
  deriveTheme,
  fromOklch,
  mixOklch,
  oklabDistance,
  parseHex,
  repairContrast,
  toHex,
  toOklch,
  type ThemeDef,
} from "./derive";

const WHITE = { r: 255, g: 255, b: 255 };
const BLACK = { r: 0, g: 0, b: 0 };

/// A deliberately BAD dark palette: the comment/subtle ink is far too dim
/// (the Nord failure mode) and two accents are near-identical, so every
/// repair path actually fires.
const FIXTURE: ThemeDef = {
  id: "fixture-dark",
  family: "fixture",
  family_name: "Fixture",
  name: "Fixture Dark",
  appearance: "dark",
  license: "MIT",
  source_url: "https://example.invalid/fixture",
  anchors: {
    base: "#1a1b26",
    surface: "#1f2130",
    sunken: "#16171f",
    overlay: "#292c3d",
    highlightLow: "#232533",
    highlightMed: "#2f3245",
    highlightHigh: "#3b3f56",
    muted: "#3b3f56",
    subtle: "#414868",
    text: "#c0caf5",
    red: "#f7768e",
    orange: "#ff9e64",
    yellow: "#e0af68",
    green: "#9ece6a",
    cyan: "#7dcfff",
    blue: "#7aa2f7",
    violet: "#bb9af7",
    magenta: "#bb9af7",
    pink: "#bb9af7",
    teal: "#73daca",
    accent: "#7aa2f7",
    accentFg: "#1a1b26",
    diffAdd: "#9ece6a",
    diffDel: "#f7768e",
    diffMod: "#7aa2f7",
    scrim: "#0b0c12",
  },
};

describe("colour primitives", () => {
  it("round-trips hex → rgb → hex", () => {
    for (const hex of ["#000000", "#ffffff", "#1a1b26", "#7aa2f7", "#f3f1ec"]) {
      expect(toHex(parseHex(hex))).toBe(hex);
    }
  });

  it("rejects anything that is not a 6-digit hex", () => {
    expect(() => parseHex("#fff")).toThrow();
    expect(() => parseHex("rebeccapurple")).toThrow();
    expect(() => parseHex("rgb(1,2,3)")).toThrow();
  });

  it("round-trips through OKLCH within a rounding step", () => {
    for (const hex of ["#1a1b26", "#7aa2f7", "#9ece6a", "#f7768e", "#f3f1ec"]) {
      const back = toHex(fromOklch(toOklch(parseHex(hex))));
      const a = parseHex(hex);
      const b = parseHex(back);
      expect(Math.abs(a.r - b.r)).toBeLessThanOrEqual(1);
      expect(Math.abs(a.g - b.g)).toBeLessThanOrEqual(1);
      expect(Math.abs(a.b - b.b)).toBeLessThanOrEqual(1);
    }
  });

  it("mixes at the endpoints exactly and monotonically in between", () => {
    const a = parseHex("#1a1b26");
    const b = parseHex("#9ece6a");
    expect(toHex(mixOklch(a, b, 0))).toBe(toHex(fromOklch(toOklch(a))));
    expect(toHex(mixOklch(a, b, 1))).toBe(toHex(fromOklch(toOklch(b))));
    const ls = [0, 0.25, 0.5, 0.75, 1].map((t) => toOklch(mixOklch(a, b, t)).l);
    for (let i = 1; i < ls.length; i++) expect(ls[i]).toBeGreaterThan(ls[i - 1]);
  });

  it("computes the WCAG anchors", () => {
    expect(contrastRatio(WHITE, BLACK)).toBeCloseTo(21, 5);
    expect(contrastRatio(WHITE, WHITE)).toBeCloseTo(1, 5);
  });
});

describe("the AA repair pass", () => {
  const bg = [parseHex("#1a1b26")];

  it("leaves a passing colour untouched", () => {
    const ok = parseHex("#c0caf5");
    expect(toHex(repairContrast(ok, bg, FLOOR_TEXT, "dark"))).toBe(toHex(ok));
  });

  it("lifts a failing colour over the floor", () => {
    const dim = parseHex("#414868");
    expect(contrastRatio(dim, bg[0])).toBeLessThan(FLOOR_TEXT);
    const fixed = repairContrast(dim, bg, FLOOR_TEXT, "dark");
    expect(contrastRatio(fixed, bg[0])).toBeGreaterThanOrEqual(FLOOR_TEXT);
  });

  it("preserves hue while repairing — a repaired Nord is still Nord", () => {
    const dim = parseHex("#414868");
    const before = toOklch(dim).h;
    const after = toOklch(repairContrast(dim, bg, FLOOR_TEXT, "dark")).h;
    expect(Math.abs(after - before)).toBeLessThan(2);
  });

  it("moves the foreground AWAY from the background, per appearance", () => {
    const dim = parseHex("#414868");
    expect(toOklch(repairContrast(dim, bg, FLOOR_TEXT, "dark")).l).toBeGreaterThan(
      toOklch(dim).l,
    );
    const lightBg = [parseHex("#f3f1ec")];
    const pale = parseHex("#cfcabb");
    expect(toOklch(repairContrast(pale, lightBg, FLOOR_TEXT, "light")).l).toBeLessThan(
      toOklch(pale).l,
    );
  });
});

describe("deriveTheme", () => {
  const d = deriveTheme(FIXTURE);

  it("is deterministic", () => {
    expect(deriveTheme(FIXTURE)).toEqual(d);
  });

  it("requires all 26 anchors", () => {
    expect(ANCHOR_KEYS).toHaveLength(26);
    for (const k of ANCHOR_KEYS) {
      const broken = { ...FIXTURE, anchors: { ...FIXTURE.anchors } };
      delete (broken.anchors as Record<string, string>)[k];
      expect(() => deriveTheme(broken)).toThrow(String(k));
    }
  });

  it("emits every token of the pre-kbc-theme/1 compatibility surface", () => {
    // The 58-token surface tokens.css shipped before this unit: no theme may
    // silently drop one, or a component reaches for a token the active theme
    // never defines and falls back through a dead cascade.
    const compat = [
      "bg", "bg-panel", "bg-panel-2", "bg-card", "bg-card-hi",
      "ink", "ink-mute", "ink-dim", "ink-faint",
      "rule", "rule-hi",
      "accent", "accent-soft", "accent-bg", "accent-bor", "accent-fg",
      "warn", "green", "red", "blue",
      "intent-flag-bg", "intent-flag-fg",
      "backdrop", "age-tint",
      "shadow", "shadow-card", "shadow-card-hover", "shadow-lift",
    ];
    for (const role of compat) expect(d.roles, role).toHaveProperty(role);
  });

  it("carries exactly highlight.rs's fifteen classes — no more, no fewer", () => {
    // Frozen against `crates/kb-code-server/src/highlight.rs`'s
    // `HighlightClass`. Widening needs the shared-`salt` split (v7.2), so a
    // sixteenth role appearing here is a bug, not a feature.
    expect([...SYNTAX_ROLES]).toEqual([
      "keyword", "string", "comment", "function", "type", "number", "variable",
      "constant", "operator", "punctuation", "property", "attribute", "label",
      "escape", "other",
    ]);
    for (const r of SYNTAX_ROLES) expect(d.roles).toHaveProperty(`syn-${r}`);
  });

  it("clears the text floor on every background a role can co-occur with", () => {
    for (const fg of CODE_TEXT_ROLES) {
      for (const bg of CODE_BG_ROLES) {
        expect(contrastRatio(d.colors[fg], d.colors[bg]), `${fg} on ${bg}`).toBeGreaterThanOrEqual(
          FLOOR_TEXT,
        );
      }
    }
    for (const fg of CHROME_TEXT_ROLES) {
      for (const bg of SURFACE_BG_ROLES) {
        expect(contrastRatio(d.colors[fg], d.colors[bg]), `${fg} on ${bg}`).toBeGreaterThanOrEqual(
          FLOOR_TEXT,
        );
      }
    }
  });

  it("records every repair rather than moving a vendor value silently", () => {
    // The fixture's `subtle` is deliberately too dim, so a repair MUST be
    // recorded — a silent fix would be the dishonest outcome.
    const inkMute = d.repairs.find((r) => r.role === "ink-mute");
    expect(inkMute).toBeDefined();
    expect(inkMute!.from).toBe("#414868");
    expect(inkMute!.before).toBeLessThan(FLOOR_TEXT);
    expect(inkMute!.after).toBeGreaterThanOrEqual(FLOOR_TEXT);
  });

  it("gives eight distinct, adequately separated provenance hues even when the wheel repeats a colour", () => {
    // The fixture maps violet/magenta/pink to ONE hex on purpose: several
    // real families do exactly that, and a naive sampler hands two sessions
    // the same rail colour.
    const hues = Array.from({ length: PROV_HUES }, (_, i) => d.colors[`prov-hue-${i}`]);
    expect(new Set(hues.map(toHex)).size).toBe(PROV_HUES);
    for (let i = 0; i < PROV_HUES; i++) {
      for (let j = i + 1; j < PROV_HUES; j++) {
        expect(oklabDistance(hues[i], hues[j]), `${i} vs ${j}`).toBeGreaterThanOrEqual(
          PROV_PAIRWISE_FLOOR,
        );
      }
      expect(contrastRatio(hues[i], d.colors["bg"])).toBeGreaterThanOrEqual(FLOOR_NONTEXT);
    }
  });

  it("ramps the age bands monotonically from newest to oldest", () => {
    const bands = Array.from({ length: AGE_BANDS }, (_, i) => d.colors[`age-band-${i}`]);
    expect(bands).toHaveLength(10);
    const base = d.colors["bg"];
    const ds = bands.map((b) => oklabDistance(b, base));
    for (let i = 1; i < ds.length; i++) expect(ds[i]).toBeLessThan(ds[i - 1]);
  });

  it("lets an authored override win over BOTH derivation and repair", () => {
    const withOverride = deriveTheme({
      ...FIXTURE,
      overrides: { "ink-mute": "#deadbe", "--accent": "#123456" },
    });
    expect(withOverride.roles["ink-mute"]).toBe("#deadbe");
    // A leading `--` is accepted and normalised away.
    expect(withOverride.roles["accent"]).toBe("#123456");
    expect(withOverride.overridden).toContain("ink-mute");
    expect(withOverride.overridden).toContain("accent");
  });

  it("keeps a non-colour override out of the lint's colour map", () => {
    const withOverride = deriveTheme({
      ...FIXTURE,
      overrides: { "accent-bg": "rgba(1, 2, 3, 0.1)" },
    });
    expect(withOverride.roles["accent-bg"]).toBe("rgba(1, 2, 3, 0.1)");
    expect(withOverride.colors["accent-bg"]).toBeUndefined();
  });
});
