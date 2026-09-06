import { afterEach, describe, expect, it } from "vitest";
import {
  DEDUPE_LINE_DELTA,
  LOCATIONS_CAP,
  RECENT_FILES_CAP,
  _applyForeignStore,
  _resetNavHistoryForTests,
  backCount,
  emptyNavHistory,
  emptyNavHistoryStore,
  enterRing,
  filterLocations,
  forwardCount,
  getNavHistoryState,
  mergeEntries,
  parseNavHistoryStore,
  peekBack,
  peekForward,
  serializeNavHistoryStore,
  getRecentFiles,
  getRecentLocations,
  goBack,
  goForward,
  jumpBack,
  jumpForward,
  makeLocation,
  makeSnippet,
  parseNavHistory,
  pushEntry,
  recentFilesFrom,
  recordJump,
  serializeNavHistory,
  type NavHistoryState,
  type NavLocation,
} from "./navHistory";

function loc(
  path: string,
  line: number,
  opts: Partial<NavLocation> & { repo?: string } = {},
): NavLocation {
  return makeLocation(opts.repo ?? "r", path, line, opts.snippet ?? `line ${line}`, opts.ts ?? line);
}

afterEach(() => {
  _resetNavHistoryForTests(emptyNavHistory());
});

describe("makeSnippet", () => {
  it("trims and caps at 120 chars", () => {
    expect(makeSnippet("  hello  ")).toBe("hello");
    const long = "x".repeat(200);
    expect(makeSnippet(long).length).toBe(120);
  });
});

describe("pushEntry", () => {
  it("prepends newest-first and resets pointer to tip", () => {
    let s = emptyNavHistory();
    s = pushEntry(s, loc("a.rs", 1));
    s = pushEntry(s, loc("b.rs", 2));
    expect(s.entries.map((e) => e.path)).toEqual(["b.rs", "a.rs"]);
    expect(s.pointer).toBe(0);
  });

  it("dedupes consecutive same-file entries within 3 lines", () => {
    let s = emptyNavHistory();
    s = pushEntry(s, loc("a.rs", 10));
    s = pushEntry(s, loc("a.rs", 10 + DEDUPE_LINE_DELTA)); // still within window
    expect(s.entries).toHaveLength(1);
    expect(s.entries[0].line).toBe(10 + DEDUPE_LINE_DELTA);
  });

  it("does NOT dedupe when more than 3 lines apart", () => {
    let s = emptyNavHistory();
    s = pushEntry(s, loc("a.rs", 10));
    s = pushEntry(s, loc("a.rs", 10 + DEDUPE_LINE_DELTA + 1));
    expect(s.entries).toHaveLength(2);
  });

  it("does NOT dedupe across different files", () => {
    let s = emptyNavHistory();
    s = pushEntry(s, loc("a.rs", 10));
    s = pushEntry(s, loc("b.rs", 10));
    expect(s.entries).toHaveLength(2);
  });

  it("caps at LOCATIONS_CAP", () => {
    let s = emptyNavHistory();
    for (let i = 0; i < LOCATIONS_CAP + 25; i++) {
      s = pushEntry(s, loc(`f${i}.rs`, i + 1));
    }
    expect(s.entries).toHaveLength(LOCATIONS_CAP);
    // Newest is the last pushed.
    expect(s.entries[0].path).toBe(`f${LOCATIONS_CAP + 24}.rs`);
  });

  it("drops the forward half when pushing from mid-list", () => {
    let s: NavHistoryState = {
      entries: [loc("c.rs", 3), loc("b.rs", 2), loc("a.rs", 1)],
      pointer: 1, // sitting on b.rs
    };
    s = pushEntry(s, loc("d.rs", 4));
    // Future (c.rs, newer than pointer) is gone; d on tip, then b, a.
    expect(s.entries.map((e) => e.path)).toEqual(["d.rs", "b.rs", "a.rs"]);
    expect(s.pointer).toBe(0);
  });
});

