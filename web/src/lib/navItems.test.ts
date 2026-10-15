import { describe, expect, it } from "vitest";
import {
  LISTS_TABS,
  NAV_ITEMS,
  WORKLOG_TABS,
  matchActiveNavItem,
  withKb,
} from "./navItems";

// Every route in app.tsx, mapped to the door that must light for it (null =
// no door: chrome-level pages).
const ROUTES: [string, string | null][] = [
  ["/", "library"],
  ["/a/mem/pref-tabs.html", "library"],
  ["/search", "search"],
  ["/memory", "memory"],
  ["/sessions", "worklog"],
  ["/slates", "worklog"],
  ["/slates/kb", "worklog"],
  ["/replay/mem/abc", "worklog"],
  ["/lists", "lists"],
  ["/lists/mem/abc", "lists"],
  ["/board/mem/abc", "lists"],
  ["/notes", "lists"],
  ["/settings", null],
  ["/inbox", null],
  ["/anchors", null],
  ["/stale-anchors", null],
  ["/review-notes", null],
  ["/ambient", null],
  // prefix matching must be on path SEGMENTS
  ["/listsx", null],
  ["/sessionsfoo", null],
];

describe("NAV_ITEMS (OL-L2: five doors)", () => {
  it("is exactly Library, Search, Memory, Worklog, Lists", () => {
    expect(NAV_ITEMS.map((i) => i.label)).toEqual([
      "Library",
      "Search",
      "Memory",
      "Worklog",
      "Lists",
    ]);
  });

  it.each(ROUTES)("%s lights %s", (path, door) => {
    expect(matchActiveNavItem(path)?.id ?? null).toBe(door);
  });

  it("never lights two doors for one route", () => {
    for (const [path] of ROUTES) {
      expect(NAV_ITEMS.filter((i) => i.isActive(path)).length).toBeLessThanOrEqual(1);
    }
  });

  it("carries the active kb into every door; the Library drops view and filters", () => {
    const hrefs = Object.fromEntries(
      NAV_ITEMS.map((i) => [i.id, i.href({ kb: "mem" })]),
    );
    expect(hrefs).toEqual({
      library: "/?kb=mem",
      search: "/search?kb=mem",
      memory: "/memory?kb=mem",
      worklog: "/sessions?kb=mem",
      lists: "/lists?kb=mem",
    });
    expect(NAV_ITEMS.find((i) => i.id === "library")!.href({ kb: null })).toBe("/");
  });

  it("keeps the e2e hooks the old tabs carried", () => {
    const t = Object.fromEntries(NAV_ITEMS.map((i) => [i.id, i.testid]));
    expect(t.search).toBe("header-search-tab");
    expect(t.worklog).toBe("header-sessions-tab");
  });
});

describe("sub-nav rows", () => {
  it("Worklog row: Sessions (kb-scoped) and Slates (never kb-scoped), each lit on its own routes", () => {
    const [sessions, slates] = WORKLOG_TABS;
    expect(sessions.href({ kb: "mem" })).toBe("/sessions?kb=mem");
    expect(slates.href({ kb: "mem" })).toBe("/slates");
    expect(slates.testid).toBe("header-slates-tab");
    expect(sessions.isActive("/replay/mem/x")).toBe(true);
    expect(sessions.isActive("/slates")).toBe(false);
    expect(slates.isActive("/slates/p")).toBe(true);
  });

  it("Lists row: Lists and Notes, Notes keeps its testid", () => {
    const [lists, notes] = LISTS_TABS;
    expect(lists.href({ kb: null })).toBe("/lists");
    expect(notes.href({ kb: "mem" })).toBe("/notes?kb=mem");
    expect(notes.testid).toBe("header-notes-tab");
    expect(lists.isActive("/board/mem/x")).toBe(true);
    expect(lists.isActive("/notes")).toBe(false);
    expect(notes.isActive("/notes")).toBe(true);
  });

  it("withKb joins with & when the path already has a query", () => {
    expect(withKb("/x?a=1", "k b")).toBe("/x?a=1&kb=k%20b");
  });
});
