// @vitest-environment jsdom
// OL F2 — the attributes `applyTheme` leaves on <html>: exactly what the
// generated CSS selects on, for a host with no builtin family (kb) and one
// with a builtin (kb-code).
import { beforeEach, describe, expect, it } from "vitest";
import { THEME_APPLIED_EVENT, applyTheme, setThemeAttributes } from "./apply";

const root = () => document.documentElement;

beforeEach(() => {
  for (const a of ["data-theme", "data-kbc-theme"]) root().removeAttribute(a);
  root().style.colorScheme = "";
});

describe("applyTheme", () => {
  it("kb: sets data-theme, data-kbc-theme and color-scheme for kb-classic", () => {
    applyTheme("light", "kb-classic");
    expect(root().getAttribute("data-theme")).toBe("light");
    expect(root().getAttribute("data-kbc-theme")).toBe("kb-classic-light");
    expect(root().style.colorScheme).toBe("light");
    applyTheme("dark", "kb-classic");
    expect(root().getAttribute("data-kbc-theme")).toBe("kb-classic-dark");
    expect(root().style.colorScheme).toBe("dark");
  });

  it("system: color-scheme follows the OS and the attribute resolves from the OS flag", () => {
    applyTheme("system", "kb-classic");
    expect(root().getAttribute("data-theme")).toBe("system");
    expect(root().style.colorScheme).toBe("light dark");
    // jsdom has no matchMedia: prefersLight() is false, so the dark member.
    expect(root().getAttribute("data-kbc-theme")).toBe("kb-classic-dark");
  });

  it("a switch to another family is one attribute flip", () => {
    applyTheme("dark", "nord");
    expect(root().getAttribute("data-kbc-theme")).toBe("nord");
    applyTheme("dark", "kb-classic");
    expect(root().getAttribute("data-kbc-theme")).toBe("kb-classic-dark");
  });

  it("kb-code: the builtin family removes the attribute instead of setting it", () => {
    applyTheme("dark", "nord", { builtinFamily: "kbc" });
    expect(root().hasAttribute("data-kbc-theme")).toBe(true);
    applyTheme("dark", "kbc", { builtinFamily: "kbc" });
    expect(root().hasAttribute("data-kbc-theme")).toBe(false);
    expect(root().getAttribute("data-theme")).toBe("dark");
  });
});

describe("setThemeAttributes", () => {
  it("is the preview half: attributes only", () => {
    setThemeAttributes("light", "kb-classic");
    expect(root().getAttribute("data-kbc-theme")).toBe("kb-classic-light");
  });
});

describe("THEME_APPLIED_EVENT (OL READ2)", () => {
  it("fires on window after the attributes are set, once per applyTheme and once per preview", () => {
    const seen: (string | null)[] = [];
    const on = () => seen.push(root().getAttribute("data-theme"));
    window.addEventListener(THEME_APPLIED_EVENT, on);
    applyTheme("light", "kb-classic");
    setThemeAttributes("dark", "kb-classic");
    window.removeEventListener(THEME_APPLIED_EVENT, on);
    expect(seen).toEqual(["light", "dark"]);
  });
});
