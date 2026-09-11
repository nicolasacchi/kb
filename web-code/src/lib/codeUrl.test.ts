import { describe, expect, it } from "vitest";
import {
  branchesUrl,
  browserPageUrl,
  buildSym,
  canvasPageUrl,
  codeUrl,
  commentsUrl,
  lanesUrl,
  commitUrl,
  compareUrl,
  findingUrl,
  formatLineParam,
  formatPane2,
  hotspotsUrl,
  langIdForPath,
  parseLineParam,
  parsePane2,
  parseReviewPs,
  parseReviewTab,
  parseDiffCtx,
  parseDiffMap,
  parseDiffPs,
  formatDiffPs,
  nextDiffCtx,
  DIFF_CTX_DIAL,
  permalinkFor,
  prsUrl,
  rangeDiffUrl,
  reviewDiffHref,
  reviewUrl,
  reviewsUrl,
  recipesPageUrl,
  stacksPageUrl,
  storyUrl,
  symbolPermalinkFor,
  symbolUrl,
  todosUrl,
  type CodeLoc,
  type CompareOpts,
  type LineSel,
  type PaneLoc,
  type RangeDiffOpts,
  type ReviewDiffHrefOpts,
  type ReviewUrlOpts,
  type SymGrammarInput,
  entityUrl,
  parseEntParam,
} from "./codeUrl";

// Golden table: CodeLoc → exact URL string. Mirrors the discipline of kb's
// own web/src/lib/galleryUrl.test.ts — one table, exact strings, no
// snapshot fuzziness. ~15+ cases covering every axis of the contract.
describe("codeUrl", () => {
  const cases: Array<[string, CodeLoc, string]> = [
    ["plain file, no query", { repo: "kb", path: "src/lib.rs" }, "/r/kb/src/lib.rs"],
    ["root path, no query", { repo: "kb", path: "" }, "/r/kb"],
    ["with ref", { repo: "kb", path: "src/lib.rs", ref: "main" }, "/r/kb/src/lib.rs?ref=main"],
    ["single line", { repo: "kb", path: "src/lib.rs", line: 42 }, "/r/kb/src/lib.rs?line=42"],
    [
      "line range",
      { repo: "kb", path: "src/lib.rs", line: { start: 10, end: 24 } },
      "/r/kb/src/lib.rs?line=10-24",
    ],
    [
      "line range + ref",
      { repo: "kb", path: "src/lib.rs", ref: "main", line: { start: 10, end: 24 } },
      "/r/kb/src/lib.rs?ref=main&line=10-24",
    ],
    [
      "swapped range normalizes to ascending",
      { repo: "kb", path: "src/lib.rs", line: { start: 24, end: 10 } },
      "/r/kb/src/lib.rs?line=10-24",
    ],
    ["root path + ref", { repo: "kb", path: "", ref: "main" }, "/r/kb?ref=main"],
    ["root path + line", { repo: "kb", path: "", line: 5 }, "/r/kb?line=5"],
    [
      "path with spaces + ref (percent-encoded like breadcrumbs.ts)",
      { repo: "my repo", path: "a b/c.rs", ref: "feature/x" },
      "/r/my%20repo/a%20b/c.rs?ref=feature%2Fx",
    ],
    [
      "path containing @",
      { repo: "kb", path: "src/user@host.rs" },
      "/r/kb/src/user%40host.rs",
    ],
    ["path containing unicode", { repo: "kb", path: "src/café.rs" }, "/r/kb/src/caf%C3%A9.rs"],
    [
      "pane2, minimal (no ref/line)",
      { repo: "kb", path: "a.rs", pane2: { path: "src/other.rs" } },
      "/r/kb/a.rs?pane2=src%2Fother.rs%40%3A",
    ],
    [
      "pane2 with ref only",
      { repo: "kb", path: "a.rs", pane2: { path: "src/other.rs", ref: "abc123" } },
      "/r/kb/a.rs?pane2=src%2Fother.rs%40abc123%3A",
    ],
    [
      "pane2 with line only",
      { repo: "kb", path: "a.rs", pane2: { path: "src/other.rs", line: 24 } },
      "/r/kb/a.rs?pane2=src%2Fother.rs%40%3A24",
    ],
    [
      "pane2 with ref + line range",
      {
        repo: "kb",
        path: "a.rs",
        pane2: { path: "src/other.rs", ref: "abc123", line: { start: 10, end: 24 } },
      },
      "/r/kb/a.rs?pane2=src%2Fother.rs%40abc123%3A10-24",
    ],
    [
      "full combo: ref + line range + pane2, param order ref,line,pane2",
      {
        repo: "kb",
        path: "a.rs",
        ref: "main",
        line: { start: 1, end: 3 },
        pane2: { path: "src/other.rs", ref: "dev", line: 7 },
      },
      "/r/kb/a.rs?ref=main&line=1-3&pane2=src%2Fother.rs%40dev%3A7",
    ],
  ];

  for (const [name, loc, expected] of cases) {
    it(name, () => {
      expect(codeUrl(loc)).toBe(expected);
    });
  }
});

