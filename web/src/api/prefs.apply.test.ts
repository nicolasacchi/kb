// @vitest-environment jsdom
// OL F2 — applyPrefs drives the shared kbc-theme/1 engine: <html> carries
// exactly the attributes the generated CSS selects on, density stays kb's own
// CSS var, and the retired inline --accent is cleared.
import { beforeEach, describe, expect, it } from "vitest";
import { DEFAULT_PREFS, DENSITY_SCALE, applyPrefs, type Prefs } from "./prefs";

const root = () => document.documentElement;

beforeEach(() => {
  for (const a of ["data-theme", "data-kbc-theme"]) root().removeAttribute(a);
  root().removeAttribute("style");
});

const prefs = (over: Partial<Prefs>): Prefs => ({ ...DEFAULT_PREFS, ...over });

describe("applyPrefs", () => {
  it("sets the appearance, the resolved kb-classic member and color-scheme", () => {
    applyPrefs(prefs({ theme: "light" }));
    expect(root().getAttribute("data-theme")).toBe("light");
    expect(root().getAttribute("data-kbc-theme")).toBe("kb-classic-light");
    expect(root().style.colorScheme).toBe("light");
    applyPrefs(prefs({ theme: "dark" }));
    expect(root().getAttribute("data-kbc-theme")).toBe("kb-classic-dark");
  });

  it("follows the chosen family", () => {
    applyPrefs(prefs({ theme: "dark", themeFamily: "nord" }));
    expect(root().getAttribute("data-kbc-theme")).toBe("nord");
  });

  it("system keeps color-scheme open so light-dark() follows the OS", () => {
    applyPrefs(prefs({ theme: "system" }));
    expect(root().getAttribute("data-theme")).toBe("system");
    expect(root().style.colorScheme).toBe("light dark");
  });

  it("writes the density scale and clears a leftover inline --accent", () => {
    root().style.setProperty("--accent", "#123456");
    applyPrefs(prefs({ density: "compact" }));
    expect(root().style.getPropertyValue("--density")).toBe(String(DENSITY_SCALE.compact));
    expect(root().style.getPropertyValue("--accent")).toBe("");
  });
});