describe("jumpBack / jumpForward pointer math", () => {
  it("back from tip records current then steps older", () => {
    let s = emptyNavHistory();
    s = pushEntry(s, loc("a.rs", 1));
    s = pushEntry(s, loc("b.rs", 2));
    // At tip (b). Ctrl-o with current at c.
    const { state, target } = jumpBack(s, loc("c.rs", 3));
    // After recording c at tip, older is b (was previous tip).
    // entries: [c, b, a], pointer 1 → b
    expect(target?.path).toBe("b.rs");
    expect(state.pointer).toBe(1);
    expect(state.entries[0].path).toBe("c.rs");
  });

  it("back when current dedupes with tip still walks older", () => {
    let s = emptyNavHistory();
    s = pushEntry(s, loc("a.rs", 1));
    s = pushEntry(s, loc("b.rs", 10));
    // Current is still on b near the tip line — dedupes, tip stays one entry.
    const { state, target } = jumpBack(s, loc("b.rs", 11));
    expect(target?.path).toBe("a.rs");
    expect(state.pointer).toBe(1);
  });

  it("back at oldest returns null (after recording current)", () => {
    let s = emptyNavHistory();
    s = pushEntry(s, loc("a.rs", 1));
    // Only one entry; after recording current (new file), two entries, back lands on a.
    const first = jumpBack(s, loc("b.rs", 2));
    expect(first.target?.path).toBe("a.rs");
    // Now at oldest — further back is null.
    const second = jumpBack(first.state, first.target!);
    expect(second.target).toBeNull();
    expect(second.state.pointer).toBe(first.state.pointer); // or at end
  });

  it("forward walks back toward the tip", () => {
    let s = emptyNavHistory();
    s = pushEntry(s, loc("a.rs", 1));
    s = pushEntry(s, loc("b.rs", 2));
    s = pushEntry(s, loc("c.rs", 3));
    // Manually sit on a (oldest).
    s = { entries: s.entries, pointer: 2 };
    const f1 = jumpForward(s);
    expect(f1.target?.path).toBe("b.rs");
    const f2 = jumpForward(f1.state);
    expect(f2.target?.path).toBe("c.rs");
    const f3 = jumpForward(f2.state);
    expect(f3.target).toBeNull();
  });

  it("forward at tip is a no-op", () => {
    let s = emptyNavHistory();
    s = pushEntry(s, loc("a.rs", 1));
    const { state, target } = jumpForward(s);
    expect(target).toBeNull();
    expect(state.pointer).toBe(0);
  });

  it("back on empty list with current creates one entry and finds nothing older", () => {
    const { state, target } = jumpBack(emptyNavHistory(), loc("a.rs", 1));
    expect(target).toBeNull();
    expect(state.entries).toHaveLength(1);
    expect(state.pointer).toBe(0);
  });
});

describe("recentFilesFrom", () => {
  it("unique by (repo,path), newest first, capped", () => {
    const entries = [
      loc("a.rs", 3, { ts: 30 }),
      loc("b.rs", 1, { ts: 20 }),
      loc("a.rs", 1, { ts: 10 }), // duplicate path — skipped
      loc("c.rs", 1, { repo: "other", ts: 5 }),
    ];
    const files = recentFilesFrom(entries);
    expect(files).toEqual([
      { repo: "r", path: "a.rs", ts: 30 },
      { repo: "r", path: "b.rs", ts: 20 },
      { repo: "other", path: "c.rs", ts: 5 },
    ]);
  });

  it("caps at RECENT_FILES_CAP", () => {
    const entries = Array.from({ length: RECENT_FILES_CAP + 10 }, (_, i) =>
      loc(`f${i}.rs`, 1, { ts: 1000 - i }),
    );
    expect(recentFilesFrom(entries)).toHaveLength(RECENT_FILES_CAP);
  });
});

describe("serialize / parse", () => {
  it("round-trips a valid state", () => {
    const s: NavHistoryState = {
      entries: [loc("a.rs", 1), loc("b.rs", 2)],
      pointer: 1,
    };
    const again = parseNavHistory(serializeNavHistory(s));
    expect(again).toEqual(s);
  });

  it("corrupt / empty input degrades to empty", () => {
    expect(parseNavHistory(null)).toEqual(emptyNavHistory());
    expect(parseNavHistory("not-json")).toEqual(emptyNavHistory());
    expect(parseNavHistory("{}")).toEqual(emptyNavHistory());
  });
});

