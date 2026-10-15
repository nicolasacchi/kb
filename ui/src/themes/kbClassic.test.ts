// OL F2 — kb's look must not change on day one. `kb-classic` is generated
// from 26 anchors (+ a few authored overrides), and this test pins every
// palette role kb's hand-written tokens.css used to declare against the
// generated value, by OKLab distance (deriveTheme is the same code that emits
// themes.gen.css, which registry.gen.test.ts pins byte-for-byte).
//
// The two documented exceptions are the AA repair pass doing its job, not
// drift: --accent-fg on dark (white on #8a7fff is 3.2:1, the generator picks
// the AA-passing black) and --accent on light (#6657f5 is 4.38:1 on --bg, the
// pass nudges lightness to clear 4.5:1). Both are commented in themes.gen.css.
import { describe, expect, it } from "vitest";
import { deriveTheme, oklabDistance, parseHex, type ThemeDef } from "./derive";

// Node's fs via `process.getBuiltinModule` typed as `any`: web/'s tsconfig pulls
// ui/src in but carries no Node typings (same trick as ui/src/tokens.test.ts).
// eslint-disable-next-line @typescript-eslint/no-explicit-any
const fs = (globalThis as any).process.getBuiltinModule("node:fs") as {
  readFileSync(url: URL, enc: "utf8"): string;
};

const registry = JSON.parse(
  fs.readFileSync(new URL("../../../crates/kb-code-server/themes/registry.json", import.meta.url), "utf8"),
) as { themes: ThemeDef[] };

// kb's pre-F2 palette, copied from the retired blocks in web/src/styles/tokens.css.
const KB_DARK: Record<string, string> = {
  bg: "#0e0e10", "bg-panel": "#141417", "bg-panel-2": "#0c0c0e", "bg-card": "#151518",
  "bg-card-hi": "#1a1a1f", ink: "#e8e6df", "ink-mute": "#8a857d", "ink-dim": "#726c65",
  "ink-faint": "#5a554f", rule: "#23232a", "rule-hi": "#2c2c36", accent: "#8a7fff",
  "accent-soft": "#aaa2ff", "accent-bg": "#1a1830", "accent-bor": "#3a3870",
  warn: "#d8a04b", green: "#7eb98f", red: "#d77a6a", blue: "#7aa9ff",
};
const KB_LIGHT: Record<string, string> = {
  bg: "#f3f1ec", "bg-panel": "#ffffff", "bg-panel-2": "#ecebe5", "bg-card": "#ffffff",
  "bg-card-hi": "#faf9f6", ink: "#1c1b19", "ink-mute": "#6d685f", "ink-dim": "#8a8376",
  "ink-faint": "#a39d92", rule: "#e4e1da", "rule-hi": "#d6d2c9", accent: "#6657f5",
  "accent-soft": "#5546e0", warn: "#78541f", green: "#366547", red: "#954131", blue: "#385a9a",
};
const KB_SHADOW_DARK = "0 1px 0 rgba(255, 255, 255, 0.04) inset, 0 8px 24px rgba(0, 0, 0, 0.4)";
const KB_SHADOW_LIGHT = "0 1px 0 rgba(255, 255, 255, 0.5) inset, 0 8px 24px rgba(0, 0, 0, 0.06)";

const THRESHOLD = 0.02;
// Exceptions: role -> allowed distance, with the reason above.
const LIGHT_EXCEPTIONS: Record<string, number> = { accent: 0.021 };

function role(id: string, name: string): string {
  const def = registry.themes.find((t) => t.id === id);
  expect(def, id).toBeTruthy();
  return deriveTheme(def as ThemeDef).roles[name];
}

describe("kb-classic reproduces kb's hand-written palettes", () => {
  for (const [id, table, exceptions] of [
    ["kb-classic-dark", KB_DARK, {}],
    ["kb-classic-light", KB_LIGHT, LIGHT_EXCEPTIONS],
  ] as const) {
    it(`${id}: every role is within OKLab ${THRESHOLD} of the old value`, () => {
      for (const [name, hex] of Object.entries(table)) {
        const got = role(id, name);
        const limit = (exceptions as Record<string, number>)[name] ?? THRESHOLD;
        const d = oklabDistance(parseHex(hex), parseHex(got));
        expect(d, `${id} --${name}: ${hex} vs ${got}`).toBeLessThanOrEqual(limit);
      }
    });
  }

  it("keeps kb's translucent light accent halo and its shadows verbatim", () => {
    expect(role("kb-classic-light", "accent-bg")).toBe("rgba(102, 87, 245, 0.1)");
    expect(role("kb-classic-light", "accent-bor")).toBe("rgba(102, 87, 245, 0.28)");
    expect(role("kb-classic-dark", "shadow")).toBe(KB_SHADOW_DARK);
    expect(role("kb-classic-light", "shadow")).toBe(KB_SHADOW_LIGHT);
  });

  it("documents the AA repairs rather than hiding them", () => {
    // The nudge is stated in the generated CSS next to the theme block.
    const css = fs.readFileSync(new URL("./themes.gen.css", import.meta.url), "utf8");
    const light = css.slice(css.indexOf("/* kb classic Light"));
    expect(light.split('html:root[data-kbc-theme="kb-classic-light"]')[0]).toMatch(
      /repaired --accent: #6657f5 → /,
    );
  });
});
