import { describe, expect, it } from "vitest";
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

describe("readTheme", () => {
  it("builds the whole theme from tokens, including selection, inactive selection and cursorAccent", () => {
    expect(readTheme(null, tokens(DARK))).toEqual({
      background: "#0c0c0e",
      foreground: "#eeeeee",
      cursor: "#6657f5",
      cursorAccent: "#0c0c0e",
      selectionBackground: "#3a3870",
      selectionInactiveBackground: "#1a1830",
    });
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
