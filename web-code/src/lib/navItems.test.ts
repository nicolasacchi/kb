import { describe, expect, it } from "vitest";
import {
  COLLECTION_GROUPS,
  COLLECTION_ITEMS,
  NAV_ITEMS,
  inboxUrl,
  matchActiveNavItem,
  navItemMatches,
  navTestAttr,
  seqCounts,
} from "./navItems";
import { branchesUrl, collectionsUrl, reviewsUrl } from "./codeUrl";

describe("navItemMatches", () => {
  it("matches an exact sentinel path", () => {
    expect(navItemMatches("/r/kb/~branches", branchesUrl("kb"))).toBe(true);
  });

  it("matches a nested child of the sentinel (reviews cockpit)", () => {
    expect(navItemMatches("/r/kb/~reviews/12", reviewsUrl("kb"))).toBe(true);
  });

  it("does not match a sibling file path or a different repo", () => {
    expect(navItemMatches("/r/kb/src/lib.rs", branchesUrl("kb"))).toBe(false);
    expect(navItemMatches("/r/other/~branches", branchesUrl("kb"))).toBe(false);
  });
});

describe("NAV_ITEMS — the four doors", () => {
  it("are Code, Search, Review, Collections in bar order", () => {
    expect(NAV_ITEMS.map((i) => [i.key, i.label])).toEqual([
      ["code", "Code"],
      ["search", "Search"],
      ["review", "Review"],
      ["collections", "Collections"],
    ]);
  });

  it("keeps the data-kbc-topbar-* attribute naming", () => {
    for (const item of NAV_ITEMS) {
      expect(navTestAttr(item.key)).toBe(`data-kbc-topbar-${item.key}`);
    }
    expect(navTestAttr("inbox")).toBe("data-kbc-topbar-inbox");
  });

  it("builds each door URL through the builders", () => {
    const urls = Object.fromEntries(NAV_ITEMS.map((i) => [i.key, i.url("kb")]));
    expect(urls).toEqual({
      code: "/r/kb",
      search: "/search?repo=kb",
      review: "/r/kb/~reviews",
      collections: "/r/kb/~collections",
    });
    expect(collectionsUrl("kb")).toBe("/r/kb/~collections");
  });

  it("S2-A: inbox is repo-less — its url ignores whichever repo is active", () => {
    expect(inboxUrl("acme/widgets")).toBe("/~inbox");
    expect(inboxUrl("other-repo")).toBe("/~inbox");
  });
});

/// Every route in `app.tsx`, as a concrete pathname, → the door that must be
/// active. Adding a route there without a row here is how a destination
/// ends up with no lit door.
const ROUTE_TABLE: [string, string | null][] = [
  ["/", null],
  ["/search", "search"],
  ["/~inbox", null],
  ["/~lens/kb/abc", null],
  ["/~lens/kb/by-path/docs/a.html", null],
  ["/r/kb/~commit/abc123", "code"],
  ["/r/kb/~compare", "review"],
  ["/r/kb/~branches", "review"],
  ["/r/kb/~range-diff", "review"],
  ["/r/kb/~prs", "review"],
  ["/r/kb/~sets", "collections"],
  ["/r/kb/~sets/s1", "collections"],
  ["/r/kb/~sets/s1/~tour", "collections"],
  ["/r/kb/~workspaces", "collections"],
  ["/r/kb/~todos", "collections"],
  ["/r/kb/~comments", "collections"],
  ["/r/kb/~lanes", "collections"],
  ["/r/kb/~rails", "collections"],
  ["/r/kb/~hotspots", "collections"],
  ["/r/kb/~reviews", "review"],
  ["/r/kb/~reviews/12", "review"],
  ["/r/kb/~reviews/12/diff", "review"],
  ["/r/kb/~reviews/12/diff/src/lib.rs", "review"],
  ["/r/kb/~reviews/12/f/f-abc", "review"],
  ["/r/kb/~recipes", "collections"],
  ["/r/kb/~recipes/runs/r1", "collections"],
  ["/r/kb/~stacks", "collections"],
  ["/r/kb/~canvas", "collections"],
  ["/r/kb/~boards", "collections"],
  ["/r/kb/~boards/b1", "collections"],
  ["/r/kb/~tours", "collections"],
  ["/r/kb/~tours/t1", "collections"],
  ["/r/kb/~collections", "collections"],
  ["/r/kb/~browser", "code"],
  ["/r/kb/~lens/kb/abc", "code"],
  ["/r/kb", "code"],
  ["/r/kb/src/lib.rs", "code"],
  ["/r/kb/src/~story", "code"],
  ["/session/s1/diff", null],
];

describe("matchActiveNavItem — every app.tsx route maps to its door", () => {
  it.each(ROUTE_TABLE)("%s -> %s", (path, door) => {
    expect(matchActiveNavItem(path, "kb")?.key ?? null).toBe(door);
  });

  it("does not light a door for a different repo", () => {
    expect(matchActiveNavItem("/r/other/~todos", "kb")).toBeNull();
  });

  it("matches a repo whose name needs URL encoding", () => {
    expect(matchActiveNavItem("/r/a%20b/~todos", "a b")?.key).toBe("collections");
  });
});

describe("COLLECTION_ITEMS", () => {
  it("lists the twelve former Explore destinations plus Boards and Lanes once each", () => {
    expect(COLLECTION_ITEMS.map((i) => i.key)).toEqual([
      "sets",
      "stacks",
      "workspaces",
      "tours",
      "boards",
      "canvas",
      "rails",
      "lanes",
      "recipes",
      "todos",
      "hotspots",
      "comments",
    ]);
  });

  it("groups them Sets / Boards / Instruments, every item in a declared group", () => {
    const groups = COLLECTION_GROUPS.map((g) => g.group);
    expect(groups).toEqual(["sets", "boards", "instruments"]);
    for (const item of COLLECTION_ITEMS) expect(groups).toContain(item.group);
    expect(COLLECTION_ITEMS.filter((i) => i.group === "sets").map((i) => i.key)).toEqual([
      "sets",
      "stacks",
      "workspaces",
      "tours",
    ]);
  });

  it("every item lights the Collections door", () => {
    for (const item of COLLECTION_ITEMS) {
      expect(matchActiveNavItem(item.url("kb"), "kb")?.key).toBe("collections");
    }
  });
});

describe("seqCounts", () => {
  const rows = [
    { projection: "set", source: "reading_sets" },
    { projection: "set", source: "reading_sets" },
    { projection: "workspace", source: "reading_sets" },
    { projection: "tour", source: "reading_sets" },
    { projection: "tour", source: "canvas_boards" },
    { projection: "board", source: "canvas_boards" },
    { projection: "board", source: "canvas_boards" },
    { projection: "board", source: "canvas_boards" },
    { projection: "board", source: "canvas_sets" },
  ];

  it("counts per card, splitting the board projection by its table", () => {
    expect(seqCounts(rows)).toEqual({
      sets: 2,
      workspaces: 1,
      tours: 1,
      boards: 3,
      canvas: 1,
    });
  });

  it("gives a count-capable card with no rows a real 0, and a count-less card nothing", () => {
    const counts = seqCounts([]);
    expect(counts.sets).toBe(0);
    expect("stacks" in counts).toBe(false);
    expect("todos" in counts).toBe(false);
  });
});
