import { describe, expect, it } from "vitest";
import type { DocSummary } from "../api/client";
import {
  categorySegments,
  decideLobbyCollapsed,
  LOBBY_AUTO_COLLAPSE_AFTER,
  pickHighlights,
  sumWords,
  topTagChips,
  type LobbyCollapseState,
} from "./lobby";

function doc(partial: Partial<DocSummary> & { id: string }): DocSummary {
  return {
    title: partial.id,
    path: `${partial.id}.html`,
    folder: "",
    source_relative: `${partial.id}.html`,
    ...partial,
  };
}

describe("pickHighlights", () => {
  it("sorts by backlinks desc, then mtime desc, then id asc", () => {
    const docs = [
      doc({ id: "a", backlinks: 1, mtime_unix: 100 }),
      doc({ id: "b", backlinks: 5, mtime_unix: 10 }),
      doc({ id: "c", backlinks: 5, mtime_unix: 50 }),
      doc({ id: "d", backlinks: 0, mtime_unix: 999 }),
    ];
    expect(pickHighlights(docs, 3).map((d) => d.id)).toEqual(["c", "b", "a"]);
  });

  it("breaks a full tie on id for a stable total order", () => {
    const docs = [
      doc({ id: "z", backlinks: 2, mtime_unix: 10 }),
      doc({ id: "a", backlinks: 2, mtime_unix: 10 }),
    ];
    expect(pickHighlights(docs, 2).map((d) => d.id)).toEqual(["a", "z"]);
  });

  it("treats missing backlinks/mtime as zero, not last", () => {
    const docs = [
      doc({ id: "no-signal" }),
      doc({ id: "has-backlink", backlinks: 1 }),
    ];
    expect(pickHighlights(docs, 2).map((d) => d.id)).toEqual([
      "has-backlink",
      "no-signal",
    ]);
  });

  it("slices to n", () => {
    const docs = [doc({ id: "a" }), doc({ id: "b" }), doc({ id: "c" })];
    expect(pickHighlights(docs, 1)).toHaveLength(1);
  });
});

describe("topTagChips", () => {
  it("sorts by count desc, alpha tiebreak, slices to n", () => {
    const tags = [
      { name: "zeta", count: 3 },
      { name: "alpha", count: 3 },
      { name: "beta", count: 9 },
      { name: "gamma", count: 1 },
    ];
    expect(topTagChips(tags, 3).map((t) => t.name)).toEqual([
      "beta",
      "alpha",
      "zeta",
    ]);
  });
});

describe("categorySegments", () => {
  it("returns [] on a zero total", () => {
    expect(categorySegments([])).toEqual([]);
    expect(categorySegments([{ value: "x", count: 0 }])).toEqual([]);
  });

  it("computes fractions summing to 1 and drops zero-count buckets", () => {
    const segs = categorySegments([
      { value: "research", count: 3 },
      { value: "notes", count: 1 },
      { value: "empty", count: 0 },
    ]);
    expect(segs.map((s) => s.value)).toEqual(["research", "notes"]);
    const sum = segs.reduce((a, s) => a + s.pct, 0);
    expect(sum).toBeCloseTo(1);
    expect(segs[0].pct).toBeCloseTo(0.75);
  });

  it("sorts desc by count, alpha tiebreak", () => {
    const segs = categorySegments([
      { value: "b", count: 2 },
      { value: "a", count: 2 },
    ]);
    expect(segs.map((s) => s.value)).toEqual(["a", "b"]);
  });
});

describe("sumWords", () => {
  it("sums word_count, treating missing as zero", () => {
    const docs = [
      doc({ id: "a", word_count: 100 }),
      doc({ id: "b" }),
      doc({ id: "c", word_count: 50 }),
    ];
    expect(sumWords(docs)).toBe(150);
  });
});

describe("decideLobbyCollapsed", () => {
  const st = (partial: Partial<LobbyCollapseState>): LobbyCollapseState => ({
    seenExpanded: 0,
    override: null,
    ...partial,
  });

  it("a fresh operator (never seen, no override) starts expanded", () => {
    expect(decideLobbyCollapsed(st({}))).toBe(false);
  });

  it("stays expanded below the threshold with no override", () => {
    expect(
      decideLobbyCollapsed(st({ seenExpanded: LOBBY_AUTO_COLLAPSE_AFTER - 1 })),
    ).toBe(false);
  });

  it("auto-collapses once seenExpanded reaches the threshold", () => {
    expect(
      decideLobbyCollapsed(st({ seenExpanded: LOBBY_AUTO_COLLAPSE_AFTER })),
    ).toBe(true);
  });

  it("stays collapsed past the threshold", () => {
    expect(
      decideLobbyCollapsed(st({ seenExpanded: LOBBY_AUTO_COLLAPSE_AFTER + 5 })),
    ).toBe(true);
  });

  it("an 'open' override wins even past the auto-collapse threshold", () => {
    expect(
      decideLobbyCollapsed(
        st({ seenExpanded: LOBBY_AUTO_COLLAPSE_AFTER + 5, override: "open" }),
      ),
    ).toBe(false);
  });

  it("a 'closed' override wins even below the auto-collapse threshold", () => {
    expect(
      decideLobbyCollapsed(st({ seenExpanded: 0, override: "closed" })),
    ).toBe(true);
  });
});