describe("module API (recordJump / goBack / goForward)", () => {
  it("recordJump accumulates and goBack/goForward walk the pointer", () => {
    recordJump({ repo: "r", path: "a.rs", line: 1, snippet: "a" });
    recordJump({ repo: "r", path: "b.rs", line: 2, snippet: "b" });
    expect(getRecentLocations().map((e) => e.path)).toEqual(["b.rs", "a.rs"]);
    expect(getRecentFiles().map((f) => f.path)).toEqual(["b.rs", "a.rs"]);

    const back = goBack({ repo: "r", path: "c.rs", line: 3, snippet: "c" });
    expect(back?.path).toBe("b.rs");

    const fwd = goForward();
    expect(fwd?.path).toBe("c.rs");
  });
});

// --- V70-A6 additions ----------------------------------------------------

describe("per-pane rings (recon G6)", () => {
  it("records each pane separately and merges for the Recent view", () => {
    _resetNavHistoryForTests();
    recordJump({ repo: "kb", path: "a.rs", line: 1, ts: 100, pane: 1, via: "tree" });
    recordJump({ repo: "kb", path: "b.rs", line: 2, ts: 200, pane: 2, via: "usage_of" });
    expect(getNavHistoryState(1).entries.map((e) => e.path)).toEqual(["a.rs"]);
    expect(getNavHistoryState(2).entries.map((e) => e.path)).toEqual(["b.rs"]);
    // "Where have I been" is a question about the READER, not a pane.
    expect(getRecentLocations().map((e) => e.path)).toEqual(["b.rs", "a.rs"]);
    expect(getRecentLocations(1).map((e) => e.path)).toEqual(["a.rs"]);
  });

  it("carries the typed via and the pane on the entry", () => {
    _resetNavHistoryForTests();
    recordJump({ repo: "kb", path: "a.rs", line: 1, ts: 1, pane: 2, via: "caller_of" });
    const e = getNavHistoryState(2).entries[0];
    expect(e.via).toBe("caller_of");
    expect(e.pane).toBe(2);
  });

  it("walks each pane's pointer independently", () => {
    _resetNavHistoryForTests();
    recordJump({ repo: "kb", path: "a.rs", line: 1, ts: 1, pane: 1 });
    recordJump({ repo: "kb", path: "b.rs", line: 1, ts: 2, pane: 1 });
    recordJump({ repo: "kb", path: "x.rs", line: 1, ts: 3, pane: 2 });
    const back1 = goBack({ repo: "kb", path: "b.rs", line: 1, pane: 1 });
    expect(back1?.path).toBe("a.rs");
    // Pane 2 never moved.
    expect(getNavHistoryState(2).pointer).toBe(0);
  });
});

describe("count badges + hover previews (the pane arrows)", () => {
  it("counts what is still there in each direction", () => {
    _resetNavHistoryForTests();
    for (let i = 1; i <= 3; i++) {
      recordJump({ repo: "kb", path: `f${i}.rs`, line: 1, ts: i, pane: 1 });
    }
    expect(backCount(getNavHistoryState(1))).toBe(2);
    expect(forwardCount(getNavHistoryState(1))).toBe(0);
    goBack({ repo: "kb", path: "f3.rs", line: 1, pane: 1 });
    expect(forwardCount(getNavHistoryState(1))).toBe(1);
  });

  it("previews the destination WITHOUT moving the pointer", () => {
    _resetNavHistoryForTests();
    recordJump({ repo: "kb", path: "a.rs", line: 4, snippet: "fn a() {", ts: 1, pane: 1 });
    recordJump({ repo: "kb", path: "b.rs", line: 9, snippet: "fn b() {", ts: 2, pane: 1 });
    const before = getNavHistoryState(1).pointer;
    expect(peekBack(getNavHistoryState(1))?.path).toBe("a.rs");
    expect(peekForward(getNavHistoryState(1))).toBeNull();
    expect(getNavHistoryState(1).pointer).toBe(before);
  });
});

