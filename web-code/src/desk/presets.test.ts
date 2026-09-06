import { describe, expect, it } from "vitest";
import { DESK_PRESETS, DESK_PRESET_NAMES, DEFAULT_PRESET, presetFontScale, presetState } from "./presets";
import { REGION_SIZE_MAX, REGION_SIZE_MIN, SPLIT_MAX, SPLIT_MIN } from "./deskState";

describe("presets", () => {
  it("the table and the name list are the same closed set", () => {
    expect([...DESK_PRESET_NAMES].sort()).toEqual(Object.keys(DESK_PRESETS).sort());
    for (const n of DESK_PRESET_NAMES) expect(DESK_PRESETS[n].name).toBe(n);
  });

  it("Read is the default and the fallback", () => {
    expect(DEFAULT_PRESET).toBe("read");
  });

  it("every preset's sizes are inside the reducer's own clamps", () => {
    for (const n of DESK_PRESET_NAMES) {
      const s = presetState(n);
      for (const r of ["dock", "rail", "drawer"] as const) {
        expect(s.regions[r].size).toBeGreaterThanOrEqual(REGION_SIZE_MIN);
        expect(s.regions[r].size).toBeLessThanOrEqual(REGION_SIZE_MAX);
      }
      expect(s.panes.split).toBeGreaterThanOrEqual(SPLIT_MIN);
      expect(s.panes.split).toBeLessThanOrEqual(SPLIT_MAX);
    }
  });

  it("presetState hands out a fresh object graph every time", () => {
    const a = presetState("read");
    const b = presetState("read");
    expect(a).toEqual(b);
    a.regions.dock.size = 99;
    a.panes.pinned.push(2);
    expect(presetState("read").regions.dock.size).toBe(b.regions.dock.size);
    expect(presetState("read").panes.pinned).toEqual([]);
  });

  it("the four presets are the design's four, with the design's rail tabs", () => {
    expect(presetState("read").railTab).toBe("all");
    expect(presetState("review").railTab).toBe("review");
    expect(presetState("explore").railTab).toBe("understand");
    expect(presetState("present").railTab).toBe("all");
  });

  it("Read leaves the drawer away; Review opens it", () => {
    expect(presetState("read").regions.drawer.collapsed).toBe(true);
    expect(presetState("review").regions.drawer.collapsed).toBe(false);
    expect(presetState("explore").regions.drawer.collapsed).toBe(true);
  });

  it("Explore asks for two panes; the others ask for one", () => {
    expect(presetState("explore").panes.count).toBe(2);
    for (const n of ["read", "review", "present"] as const) expect(presetState(n).panes.count).toBe(1);
  });

  it("Present collapses every region to its stripe and is the ONLY opt-in preset", () => {
    const p = presetState("present");
    expect(p.regions.dock.collapsed).toBe(true);
    expect(p.regions.rail.collapsed).toBe(true);
    expect(p.regions.drawer.collapsed).toBe(true);
    expect(DESK_PRESETS.present.autoApplicable).toBe(false);
    for (const n of ["read", "review", "explore"] as const) expect(DESK_PRESETS[n].autoApplicable).toBe(true);
  });

  it("only Present scales the reading type", () => {
    expect(presetFontScale("present")).toBeGreaterThan(1);
    for (const n of ["read", "review", "explore"] as const) expect(presetFontScale(n)).toBe(1);
  });

  it("no preset starts dirty", () => {
    for (const n of DESK_PRESET_NAMES) expect(presetState(n).dirty).toBe(false);
  });

  it("the reference viewport leaves main the majority of the width in every non-Present preset", () => {
    // 1280 minus the two 40px stripes is the group; the viewport golden
    // (e2e/desk-viewport.spec.ts) measures the REAL column count — this
    // is only the arithmetic guard that a preset edit can't quietly
    // hand the chrome more than the code.
    for (const n of ["read", "review", "explore"] as const) {
      const s = presetState(n);
      const chrome = s.regions.dock.size + s.regions.rail.size;
      expect(chrome, `${n} spends too much on chrome`).toBeLessThanOrEqual(45);
    }
  });
});
