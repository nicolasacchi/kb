import { describe, expect, it } from "vitest";
import {
  DESK_STATE_VERSION,
  RAIL_TABS,
  REGION_SIZE_MAX,
  REGION_SIZE_MIN,
  SPLIT_MAX,
  SPLIT_MIN,
  clampRegionSize,
  clampSplit,
  deskReducer,
  deskStorageKey,
  loadDeskState,
  migrateDeskState,
  saveDeskState,
  type DeskAction,
  type DeskState,
  type StorageLike,
} from "./deskState";
import { DESK_PRESET_NAMES, DEFAULT_PRESET, presetState } from "./presets";

function memStorage(seed: Record<string, string> = {}): StorageLike & { data: Record<string, string> } {
  const data = { ...seed };
  return {
    data,
    getItem: (k) => (k in data ? data[k] : null),
    setItem: (k, v) => {
      data[k] = v;
    },
    removeItem: (k) => {
      delete data[k];
    },
  };
}

/// Every reachable shape, enumerated: four presets × the eight
/// collapsed combinations × both pane counts × every rail tab, plus the
/// dirty flag. Small enough to be exhaustive, which is the point — the
/// round-trip golden below has nothing to sample.
function everyState(): DeskState[] {
  const out: DeskState[] = [];
  for (const preset of DESK_PRESET_NAMES) {
    for (const dock of [false, true]) {
      for (const rail of [false, true]) {
        for (const drawer of [false, true]) {
          for (const count of [1, 2] as const) {
            for (const tab of RAIL_TABS) {
              for (const dirty of [false, true]) {
                const base = presetState(preset);
                out.push({
                  ...base,
                  regions: {
                    dock: { ...base.regions.dock, collapsed: dock },
                    rail: { ...base.regions.rail, collapsed: rail },
                    drawer: { ...base.regions.drawer, collapsed: drawer },
                  },
                  panes: { ...base.panes, count, focused: count === 2 ? 2 : 1, pinned: count === 2 ? [1] : [] },
                  railTab: tab,
                  dirty,
                });
              }
            }
          }
        }
      }
    }
  }
  return out;
}

describe("deskState — the golden round trip", () => {
  it("survives JSON serialise → parse → migrate byte-for-byte, for every reachable state", () => {
    const states = everyState();
    expect(states.length).toBe(4 * 2 * 2 * 2 * 2 * 5 * 2);
    for (const s of states) {
      const back = migrateDeskState(JSON.parse(JSON.stringify(s)));
      expect(back, `round trip lost ${JSON.stringify(s)}`).toEqual(s);
    }
  });

  it("round-trips through storage for every reachable state", () => {
    const store = memStorage();
    for (const s of everyState()) {
      saveDeskState("repo-x", s, store);
      expect(loadDeskState("repo-x", store)).toEqual(s);
    }
  });

  it("keys storage per repo", () => {
    expect(deskStorageKey("kb")).toBe("kbc:desk:kb");
    const store = memStorage();
    saveDeskState("a", { ...presetState("review") }, store);
    saveDeskState("b", { ...presetState("explore") }, store);
    expect(loadDeskState("a", store).preset).toBe("review");
    expect(loadDeskState("b", store).preset).toBe("explore");
  });
});

describe("deskState — corrupt input never bricks the shell", () => {
  const bad: [string, string][] = [
    ["not json", "{{{"],
    ["json null", "null"],
    ["json array", "[1,2,3]"],
    ["json string", '"read"'],
    ["no version", '{"preset":"review"}'],
    ["future version", `{"v":${DESK_STATE_VERSION + 1},"preset":"review"}`],
    ["empty object", "{}"],
  ];
  for (const [name, raw] of bad) {
    it(`falls back to the Read preset on ${name}`, () => {
      const store = memStorage({ [deskStorageKey("r")]: raw });
      expect(loadDeskState("r", store)).toEqual(presetState(DEFAULT_PRESET));
    });
  }

  it("falls back when there is no storage at all", () => {
    expect(loadDeskState("r", null)).toEqual(presetState(DEFAULT_PRESET));
  });

  it("falls back when getItem throws", () => {
    const store: StorageLike = {
      getItem() {
        throw new Error("denied");
      },
      setItem() {},
      removeItem() {},
    };
    expect(loadDeskState("r", store)).toEqual(presetState(DEFAULT_PRESET));
  });

  it("swallows a throwing setItem", () => {
    const store: StorageLike = {
      getItem: () => null,
      setItem() {
        throw new Error("quota");
      },
      removeItem() {},
    };
    expect(() => saveDeskState("r", presetState("read"), store)).not.toThrow();
  });

  it("keeps the fields it can read and preset-fills the rest", () => {
    const got = migrateDeskState({ v: 1, preset: "review", regions: { dock: { size: 30, collapsed: true } } });
    expect(got).not.toBeNull();
    expect(got!.preset).toBe("review");
    expect(got!.regions.dock).toEqual({ size: 30, collapsed: true });
    // rail/drawer were absent → the Review preset's own values, not zeros.
    expect(got!.regions.rail).toEqual(presetState("review").regions.rail);
    expect(got!.regions.drawer).toEqual(presetState("review").regions.drawer);
  });

  it("clamps an out-of-range persisted size instead of trusting it", () => {
    const got = migrateDeskState({ v: 1, preset: "read", regions: { rail: { size: 999, collapsed: false } } });
    expect(got!.regions.rail.size).toBe(REGION_SIZE_MAX);
  });

  it("drops an unknown preset name to Read rather than inventing one", () => {
    const got = migrateDeskState({ v: 1, preset: "zen" });
    expect(got!.preset).toBe(DEFAULT_PRESET);
  });

  it("drops an unknown rail tab to the preset's own tab", () => {
    const got = migrateDeskState({ v: 1, preset: "review", railTab: "outline" });
    expect(got!.railTab).toBe("review");
  });
});

