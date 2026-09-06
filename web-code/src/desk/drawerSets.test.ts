import { describe, expect, it } from "vitest";
import {
  DRAWER_EVICTED_RETAIN,
  DRAWER_SET_CAP,
  activeDrawerSet,
  drawerSetId,
  drawerSetsReducer,
  drawerTabOrder,
  initialDrawerSets,
  type DrawerRow,
  type DrawerSetsAction,
  type DrawerSetsState,
} from "./drawerSets";

const rows = (n: number): DrawerRow[] =>
  Array.from({ length: n }, (_, i) => ({ repo: "fixture", path: `src/f${i}.rs`, line: i + 1 }));

function keep(state: DrawerSetsState, n: number, rowCount = 3): DrawerSetsState {
  return drawerSetsReducer(state, {
    type: "keep",
    set: { id: `usages:s${n}`, title: `set ${n}`, kind: "usages", rows: rows(rowCount) },
  });
}

function run(actions: DrawerSetsAction[], from = initialDrawerSets): DrawerSetsState {
  return actions.reduce(drawerSetsReducer, from);
}

describe("drawerSets — ids", () => {
  it("builds a stable id from kind + key", () => {
    expect(drawerSetId("usages", "KNOWN@fixture/src/lib.rs:12")).toBe("usages:KNOWN@fixture/src/lib.rs:12");
  });
});

describe("drawerSets — keep", () => {
  it("opens a set and makes it active", () => {
    const s = keep(initialDrawerSets, 1);
    expect(s.sets).toHaveLength(1);
    expect(s.activeId).toBe("usages:s1");
    expect(activeDrawerSet(s)?.rows).toHaveLength(3);
  });

  it("re-keeping the same id REFRESHES in place — one tab, not two", () => {
    let s = keep(initialDrawerSets, 1, 3);
    s = drawerSetsReducer(s, { type: "moveCursor", delta: 2 });
    expect(activeDrawerSet(s)?.cursor).toBe(2);
    s = keep(s, 1, 10);
    expect(s.sets).toHaveLength(1);
    expect(activeDrawerSet(s)?.rows).toHaveLength(10);
    expect(activeDrawerSet(s)?.cursor).toBe(2);
  });

  it("clamps the cursor when a refresh returns fewer rows", () => {
    let s = keep(initialDrawerSets, 1, 8);
    s = drawerSetsReducer(s, { type: "setCursor", index: 7 });
    s = keep(s, 1, 2);
    expect(activeDrawerSet(s)?.cursor).toBe(1);
  });

  it("re-keeping an EVICTED set brings it back with its rows", () => {
    let s = keep(initialDrawerSets, 1);
    s = keep(s, 2);
    s = drawerSetsReducer(s, { type: "close", id: "usages:s1" });
    expect(s.sets.find((x) => x.id === "usages:s1")?.evicted).toBe(true);
    s = keep(s, 1);
    expect(s.sets.find((x) => x.id === "usages:s1")?.evicted).toBe(false);
    expect(s.activeId).toBe("usages:s1");
  });
});

describe("drawerSets — the cap is a VIEW operation", () => {
  it("holds at most 9 live sets, evicting the oldest unpinned one", () => {
    let s = initialDrawerSets;
    for (let i = 1; i <= DRAWER_SET_CAP + 1; i++) s = keep(s, i);
    const live = s.sets.filter((x) => !x.evicted);
    expect(live).toHaveLength(DRAWER_SET_CAP);
    expect(s.sets.find((x) => x.id === "usages:s1")?.evicted).toBe(true);
  });

  it("an evicted set keeps every row — eviction never destroys data", () => {
    let s = initialDrawerSets;
    for (let i = 1; i <= DRAWER_SET_CAP + 1; i++) s = keep(s, i, 5);
    const gone = s.sets.find((x) => x.id === "usages:s1");
    expect(gone?.evicted).toBe(true);
    expect(gone?.rows).toHaveLength(5);
    const back = drawerSetsReducer(s, { type: "reopen", id: "usages:s1" });
    expect(back.sets.find((x) => x.id === "usages:s1")?.rows).toHaveLength(5);
    expect(back.activeId).toBe("usages:s1");
  });

  it("pinned sets never evict", () => {
    let s = keep(initialDrawerSets, 1);
    s = drawerSetsReducer(s, { type: "togglePin", id: "usages:s1" });
    for (let i = 2; i <= DRAWER_SET_CAP + 3; i++) s = keep(s, i);
    expect(s.sets.find((x) => x.id === "usages:s1")?.evicted).toBe(false);
    expect(s.sets.filter((x) => !x.evicted)).toHaveLength(DRAWER_SET_CAP);
  });

  it("a desk of all-pinned sets simply stops capping rather than evicting one", () => {
    let s = initialDrawerSets;
    for (let i = 1; i <= DRAWER_SET_CAP; i++) {
      s = keep(s, i);
      s = drawerSetsReducer(s, { type: "togglePin", id: `usages:s${i}` });
    }
    s = keep(s, 99);
    expect(s.sets.filter((x) => !x.evicted)).toHaveLength(DRAWER_SET_CAP + 1);
    expect(s.sets.every((x) => !x.evicted)).toBe(true);
  });

  it("reopening past the cap evicts a different live set, never the one reopened", () => {
    let s = initialDrawerSets;
    for (let i = 1; i <= DRAWER_SET_CAP + 1; i++) s = keep(s, i);
    s = drawerSetsReducer(s, { type: "reopen", id: "usages:s1" });
    expect(s.sets.find((x) => x.id === "usages:s1")?.evicted).toBe(false);
    expect(s.sets.filter((x) => !x.evicted)).toHaveLength(DRAWER_SET_CAP);
  });

  it("releases the oldest evicted sets past the retention ceiling", () => {
    let s = initialDrawerSets;
    const total = DRAWER_SET_CAP + DRAWER_EVICTED_RETAIN + 3;
    for (let i = 1; i <= total; i++) s = keep(s, i);
    expect(s.sets.filter((x) => x.evicted)).toHaveLength(DRAWER_EVICTED_RETAIN);
    // The very oldest are gone entirely (a memory bound, not eviction).
    expect(s.sets.find((x) => x.id === "usages:s1")).toBeUndefined();
  });
});

