// V70-A6 — the Ramp's goldens.
//
// Two kinds of check. The first is ordinary coverage of the two pure key/
// mouse tables — they mirror registry rows, so a divergence between what the
// `?` sheet promises and what a list actually does shows up here.
//
// The second is the ONE-HOME check: every result surface the deliverable
// names must IMPORT this module. That is a crude test and it is the right
// one — the failure it catches is a new surface (or a refactored old one)
// growing its own private `if (e.metaKey) window.open(...)`, which is exactly
// how "one commitment gradient on every result row" quietly becomes eleven
// slightly different gradients.
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { locationLabel, rungForKey, rungForMouse, targetPaneLoc, targetUrl } from "./ramp";
import { decode } from "./location";

describe("rungForKey mirrors the registry rows", () => {
  it("Enter is the focused pane", () => {
    expect(rungForKey({ key: "Enter" })).toBe("here");
  });
  it("Shift-Enter is the other pane (`ramp.open-pane2`)", () => {
    expect(rungForKey({ key: "Enter", shiftKey: true })).toBe("other");
  });
  it("Ctrl-Enter and Cmd-Enter are a new tab (`ramp.open-tab`)", () => {
    expect(rungForKey({ key: "Enter", ctrlKey: true })).toBe("tab");
    expect(rungForKey({ key: "Enter", metaKey: true })).toBe("tab");
  });
  it("`o` is a new tab and `O` is a new window", () => {
    expect(rungForKey({ key: "o" })).toBe("tab");
    expect(rungForKey({ key: "O" })).toBe("window");
  });
  it("`K` peeks (`peek.hover`, widened to global by this unit)", () => {
    expect(rungForKey({ key: "K" })).toBe("peek");
  });
  it("a modified `o` is the browser's, not ours", () => {
    // Ctrl-o is the jump list; Cmd-o is the OS file dialog. Neither is a Ramp
    // rung, and claiming them would break two things that already work.
    expect(rungForKey({ key: "o", ctrlKey: true })).toBeNull();
    expect(rungForKey({ key: "o", metaKey: true })).toBeNull();
  });
  it("Alt is never ours", () => {
    expect(rungForKey({ key: "Enter", altKey: true })).toBeNull();
  });
  it("anything else is nobody's", () => {
    expect(rungForKey({ key: "j" })).toBeNull();
    expect(rungForKey({ key: "Escape" })).toBeNull();
  });
});

describe("rungForMouse restores what a browser already means", () => {
  it("middle-click opens a tab", () => {
    expect(rungForMouse({ button: 1 })).toBe("tab");
  });
  it("Ctrl/Cmd-click opens a tab", () => {
    expect(rungForMouse({ button: 0, ctrlKey: true })).toBe("tab");
    expect(rungForMouse({ button: 0, metaKey: true })).toBe("tab");
  });
  it("Shift-click opens the other pane", () => {
    expect(rungForMouse({ button: 0, shiftKey: true })).toBe("other");
  });
  it("a plain left click opens here", () => {
    expect(rungForMouse({ button: 0 })).toBe("here");
  });
  it("right-click is the context menu's, never a navigation", () => {
    expect(rungForMouse({ button: 2 })).toBeNull();
  });
});

describe("target → URL goes through the one builder", () => {
  it("a line target", () => {
    expect(targetUrl({ repo: "kb", path: "src/a.rs", line: 12, via: "search" })).toBe(
      "/r/kb/src/a.rs?line=12",
    );
  });
  it("a ref-pinned target", () => {
    expect(targetUrl({ repo: "kb", path: "src/a.rs", ref: "abc", line: 3, via: "tree" })).toBe(
      "/r/kb/src/a.rs?ref=abc&line=3",
    );
  });
  it("a SYMBOL target keeps the line as the honest fallback anchor", () => {
    expect(
      targetUrl({ repo: "kb", path: "src/a.rs", line: 3, sym: "rust:Foo:bar", via: "definition_of" }),
    ).toBe("/r/kb/src/a.rs?line=3&sym=rust%3AFoo%3Abar");
  });
  it("a pane-2 location omits what it does not know", () => {
    expect(targetPaneLoc({ repo: "kb", path: "a.rs", via: "tree" })).toEqual({ path: "a.rs" });
    expect(targetPaneLoc({ repo: "kb", path: "a.rs", ref: "r", line: 9, via: "tree" })).toEqual({
      path: "a.rs",
      ref: "r",
      line: 9,
    });
  });
});

describe("locationLabel is what a trail step stores", () => {
  it("path:line inside the reader", () => {
    expect(locationLabel(decode("/r/kb/src/a.rs?line=88"))).toBe("src/a.rs:88");
  });
  it("bare path with no line", () => {
    expect(locationLabel(decode("/r/kb/src/a.rs"))).toBe("src/a.rs");
  });
  it("a page names itself rather than pretending to be a file", () => {
    expect(locationLabel(decode("/r/kb/~todos"))).toBe("todos");
    expect(locationLabel(decode("/search?q=x"))).toBe("search");
  });
});

// --- the one-home check --------------------------------------------------

const SURFACES = [
  "components/search/SearchSection.tsx",
  "components/peek/PeekPanel.tsx",
  "components/hierarchy/HierarchyPanel.tsx",
  "components/impact/ImpactPanel.tsx",
  "components/FileTree.tsx",
  "desk/Drawer.tsx",
  "components/RecentLocations.tsx",
  "components/reviews/ReviewThreadsCard.tsx",
];

describe("every result surface routes through the ONE Ramp", () => {
  for (const rel of SURFACES) {
    it(rel, () => {
      const src = readFileSync(fileURLToPath(new URL(`../${rel}`, import.meta.url)), "utf8");
      expect(src, `${rel} must import nav/ramp — see this file's module doc`).toMatch(
        /from "(\.\.\/)+nav\/ramp"/,
      );
    });
  }
});
