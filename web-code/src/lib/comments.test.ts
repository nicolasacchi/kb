import { describe, expect, it } from "vitest";
import type { AnnotationView, CommentOut, CommentState } from "../api/types";
import {
  ACTIONABLE_STATES,
  claimAnnotationBody,
  commentAtLine,
  commentGutterMarkers,
  commentVisibleInMode,
  dashboardQueryParams,
  deriveBridgeRows,
  filterCommentsForMode,
  freshnessCaption,
  groupCommentsByState,
  isBridgeable,
  isYardShaped,
  nextCommentLine,
  nextGutterMode,
  parseRubySignatureParams,
  parseYardParams,
  sortStatesForDashboard,
  yardSignatureDisagreement,
} from "./comments";

const NONE_STATE: CommentState = { state: "none" };

function comment(overrides: Partial<CommentOut> = {}): CommentOut {
  return {
    path: "a.rb",
    kind: "prose",
    line_start: 1,
    line_end: 1,
    text: "a comment",
    text_truncated: false,
    blob_sha: "deadbeef",
    state: NONE_STATE,
    ...overrides,
  };
}

function annotation(overrides: Partial<AnnotationView> = {}): AnnotationView {
  return {
    id: "ann-1",
    repo: "r",
    path: "a.rb",
    anchor: { css_path: "", offset: 1, snippet: "x" } as unknown as AnnotationView["anchor"],
    anchor_kind: "line",
    intent: "claim",
    parent_id: null,
    body: "TODO drop the shim",
    author: "you",
    created_at: 0,
    updated_at: 0,
    resolved: false,
    line: 1,
    stale: false,
    line_end: null,
    sha: null,
    ...overrides,
  };
}

describe("comment gutter modes", () => {
  it("cycles all -> quiet -> doc-only -> all", () => {
    expect(nextGutterMode("all")).toBe("quiet");
    expect(nextGutterMode("quiet")).toBe("doc-only");
    expect(nextGutterMode("doc-only")).toBe("all");
  });

  it("`all` shows every kind regardless of state", () => {
    for (const kind of ["doc", "annotation", "directive", "section", "licence", "generated", "commented_code", "prose"]) {
      expect(commentVisibleInMode(comment({ kind }), "all")).toBe(true);
    }
  });

  it("`doc-only` shows doc rows only, regardless of state", () => {
    expect(commentVisibleInMode(comment({ kind: "doc", state: { state: "fresh" } }), "doc-only")).toBe(true);
    expect(commentVisibleInMode(comment({ kind: "doc", state: { state: "unknown", reason: "uncommitted" } }), "doc-only")).toBe(true);
    expect(commentVisibleInMode(comment({ kind: "prose" }), "doc-only")).toBe(false);
    expect(commentVisibleInMode(comment({ kind: "annotation", state: { state: "aged" } }), "doc-only")).toBe(false);
  });

  it("`quiet` shows only annotation/directive rows carrying a non-none state", () => {
    expect(commentVisibleInMode(comment({ kind: "annotation", state: { state: "aged" } }), "quiet")).toBe(true);
    expect(commentVisibleInMode(comment({ kind: "directive", state: { state: "unreasoned" } }), "quiet")).toBe(true);
    expect(commentVisibleInMode(comment({ kind: "annotation", state: NONE_STATE }), "quiet")).toBe(false);
    expect(commentVisibleInMode(comment({ kind: "doc", state: { state: "drifted" } }), "quiet")).toBe(false);
    expect(commentVisibleInMode(comment({ kind: "prose" }), "quiet")).toBe(false);
  });

  it("filterCommentsForMode never silently drops a kind `all` doesn't filter", () => {
    const rows = [
      comment({ kind: "doc" }),
      comment({ kind: "annotation", keyword: "TODO", state: { state: "aged" } }),
      comment({ kind: "prose" }),
    ];
    expect(filterCommentsForMode(rows, "all")).toHaveLength(3);
    expect(filterCommentsForMode(rows, "doc-only")).toHaveLength(1);
    expect(filterCommentsForMode(rows, "quiet")).toHaveLength(1);
  });
});