describe("drawerSets — close / drop / activate", () => {
  it("close evicts and hands the active slot to another live set", () => {
    let s = keep(keep(initialDrawerSets, 1), 2);
    s = drawerSetsReducer(s, { type: "close", id: "usages:s2" });
    expect(s.activeId).toBe("usages:s1");
    expect(s.sets.find((x) => x.id === "usages:s2")?.evicted).toBe(true);
  });

  it("closing the last live set leaves no active set", () => {
    let s = keep(initialDrawerSets, 1);
    s = drawerSetsReducer(s, { type: "close", id: "usages:s1" });
    expect(s.activeId).toBeNull();
    expect(activeDrawerSet(s)).toBeNull();
  });

  it("activating an evicted set is the same gesture as reopening it", () => {
    let s = keep(keep(initialDrawerSets, 1), 2);
    s = drawerSetsReducer(s, { type: "close", id: "usages:s1" });
    s = drawerSetsReducer(s, { type: "activate", id: "usages:s1" });
    expect(s.sets.find((x) => x.id === "usages:s1")?.evicted).toBe(false);
    expect(s.activeId).toBe("usages:s1");
  });

  it("drop is the ONE data operation and forgets the rows", () => {
    let s = keep(keep(initialDrawerSets, 1), 2);
    s = drawerSetsReducer(s, { type: "drop", id: "usages:s2" });
    expect(s.sets.find((x) => x.id === "usages:s2")).toBeUndefined();
    expect(s.activeId).toBe("usages:s1");
  });

  it("acting on an unknown id is an identity no-op", () => {
    const s = keep(initialDrawerSets, 1);
    for (const a of [
      { type: "activate", id: "nope" },
      { type: "close", id: "nope" },
      { type: "reopen", id: "nope" },
      { type: "drop", id: "nope" },
      { type: "togglePin", id: "nope" },
    ] as DrawerSetsAction[]) {
      expect(drawerSetsReducer(s, a)).toBe(s);
    }
  });
});

describe("drawerSets — walking", () => {
  it("j/k clamp at both ends", () => {
    let s = keep(initialDrawerSets, 1, 3);
    s = drawerSetsReducer(s, { type: "moveCursor", delta: -1 });
    expect(activeDrawerSet(s)?.cursor).toBe(0);
    s = drawerSetsReducer(s, { type: "moveCursor", delta: 99 });
    expect(activeDrawerSet(s)?.cursor).toBe(2);
  });

  it("a cursor move on an empty set is a no-op", () => {
    const s = keep(initialDrawerSets, 1, 0);
    expect(drawerSetsReducer(s, { type: "moveCursor", delta: 1 })).toBe(s);
  });

  it("stepSet wraps over LIVE tabs only", () => {
    let s = run([]);
    for (let i = 1; i <= 3; i++) s = keep(s, i);
    s = drawerSetsReducer(s, { type: "close", id: "usages:s2" });
    s = drawerSetsReducer(s, { type: "activate", id: "usages:s1" });
    s = drawerSetsReducer(s, { type: "stepSet", delta: 1 });
    expect(s.activeId).toBe("usages:s3");
    s = drawerSetsReducer(s, { type: "stepSet", delta: 1 });
    expect(s.activeId).toBe("usages:s1");
    s = drawerSetsReducer(s, { type: "stepSet", delta: -1 });
    expect(s.activeId).toBe("usages:s3");
  });

  it("stepSet on an empty drawer is a no-op", () => {
    expect(drawerSetsReducer(initialDrawerSets, { type: "stepSet", delta: 1 })).toBe(initialDrawerSets);
  });
});

describe("drawerSets — tab order", () => {
  it("live tabs keep insertion order, evicted ones sink below without reshuffling them", () => {
    let s = initialDrawerSets;
    for (let i = 1; i <= 4; i++) s = keep(s, i);
    s = drawerSetsReducer(s, { type: "close", id: "usages:s2" });
    expect(drawerTabOrder(s).map((x) => x.id)).toEqual([
      "usages:s1",
      "usages:s3",
      "usages:s4",
      "usages:s2",
    ]);
  });
});
