import { describe, expect, it } from "vitest";
import { presetState } from "../desk/presets";
import { buildWorkspaceSnapshot, parseWorkspaceSnapshot } from "./workspaceSnapshot";

describe("workspaceSnapshot — the desk-sidecar round trip", () => {
  it("serialize (buildWorkspaceSnapshot → JSON.stringify) → desk_json → parseWorkspaceSnapshot is identity", () => {
    const desk = presetState("review");
    const snapshot = buildWorkspaceSnapshot({
      desk,
      drawerTabs: [
        { title: "usages: Order#total", pinned: true },
        { title: "search: refund", pinned: false },
      ],
      lines: { "src/order.rs": 42, "src/refund.rs": 7 },
      focusedPane: 2,
      pane1: { path: "src/order.rs", line: 42 },
      pane2: { path: "src/refund.rs", line: 7 },
    });
    const deskJson = JSON.stringify(snapshot);
    const back = parseWorkspaceSnapshot(deskJson);
    expect(back).toEqual(snapshot);
  });

  it("round-trips every preset, both focused panes, and an empty envelope", () => {
    for (const preset of ["read", "review", "explore", "present"] as const) {
      for (const focusedPane of [1, 2] as const) {
        const snapshot = buildWorkspaceSnapshot({
          desk: presetState(preset),
          drawerTabs: [],
          lines: {},
          focusedPane,
          pane1: null,
          pane2: null,
        });
        const back = parseWorkspaceSnapshot(JSON.stringify(snapshot));
        expect(back).toEqual(snapshot);
      }
    }
  });

  it("round-trips a pane1-only snapshot (no split open) and a lineless pane", () => {
    const snapshot = buildWorkspaceSnapshot({
      desk: presetState("read"),
      drawerTabs: [],
      lines: {},
      focusedPane: 1,
      pane1: { path: "src/lib.rs" },
      pane2: null,
    });
    const back = parseWorkspaceSnapshot(JSON.stringify(snapshot));
    expect(back).toEqual(snapshot);
    expect(back?.pane1).toEqual({ path: "src/lib.rs" });
    expect(back?.pane2).toBeNull();
  });

  it("parseWorkspaceSnapshot never throws on garbage input — returns null instead", () => {
    expect(parseWorkspaceSnapshot("not json")).toBeNull();
    expect(parseWorkspaceSnapshot("null")).toBeNull();
    expect(parseWorkspaceSnapshot("42")).toBeNull();
    expect(parseWorkspaceSnapshot("{}")).toBeNull();
    expect(parseWorkspaceSnapshot(JSON.stringify({ v: 2, desk: presetState("read") }))).toBeNull();
    expect(parseWorkspaceSnapshot(JSON.stringify({ v: 1, desk: { garbage: true } }))).toBeNull();
  });

  it("drops non-positive/non-finite `lines` entries and unrecognized drawerTabs shapes rather than propagating them", () => {
    const raw = JSON.stringify({
      v: 1,
      desk: presetState("read"),
      drawerTabs: [{ title: "ok", pinned: false }, { title: "missing pinned" }, "not an object"],
      lines: { "a.rs": 10, "b.rs": -1, "c.rs": 0, "d.rs": Number.NaN, "e.rs": "not a number" },
      focusedPane: 1,
      pane1: null,
      pane2: null,
    });
    const back = parseWorkspaceSnapshot(raw);
    expect(back).not.toBeNull();
    expect(back?.lines).toEqual({ "a.rs": 10 });
    expect(back?.drawerTabs).toEqual([{ title: "ok", pinned: false }]);
  });

  it("defaults an out-of-range focusedPane to 1 and a malformed pane to null", () => {
    const raw = JSON.stringify({
      v: 1,
      desk: presetState("read"),
      drawerTabs: [],
      lines: {},
      focusedPane: 99,
      pane1: { path: "" },
      pane2: "not an object",
    });
    const back = parseWorkspaceSnapshot(raw);
    expect(back?.focusedPane).toBe(1);
    expect(back?.pane1).toBeNull();
    expect(back?.pane2).toBeNull();
  });
});
