import { describe, expect, it } from "vitest";
import {
  INSPECTOR_TABS,
  LEGACY_TAB_MAP,
  normalizeInspectorTab,
  type LegacyInspectorTab,
} from "./useInspectorTab";
import { RAIL_TABS } from "../desk/deskState";

describe("useInspectorTab — the six→four migration", () => {
  it("the tab set IS the desk's rail-tab vocabulary — one union, not two", () => {
    expect(INSPECTOR_TABS).toEqual(RAIL_TABS);
  });

  it("every legacy id maps to a tab that still exists", () => {
    const legacy: LegacyInspectorTab[] = [
      "outline",
      "provenance",
      "history",
      "annotations",
      "bookmarks",
      "entity",
    ];
    expect(Object.keys(LEGACY_TAB_MAP).sort()).toEqual([...legacy].sort());
    for (const l of legacy) expect(INSPECTOR_TABS).toContain(LEGACY_TAB_MAP[l]);
  });

  it("maps each old SOURCE tab onto the TASK tab that now holds it", () => {
    expect(normalizeInspectorTab("outline")).toBe("understand");
    expect(normalizeInspectorTab("entity")).toBe("understand");
    expect(normalizeInspectorTab("provenance")).toBe("history");
    expect(normalizeInspectorTab("annotations")).toBe("notes");
    expect(normalizeInspectorTab("bookmarks")).toBe("notes");
  });

  it("history keeps its own name AND its meaning", () => {
    expect(normalizeInspectorTab("history")).toBe("history");
  });

  it("a current id passes through untouched", () => {
    for (const t of INSPECTOR_TABS) expect(normalizeInspectorTab(t)).toBe(t);
  });

  it("garbage lands on All — never a throw, never a blank rail", () => {
    for (const junk of [null, undefined, "", "zzz", "Outline", "1"]) {
      expect(normalizeInspectorTab(junk)).toBe("all");
    }
  });
});