describe("formatLineParam", () => {
  it("formats a single line", () => {
    expect(formatLineParam(10)).toBe("10");
  });

  it("formats a range", () => {
    expect(formatLineParam({ start: 10, end: 24 })).toBe("10-24");
  });

  it("collapses an equal-bound range to a plain number", () => {
    expect(formatLineParam({ start: 5, end: 5 })).toBe("5");
  });

  it("reorders a descending range", () => {
    expect(formatLineParam({ start: 24, end: 10 })).toBe("10-24");
  });

  it("returns empty string for non-positive/non-finite input", () => {
    expect(formatLineParam(0)).toBe("");
    expect(formatLineParam(-1)).toBe("");
    expect(formatLineParam(Number.NaN)).toBe("");
    expect(formatLineParam({ start: 0, end: 5 })).toBe("");
    expect(formatLineParam({ start: 5, end: -1 })).toBe("");
  });
});

describe("parseLineParam", () => {
  it("parses a single line", () => {
    expect(parseLineParam("10")).toEqual({ start: 10, end: 10 });
  });

  it("parses a range", () => {
    expect(parseLineParam("10-24")).toEqual({ start: 10, end: 24 });
  });

  it("swaps a descending range", () => {
    expect(parseLineParam("24-10")).toEqual({ start: 10, end: 24 });
  });

  it("returns null on junk/zero/negative/NaN/missing", () => {
    expect(parseLineParam(null)).toBeNull();
    expect(parseLineParam("")).toBeNull();
    expect(parseLineParam("0")).toBeNull();
    expect(parseLineParam("-5")).toBeNull();
    expect(parseLineParam("abc")).toBeNull();
    expect(parseLineParam("10-")).toBeNull();
    expect(parseLineParam("-10-20")).toBeNull();
    expect(parseLineParam("NaN")).toBeNull();
  });

  it("accepts a leading-zero line number", () => {
    expect(parseLineParam("007")).toEqual({ start: 7, end: 7 });
  });
});

describe("line param round-trips", () => {
  const cases: Array<[LineSel, { start: number; end: number }]> = [
    [10, { start: 10, end: 10 }],
    [{ start: 10, end: 24 }, { start: 10, end: 24 }],
    [{ start: 24, end: 10 }, { start: 10, end: 24 }],
    [{ start: 5, end: 5 }, { start: 5, end: 5 }],
  ];

  for (const [sel, normalized] of cases) {
    it(`round-trips ${JSON.stringify(sel)}`, () => {
      expect(parseLineParam(formatLineParam(sel))).toEqual(normalized);
    });
  }
});

describe("formatPane2", () => {
  it("formats path only", () => {
    expect(formatPane2({ path: "src/other.rs" })).toBe("src/other.rs@:");
  });

  it("formats path + ref", () => {
    expect(formatPane2({ path: "src/other.rs", ref: "abc123" })).toBe("src/other.rs@abc123:");
  });

  it("formats path + line", () => {
    expect(formatPane2({ path: "src/other.rs", line: 24 })).toBe("src/other.rs@:24");
  });

  it("formats path + ref + line range", () => {
    expect(formatPane2({ path: "src/other.rs", ref: "abc123", line: { start: 10, end: 24 } })).toBe(
      "src/other.rs@abc123:10-24",
    );
  });
});

describe("parsePane2", () => {
  it("parses path only", () => {
    expect(parsePane2("src/other.rs@:")).toEqual({ path: "src/other.rs" });
  });

  it("parses path + ref", () => {
    expect(parsePane2("src/other.rs@abc123:")).toEqual({ path: "src/other.rs", ref: "abc123" });
  });

  it("parses path + line", () => {
    expect(parsePane2("src/other.rs@:24")).toEqual({
      path: "src/other.rs",
      line: { start: 24, end: 24 },
    });
  });

  it("parses path + ref + line range", () => {
    expect(parsePane2("src/other.rs@abc123:10-24")).toEqual({
      path: "src/other.rs",
      ref: "abc123",
      line: { start: 10, end: 24 },
    });
  });

  it("returns null for null/empty/malformed input", () => {
    expect(parsePane2(null)).toBeNull();
    expect(parsePane2("")).toBeNull();
    expect(parsePane2("no-at-sign:10")).toBeNull();
    expect(parsePane2("path@ref-no-colon")).toBeNull();
    expect(parsePane2("@:")).toBeNull();
    expect(parsePane2("path@ref:notanumber")).toBeNull();
  });

  it("splits on the LAST @ when the path itself contains one", () => {
    expect(parsePane2("a@b.rs@:")).toEqual({ path: "a@b.rs" });
    expect(parsePane2("a@b.rs@abc123:10-24")).toEqual({
      path: "a@b.rs",
      ref: "abc123",
      line: { start: 10, end: 24 },
    });
  });
});

