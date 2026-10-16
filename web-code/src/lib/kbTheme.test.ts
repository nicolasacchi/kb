// @vitest-environment jsdom
// OL S1 - the theme family travels kb -> kb-code, one way: kb's family is
// followed only until the user chooses one here, and nothing is written back.
import { beforeEach, describe, expect, it, vi } from "vitest";
import { createElement } from "react";
import { renderToString } from "react-dom/server";
import ThemeSourceCaption from "../components/ThemeSourceCaption";
import { familyFromKbUi, followKbTheme, themeSourceCaption } from "./kbTheme";
import {
  BUILTIN_FAMILY,
  hasLocalThemeFamily,
  loadThemeFamily,
  setFollowedThemeFamily,
  setThemePrefs,
  themeFamilySource,
} from "./prefs";
import type { KbUiOut } from "../api/types";

const ui = (over: Partial<KbUiOut> = {}): KbUiOut => ({
  theme: "dark",
  theme_family: "catppuccin",
  ...over,
});

function html(): HTMLElement {
  return document.documentElement;
}

beforeEach(() => {
  localStorage.clear();
  setFollowedThemeFamily(null);
  html().removeAttribute("data-kbc-theme");
  html().removeAttribute("data-theme");
});

describe("familyFromKbUi", () => {
  it("takes a well-formed slug and nothing else", () => {
    expect(familyFromKbUi(ui())).toBe("catppuccin");
    expect(familyFromKbUi(ui({ theme_family: "kb-classic" }))).toBe("kb-classic");
    for (const bad of [null, "", "Kb", "a b", "<b>", "x".repeat(41)]) {
      expect(familyFromKbUi(ui({ theme_family: bad })), String(bad)).toBeNull();
    }
    expect(familyFromKbUi(null)).toBeNull();
    expect(familyFromKbUi({ theme: null, theme_family: null, unavailable: "kb unreachable" })).toBeNull();
  });
});

describe("followKbTheme", () => {
  it("applies kb's family when the user has not chosen one here", async () => {
    expect(themeFamilySource()).toBe("default");
    await followKbTheme(async () => ui());
    expect(themeFamilySource()).toBe("kb");
    expect(loadThemeFamily()).toBe("catppuccin");
    expect(html().getAttribute("data-kbc-theme")).toBe("catppuccin-mocha");
  });

  it("never writes kb's choice into kb-code's own prefs (one way, nothing persisted)", async () => {
    await followKbTheme(async () => ui());
    const blob = JSON.parse(localStorage.getItem("kbc:prefs") ?? "{}");
    expect(blob.themeFamily ?? BUILTIN_FAMILY).toBe(BUILTIN_FAMILY);
    expect(blob.themeFamilyChosen).toBeUndefined();
    expect(hasLocalThemeFamily()).toBe(false);
  });

  it("a family chosen here wins and kb is not even asked", async () => {
    setThemePrefs({ themeFamily: "nord" });
    expect(themeFamilySource()).toBe("local");
    const fetchUi = vi.fn(async () => ui());
    await followKbTheme(fetchUi);
    expect(fetchUi).not.toHaveBeenCalled();
    expect(loadThemeFamily()).toBe("nord");
    expect(html().getAttribute("data-kbc-theme")).toBe("nord");
  });

  it("a choice made while the request is in flight still wins", async () => {
    await followKbTheme(async () => {
      setThemePrefs({ themeFamily: "nord" });
      return ui();
    });
    expect(themeFamilySource()).toBe("local");
    expect(loadThemeFamily()).toBe("nord");
  });

  it("picking a family after following flips the source to local and persists it", async () => {
    await followKbTheme(async () => ui());
    expect(themeFamilySource()).toBe("kb");
    setThemePrefs({ themeFamily: "gruvbox" });
    expect(themeFamilySource()).toBe("local");
    expect(JSON.parse(localStorage.getItem("kbc:prefs")!).themeFamilyChosen).toBe(true);
    expect(loadThemeFamily()).toBe("gruvbox");
  });

  it("changing only the appearance does not count as choosing a family", async () => {
    await followKbTheme(async () => ui());
    setThemePrefs({ theme: "light" });
    expect(themeFamilySource()).toBe("kb");
    expect(html().getAttribute("data-kbc-theme")).toBe("catppuccin-latte");
  });

  it("an older blob that already holds a non-built-in family counts as chosen here", async () => {
    localStorage.setItem("kbc:prefs", JSON.stringify({ themeFamily: "gruvbox" }));
    const fetchUi = vi.fn(async () => ui());
    await followKbTheme(fetchUi);
    expect(fetchUi).not.toHaveBeenCalled();
    expect(themeFamilySource()).toBe("local");
  });

  it("ignores an unavailable kb, a thrown fetch and a family this bundle does not carry", async () => {
    await followKbTheme(async () => ({ theme: null, theme_family: null, unavailable: "kb disabled" }));
    expect(themeFamilySource()).toBe("default");
    await followKbTheme(async () => {
      throw new Error("down");
    });
    expect(themeFamilySource()).toBe("default");
    await followKbTheme(async () => ui({ theme_family: "family-from-the-future" }));
    expect(themeFamilySource()).toBe("default");
    expect(loadThemeFamily()).toBe(BUILTIN_FAMILY);
    expect(html().hasAttribute("data-kbc-theme")).toBe(false);
  });

  it("tells the theme controls so they can repaint", async () => {
    const seen = vi.fn();
    window.addEventListener("kbc:theme.changed", seen);
    await followKbTheme(async () => ui());
    window.removeEventListener("kbc:theme.changed", seen);
    expect(seen).toHaveBeenCalledTimes(1);
  });
});

describe("the picker caption", () => {
  it("says which side the family came from, and nothing when there is no story", () => {
    expect(themeSourceCaption("kb")).toBe("Following kb");
    expect(themeSourceCaption("local")).toBe("Chosen here");
    expect(themeSourceCaption("default")).toBeNull();
  });

  it("renders the caption with its source marker, or nothing", () => {
    const kb = renderToString(createElement(ThemeSourceCaption, { source: "kb" }));
    expect(kb).toContain("Following kb");
    expect(kb).toContain('data-kbc-theme-source="kb"');
    const local = renderToString(createElement(ThemeSourceCaption, { source: "local" }));
    expect(local).toContain("Chosen here");
    expect(renderToString(createElement(ThemeSourceCaption, { source: "default" }))).toBe("");
  });
});
