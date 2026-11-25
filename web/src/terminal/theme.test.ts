import { describe, expect, it } from "vitest";
import { contrastRatio, parseHex } from "@kb/ui/themes/derive";
import { readTheme, type TokenSource } from "./theme";

const tokens = (map: Record<string, string>): TokenSource => ({ getPropertyValue: (n: string) => map[n] ?? "" });

const DARK = {
  "--bg-panel-2": " #0c0c0e ",
  "--ink": "#eeeeee",
  "--accent": "#6657f5",
  "--accent-bor": "#3a3870",
  "--accent-bg": "#1a1830",
};
const LIGHT = {
  "--bg-panel-2": "#ecebe5",
  "--ink": "#1a1a1c",
  "--accent": "#6150ee",
  "--accent-bor": "rgba(102, 87, 245, 0.28)",
  "--accent-bg": "rgba(102, 87, 245, 0.1)",
};

const BASE_KEYS = ["background", "foreground", "cursor", "cursorAccent", "selectionBackground", "selectionInactiveBackground"];
const ANSI_KEYS = [
  "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white",
  "brightBlack", "brightRed", "brightGreen", "brightYellow", "brightBlue", "brightMagenta", "brightCyan", "brightWhite",
];

describe("readTheme", () => {
  it("builds the whole theme from tokens, including selection, inactive selection and cursorAccent", () => {
    const t = readTheme(null, tokens(DARK));
    expect(t).toEqual(
      expect.objectContaining({
        background: "#0c0c0e",
        foreground: "#eeeeee",
        cursor: "#6657f5",
        cursorAccent: "#0c0c0e",
        selectionBackground: "#3a3870",
        selectionInactiveBackground: "#1a1830",
      }),
    );
    expect(Object.keys(t).sort()).toEqual([...BASE_KEYS, ...ANSI_KEYS].sort());
    for (const k of ANSI_KEYS) expect(t[k as keyof typeof t]).toMatch(/^#[0-9a-f]{6}$/);
  });

  it("the ANSI colours are judged against the EFFECTIVE background (a Profile background wins)", () => {
    const t = readTheme({ background: "#ffffff" }, tokens({ ...DARK, "--red": "#e06060", "--green": "#60c060", "--blue": "#6080e0" }));
    const white = parseHex("#ffffff");
    for (const k of ["red", "green", "blue", "brightRed", "yellow", "white", "brightBlack"] as const) {
      expect(contrastRatio(parseHex(t[k] as string), white), k).toBeGreaterThanOrEqual(4.5);
    }
  });

  it("a light theme and a dark theme get different ANSI sets", () => {
    const d = readTheme(null, tokens(DARK));
    const l = readTheme(null, tokens(LIGHT));
    expect(l.red).not.toBe(d.red);
    expect(l.brightWhite).not.toBe(d.brightWhite);
  });

  it("a Profile palette's unknown ANSI keys stay dropped", () => {
    const t = readTheme({ red: "#ff0000", brightBlack: "#123456" }, tokens(DARK));
    expect(t.red).not.toBe("#ff0000");
    expect(t.brightBlack).not.toBe("#123456");
  });

  it("follows the light tokens (a visible selection on light)", () => {
    const t = readTheme(null, tokens(LIGHT));
    expect(t.background).toBe("#ecebe5");
    expect(t.selectionBackground).toBe("rgba(102, 87, 245, 0.28)");
    expect(t.selectionInactiveBackground).toBe("rgba(102, 87, 245, 0.1)");
  });

  it("the Profile palette overrides background, foreground and cursor only; selection stays token-driven", () => {
    const t = readTheme({ background: "#000000", foreground: "#ffffff", cursor: "#ff0000", selectionBackground: "#123456", junk: 1 }, tokens(DARK));
    expect(t.background).toBe("#000000");
    expect(t.foreground).toBe("#ffffff");
    expect(t.cursor).toBe("#ff0000");
    expect(t.cursorAccent).toBe("#000000");
    expect(t.selectionBackground).toBe("#3a3870");
    expect(Object.keys(t)).not.toContain("junk");
  });

  it("omits a slot whose token is not defined", () => {
    expect(readTheme(null, tokens({}))).toEqual({});
  });
});
