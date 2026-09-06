// `ReviewDiff`'s pure helpers (V73-K2b).
//
// These six functions were unreachable from a unit test before the split —
// they lived inside a 2100-line route module whose import pulls in CodeMirror,
// react-router and forty hooks. Extracting them is most of the point of the
// refactor: the two that decide what the page ADDRESSES (`splatPath`,
// `orderedRows`) are now pinned by name.
import { describe, expect, it } from "vitest";
import type { ReviewFileRow, ReviewReadingStop } from "../../api/types";
import { cssAttr, msg, orderedRows, parseLine, parseSide, parseView, splatPath } from "./helpers";

function row(path: string): ReviewFileRow {
  return {
    path,
    old_path: null,
    status: "M",
    additions: 1,
    deletions: 0,
    blob_sha: "abc",
    viewed: false,
    viewed_stale: false,
    open_annotations: 0,
  };
}

describe("parseView / parseSide / parseLine", () => {
  it("accepts only the two documented layouts", () => {
    expect(parseView("unified")).toBe("unified");
    expect(parseView("split")).toBe("split");
    expect(parseView("sideways")).toBeNull();
    expect(parseView(null)).toBeNull();
  });

  it("accepts only the two documented sides", () => {
    expect(parseSide("old")).toBe("old");
    expect(parseSide("new")).toBe("new");
    expect(parseSide("both")).toBeNull();
  });

  it("takes a positive finite line and refuses anything else", () => {
    expect(parseLine("12")).toBe(12);
    expect(parseLine("0")).toBeNull();
    expect(parseLine("-3")).toBeNull();
    expect(parseLine("nope")).toBeNull();
    expect(parseLine("")).toBeNull();
    expect(parseLine(null)).toBeNull();
  });
});

describe("cssAttr", () => {
  it("escapes a value for use inside an attribute selector", () => {
    // jsdom is absent here (`environment: "node"`), so this exercises the
    // FALLBACK arm — which is the one that has to be right on an old engine.
    expect(cssAttr('a"b')).toContain('\\"');
    expect(cssAttr("a\\b")).toContain("\\\\");
  });
});

describe("orderedRows", () => {
  it("returns the files untouched when there is no reading order", () => {
    const files = [row("b.rs"), row("a.rs")];
    expect(orderedRows(files, null).map((f) => f.path)).toEqual(["b.rs", "a.rs"]);
    expect(orderedRows(files, []).map((f) => f.path)).toEqual(["b.rs", "a.rs"]);
  });

  it("puts the reading order first and keeps every unlisted file after it", () => {
    const files = [row("a.rs"), row("b.rs"), row("c.rs")];
    const stops = [{ path: "c.rs" }, { path: "a.rs" }] as ReviewReadingStop[];
    expect(orderedRows(files, stops).map((f) => f.path)).toEqual(["c.rs", "a.rs", "b.rs"]);
  });

  it("never drops or duplicates a file when the order names one twice or names a stranger", () => {
    const files = [row("a.rs"), row("b.rs")];
    const stops = [{ path: "b.rs" }, { path: "b.rs" }, { path: "gone.rs" }] as ReviewReadingStop[];
    expect(orderedRows(files, stops).map((f) => f.path)).toEqual(["b.rs", "a.rs"]);
  });
});

describe("splatPath", () => {
  it("decodes each segment on its own and rejoins them", () => {
    expect(splatPath("app/models/order.rb")).toBe("app/models/order.rb");
    expect(splatPath("app%2Fx/y%20z.rb")).toBe("app/x/y z.rb");
  });

  it("keeps a segment that is not valid percent-encoding verbatim rather than throwing", () => {
    expect(splatPath("a/%zz/b")).toBe("a/%zz/b");
  });

  it("drops empty segments and answers '' for an empty splat", () => {
    expect(splatPath("//a//b//")).toBe("a/b");
    expect(splatPath("")).toBe("");
  });
});

describe("msg", () => {
  it("prefers an Error's message and stringifies anything else", () => {
    expect(msg(new Error("boom"))).toBe("boom");
    expect(msg("boom")).toBe("boom");
    expect(msg(404)).toBe("404");
  });
});
