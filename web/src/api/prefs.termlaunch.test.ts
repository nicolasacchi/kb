// @vitest-environment jsdom
import { beforeEach, describe, expect, it } from "vitest";
import {
  RECENT_TERM_CWDS_MAX,
  loadLastTermHarness,
  loadRecentTermCwds,
  saveLastTermHarness,
  saveRecentTermCwd,
  settingsPatchBody,
  loadPrefs,
} from "./prefs";

beforeEach(() => localStorage.clear());

describe("New-terminal prefs (browser-local)", () => {
  it("remembers the last harness", () => {
    expect(loadLastTermHarness()).toBeNull();
    saveLastTermHarness("codex");
    expect(loadLastTermHarness()).toBe("codex");
  });
  it("keeps recent cwds newest-first, unique and capped", () => {
    for (let i = 0; i < RECENT_TERM_CWDS_MAX + 3; i += 1) saveRecentTermCwd(`/d${i}`);
    saveRecentTermCwd("/d6");
    const got = loadRecentTermCwds();
    expect(got).toHaveLength(RECENT_TERM_CWDS_MAX);
    expect(got[0]).toBe("/d6");
    expect(new Set(got).size).toBe(got.length);
  });
  it("degrades a hand-edited blob to no memory", () => {
    localStorage.setItem("kb:prefs", JSON.stringify({ lastTermHarness: 5, recentTermCwds: "x" }));
    expect(loadLastTermHarness()).toBeNull();
    expect(loadRecentTermCwds()).toEqual([]);
  });
  it("never reaches the daemon patch body", () => {
    saveLastTermHarness("omp");
    saveRecentTermCwd("/w");
    expect(Object.keys(settingsPatchBody(loadPrefs())).sort()).toEqual(["density", "theme", "themeFamily"]);
  });
});