describe("enterRing — Ctrl-o from a surface with no file of its own", () => {
  it("lands on the ring's current entry and records nothing", () => {
    _resetNavHistoryForTests();
    recordJump({ repo: "kb", path: "a.rs", line: 7, ts: 1, pane: 1 });
    const before = getNavHistoryState(1).entries.length;
    expect(enterRing(1)?.path).toBe("a.rs");
    expect(getNavHistoryState(1).entries).toHaveLength(before);
  });
  it("is null on an empty ring", () => {
    _resetNavHistoryForTests();
    expect(enterRing(1)).toBeNull();
  });
});

describe("cross-tab convergence (recon G8)", () => {
  const loc = (path: string, ts: number) => makeLocation("kb", path, 1, "", ts);

  it("merges by timestamp, newest first, de-duplicated", () => {
    const mine = [loc("b.rs", 200), loc("a.rs", 100)];
    const theirs = [loc("c.rs", 300), loc("a.rs", 100)];
    expect(mergeEntries(mine, theirs).map((e) => e.path)).toEqual(["c.rs", "b.rs", "a.rs"]);
  });

  it("caps the merge", () => {
    const mine = Array.from({ length: 80 }, (_, i) => loc(`m${i}.rs`, i));
    const theirs = Array.from({ length: 80 }, (_, i) => loc(`t${i}.rs`, 1000 + i));
    expect(mergeEntries(mine, theirs)).toHaveLength(LOCATIONS_CAP);
  });

  it("a foreign store merges into a ring at the tip", () => {
    _resetNavHistoryForTests();
    recordJump({ repo: "kb", path: "mine.rs", line: 1, ts: 100, pane: 1 });
    _applyForeignStore({
      panes: {
        1: { entries: [loc("theirs.rs", 200)], pointer: 0 },
        2: emptyNavHistory(),
      },
    });
    expect(getNavHistoryState(1).entries.map((e) => e.path)).toEqual(["theirs.rs", "mine.rs"]);
  });

  it("NEVER merges under a live Ctrl-o cursor, and never adopts a foreign pointer", () => {
    _resetNavHistoryForTests();
    recordJump({ repo: "kb", path: "a.rs", line: 1, ts: 1, pane: 1 });
    recordJump({ repo: "kb", path: "b.rs", line: 1, ts: 2, pane: 1 });
    goBack({ repo: "kb", path: "b.rs", line: 1, pane: 1 });
    const mid = getNavHistoryState(1);
    _applyForeignStore({
      panes: { 1: { entries: [loc("theirs.rs", 500)], pointer: 0 }, 2: emptyNavHistory() },
    });
    expect(getNavHistoryState(1)).toEqual(mid);
  });
});

describe("the store's own persistence", () => {
  it("round-trips two panes", () => {
    const store = {
      panes: {
        1: { entries: [makeLocation("kb", "a.rs", 3, "x", 5, { via: "search" as const })], pointer: 0 },
        2: { entries: [makeLocation("kb", "b.rs", 4, "y", 6)], pointer: 0 },
      },
    };
    const back = parseNavHistoryStore(serializeNavHistoryStore(store));
    expect(back.panes[1].entries[0].via).toBe("search");
    expect(back.panes[2].entries[0].path).toBe("b.rs");
  });

  it("MIGRATES a pre-A6 single-ring blob into pane 1 rather than dropping it", () => {
    const legacy = JSON.stringify({
      entries: [{ repo: "kb", path: "old.rs", line: 2, snippet: "s", ts: 9 }],
      pointer: 0,
    });
    const store = parseNavHistoryStore(legacy);
    expect(store.panes[1].entries.map((e) => e.path)).toEqual(["old.rs"]);
    expect(store.panes[2].entries).toEqual([]);
  });

  it("degrades to empty on junk", () => {
    expect(parseNavHistoryStore("{{{")).toEqual(emptyNavHistoryStore());
    expect(parseNavHistoryStore(null)).toEqual(emptyNavHistoryStore());
  });
});

describe("filterLocations includes the via kind", () => {
  it("so \"definition hops only\" is a search, not a second store", () => {
    const entries = [
      makeLocation("kb", "a.rs", 1, "", 1, { via: "definition_of" }),
      makeLocation("kb", "b.rs", 1, "", 2, { via: "usage_of" }),
    ];
    expect(filterLocations(entries, "definition").map((h) => h.item.path)).toEqual(["a.rs"]);
  });
});
