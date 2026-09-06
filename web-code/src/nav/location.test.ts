// V70-A6 — the Location Contract's goldens.
//
// Two things are pinned here and they are the whole contract:
//
//   1. **`encode` ∘ `decode` is the identity** for every URL shape the SPA
//      can produce — including the ones this contract deliberately does NOT
//      model, which round-trip through `mode: "other"` verbatim. That is what
//      makes `navigate()` safe as the single door: it can be handed any URL
//      a builder produced without a modelling gap silently rewriting it.
//   2. **The transition TABLE**, as a literal. §P7 asks for "one pure
//      `transition(from, to) -> push | replace | none` table, golden-pinned";
//      a table nobody can read is not one, so it is written out case by case
//      with the rule each case exercises named.
import { describe, expect, it } from "vitest";
import { decode, emptyLocation, encode, locationKey, transition, type Location } from "./location";

const ROUND_TRIP_URLS = [
  "/",
  "/~inbox",
  "/search",
  "/search?q=refund",
  "/r/kb",
  "/r/kb/src/main.rs",
  "/r/kb/src/main.rs?line=42",
  "/r/kb/src/main.rs?line=10-24",
  "/r/kb/src/main.rs?ref=abc123&line=42",
  "/r/kb/src/main.rs?line=42&pane2=src%2Flib.rs%40%3A7",
  "/r/kb/src/main.rs?line=42&sym=rust%3AFoo%3Abar",
  "/r/kb/src/main.rs/~diff?from=a&to=b",
  "/r/kb/src/main.rs/~story?at=deadbeef",
  "/r/kb/~branches",
  "/r/kb/~todos",
  "/r/kb/~hotspots",
  "/r/kb/~sets",
  "/r/kb/~prs",
  "/r/kb/~recipes",
  "/r/kb/~stacks",
  "/r/kb/~canvas",
  "/r/kb/~reviews",
  "/r/kb/~commit/deadbeef",
  "/r/kb/~compare?from=a&to=b&dots=3",
  "/r/kb/~reviews/7",
  "/r/kb/~reviews/7?tab=report",
  "/r/kb/~reviews/7?tab=report&ps=2",
  "/r/kb/~reviews/7/diff",
  "/r/kb/~reviews/7/diff/src/main.rs",
  "/r/kb/~reviews/7/diff/src/main.rs?finding=f-3",
  // Not modelled — must survive verbatim.
  "/session/abc/diff?repo=kb",
  "/~lens/notes/by-path/a/b.html",
  "/r/kb/~browser?symbol=foo",
  "/r/kb/~range-diff?old=a&new=b",
  "/r/kb/~lens/notes/doc1",
];

describe("encode ∘ decode is the identity", () => {
  for (const url of ROUND_TRIP_URLS) {
    it(url, () => {
      expect(encode(decode(url))).toBe(url);
    });
  }

  it("carries the trail triple through unchanged", () => {
    const url = "/r/kb/src/main.rs?line=42&trail=abc123&step=3&via=usage_of";
    expect(encode(decode(url))).toBe(url);
    expect(decode(url).trail).toEqual({ id: "abc123", step: 3, via: "usage_of" });
  });

  it("drops a HALF trail link rather than half-rendering it", () => {
    // `trail` with no usable `step` is not a link (`parseTrailLink`'s rule):
    // an origin chip pointing at nothing is worse than no chip.
    expect(decode("/r/kb/a.rs?trail=abc").trail).toBeUndefined();
    expect(decode("/r/kb/a.rs?trail=abc&step=-1").trail).toBeUndefined();
  });

  it("treats an unknown ~sentinel as `other`, never as a file path", () => {
    const loc = decode("/r/kb/~someFutureThing/x");
    expect(loc.mode).toBe("other");
    expect(loc.path).toBeUndefined();
    expect(encode(loc)).toBe("/r/kb/~someFutureThing/x");
  });

  it("`locationKey` ignores trail linkage — one hop, one place", () => {
    const a = decode("/r/kb/a.rs?line=4");
    const b = decode("/r/kb/a.rs?line=4&trail=x&step=0&via=search");
    expect(locationKey(a)).toBe(locationKey(b));
  });
});

