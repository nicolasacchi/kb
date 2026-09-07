// The TS half of `kbc-refs/1` beyond the shared golden (V73-K2b).
//
// `kbcRefs.golden.test.ts` walks the CROSS-LANGUAGE fixture (one body → one
// class). This file pins the two things that fixture cannot express: the
// DOCUMENT scanner (what a `[[…]]` span means depends on where it sits) and
// the front-matter body split. Every case here mirrors a named test in
// `review_doc/refs.rs` / `review_doc/frontmatter.rs`, so a divergence in the
// scanner fails on this side too rather than only in the card join.
import { describe, expect, it } from "vitest";
import { classify, docBody, parseRef, refsIn, scanRefs, SCHEMES } from "./kbcRefs";

describe("kbc-refs/1 parser", () => {
  it("claims every declared scheme — a scheme with no arm would be a silent wikilink", () => {
    for (const s of SCHEMES) {
      const probe = `${s}:x/1#1@abcd`;
      const got = classify(probe);
      expect(got.kind, `${s} is declared but disowned as a wikilink`).not.toBe("wikilink");
    }
  });

  it("never treats a bare wikilink as a kbc ref (root invariant #29)", () => {
    for (const body of ["Order", "Order|the order model", "docs/design", "a:b", ""]) {
      expect(parseRef(body), `${body} must stay a kb wikilink`).toBeNull();
      expect(classify(body).kind).toBe("wikilink");
    }
  });

  it("reports a known scheme that does not parse as malformed, never as a wikilink", () => {
    for (const body of ["code:", "gh:nope/1", "hunk:a.rb@2", "finding:F-7", "kb:solo"]) {
      const got = classify(body);
      expect(got.kind, `${body} should be malformed`).toBe("malformed");
    }
  });

  it("parses a code ref right to left, so an '@' in a path is still a path", () => {
    expect(parseRef("code:app/models/order.rb:120-134@a1b2c3d")).toEqual({
      scheme: "code",
      raw: "code:app/models/order.rb:120-134@a1b2c3d",
      path: "app/models/order.rb",
      line: 120,
      line_end: 134,
      sha: "a1b2c3d",
    });
    expect(parseRef("code:app/mail@er.rb")).toEqual({
      scheme: "code",
      raw: "code:app/mail@er.rb",
      path: "app/mail@er.rb",
    });
  });

  it("never splits a sym ref on '::' as a field boundary", () => {
    expect(parseRef("sym:kb_core::config::ServerSection")).toEqual({
      scheme: "sym",
      raw: "sym:kb_core::config::ServerSection",
      container: "kb_core::config",
      name: "ServerSection",
    });
    expect(parseRef("sym:Namespace::Class#method")).toEqual({
      scheme: "sym",
      raw: "sym:Namespace::Class#method",
      container: "Namespace::Class",
      name: "method",
    });
  });

  it("rejects a code line range that ends before it starts rather than swapping it", () => {
    const got = classify("code:app/x.rb:9-2");
    expect(got.kind).toBe("malformed");
    if (got.kind === "malformed") expect(got.reason).toContain("ends before it starts");
  });

  it("accepts both patchset spellings on a hunk ref", () => {
    expect(parseRef("hunk:a.rb@2#3")).toEqual({
      scheme: "hunk",
      raw: "hunk:a.rb@2#3",
      path: "a.rb",
      ps: 2,
      index: 3,
    });
    expect(parseRef("hunk:a.rb@ps2#0")).toEqual({
      scheme: "hunk",
      raw: "hunk:a.rb@ps2#0",
      path: "a.rb",
      ps: 2,
      index: 0,
    });
  });
});

describe("kbc-refs/1 scanner", () => {
  it("skips fences, inline code spans and a leading front-matter region", () => {
    const doc =
      "---\nsummary_md: see [[code:fm.rb:1]]\n---\n" +
      "prose [[code:a.rb:1]] and `[[code:span.rb:1]]`\n" +
      "```\n[[code:fenced.rb:1]]\n```\n" +
      "after [[ent:Order]]\n";
    expect(refsIn(doc).map((r) => r.raw)).toEqual(["code:a.rb:1", "ent:Order"]);
  });

  it("reports the line and column of each span", () => {
    const found = scanRefs("x\ny [[ent:Order]]\n");
    expect(found).toHaveLength(1);
    expect([found[0].line, found[0].col]).toEqual([2, 3]);
  });

  it("dedups by raw and keeps document order", () => {
    expect(refsIn("[[ent:B]] [[ent:A]] [[ent:B]]").map((r) => r.raw)).toEqual([
      "ent:B",
      "ent:A",
    ]);
  });

  it("keeps a malformed span visible to the scanner even though refsIn drops it", () => {
    const found = scanRefs("a [[code:]] b [[Order]]");
    expect(found.map((f) => f.class.kind)).toEqual(["malformed", "wikilink"]);
    expect(refsIn("a [[code:]] b [[Order]]")).toEqual([]);
  });

  it("leaves an unterminated '[[' alone", () => {
    expect(scanRefs("dangling [[ent:Order")).toEqual([]);
  });
});

describe("docBody", () => {
  it("returns everything after the front matter's closing delimiter", () => {
    expect(docBody("---\nsummary_md: hi\n---\n# Title\n\nbody\n")).toBe("# Title\n\nbody\n");
  });

  it("accepts '...' as a closing delimiter", () => {
    expect(docBody("---\na: 1\n...\nbody\n")).toBe("body\n");
  });

  it("returns a document with no front matter verbatim", () => {
    expect(docBody("# Title\n\nbody\n")).toBe("# Title\n\nbody\n");
  });

  it("returns an unclosed front matter verbatim rather than swallowing the document", () => {
    expect(docBody("---\na: 1\nno close here\n")).toBe("---\na: 1\nno close here\n");
  });
});
