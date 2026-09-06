// V70-A7 — the pure half of theme resolution.
//
// `resolveThemeId` decides the ONE thing that must never be wrong: whether
// `<html>` gets a `data-kbc-theme` attribute at all. Returning a value for
// the built-in family would change the DOM (and the painted colours) for
// every user who has never opened the picker; returning `null` for a chosen
// family would silently strand them on the default. It takes `light` as an
// argument rather than reading `matchMedia`, so it is testable without a DOM
// — the same discipline `coldSeedRepo` in this module already follows.
import { describe, expect, it } from "vitest";
import { BUILTIN_FAMILY, resolveThemeId } from "./prefs";

describe("resolveThemeId", () => {
  it("returns null for the built-in family, in every appearance", () => {
    for (const a of ["light", "dark", "system"] as const) {
      expect(resolveThemeId(BUILTIN_FAMILY, a, false)).toBeNull();
      expect(resolveThemeId(BUILTIN_FAMILY, a, true)).toBeNull();
    }
  });

  it("returns null for a family the registry does not carry", () => {
    expect(resolveThemeId("not-a-real-family", "dark", false)).toBeNull();
  });

  it("picks the member matching an explicit appearance", () => {
    expect(resolveThemeId("catppuccin", "dark", false)).toBe("catppuccin-mocha");
    expect(resolveThemeId("catppuccin", "light", false)).toBe("catppuccin-latte");
  });

  it("resolves `system` from the injected OS preference, not from a media query", () => {
    expect(resolveThemeId("catppuccin", "system", true)).toBe("catppuccin-latte");
    expect(resolveThemeId("catppuccin", "system", false)).toBe("catppuccin-mocha");
  });

  it("is stable across every bundled family", () => {
    for (const family of ["flexoki", "modus", "rose-pine", "github", "nord", "solarized"]) {
      expect(resolveThemeId(family, "light", false)).toBeTruthy();
      expect(resolveThemeId(family, "dark", false)).toBeTruthy();
      expect(resolveThemeId(family, "light", false)).not.toBe(
        resolveThemeId(family, "dark", false),
      );
    }
  });
});
