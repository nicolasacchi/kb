import { describe, expect, it, test } from "vitest";
import { paneSplitOpen, formatPane2, parsePane2, samePane, type PaneLoc } from "./paneUrl";

// The grammar is golden-pinned in BOTH directions: `formatPane2` byte-exact
// strings (so a future refactor can't quietly change the wire shape a shared
// permalink already carries) and `parsePane2` totality (malformed ⇒ null,
// never a partial object).

describe("formatPane2 — golden wire strings", () => {
  it("serialises kb + path with the ':' field separator", () => {
    expect(formatPane2({ kb: "canon", sourceRelative: "kitchen-sink.html" })).toBe(
      "canon:kitchen-sink.html",
    );
  });

  it("keeps nested path separators literal", () => {
    expect(formatPane2({ kb: "canon", sourceRelative: "pm/index.html" })).toBe(
      "canon:pm%2Findex.html",
    );
  });

  it("appends the optional ?sec= analogue as a third field", () => {
    expect(
      formatPane2({ kb: "canon", sourceRelative: "a.html", sec: "why-pin" }),
    ).toBe("canon:a.html:why-pin");
  });

  it("escapes a literal ':' inside every field so it can't forge a boundary", () => {
    expect(
      formatPane2({ kb: "a:b", sourceRelative: "c:d.html", sec: "e:f" }),
    ).toBe("a%3Ab:c%3Ad.html:e%3Af");
  });

  it("escapes spaces + query-significant chars", () => {
    expect(formatPane2({ kb: "my kb", sourceRelative: "a b&c?d.html" })).toBe(
      "my%20kb:a%20b%26c%3Fd.html",
    );
  });

  it("omits the sec field for an empty-string sec (falsy)", () => {
    expect(formatPane2({ kb: "k", sourceRelative: "x.html", sec: "" })).toBe(
      "k:x.html",
    );
  });
});

describe("parsePane2 — golden parses", () => {
  it("reads the two-field shape", () => {
    expect(parsePane2("canon:kitchen-sink.html")).toEqual({
      kb: "canon",
      sourceRelative: "kitchen-sink.html",
    });
  });

  it("decodes nested paths", () => {
    expect(parsePane2("canon:pm%2Findex.html")).toEqual({
      kb: "canon",
      sourceRelative: "pm/index.html",
    });
  });

  it("reads the three-field shape", () => {
    expect(parsePane2("canon:a.html:why-pin")).toEqual({
      kb: "canon",
      sourceRelative: "a.html",
      sec: "why-pin",
    });
  });

  it("decodes escaped separators back into field content", () => {
    expect(parsePane2("a%3Ab:c%3Ad.html:e%3Af")).toEqual({
      kb: "a:b",
      sourceRelative: "c:d.html",
      sec: "e:f",
    });
  });

  it("omits `sec` entirely (not an undefined-valued key) when absent", () => {
    const loc = parsePane2("k:x.html");
    expect(loc).not.toBeNull();
    expect(Object.keys(loc as PaneLoc).sort()).toEqual(["kb", "sourceRelative"]);
  });
});

describe("parsePane2 — TOTAL: malformed ⇒ null, never a partial", () => {
  const bad: [string, string | null | undefined][] = [
    ["null", null],
    ["undefined", undefined],
    ["empty string", ""],
    ["no separator at all", "canon"],
    ["four fields", "a:b:c:d"],
    ["empty kb", ":x.html"],
    ["empty path", "canon:"],
    ["empty sec (third field present but blank)", "canon:x.html:"],
    ["invalid percent-encoding in the kb", "%zz:x.html"],
    ["invalid percent-encoding in the path", "canon:%E0%A4%A"],
    ["invalid percent-encoding in the sec", "canon:x.html:%"],
    ["absolute path", "canon:%2Fetc%2Fpasswd"],
    ["traversal segment", "canon:..%2F..%2Fsecret.html"],
    ["traversal segment mid-path", "canon:a%2F..%2Fb.html"],
  ];
  test.each(bad)("%s ⇒ null", (_label, value) => {
    expect(parsePane2(value)).toBeNull();
  });

  it("a lone '..' filename that isn't a segment is fine", () => {
    expect(parsePane2("canon:a..b.html")).toEqual({
      kb: "canon",
      sourceRelative: "a..b.html",
    });
  });
});

describe("round-trip", () => {
  const locs: PaneLoc[] = [
    { kb: "canon", sourceRelative: "kitchen-sink.html" },
    { kb: "canon", sourceRelative: "pm/index.html" },
    { kb: "my kb", sourceRelative: "a b/c#d.html" },
    { kb: "a:b", sourceRelative: "c:d.html", sec: "e:f" },
    { kb: "canon", sourceRelative: "x.html", sec: "why-pin" },
  ];
  test.each(locs.map((l) => [formatPane2(l), l] as const))(
    "%s parses back to the same location",
    (_wire, loc) => {
      expect(parsePane2(formatPane2(loc))).toEqual(loc);
    },
  );

  it("format(parse(v)) === v for a canonical wire value", () => {
    const v = "canon:pm%2Findex.html:intro";
    expect(formatPane2(parsePane2(v) as PaneLoc)).toBe(v);
  });
});

describe("samePane", () => {
  it("is true for the same kb + path (sec is irrelevant)", () => {
    expect(
      samePane(
        { kb: "canon", sourceRelative: "a.html" },
        { kb: "canon", sourceRelative: "a.html", sec: "x" },
      ),
    ).toBe(true);
  });
  it("is false across kbs", () => {
    expect(
      samePane(
        { kb: "canon", sourceRelative: "a.html" },
        { kb: "other", sourceRelative: "a.html" },
      ),
    ).toBe(false);
  });
  it("is false across paths", () => {
    expect(
      samePane(
        { kb: "canon", sourceRelative: "a.html" },
        { kb: "canon", sourceRelative: "b.html" },
      ),
    ).toBe(false);
  });
});

describe("paneSplitOpen (OL TIME2)", () => {
  const a = { kb: "canon", sourceRelative: "a.html" };
  const b = { kb: "canon", sourceRelative: "b.html" };
  it("shows a split for a different artifact, never on mobile or with no pane2", () => {
    expect(paneSplitOpen(b, a, null, false)).toBe(true);
    expect(paneSplitOpen(b, a, null, true)).toBe(false);
    expect(paneSplitOpen(null, a, null, false)).toBe(false);
  });
  it("refuses a self-split in the ordinary reader", () => {
    expect(paneSplitOpen(a, a, null, false)).toBe(false);
  });
  it("allows the same artifact in both panes only while ?at= is set (then|now)", () => {
    expect(paneSplitOpen(a, a, 1_700_000_000, false)).toBe(true);
    // at=0 is a real (if ancient) instant, not "absent".
    expect(paneSplitOpen(a, a, 0, false)).toBe(true);
    expect(paneSplitOpen(a, a, 1_700_000_000, true)).toBe(false);
  });
});