describe("pane2 round-trips", () => {
  const cases: PaneLoc[] = [
    { path: "src/other.rs" },
    { path: "src/other.rs", ref: "abc123" },
    { path: "src/other.rs", line: 24 },
    { path: "src/other.rs", ref: "abc123", line: { start: 10, end: 24 } },
    { path: "a@b.rs", ref: "abc123", line: { start: 2, end: 9 } },
  ];

  for (const p of cases) {
    it(`round-trips ${JSON.stringify(p)}`, () => {
      const parsed = parsePane2(formatPane2(p));
      const normalizedLine =
        p.line === undefined
          ? undefined
          : typeof p.line === "number"
            ? { start: p.line, end: p.line }
            : { start: Math.min(p.line.start, p.line.end), end: Math.max(p.line.start, p.line.end) };
      expect(parsed).toEqual({
        path: p.path,
        ...(p.ref !== undefined ? { ref: p.ref } : {}),
        ...(normalizedLine !== undefined ? { line: normalizedLine } : {}),
      });
    });
  }
});

// Phase C-SPA — golden table for the time-first-class URL builders, same
// discipline as `codeUrl`'s own table above.
describe("commitUrl", () => {
  const cases: Array<[string, [string, string], string]> = [
    ["plain sha", ["kb", "abc123"], "/r/kb/~commit/abc123"],
    ["full 40-hex sha", ["kb", "a".repeat(40)], `/r/kb/~commit/${"a".repeat(40)}`],
    ["repo needing encoding", ["my repo", "abc123"], "/r/my%20repo/~commit/abc123"],
  ];
  for (const [name, [repo, sha], expected] of cases) {
    it(name, () => {
      expect(commitUrl(repo, sha)).toBe(expected);
    });
  }
});

describe("compareUrl", () => {
  const cases: Array<[string, string, CompareOpts, string]> = [
    ["two-dot", "kb", { from: "main", to: "feature" }, "/r/kb/~compare?from=main&to=feature"],
    [
      "three-dot",
      "kb",
      { from: "main", to: "feature", threeDot: true },
      "/r/kb/~compare?from=main&to=feature&dots=3",
    ],
    [
      "threeDot: false is the same as omitted",
      "kb",
      { from: "main", to: "feature", threeDot: false },
      "/r/kb/~compare?from=main&to=feature",
    ],
    [
      "refs containing slashes are query-encoded",
      "kb",
      { from: "feature/x", to: "release/1.0" },
      "/r/kb/~compare?from=feature%2Fx&to=release%2F1.0",
    ],
    [
      "repo needing encoding",
      "my repo",
      { from: "main", to: "feature" },
      "/r/my%20repo/~compare?from=main&to=feature",
    ],
  ];
  for (const [name, repo, opts, expected] of cases) {
    it(name, () => {
      expect(compareUrl(repo, opts)).toBe(expected);
    });
  }
});

describe("branchesUrl", () => {
  it("builds the repo-scoped branches page URL", () => {
    expect(branchesUrl("kb")).toBe("/r/kb/~branches");
  });

  it("encodes a repo name needing it", () => {
    expect(branchesUrl("my repo")).toBe("/r/my%20repo/~branches");
  });
});

describe("todosUrl", () => {
  it("builds the repo-scoped TODOs page URL", () => {
    expect(todosUrl("kb")).toBe("/r/kb/~todos");
  });

  it("encodes a repo name needing it", () => {
    expect(todosUrl("my repo")).toBe("/r/my%20repo/~todos");
  });
});

describe("commentsUrl", () => {
  it("builds the repo-scoped comments/1 dashboard URL", () => {
    expect(commentsUrl("kb")).toBe("/r/kb/~comments");
  });

  it("encodes a repo name needing it", () => {
    expect(commentsUrl("my repo")).toBe("/r/my%20repo/~comments");
  });
});

describe("lanesUrl", () => {
  it("builds the repo-scoped aug-lane/1 dock URL", () => {
    expect(lanesUrl("kb")).toBe("/r/kb/~lanes");
  });

  it("encodes a repo name needing it", () => {
    expect(lanesUrl("my repo")).toBe("/r/my%20repo/~lanes");
  });
});

describe("hotspotsUrl", () => {
  it("builds the repo-scoped hotspots page URL", () => {
    expect(hotspotsUrl("kb")).toBe("/r/kb/~hotspots");
  });

  it("encodes a repo name needing it", () => {
    expect(hotspotsUrl("my repo")).toBe("/r/my%20repo/~hotspots");
  });
});

