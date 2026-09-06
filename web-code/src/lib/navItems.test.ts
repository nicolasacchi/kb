import { describe, expect, it } from "vitest";
import {
  NAV_ITEMS,
  matchActiveNavItem,
  navItemMatches,
  navTestAttr,
} from "./navItems";
import { branchesUrl, reviewsUrl, todosUrl } from "./codeUrl";

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

describe("matchActiveNavItem", () => {
  it("returns the explore item for a ~todos location", () => {
    const hit = matchActiveNavItem("/r/kb/~todos", "kb");
    expect(hit?.key).toBe("todos");
    expect(hit?.group).toBe("explore");
    expect(navItemMatches("/r/kb/~todos", todosUrl("kb"))).toBe(true);
  });

  it("returns null off the catalog (reader / home)", () => {
    expect(matchActiveNavItem("/r/kb/src/lib.rs", "kb")).toBeNull();
    expect(matchActiveNavItem("/", "kb")).toBeNull();
  });
});

describe("NAV_ITEMS catalog", () => {
  it("covers every review + explore destination exactly once", () => {
    expect(NAV_ITEMS.filter((i) => i.group === "review").map((i) => i.key)).toEqual([
      "branches",
      "prs",
      "reviews",
    ]);
    expect(NAV_ITEMS.filter((i) => i.group === "explore").map((i) => i.key)).toEqual([
      "browser",
      "sets",
      // V70-A10 — workspaces ("Workspaces v0").
      "workspaces",
      "hotspots",
      "todos",
      // V72-J2 — comments/1's dashboard (the richer surface `todos` links to).
      "comments",
      // V72-I2 — `~rails` (`rails/1`).
      "rails",
      "recipes",
      "stacks",
      "canvas",
      // S2-A — unified inbox (design-s2.md §S2-A).
      "inbox",
    ]);
    expect(new Set(NAV_ITEMS.map((i) => i.key)).size).toBe(NAV_ITEMS.length);
  });

  it("keeps the historical data-kbc-topbar-* names", () => {
    for (const item of NAV_ITEMS) {
      expect(navTestAttr(item.key)).toBe(`data-kbc-topbar-${item.key}`);
    }
  });

  it("S2-A: inbox is repo-less — its url ignores whichever repo is active", () => {
    const inbox = NAV_ITEMS.find((i) => i.key === "inbox");
    expect(inbox?.url("acme/widgets")).toBe("/~inbox");
    expect(inbox?.url("other-repo")).toBe("/~inbox");
  });
});