describe("deskState — clamps", () => {
  it("clamps region sizes into range and rejects non-finite", () => {
    expect(clampRegionSize(0)).toBe(REGION_SIZE_MIN);
    expect(clampRegionSize(1000)).toBe(REGION_SIZE_MAX);
    expect(clampRegionSize(Number.NaN)).toBe(REGION_SIZE_MIN);
    expect(clampRegionSize(18.129)).toBe(18.13);
  });
  it("clamps the pane split into range", () => {
    expect(clampSplit(0)).toBe(SPLIT_MIN);
    expect(clampSplit(100)).toBe(SPLIT_MAX);
    expect(clampSplit(Number.POSITIVE_INFINITY)).toBe(50);
  });
});

describe("deskState — the reducer", () => {
  const s0 = presetState("read");

  it("a user resize marks the desk dirty; a programmatic one does not", () => {
    const prog = deskReducer(s0, { type: "resize", target: "dock", size: 25 });
    expect(prog.regions.dock.size).toBe(25);
    expect(prog.dirty).toBe(false);
    const user = deskReducer(s0, { type: "resize", target: "dock", size: 25, user: true });
    expect(user.dirty).toBe(true);
  });

  it("resizing to the floor reads as a collapse, keeping the last expanded size", () => {
    const dragged = deskReducer(s0, { type: "resize", target: "dock", size: 30 });
    const collapsed = deskReducer(dragged, { type: "collapse", region: "dock" });
    const floored = deskReducer(collapsed, { type: "resize", target: "dock", size: 0 });
    expect(floored.regions.dock.collapsed).toBe(true);
    expect(floored.regions.dock.size).toBe(30);
    expect(deskReducer(floored, { type: "expand", region: "dock" }).regions.dock).toEqual({
      size: 30,
      collapsed: false,
    });
  });

  it("collapse/expand are idempotent and identity-stable", () => {
    const c = deskReducer(s0, { type: "collapse", region: "rail" });
    expect(deskReducer(c, { type: "collapse", region: "rail" })).toBe(c);
    const e = deskReducer(c, { type: "expand", region: "rail" });
    expect(deskReducer(e, { type: "expand", region: "rail" })).toBe(e);
  });

  it("an implicit preset never overrides a dirty desk; an explicit one always does", () => {
    const dirty = deskReducer(s0, { type: "resize", target: "dock", size: 30, user: true });
    expect(deskReducer(dirty, { type: "setPreset", preset: "review", implicit: true })).toBe(dirty);
    const explicit = deskReducer(dirty, { type: "setPreset", preset: "review" });
    expect(explicit.preset).toBe("review");
    expect(explicit.dirty).toBe(false);
    expect(explicit.regions).toEqual(presetState("review").regions);
  });

  it("a preset keeps the operator's pane focus and pins", () => {
    const focused = deskReducer(deskReducer(s0, { type: "focusPane", pane: 2 }), {
      type: "togglePinPane",
      pane: 2,
    });
    const applied = deskReducer(focused, { type: "setPreset", preset: "explore" });
    expect(applied.panes.focused).toBe(2);
    expect(applied.panes.pinned).toEqual([2]);
    expect(applied.panes.split).toBe(presetState("explore").panes.split);
  });

  it("reset restores the CURRENT preset and clears dirty", () => {
    const messy = [
      { type: "setPreset", preset: "review" } as DeskAction,
      { type: "resize", target: "rail", size: 40, user: true } as DeskAction,
      { type: "collapse", region: "dock" } as DeskAction,
      { type: "setTab", tab: "notes" } as DeskAction,
    ].reduce(deskReducer, s0);
    expect(messy.dirty).toBe(true);
    const reset = deskReducer(messy, { type: "reset" });
    expect(reset).toEqual({ ...presetState("review"), panes: { ...presetState("review").panes } });
  });

  it("togglePinPane is a toggle and keeps the list sorted", () => {
    const one = deskReducer(s0, { type: "togglePinPane", pane: 2 });
    expect(one.panes.pinned).toEqual([2]);
    const two = deskReducer(one, { type: "togglePinPane", pane: 1 });
    expect(two.panes.pinned).toEqual([1, 2]);
    expect(deskReducer(two, { type: "togglePinPane", pane: 2 }).panes.pinned).toEqual([1]);
  });

  it("setTab is identity-stable for the tab already showing", () => {
    const t = deskReducer(s0, { type: "setTab", tab: "history" });
    expect(deskReducer(t, { type: "setTab", tab: "history" })).toBe(t);
  });

  it("the version is stamped on everything the reducer produces", () => {
    for (const p of DESK_PRESET_NAMES) expect(presetState(p).v).toBe(DESK_STATE_VERSION);
  });
});