describe("reviewsUrl / reviewUrl", () => {
  it("builds the repo-scoped reviews list URL", () => {
    expect(reviewsUrl("kb")).toBe("/r/kb/~reviews");
  });

  it("builds a review detail URL", () => {
    expect(reviewUrl("kb", 42)).toBe("/r/kb/~reviews/42");
  });

  it("encodes a repo name needing it", () => {
    expect(reviewsUrl("my repo")).toBe("/r/my%20repo/~reviews");
    expect(reviewUrl("my repo", 7)).toBe("/r/my%20repo/~reviews/7");
  });
});

// V70-A3S — cockpit tab + selected patchset, threaded onto `reviewUrl` as
// `?tab=`/`?ps=` (ReviewDetail.tsx's own local state, promoted to URL
// state). Golden table, same discipline as every other builder above.
describe("reviewUrl opts (?tab=/?ps=)", () => {
  const cases: Array<[string, ReviewUrlOpts | undefined, string]> = [
    ["opts undefined is a byte-identical no-op", undefined, "/r/kb/~reviews/42"],
    ["empty opts object is a no-op", {}, "/r/kb/~reviews/42"],
    ["tab at its default 'files' is omitted", { tab: "files" }, "/r/kb/~reviews/42"],
    ["ps at its default 'latest' is omitted", { ps: "latest" }, "/r/kb/~reviews/42"],
    ["tab='files' + ps='latest' together is still a no-op", { tab: "files", ps: "latest" }, "/r/kb/~reviews/42"],
    ["tab=map only", { tab: "map" }, "/r/kb/~reviews/42?tab=map"],
    ["tab=report only", { tab: "report" }, "/r/kb/~reviews/42?tab=report"],
    ["tab=order only", { tab: "order" }, "/r/kb/~reviews/42?tab=order"],
    ["tab=timeline only", { tab: "timeline" }, "/r/kb/~reviews/42?tab=timeline"],
    ["ps=3 only", { ps: 3 }, "/r/kb/~reviews/42?ps=3"],
    ["tab then ps, param order tab first", { tab: "map", ps: 3 }, "/r/kb/~reviews/42?tab=map&ps=3"],
  ];
  for (const [name, opts, expected] of cases) {
    it(name, () => {
      expect(reviewUrl("kb", 42, opts)).toBe(expected);
    });
  }

  it("encodes a repo name needing it, opts appended after the base path", () => {
    expect(reviewUrl("my repo", 7, { tab: "map", ps: 3 })).toBe("/r/my%20repo/~reviews/7?tab=map&ps=3");
  });
});

describe("parseReviewTab", () => {
  it("parses every known tab", () => {
    expect(parseReviewTab("report")).toBe("report");
    expect(parseReviewTab("files")).toBe("files");
    expect(parseReviewTab("map")).toBe("map");
    expect(parseReviewTab("order")).toBe("order");
    expect(parseReviewTab("timeline")).toBe("timeline");
  });

  it("returns null for absent, empty, or unrecognized values", () => {
    expect(parseReviewTab(null)).toBeNull();
    expect(parseReviewTab("")).toBeNull();
    expect(parseReviewTab("bogus")).toBeNull();
  });
});

describe("parseReviewPs", () => {
  it("parses a positive integer", () => {
    expect(parseReviewPs("3")).toBe(3);
    expect(parseReviewPs("1")).toBe(1);
  });

  it("returns null for absent, 'latest', zero, negative, non-integer, or junk", () => {
    expect(parseReviewPs(null)).toBeNull();
    expect(parseReviewPs("latest")).toBeNull();
    expect(parseReviewPs("0")).toBeNull();
    expect(parseReviewPs("-1")).toBeNull();
    expect(parseReviewPs("1.5")).toBeNull();
    expect(parseReviewPs("abc")).toBeNull();
    expect(parseReviewPs("")).toBeNull();
  });
});

describe("storyUrl", () => {
  it("builds the file-scoped story URL with no ?at=", () => {
    expect(storyUrl("kb", "src/lib.rs")).toBe("/r/kb/src/lib.rs/~story");
  });

  it("appends ?at= when a sha is given", () => {
    expect(storyUrl("kb", "src/lib.rs", "abc123")).toBe("/r/kb/src/lib.rs/~story?at=abc123");
  });

  it("encodes repo/path like codeUrl", () => {
    expect(storyUrl("my repo", "a b/c.rs")).toBe("/r/my%20repo/a%20b/c.rs/~story");
  });
});

