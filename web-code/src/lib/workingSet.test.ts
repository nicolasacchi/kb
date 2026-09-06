import { beforeEach, describe, expect, it, vi } from "vitest";
import {
  cycle,
  emptyWorkingSet,
  loadWorkingSet,
  MAX_UNPINNED,
  pin,
  remove,
  reorder,
  saveWorkingSet,
  splitPath,
  touch,
  unpin,
  type WorkingSetState,
} from "./workingSet";
// V71-K2 — the restore-order tests below assert the CONSEQUENCE, not just
// the array: order is load-bearing because this is what reads it.
import { isWorkingSetDirty } from "./workspaceDirty";

describe("touch", () => {
  it("adds a new path at the end of the stable order", () => {
    let s = emptyWorkingSet();
    s = touch(s, "a.rs");
    s = touch(s, "b.rs");
    expect(s.entries.map((e) => e.path)).toEqual(["a.rs", "b.rs"]);
  });

  it("re-touching an existing path bumps touchedAt but keeps its position", () => {
    let s = emptyWorkingSet();
    s = touch(s, "a.rs");
    s = touch(s, "b.rs");
    s = touch(s, "a.rs");
    expect(s.entries.map((e) => e.path)).toEqual(["a.rs", "b.rs"]); // NOT reordered
    const a = s.entries.find((e) => e.path === "a.rs")!;
    const b = s.entries.find((e) => e.path === "b.rs")!;
    expect(a.touchedAt).toBeGreaterThan(b.touchedAt);
  });

  it("evicts the least-recently-touched UNPINNED entry once the cap is exceeded", () => {
    let s = emptyWorkingSet();
    for (let i = 0; i < MAX_UNPINNED; i++) s = touch(s, `f${i}.rs`);
    expect(s.entries).toHaveLength(MAX_UNPINNED);
    s = touch(s, "new.rs");
    expect(s.entries).toHaveLength(MAX_UNPINNED);
    expect(s.entries.some((e) => e.path === "f0.rs")).toBe(false); // LRU evicted
    expect(s.entries.some((e) => e.path === "new.rs")).toBe(true);
  });

  it("re-touching an entry protects it from eviction (moves it off the LRU end)", () => {
    let s = emptyWorkingSet();
    for (let i = 0; i < MAX_UNPINNED; i++) s = touch(s, `f${i}.rs`);
    s = touch(s, "f0.rs"); // f0 is now the MOST recently touched
    s = touch(s, "new.rs"); // pushes past the cap again
    expect(s.entries.some((e) => e.path === "f0.rs")).toBe(true);
    expect(s.entries.some((e) => e.path === "f1.rs")).toBe(false); // now the LRU one
  });

  it("pinned entries are exempt from the cap — unlimited pinned + up to 12 unpinned", () => {
    let s = emptyWorkingSet();
    for (let i = 0; i < 5; i++) {
      s = touch(s, `pinned${i}.rs`);
      s = pin(s, `pinned${i}.rs`);
    }
    for (let i = 0; i < MAX_UNPINNED; i++) s = touch(s, `f${i}.rs`);
    // All 5 pinned + all 12 unpinned survive (cap only counts unpinned).
    expect(s.entries).toHaveLength(5 + MAX_UNPINNED);
    s = touch(s, "one-more.rs");
    expect(s.entries).toHaveLength(5 + MAX_UNPINNED); // f0 evicted, pinned untouched
    for (let i = 0; i < 5; i++) expect(s.entries.some((e) => e.path === `pinned${i}.rs`)).toBe(true);
    expect(s.entries.some((e) => e.path === "f0.rs")).toBe(false);
  });
});

describe("pin / unpin", () => {
  it("pin marks an entry pinned (non-null pinnedAt)", () => {
    let s = touch(emptyWorkingSet(), "a.rs");
    s = pin(s, "a.rs");
    expect(s.entries[0].pinnedAt).not.toBeNull();
  });

  it("pin is a no-op for a path not in the working set", () => {
    const s = emptyWorkingSet();
    expect(pin(s, "ghost.rs")).toBe(s);
  });

  it("pin is a no-op when already pinned", () => {
    let s = touch(emptyWorkingSet(), "a.rs");
    s = pin(s, "a.rs");
    const pinnedAt = s.entries[0].pinnedAt;
    s = pin(s, "a.rs");
    expect(s.entries[0].pinnedAt).toBe(pinnedAt);
  });

  it("unpin clears pinnedAt back to null", () => {
    let s = touch(emptyWorkingSet(), "a.rs");
    s = pin(s, "a.rs");
    s = unpin(s, "a.rs");
    expect(s.entries[0].pinnedAt).toBeNull();
  });

  it("unpin is a no-op for a path not in the working set, or not pinned", () => {
    let s = touch(emptyWorkingSet(), "a.rs");
    expect(unpin(s, "ghost.rs")).toBe(s);
    expect(unpin(s, "a.rs")).toBe(s); // never pinned
  });

  it("unpinning past the cap immediately evicts the LRU unpinned entries", () => {
    let s = emptyWorkingSet();
    for (let i = 0; i < MAX_UNPINNED; i++) s = touch(s, `f${i}.rs`);
    s = pin(s, "f0.rs"); // now 11 unpinned, 1 pinned — under cap, nothing evicted yet
    expect(s.entries).toHaveLength(MAX_UNPINNED);
    s = touch(s, "extra.rs"); // 12 unpinned again — fine, at cap
    expect(s.entries).toHaveLength(MAX_UNPINNED + 1);
    s = unpin(s, "f0.rs"); // 13 unpinned now — evicts the LRU one immediately
    expect(s.entries.length).toBeLessThanOrEqual(MAX_UNPINNED);
  });
});