describe("commentGutterMarkers", () => {
  it("marks every line in a block's range with the same mark", () => {
    const marks = commentGutterMarkers([comment({ kind: "licence", line_start: 1, line_end: 3 })]);
    expect([...marks.keys()].sort()).toEqual([1, 2, 3]);
    expect(marks.get(2)?.kind).toBe("licence");
  });

  it("a single-line block (line_end < line_start, an unreported end) marks just its own line", () => {
    const marks = commentGutterMarkers([comment({ kind: "annotation", line_start: 5, line_end: 0 })]);
    expect([...marks.keys()]).toEqual([5]);
  });

  it("carries the state's reason for an unknown row, and omits it for `none`", () => {
    const marks = commentGutterMarkers([
      comment({ kind: "doc", line_start: 1, line_end: 1, state: { state: "unknown", reason: "blame-budget" } }),
      comment({ kind: "prose", line_start: 2, line_end: 2 }),
    ]);
    expect(marks.get(1)?.reason).toBe("blame-budget");
    expect(marks.get(2)?.reason).toBeUndefined();
  });
});

describe("buffer navigation", () => {
  const rows = [
    comment({ line_start: 3, line_end: 3 }),
    comment({ line_start: 10, line_end: 10 }),
    comment({ line_start: 20, line_end: 20 }),
  ];

  it("finds the next marker past the cursor, wrapping at EOF", () => {
    expect(nextCommentLine(rows, 5, 1)).toBe(10);
    expect(nextCommentLine(rows, 20, 1)).toBe(3);
    expect(nextCommentLine(rows, 25, 1)).toBe(3);
  });

  it("finds the previous marker before the cursor, wrapping at BOF", () => {
    expect(nextCommentLine(rows, 15, -1)).toBe(10);
    expect(nextCommentLine(rows, 3, -1)).toBe(20);
    expect(nextCommentLine(rows, 1, -1)).toBe(20);
  });

  it("returns null for an empty file", () => {
    expect(nextCommentLine([], 1, 1)).toBeNull();
  });

  it("commentAtLine prefers a covering block, else the closest line_start", () => {
    const block = comment({ line_start: 5, line_end: 8 });
    expect(commentAtLine([block], 6)).toBe(block);
    expect(commentAtLine([block], 8)).toBe(block);
    const near = comment({ line_start: 20, line_end: 20 });
    expect(commentAtLine([block, near], 15)).toBe(near);
    expect(commentAtLine([], 1)).toBeNull();
  });
});

describe("freshnessCaption", () => {
  it("renders each state as its own honest sentence", () => {
    expect(freshnessCaption({ state: "fresh" })).toBe("fresh");
    expect(freshnessCaption({ state: "none" })).toBe("");
    expect(
      freshnessCaption({ state: "drifted", age_days: 5, code_commit: "abcdef0123", doc_commit: "0123abcdef" }),
    ).toBe("drifted 5 days (code moved at abcdef0, doc last touched at 0123abc)");
    expect(freshnessCaption({ state: "drifted", age_days: 1, code_commit: "a", doc_commit: "b" })).toContain(
      "drifted 1 day (",
    );
    expect(freshnessCaption({ state: "unknown", reason: "blame-budget" })).toBe("unknown: blame-budget");
    expect(freshnessCaption({ state: "unknown" })).toBe("unknown: no reason given");
    expect(freshnessCaption({ state: "aged", age_days: 30, on_date: "2027-09-01" })).toBe(
      "aged 30 days (due 2027-09-01)",
    );
    expect(freshnessCaption({ state: "unreasoned", tool: "rubocop" })).toBe("unreasoned suppression (rubocop)");
    expect(freshnessCaption({ state: "unreasoned" })).toBe("unreasoned suppression");
  });

  it("degrades an unrecognized state to itself, never blank", () => {
    expect(freshnessCaption({ state: "brand-new-state" } as unknown as CommentState)).toBe("brand-new-state");
  });
});