// Phase G-server — golden table for the review-workflow URL builders, same
// discipline as `compareUrl`'s own table above.
describe("rangeDiffUrl", () => {
  const cases: Array<[string, string, Partial<RangeDiffOpts> | undefined, string]> = [
    ["both omitted (blank-input landing)", "kb", undefined, "/r/kb/~range-diff"],
    ["empty opts object", "kb", {}, "/r/kb/~range-diff"],
    [
      "both given",
      "kb",
      { old: "main..topic@{1}", new: "main..topic" },
      "/r/kb/~range-diff?old=main..topic%40%7B1%7D&new=main..topic",
    ],
    ["old only", "kb", { old: "main..topic@{1}" }, "/r/kb/~range-diff?old=main..topic%40%7B1%7D"],
    ["new only", "kb", { new: "main..topic" }, "/r/kb/~range-diff?new=main..topic"],
    [
      "repo needing encoding",
      "my repo",
      { old: "a", new: "b" },
      "/r/my%20repo/~range-diff?old=a&new=b",
    ],
  ];
  for (const [name, repo, opts, expected] of cases) {
    it(name, () => {
      expect(rangeDiffUrl(repo, opts)).toBe(expected);
    });
  }
});

describe("prsUrl", () => {
  it("builds the repo-scoped PR overlay URL", () => {
    expect(prsUrl("kb")).toBe("/r/kb/~prs");
  });

  it("encodes a repo name needing it", () => {
    expect(prsUrl("my repo")).toBe("/r/my%20repo/~prs");
  });
});

describe("recipesPageUrl / stacksPageUrl (V3.3-U1)", () => {
  it("builds recipes sentinel", () => {
    expect(recipesPageUrl("kb")).toBe("/r/kb/~recipes");
  });
  it("builds stacks sentinel", () => {
    expect(stacksPageUrl("kb")).toBe("/r/kb/~stacks");
  });
  it("encodes repo names", () => {
    expect(recipesPageUrl("my repo")).toBe("/r/my%20repo/~recipes");
    expect(stacksPageUrl("my repo")).toBe("/r/my%20repo/~stacks");
  });
});

describe("canvasPageUrl (V3.4-C2)", () => {
  it("builds canvas sentinel", () => {
    expect(canvasPageUrl("kb")).toBe("/r/kb/~canvas");
  });
  it("encodes repo names", () => {
    expect(canvasPageUrl("my repo")).toBe("/r/my%20repo/~canvas");
  });
});

describe("browserPageUrl (V3.4-C3)", () => {
  it("builds browser sentinel", () => {
    expect(browserPageUrl("kb")).toBe("/r/kb/~browser");
  });
  it("encodes repo names", () => {
    expect(browserPageUrl("my repo")).toBe("/r/my%20repo/~browser");
  });
});

// --- PRR-U3 — full-page diff findings overlay -----------------------------

