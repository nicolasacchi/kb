import { describe, expect, it } from "vitest";
import { contrastRatio, parseHex, toOklch } from "@kb/ui/themes/derive";
import { KBC_THEMES } from "@kb/ui/themes/registry.gen";
import { deriveAnsi } from "./ansi";
import type { TokenSource } from "./theme";

const tokens = (map: Record<string, string>): TokenSource => ({ getPropertyValue: (n: string) => map[n] ?? "" });

const KEYS = [
  "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white",
  "brightBlack", "brightRed", "brightGreen", "brightYellow", "brightBlue", "brightMagenta", "brightCyan", "brightWhite",
] as const;
const HEX = /^#[0-9a-f]{6}$/;

function hueDist(a: number, b: number): number {
  const d = Math.abs(a - b) % 360;
  return d > 180 ? 360 - d : d;
}

function check(label: string, src: TokenSource, bg: string, fg: string): void {
  const a = deriveAnsi(src, bg, fg);
  expect(a, label).not.toBeNull();
  if (!a) return;
  expect(Object.keys(a).sort(), label).toEqual([...KEYS].sort());
  const bgRgb = parseHex(bg);
  for (const k of KEYS) {
    expect(a[k], `${label} ${k}`).toMatch(HEX);
    if (k !== "black") {
      expect(contrastRatio(parseHex(a[k]), bgRgb), `${label} ${k} ${a[k]} on ${bg}`).toBeGreaterThanOrEqual(4.5);
    }
  }
  const hues = (["red", "green", "blue", "yellow"] as const).map((k) => toOklch(parseHex(a[k])).h);
  for (let i = 0; i < hues.length; i++) {
    for (let j = i + 1; j < hues.length; j++) {
      expect(hueDist(hues[i], hues[j]), `${label} hue ${i}/${j}`).toBeGreaterThanOrEqual(20);
    }
  }
}

function themeBlocks(): Map<string, Record<string, string>> {
  const fs = (globalThis as unknown as {
    process: { getBuiltinModule(n: string): { readFileSync(p: URL, e: string): string } };
  }).process.getBuiltinModule("node:fs");
  const css = fs.readFileSync(new URL("../../../ui/src/themes/themes.gen.css", import.meta.url), "utf8");
  const out = new Map<string, Record<string, string>>();
  const re = /\[data-kbc-theme="([^"]+)"\]\s*\{([^}]*)\}/g;
  for (let m = re.exec(css); m; m = re.exec(css)) {
    const vars: Record<string, string> = {};
    for (const d of m[2].matchAll(/(--[a-z0-9-]+):\s*([^;]+);/g)) vars[d[1]] = d[2].trim();
    out.set(m[1], vars);
  }
  return out;
}

describe("deriveAnsi", () => {
  it("every registered theme gets 16 legible, distinct colours", () => {
    const blocks = themeBlocks();
    expect(blocks.size).toBe(KBC_THEMES.length);
    for (const t of KBC_THEMES) {
      const v = blocks.get(t.id);
      expect(v, t.id).toBeDefined();
      if (!v) continue;
      check(t.id, tokens(v), v["--bg-panel-2"], v["--ink"]);
    }
  });

  it("covers the built-in dark and light values", () => {
    check("dark", tokens({ "--red": "#d77a6a", "--green": "#7fc08a", "--blue": "#7aa2e8" }), "#0c0c0e", "#eeeeee");
    check("light", tokens({ "--red": "#954131", "--green": "#3f7a47", "--blue": "#35609c" }), "#ecebe5", "#1a1a1c");
  });

  it("is deterministic", () => {
    const src = tokens({ "--red": "#d77a6a", "--green": "#7fc08a", "--blue": "#7aa2e8" });
    expect(deriveAnsi(src, "#0c0c0e", "#eeeeee")).toEqual(deriveAnsi(src, "#0c0c0e", "#eeeeee"));
  });

  it("falls back to fixed hues with only a background and ink", () => {
    check("fallback-dark", tokens({}), "#101014", "#e8e8e8");
    check("fallback-light", tokens({}), "#f4f4f0", "#1c1c1c");
  });

  it("a non-hex colour token falls back instead of throwing", () => {
    const a = deriveAnsi(tokens({ "--red": "rgb(200, 10, 10)", "--green": "nope", "--blue": "#7aa2e8" }), "#101014", "#e8e8e8");
    expect(a).not.toBeNull();
    expect(a?.red).toMatch(HEX);
  });

  it("returns null when the background or foreground is not a hex colour", () => {
    expect(deriveAnsi(tokens({}), "rgb(0, 0, 0)", "#ffffff")).toBeNull();
    expect(deriveAnsi(tokens({}), "#000000", "")).toBeNull();
  });

  it("bright colours move away from the background: lighter on dark, darker on light", () => {
    const dark = deriveAnsi(tokens({ "--red": "#e06060", "--green": "#60c060", "--blue": "#6080e0" }), "#101010", "#eeeeee");
    const light = deriveAnsi(tokens({ "--red": "#a03030", "--green": "#307030", "--blue": "#304fa0" }), "#ffffff", "#111111");
    expect(dark && light).toBeTruthy();
    if (!dark || !light) return;
    expect(toOklch(parseHex(dark.brightRed)).l).toBeGreaterThan(toOklch(parseHex(dark.red)).l);
    expect(toOklch(parseHex(light.brightRed)).l).toBeLessThan(toOklch(parseHex(light.red)).l);
  });

  it("repair is bounded: a mid-grey background still terminates with valid colours", () => {
    const a = deriveAnsi(tokens({ "--red": "#808080", "--green": "#808080", "--blue": "#808080" }), "#808080", "#808080");
    expect(a).not.toBeNull();
    for (const k of KEYS) expect(a?.[k]).toMatch(HEX);
  });
});
