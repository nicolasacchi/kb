// OL F2 — the pure half of the shared theme engine. `resolveThemeId` decides
// the ONE thing that must never be wrong: whether <html> gets a
// `data-kbc-theme` at all, and which member. It takes `light` as an argument
// instead of reading matchMedia, so it is testable without a DOM.
import { describe, expect, it } from "vitest";
import { coerceFamily, resolveThemeId } from "./apply";

describe("resolveThemeId", () => {
  it("returns null only for the host's builtin family", () => {
    const host = { builtinFamily: "kbc" };
    for (const a of ["light", "dark", "system"] as const) {
      expect(resolveThemeId("kbc", a, true, host)).toBeNull();
      expect(resolveThemeId("kbc", a, false, host)).toBeNull();
    }
    // Without a builtin, kbc is an ordinary family with an attribute.
    expect(resolveThemeId("kbc", "dark", false)).toBe("kbc-dark");
  });

  it("returns null for a family the registry does not carry", () => {
    expect(resolveThemeId("not-a-real-family", "dark", false)).toBeNull();
  });

  it("resolves kb-classic to an attribute in every appearance (kb has no builtin)", () => {
    expect(resolveThemeId("kb-classic", "dark", false)).toBe("kb-classic-dark");
    expect(resolveThemeId("kb-classic", "light", true)).toBe("kb-classic-light");
    expect(resolveThemeId("kb-classic", "system", true)).toBe("kb-classic-light");
    expect(resolveThemeId("kb-classic", "system", false)).toBe("kb-classic-dark");
  });

  it("picks the member matching an explicit appearance, and system from the injected OS flag", () => {
    expect(resolveThemeId("catppuccin", "dark", false)).toBe("catppuccin-mocha");
    expect(resolveThemeId("catppuccin", "light", false)).toBe("catppuccin-latte");
    expect(resolveThemeId("catppuccin", "system", true)).toBe("catppuccin-latte");
    expect(resolveThemeId("catppuccin", "system", false)).toBe("catppuccin-mocha");
  });
});

describe("coerceFamily", () => {
  it("keeps a registry family and degrades an unknown or missing one to the fallback", () => {
    expect(coerceFamily("nord", "kb-classic")).toBe("nord");
    expect(coerceFamily("nope", "kb-classic")).toBe("kb-classic");
    expect(coerceFamily(null, "kb-classic")).toBe("kb-classic");
    expect(coerceFamily(undefined, "kbc")).toBe("kbc");
  });

  it("keeps the host's builtin family even though it is also a registry family", () => {
    expect(coerceFamily("kbc", "kbc", { builtinFamily: "kbc" })).toBe("kbc");
  });
});