describe("reviewDiffHref", () => {
  const cases: Array<[string, [string, number | string, string?, ReviewDiffHrefOpts?], string]> = [
    ["review-level, no file", ["kb", 7], "/r/kb/~reviews/7/diff"],
    ["with file", ["kb", 7, "src/lib.rs"], "/r/kb/~reviews/7/diff/src/lib.rs"],
    [
      "file with spaces + segments are individually encoded",
      ["kb", 7, "a b/c.rs"],
      "/r/kb/~reviews/7/diff/a%20b/c.rs",
    ],
    ["repo needing encoding", ["my repo", 7], "/r/my%20repo/~reviews/7/diff"],
    [
      "id as a string",
      ["kb", "7"],
      "/r/kb/~reviews/7/diff",
    ],
    [
      "opts undefined is a byte-identical no-op",
      ["kb", 7, "src/lib.rs", undefined],
      "/r/kb/~reviews/7/diff/src/lib.rs",
    ],
    [
      "opts empty object is a no-op",
      ["kb", 7, "src/lib.rs", {}],
      "/r/kb/~reviews/7/diff/src/lib.rs",
    ],
    [
      "finding= only",
      ["kb", 7, "src/lib.rs", { finding: "f-dedup-race" }],
      "/r/kb/~reviews/7/diff/src/lib.rs?finding=f-dedup-race",
    ],
    [
      "overlay= only",
      ["kb", 7, "src/lib.rs", { overlay: "findings" }],
      "/r/kb/~reviews/7/diff/src/lib.rs?overlay=findings",
    ],
    [
      "overlay 'all' is omitted (the default)",
      ["kb", 7, "src/lib.rs", { overlay: "all" }],
      "/r/kb/~reviews/7/diff/src/lib.rs",
    ],
    [
      // PRR-U9 — the "diagnostics" overlay lane (design-addendum-2.md §D).
      "overlay= diagnostics",
      ["kb", 7, "src/lib.rs", { overlay: "diagnostics" }],
      "/r/kb/~reviews/7/diff/src/lib.rs?overlay=diagnostics",
    ],
    [
      "finding= + overlay=, param order finding then overlay",
      ["kb", 7, "src/lib.rs", { finding: "f-x", overlay: "comments" }],
      "/r/kb/~reviews/7/diff/src/lib.rs?finding=f-x&overlay=comments",
    ],
    [
      "finding= with no file (review-level landing)",
      ["kb", 7, undefined, { finding: "f-x" }],
      "/r/kb/~reviews/7/diff?finding=f-x",
    ],
    [
      "finding slug is query-encoded",
      ["kb", 7, "a.rs", { finding: "f-a b" }],
      "/r/kb/~reviews/7/diff/a.rs?finding=f-a%20b",
    ],
    // --- V73-K2a — diff v2's six params, appended LAST ------------------
    //
    // Every row above is unchanged, which is the point: `opts` grew and no
    // existing URL moved a byte.
    ["ps= a single patchset", ["kb", 7, undefined, { ps: 3 }], "/r/kb/~reviews/7/diff?ps=3"],
    [
      "ps= an interdiff RANGE",
      ["kb", 7, undefined, { ps: { from: 2, to: 5 } }],
      "/r/kb/~reviews/7/diff?ps=2..5",
    ],
    ["ctx= 10", ["kb", 7, undefined, { ctx: 10 }], "/r/kb/~reviews/7/diff?ctx=10"],
    ["ctx= full", ["kb", 7, undefined, { ctx: "full" }], "/r/kb/~reviews/7/diff?ctx=full"],
    ["ctx 3 is omitted (the default)", ["kb", 7, undefined, { ctx: 3 }], "/r/kb/~reviews/7/diff"],
    [
      "noise= collapsed",
      ["kb", 7, undefined, { noise: "collapsed" }],
      "/r/kb/~reviews/7/diff?noise=collapsed",
    ],
    [
      "noise 'shown' is omitted (the default)",
      ["kb", 7, undefined, { noise: "shown" }],
      "/r/kb/~reviews/7/diff",
    ],
    ["map=0 when hidden", ["kb", 7, undefined, { map: false }], "/r/kb/~reviews/7/diff?map=0"],
    ["map true is omitted (the default)", ["kb", 7, undefined, { map: true }], "/r/kb/~reviews/7/diff"],
    [
      "file= is path-encoded as ONE query value",
      ["kb", 7, undefined, { file: "a b/c.rs" }],
      "/r/kb/~reviews/7/diff?file=a%20b%2Fc.rs",
    ],
    [
      "hunk= carries the content address",
      ["kb", 7, undefined, { hunk: "3ae1e11fec5881f1" }],
      "/r/kb/~reviews/7/diff?hunk=3ae1e11fec5881f1",
    ],
    [
      "param ORDER is fixed: finding, overlay, ps, ctx, noise, map, file, hunk",
      [
        "kb",
        7,
        "a.rs",
        {
          finding: "f-x",
          overlay: "findings",
          ps: { from: 1, to: 2 },
          ctx: "full",
          noise: "collapsed",
          map: false,
          file: "a.rs",
          hunk: "deadbeefdeadbeef",
        },
      ],
      "/r/kb/~reviews/7/diff/a.rs?finding=f-x&overlay=findings&ps=1..2&ctx=full&noise=collapsed&map=0&file=a.rs&hunk=deadbeefdeadbeef",
    ],
  ];
  for (const [name, args, expected] of cases) {
    it(name, () => {
      expect(reviewDiffHref(...args)).toBe(expected);
    });
  }
});

// --- V73-K2a — the diff v2 param PARSERS ---------------------------------
//
// Every one is TOTAL: junk degrades to the documented default and never
// throws, so a hand-edited or stale URL always renders a view.

describe("parseDiffPs", () => {
  it("parses a bare patchset number", () => {
    expect(parseDiffPs("3")).toBe(3);
  });

  it("parses an interdiff range", () => {
    expect(parseDiffPs("2..5")).toEqual({ from: 2, to: 5 });
  });

  const nulls = [null, "", "latest", "0", "-1", "1.5", "abc", "..", "3..", "..3", "a..b"];
  for (const raw of nulls) {
    it(`${JSON.stringify(raw)} → null (read as "latest")`, () => {
      expect(parseDiffPs(raw)).toBeNull();
    });
  }

  it("REFUSES an inverted or degenerate range rather than swapping it", () => {
    // Guessing which end the operator meant is the quiet repair this
    // codebase refuses; `null` means "latest", which is honest.
    expect(parseDiffPs("5..2")).toBeNull();
    expect(parseDiffPs("3..3")).toBeNull();
  });

  it("round-trips through formatDiffPs", () => {
    expect(formatDiffPs(3)).toBe("3");
    expect(formatDiffPs({ from: 2, to: 5 })).toBe("2..5");
    expect(parseDiffPs(formatDiffPs({ from: 2, to: 5 }))).toEqual({ from: 2, to: 5 });
  });
});