describe("YARD-vs-signature disagreement", () => {
  const yardDoc = [
    "Computes the widget's display name.",
    "@param name [String] the raw name",
    "@param opts [Hash]",
    "@return [String]",
  ].join("\n");

  it("is not YARD-shaped without @param/@return", () => {
    expect(isYardShaped("just a plain comment")).toBe(false);
    expect(yardSignatureDisagreement("just a plain comment", "def foo(a)")).toBeNull();
  });

  it("parses @param names, skipping the optional [Type] tag", () => {
    expect(parseYardParams(yardDoc)).toEqual(["name", "opts"]);
  });

  it("parses Ruby signature params across splats/kwargs/defaults/blocks", () => {
    expect(parseRubySignatureParams("def foo(a, b = 1, *c, d:, e: 2, **f, &g)")).toEqual([
      "a",
      "b",
      "c",
      "d",
      "e",
      "f",
      "g",
    ]);
  });

  it("returns [] honestly for no parens, empty parens, or absent signature", () => {
    expect(parseRubySignatureParams(null)).toEqual([]);
    expect(parseRubySignatureParams(undefined)).toEqual([]);
    expect(parseRubySignatureParams("def foo")).toEqual([]);
    expect(parseRubySignatureParams("def foo()")).toEqual([]);
  });

  it("is null (never a guess) when the signature has nothing to compare against", () => {
    expect(yardSignatureDisagreement(yardDoc, null)).toBeNull();
    expect(yardSignatureDisagreement(yardDoc, "def foo()")).toBeNull();
  });

  it("is null when YARD and the signature agree exactly", () => {
    expect(yardSignatureDisagreement(yardDoc, "def foo(name, opts)")).toBeNull();
  });

  it("names both directions of a disagreement without ever saying who is right", () => {
    const d = yardSignatureDisagreement(yardDoc, "def foo(name, extra)");
    expect(d).not.toBeNull();
    expect(d?.missingInSig).toEqual(["opts"]);
    expect(d?.missingInYard).toEqual(["extra"]);
  });
});

describe("the claim -> annotation bridge", () => {
  const todoFamily = ["TODO", "FIXME", "HACK", "XXX", "BUG"];

  it("isBridgeable requires kind:annotation and a TODO-family keyword off the server list", () => {
    expect(isBridgeable(comment({ kind: "annotation", keyword: "TODO" }), todoFamily)).toBe(true);
    expect(isBridgeable(comment({ kind: "annotation", keyword: "NOTE" }), todoFamily)).toBe(false);
    expect(isBridgeable(comment({ kind: "annotation" }), todoFamily)).toBe(false);
    expect(isBridgeable(comment({ kind: "doc", keyword: "TODO" }), todoFamily)).toBe(false);
  });

  it("derives `open` for a bridgeable comment with no claim annotation", () => {
    const c = comment({ kind: "annotation", keyword: "TODO", line_start: 18, line_end: 18 });
    const rows = deriveBridgeRows([c], todoFamily, []);
    expect(rows).toEqual([{ state: "open", comment: c, annotation: null }]);
  });

  it("derives `tracked` for an unresolved claim matched by live line", () => {
    const c = comment({ kind: "annotation", keyword: "TODO", line_start: 18, line_end: 18 });
    const a = annotation({ line: 18, resolved: false });
    const rows = deriveBridgeRows([c], todoFamily, [a]);
    expect(rows).toEqual([{ state: "tracked", comment: c, annotation: a }]);
  });

  it("derives `resolved` for a resolved claim, whether or not the comment remains", () => {
    const c = comment({ kind: "annotation", keyword: "TODO", line_start: 18, line_end: 18 });
    const a = annotation({ line: 18, resolved: true });
    expect(deriveBridgeRows([c], todoFamily, [a])).toEqual([{ state: "resolved", comment: c, annotation: a }]);
    expect(deriveBridgeRows([], todoFamily, [a])).toEqual([{ state: "resolved", comment: null, annotation: a }]);
  });

  it("derives `gone` for an unresolved claim whose line no longer matches any comment", () => {
    const a = annotation({ line: 99, resolved: false });
    const rows = deriveBridgeRows([], todoFamily, [a]);
    expect(rows).toEqual([{ state: "gone", comment: null, annotation: a }]);
  });

  it("never counts a REPLY as a claim of its own", () => {
    const reply = annotation({ id: "reply-1", parent_id: "ann-1", line: 18 });
    expect(deriveBridgeRows([], todoFamily, [reply])).toEqual([]);
  });

  it("claimAnnotationBody carries the comment text plus its smart_todo fields", () => {
    const c = comment({
      text: "TODO(on: date('2027-09-01'), to: 'owner@example.com') drop the shim",
      fields: { raw: { on: "date('2027-09-01')", to: "'owner@example.com'" }, on_date: "2027-09-01", to: "owner@example.com" },
    });
    const body = claimAnnotationBody(c);
    expect(body).toContain("drop the shim");
    expect(body).toContain("on: date('2027-09-01')");
    expect(body).toContain("to: 'owner@example.com'");
  });

  it("claimAnnotationBody is just the text when there are no fields", () => {
    const c = comment({ text: "TODO drop the shim", fields: { raw: {} } });
    expect(claimAnnotationBody(c)).toBe("TODO drop the shim");
  });
});