describe("remove", () => {
  it("drops a path regardless of pinned state", () => {
    let s = touch(emptyWorkingSet(), "a.rs");
    s = touch(s, "b.rs");
    s = pin(s, "b.rs");
    s = remove(s, "b.rs");
    expect(s.entries.map((e) => e.path)).toEqual(["a.rs"]);
  });

  it("is a no-op for a path not present", () => {
    const s = touch(emptyWorkingSet(), "a.rs");
    expect(remove(s, "ghost.rs")).toBe(s);
  });
});

describe("cycle", () => {
  let s: WorkingSetState;
  beforeEach(() => {
    s = emptyWorkingSet();
    s = touch(s, "a.rs");
    s = touch(s, "b.rs");
    s = touch(s, "c.rs");
  });

  it("]f (dir 1) moves to the next entry in stable order", () => {
    expect(cycle(s, "a.rs", 1)).toBe("b.rs");
    expect(cycle(s, "b.rs", 1)).toBe("c.rs");
  });

  it("[f (dir -1) moves to the previous entry", () => {
    expect(cycle(s, "c.rs", -1)).toBe("b.rs");
    expect(cycle(s, "b.rs", -1)).toBe("a.rs");
  });

  it("wraps forward past the last entry", () => {
    expect(cycle(s, "c.rs", 1)).toBe("a.rs");
  });

  it("wraps backward past the first entry", () => {
    expect(cycle(s, "a.rs", -1)).toBe("c.rs");
  });

  it("an empty working set has nothing to cycle to", () => {
    expect(cycle(emptyWorkingSet(), "a.rs", 1)).toBeNull();
  });

  it("current undefined (nothing open) lands on the first/last entry", () => {
    expect(cycle(s, undefined, 1)).toBe("a.rs");
    expect(cycle(s, undefined, -1)).toBe("c.rs");
  });

  it("current not a member lands on the first/last entry", () => {
    expect(cycle(s, "ghost.rs", 1)).toBe("a.rs");
    expect(cycle(s, "ghost.rs", -1)).toBe("c.rs");
  });
});

describe("splitPath", () => {
  it("splits a nested path into dir + base", () => {
    expect(splitPath("src/lib/foo.rs")).toEqual({ dir: "src/lib", base: "foo.rs" });
  });

  it("a root-level file has an empty dir", () => {
    expect(splitPath("README.md")).toEqual({ dir: "", base: "README.md" });
  });

  it("keeps only the immediate parent, not the full ancestry, as `dir`", () => {
    // `dir` is everything up to the LAST slash — the caller renders the
    // whole thing dimmed, not just the immediate parent name, so this is
    // "everything but the basename," not "one path segment."
    expect(splitPath("a/b/c/d.rs")).toEqual({ dir: "a/b/c", base: "d.rs" });
  });
});

// vitest runs in the node env here (no DOM) — stub a minimal in-memory
// sessionStorage for the persistence round-trip, same approach
// `lib/prefs.test.ts` uses for `localStorage`.
function stubSessionStorage() {
  const store: Record<string, string> = {};
  vi.stubGlobal("sessionStorage", {
    getItem: (k: string) => (k in store ? store[k] : null),
    setItem: (k: string, v: string) => {
      store[k] = v;
    },
    removeItem: (k: string) => {
      delete store[k];
    },
    clear: () => {
      for (const k of Object.keys(store)) delete store[k];
    },
  });
}