describe("parseDiffCtx", () => {
  it("knows exactly three stops", () => {
    expect(parseDiffCtx("3")).toBe(3);
    expect(parseDiffCtx("10")).toBe(10);
    expect(parseDiffCtx("full")).toBe("full");
  });

  it("defaults to 3 for anything else", () => {
    for (const raw of [null, "", "0", "7", "whole", "FULL"]) {
      expect(parseDiffCtx(raw)).toBe(3);
    }
  });

  it("cycles 3 → 10 → full → 3", () => {
    expect(DIFF_CTX_DIAL).toEqual([3, 10, "full"]);
    expect(nextDiffCtx(3)).toBe(10);
    expect(nextDiffCtx(10)).toBe("full");
    expect(nextDiffCtx("full")).toBe(3);
  });
});

describe("parseDiffMap", () => {
  it("is shown unless the URL says 0", () => {
    expect(parseDiffMap(null)).toBe(true);
    expect(parseDiffMap("1")).toBe(true);
    expect(parseDiffMap("")).toBe(true);
    expect(parseDiffMap("0")).toBe(false);
  });
});

describe("findingUrl", () => {
  it("builds the short share/CLI-printable form", () => {
    expect(findingUrl("kb", 7, "f-dedup-race")).toBe("/r/kb/~reviews/7/f/f-dedup-race");
  });

  it("encodes repo + slug", () => {
    expect(findingUrl("my repo", 7, "f-a b")).toBe("/r/my%20repo/~reviews/7/f/f-a%20b");
  });

  it("accepts a string id", () => {
    expect(findingUrl("kb", "7", "f-x")).toBe("/r/kb/~reviews/7/f/f-x");
  });
});

describe("permalinkFor", () => {
  it("joins origin + codeUrl", () => {
    expect(permalinkFor("https://kbc.example.com", { repo: "kb", path: "src/lib.rs", line: 42 })).toBe(
      "https://kbc.example.com/r/kb/src/lib.rs?line=42",
    );
  });

  it("trims a trailing slash on origin", () => {
    expect(permalinkFor("https://kbc.example.com/", { repo: "kb", path: "src/lib.rs" })).toBe(
      "https://kbc.example.com/r/kb/src/lib.rs",
    );
  });
});

// --- T1 — `?sym=` deep links -----------------------------------------------

describe("buildSym", () => {
  const cases: Array<[string, SymGrammarInput, string]> = [
    [
      "no container — 2-field short form",
      { namespace: "rust", name: "widget" },
      "rust:widget",
    ],
    [
      "null container — same short form",
      { namespace: "rust", name: "widget", container: null },
      "rust:widget",
    ],
    [
      "container, no kind — 3-field form",
      { namespace: "ruby", name: "create", container: "UsersController" },
      "ruby:UsersController:create",
    ],
    [
      "container + kind — 4-field form",
      { namespace: "rust", name: "ServerSection", container: "kb_core::config", kind: "struct" },
      "rust:kb_core::config:ServerSection:struct",
    ],
    [
      "kind ignored when container absent (no slot for it in the short form)",
      { namespace: "rust", name: "widget", kind: "fn" },
      "rust:widget",
    ],
    [
      "rails namespace, construct-as-container form",
      { namespace: "rails", name: "users#create", container: "route" },
      "rails:route:users#create",
    ],
  ];
  for (const [name, input, expected] of cases) {
    it(name, () => {
      expect(buildSym(input)).toBe(expected);
    });
  }
});

describe("langIdForPath", () => {
  const cases: Array<[string, string | null]> = [
    ["a.rs", "rust"],
    ["a.py", "python"],
    ["a.rb", "ruby"],
    ["a.ts", "typescript"],
    ["a.mts", "typescript"],
    ["a.cts", "typescript"],
    ["a.tsx", "tsx"],
    ["a.js", "javascript"],
    ["a.jsx", "javascript"],
    ["a.mjs", "javascript"],
    ["a.cjs", "javascript"],
    ["a.sh", "bash"],
    ["a.bash", "bash"],
    ["a.yml", "yaml"],
    ["a.yaml", "yaml"],
    ["a.go", "go"],
    ["a.toml", "toml"],
    ["a.json", "json"],
    ["app/views/x/show.html.erb", "erb"],
    ["a.RS", "rust"], // case-insensitive extension match
    ["a.txt", null],
    ["Makefile", null], // no extension at all
    ["a.", null], // trailing dot, nothing after it
  ];
  for (const [path, expected] of cases) {
    it(`detects ${JSON.stringify(path)} → ${expected}`, () => {
      expect(langIdForPath(path)).toBe(expected);
    });
  }
});