describe("the ~comments dashboard", () => {
  it("ACTIONABLE_STATES mirrors the server's drift::ACTIONABLE_STATES exactly", () => {
    expect(ACTIONABLE_STATES).toEqual(["drifted", "aged", "unreasoned"]);
  });

  it("dashboardQueryParams builds the exact server query for a lane", () => {
    expect(
      dashboardQueryParams("acme", { kind: null, keyword: null, state: null, pathPrefix: null }, "drifted", 50),
    ).toEqual({ repo: "acme", state: "drifted", limit: 50, offset: 0 });
  });

  it("dashboardQueryParams folds every facet in, omitting absent ones", () => {
    expect(
      dashboardQueryParams(
        "acme",
        { kind: "directive", keyword: "TODO", state: null, pathPrefix: "app/models/" },
        "unreasoned",
        25,
        25,
      ),
    ).toEqual({
      repo: "acme",
      path: "app/models/",
      kind: "directive",
      keyword: "TODO",
      state: "unreasoned",
      limit: 25,
      offset: 25,
    });
  });

  it("the show-everything toggle (state: null) omits the state param entirely", () => {
    expect(
      dashboardQueryParams("acme", { kind: null, keyword: null, state: null, pathPrefix: null }, null, 50),
    ).toEqual({ repo: "acme", limit: 50, offset: 0 });
  });

  it("sortStatesForDashboard orders actionable lanes first, unknown states last-but-honest", () => {
    expect(sortStatesForDashboard(["none", "fresh", "unreasoned", "drifted", "aged", "unknown"])).toEqual([
      "drifted",
      "aged",
      "unreasoned",
      "unknown",
      "fresh",
      "none",
    ]);
    expect(sortStatesForDashboard(["mystery", "drifted"])).toEqual(["drifted", "mystery"]);
  });

  it("groupCommentsByState preserves server order within each group", () => {
    const a = comment({ path: "a.rb", state: { state: "drifted" } });
    const b = comment({ path: "b.rb", state: { state: "drifted" } });
    const c = comment({ path: "c.rb", state: { state: "aged" } });
    const groups = groupCommentsByState([a, b, c]);
    expect(groups.get("drifted")).toEqual([a, b]);
    expect(groups.get("aged")).toEqual([c]);
  });
});