describe("sessionStorage persistence", () => {
  beforeEach(() => {
    stubSessionStorage();
  });

  it("round-trips through save/load", () => {
    let s = touch(emptyWorkingSet(), "a.rs");
    s = touch(s, "b.rs");
    s = pin(s, "b.rs");
    saveWorkingSet("myrepo", s);
    expect(loadWorkingSet("myrepo")).toEqual(s);
  });

  it("is scoped per repo — a different repo's key doesn't leak in", () => {
    const s = touch(emptyWorkingSet(), "a.rs");
    saveWorkingSet("repo-a", s);
    expect(loadWorkingSet("repo-b")).toEqual(emptyWorkingSet());
  });

  it("returns an empty state when nothing is stored yet", () => {
    expect(loadWorkingSet("fresh-repo")).toEqual(emptyWorkingSet());
  });

  it("degrades to empty on corrupt JSON rather than throwing", () => {
    sessionStorage.setItem("kbc:ws:bad", "{not json");
    expect(loadWorkingSet("bad")).toEqual(emptyWorkingSet());
  });

  it("degrades to empty when the shape is wrong (missing entries/seq)", () => {
    sessionStorage.setItem("kbc:ws:bad2", JSON.stringify({ foo: "bar" }));
    expect(loadWorkingSet("bad2")).toEqual(emptyWorkingSet());
  });

  it("filters out malformed individual entries rather than failing the whole load", () => {
    sessionStorage.setItem(
      "kbc:ws:partial",
      JSON.stringify({ entries: [{ path: "ok.rs", touchedAt: 1, pinnedAt: null }, { path: 123 }], seq: 1 }),
    );
    expect(loadWorkingSet("partial").entries).toEqual([{ path: "ok.rs", touchedAt: 1, pinnedAt: null }]);
  });
});

// V71-K2 — the restore path's own order contract. V70-A10 promised "open
// every saved entry into the working set, IN ORDER" and implemented it as a
// `touch` loop; the first test below is the proof that that could never
// work, and the rest pin what replaced it.
describe("reorder (workspace restore)", () => {
  it("a touch loop CANNOT restore saved order once the focused file is already open", () => {
    // Exactly the live sequence: the restored URL names the saved FOCUSED
    // pane's file, so the reader's open effect touches it first; the
    // restore loop then walks the saved spans. `touch` never moves a path
    // that is already present (by design — chips must not reshuffle), so
    // the strip comes back focused-file-first, not saved-order-first.
    let s = touch(emptyWorkingSet(), "caller.rs");
    for (const p of ["lib.rs", "caller.rs"]) s = touch(s, p);
    expect(s.entries.map((e) => e.path)).toEqual(["caller.rs", "lib.rs"]);
    // …and that is DIRTY against its own saved order, which is the bug.
    expect(isWorkingSetDirty(s.entries.map((e) => e.path), ["lib.rs", "caller.rs"])).toBe(true);
  });

  it("lays the strip out in the given order, whichever entries already existed", () => {
    let s = touch(emptyWorkingSet(), "caller.rs");
    s = reorder(s, ["lib.rs", "caller.rs"]);
    expect(s.entries.map((e) => e.path)).toEqual(["lib.rs", "caller.rs"]);
    expect(isWorkingSetDirty(s.entries.map((e) => e.path), ["lib.rs", "caller.rs"])).toBe(false);
  });

  it("is idempotent, and a later touch on a present path does not disturb it", () => {
    // The two effects race; the result may not depend on who wins.
    let s = reorder(emptyWorkingSet(), ["lib.rs", "caller.rs"]);
    s = touch(s, "caller.rs");
    s = reorder(s, ["lib.rs", "caller.rs"]);
    expect(s.entries.map((e) => e.path)).toEqual(["lib.rs", "caller.rs"]);
  });

  it("carries pin + touchedAt across the move — a restore is not a visit", () => {
    let s = touch(emptyWorkingSet(), "a.rs");
    s = touch(s, "b.rs");
    s = pin(s, "a.rs");
    const before = s.entries.find((e) => e.path === "a.rs");
    s = reorder(s, ["b.rs", "a.rs"]);
    expect(s.entries.map((e) => e.path)).toEqual(["b.rs", "a.rs"]);
    expect(s.entries.find((e) => e.path === "a.rs")).toEqual(before);
  });

  it("keeps a live entry the restore does not name, after the named ones", () => {
    // Opening a workspace ADDS its files; it never closes yours.
    let s = touch(emptyWorkingSet(), "scratch.rs");
    s = reorder(s, ["lib.rs", "caller.rs"]);
    expect(s.entries.map((e) => e.path)).toEqual(["lib.rs", "caller.rs", "scratch.rs"]);
  });

  it("collapses a duplicated path to its first occurrence", () => {
    const s = reorder(emptyWorkingSet(), ["a.rs", "b.rs", "a.rs"]);
    expect(s.entries.map((e) => e.path)).toEqual(["a.rs", "b.rs"]);
  });

  it("still honours the unpinned cap", () => {
    const many = Array.from({ length: MAX_UNPINNED + 3 }, (_, i) => `f${i}.rs`);
    const s = reorder(emptyWorkingSet(), many);
    expect(s.entries).toHaveLength(MAX_UNPINNED);
    // Oldest-first eviction: the earliest-stamped names go.
    expect(s.entries.map((e) => e.path)).toEqual(many.slice(3));
  });

  it("an empty order is a no-op rather than a wipe", () => {
    const s = touch(emptyWorkingSet(), "a.rs");
    expect(reorder(s, []).entries.map((e) => e.path)).toEqual(["a.rs"]);
  });
});