const reader = (over: Partial<Location> = {}): Location => ({
  repo: "kb",
  mode: "reader",
  path: "src/main.rs",
  panes: { focused: 1 },
  ...over,
});

describe("the transition table (§P7)", () => {
  const CASES: Array<[string, Location | null, Location, "push" | "replace" | "none"]> = [
    ["no previous location at all", null, reader(), "push"],
    [
      "a different FILE is a new place",
      reader({ path: "a.rs" }),
      reader({ path: "b.rs" }),
      "push",
    ],
    [
      "a different SYMBOL is a new place",
      reader({ sym: "rust:Foo" }),
      reader({ sym: "rust:Bar" }),
      "push",
    ],
    [
      "a different REPO is a new place",
      reader({ repo: "kb" }),
      reader({ repo: "other" }),
      "push",
    ],
    [
      "a different CENTER MODE is a new place",
      reader(),
      { ...reader(), mode: "diff" as const },
      "push",
    ],
    [
      "a different PAGE is a new place",
      { repo: "kb", mode: "page", page: "todos", panes: { focused: 1 } },
      { repo: "kb", mode: "page", page: "branches", panes: { focused: 1 } },
      "push",
    ],
    [
      "a different REVIEW is a new place",
      { repo: "kb", mode: "review", review: { id: "1" }, panes: { focused: 1 } },
      { repo: "kb", mode: "review", review: { id: "2" }, panes: { focused: 1 } },
      "push",
    ],
    [
      "same file, moved cursor — a cursor move is not a navigation event",
      reader({ anchor: { line: 10 } }),
      reader({ anchor: { line: 88 } }),
      "replace",
    ],
    [
      "same file, a selection RANGE instead of a line",
      reader({ anchor: { line: 10 } }),
      reader({ anchor: { line: 10, range: { start: 10, end: 24 } } }),
      "replace",
    ],
    [
      "the review cockpit's tab is view state",
      { repo: "kb", mode: "review", review: { id: "1" }, panes: { focused: 1 } },
      { repo: "kb", mode: "review", review: { id: "1", tab: "report" }, panes: { focused: 1 } },
      "replace",
    ],
    [
      "the selected patchset is view state",
      { repo: "kb", mode: "review", review: { id: "1" }, panes: { focused: 1 } },
      { repo: "kb", mode: "review", review: { id: "1", ps: 3 }, panes: { focused: 1 } },
      "replace",
    ],
    ["the rail tab is view state", reader(), reader({ railTab: "history" }), "replace"],
    ["the drawer tab is view state", reader(), reader({ drawerTab: "usages" }), "replace"],
    ["pane FOCUS is view state", reader(), reader({ panes: { focused: 2 } }), "replace"],
    [
      "opening pane 2 refines the same place",
      reader(),
      reader({ panes: { focused: 1, pane2: { path: "b.rs" } } }),
      "replace",
    ],
    [
      "an overlay opening is not a navigation at all",
      reader(),
      reader({ overlays: ["peek"] }),
      "none",
    ],
    [
      "an overlay closing is not a navigation at all",
      reader({ overlays: ["peek"] }),
      reader(),
      "none",
    ],
    ["nothing changed", reader({ anchor: { line: 3 } }), reader({ anchor: { line: 3 } }), "none"],
    [
      "a trail-only difference is still the same place",
      reader({ anchor: { line: 3 } }),
      reader({ anchor: { line: 3 }, trail: { id: "t", step: 0 } }),
      "replace",
    ],
  ];

  for (const [name, from, to, want] of CASES) {
    it(`${name} → ${want}`, () => {
      expect(transition(from, to)).toBe(want);
    });
  }

  it("is total over the empty location", () => {
    expect(transition(emptyLocation("kb"), reader())).toBe("push");
  });
});