describe("symbolUrl", () => {
  const cases: Array<[string, [string, string, Parameters<typeof symbolUrl>[2]], string]> = [
    [
      "fallback path only",
      ["kb", "rust:widget", { fallbackPath: "src/lib.rs" }],
      "/r/kb/src/lib.rs?sym=rust%3Awidget",
    ],
    [
      "fallback path + line",
      ["kb", "rust:widget", { fallbackPath: "src/lib.rs", fallbackLine: 42 }],
      "/r/kb/src/lib.rs?line=42&sym=rust%3Awidget",
    ],
    [
      "fallback path + line range",
      ["kb", "ruby:UsersController:create", { fallbackPath: "app.rb", fallbackLine: { start: 10, end: 24 } }],
      "/r/kb/app.rb?line=10-24&sym=ruby%3AUsersController%3Acreate",
    ],
    [
      "repo needing encoding",
      ["my repo", "rust:widget", { fallbackPath: "a.rs" }],
      "/r/my%20repo/a.rs?sym=rust%3Awidget",
    ],
  ];
  for (const [name, args, expected] of cases) {
    it(name, () => {
      expect(symbolUrl(...args)).toBe(expected);
    });
  }
});

describe("symbolPermalinkFor", () => {
  it("joins origin + symbolUrl", () => {
    expect(
      symbolPermalinkFor("https://kbc.example.com", "kb", "rust:widget", { fallbackPath: "src/lib.rs" }),
    ).toBe("https://kbc.example.com/r/kb/src/lib.rs?sym=rust%3Awidget");
  });

  it("trims a trailing slash on origin", () => {
    expect(
      symbolPermalinkFor("https://kbc.example.com/", "kb", "rust:widget", { fallbackPath: "src/lib.rs" }),
    ).toBe("https://kbc.example.com/r/kb/src/lib.rs?sym=rust%3Awidget");
  });
});

// --- V72-G1.2 — `?ent=` ----------------------------------------------------

describe("entityUrl", () => {
  const cases: Array<[string, Parameters<typeof entityUrl>, string]> = [
    ["bare repo (no file context)", ["kb", "Shop::Order"], "/r/kb?ent=Shop%3A%3AOrder"],
    [
      "with the file the reader was on",
      ["kb", "Shop::Order", { path: "app/models/shop/order.rb" }],
      "/r/kb/app/models/shop/order.rb?ent=Shop%3A%3AOrder",
    ],
    [
      "ref + line ride BEFORE ent, in codeUrl's own param order",
      ["kb", "Shop::Order", { path: "a.rb", ref: "main", line: 12 }],
      "/r/kb/a.rb?ref=main&line=12&ent=Shop%3A%3AOrder",
    ],
    [
      "a line RANGE serialises the same way codeUrl does",
      ["kb", "Shop::Order", { path: "a.rb", line: { start: 24, end: 10 } }],
      "/r/kb/a.rb?line=10-24&ent=Shop%3A%3AOrder",
    ],
    [
      "repo + entity needing encoding",
      ["my repo", "A&B::C", { path: "x y.rb" }],
      "/r/my%20repo/x%20y.rb?ent=A%26B%3A%3AC",
    ],
  ];
  for (const [name, args, expected] of cases) {
    it(name, () => {
      expect(entityUrl(...args)).toBe(expected);
    });
  }

  it("a non-positive line is omitted, never emitted as junk", () => {
    expect(entityUrl("kb", "Foo", { path: "a.rb", line: 0 })).toBe("/r/kb/a.rb?ent=Foo");
  });
});

describe("parseEntParam", () => {
  it("round-trips what entityUrl emits", () => {
    const url = entityUrl("kb", "Shop::Order", { path: "a.rb" });
    const value = new URLSearchParams(url.slice(url.indexOf("?"))).get("ent");
    expect(parseEntParam(value)).toBe("Shop::Order");
  });

  it("is TOTAL: absent, empty and whitespace-only are all `null`", () => {
    // A blank `ent=` is not an address — returning `""` would put the shell
    // into dossier mode over nothing.
    expect(parseEntParam(null)).toBeNull();
    expect(parseEntParam("")).toBeNull();
    expect(parseEntParam("   ")).toBeNull();
  });

  it("does NOT validate Ruby's constant grammar — the daemon decides", () => {
    // This module has no business inventing a constant grammar; an address it
    // cannot resolve earns an honest `entity-unknown` from the route, which is
    // a better answer than a client-side guess.
    expect(parseEntParam("not a constant")).toBe("not a constant");
    expect(parseEntParam("  Shop::Order  ")).toBe("Shop::Order");
  });
});
