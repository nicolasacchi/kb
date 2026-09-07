import { describe, expect, it } from "vitest";
import {
  addrHref,
  addrLabel,
  addrsToSetSpans,
  addrToSetSpan,
  censusExplain,
  recipeIntentLabel,
  RECIPE_INTENT_ORDER,
} from "./recipeAddr";
import type { KbcAddr, KbcAddrKind, KbcStepCensus } from "../api/types";

function addr(kind: KbcAddrKind, extra: Partial<KbcAddr> = {}): KbcAddr {
  return { kind, repo: "r", blob: "unknown", trust: "unknown", ...extra };
}

describe("addrHref — every AddrKind, exhaustively", () => {
  it("file: links the reader at the path", () => {
    expect(addrHref("r", addr("file", { path: "a/b.rs" }))).toBe("/r/r/a/b.rs");
  });
  it("file: null with no path", () => {
    expect(addrHref("r", addr("file"))).toBeNull();
  });
  it("line: links the reader at path+line", () => {
    expect(addrHref("r", addr("line", { path: "a/b.rs", line: 42 }))).toBe("/r/r/a/b.rs?line=42");
  });
  it("line: null with no path", () => {
    expect(addrHref("r", addr("line", { line: 42 }))).toBeNull();
  });
  it("symbol: uses symbolUrl when both path and symbol are present", () => {
    const href = addrHref("r", addr("symbol", { path: "a/b.rs", symbol: "Foo::bar", line: 10 }));
    expect(href).toContain("sym=Foo%3A%3Abar");
    expect(href).toContain("/r/r/a/b.rs");
  });
  it("symbol: falls back to a plain reader link with no symbol name", () => {
    expect(addrHref("r", addr("symbol", { path: "a/b.rs", line: 10 }))).toBe("/r/r/a/b.rs?line=10");
  });
  it("symbol: null with neither path nor symbol", () => {
    expect(addrHref("r", addr("symbol"))).toBeNull();
  });
  it("entity: links the dossier", () => {
    expect(addrHref("r", addr("entity", { entity: "Shop::Order" }))).toBe("/r/r?ent=Shop%3A%3AOrder");
  });
  it("entity: null with no entity", () => {
    expect(addrHref("r", addr("entity"))).toBeNull();
  });
  it("commit: links the commit view", () => {
    expect(addrHref("r", addr("commit", { commit: "abc123" }))).toBe("/r/r/~commit/abc123");
  });
  it("commit: null with no sha", () => {
    expect(addrHref("r", addr("commit"))).toBeNull();
  });
  it("comment: links the reader at its anchor", () => {
    expect(addrHref("r", addr("comment", { path: "a.rs", line: 3 }))).toBe("/r/r/a.rs?line=3");
  });
  it("comment: null with no path", () => {
    expect(addrHref("r", addr("comment"))).toBeNull();
  });
  it("fact: links the reader at its anchor", () => {
    expect(addrHref("r", addr("fact", { path: "a.rs", line: 3 }))).toBe("/r/r/a.rs?line=3");
  });
  it("fact: null with no path", () => {
    expect(addrHref("r", addr("fact"))).toBeNull();
  });
  it("finding: links the finding permalink when review_id+id are present", () => {
    const href = addrHref(
      "r",
      addr("finding", { id: "f-abc", scalars: { review_id: 42 } }),
    );
    expect(href).toBe("/r/r/~reviews/42/f/f-abc");
  });
  it("finding: falls back to the reader anchor with no review_id", () => {
    expect(addrHref("r", addr("finding", { id: "f-abc", path: "a.rs", line: 1 }))).toBe(
      "/r/r/a.rs?line=1",
    );
  });
  it("finding: null with neither review_id nor path", () => {
    expect(addrHref("r", addr("finding", { id: "f-abc" }))).toBeNull();
  });
  it("node: prefers the code anchor when the node has one", () => {
    expect(
      addrHref("r", addr("node", { id: "slug/n1", path: "a.rs", line: 1, scalars: { board: "slug" } })),
    ).toBe("/r/r/a.rs?line=1");
  });
  it("node: falls back to the board when there is no code anchor", () => {
    expect(addrHref("r", addr("node", { id: "slug/n1", scalars: { board: "slug" } }))).toBe(
      "/r/r/~boards/slug",
    );
  });
  it("node: null with neither a code anchor nor a board scalar", () => {
    expect(addrHref("r", addr("node", { id: "slug/n1" }))).toBeNull();
  });
});

describe("addrLabel", () => {
  it("prefers path for file/line, symbol for symbol, entity for entity, commit for commit", () => {
    expect(addrLabel(addr("file", { path: "a.rs" }))).toBe("a.rs");
    expect(addrLabel(addr("line", { path: "a.rs", line: 2 }))).toBe("a.rs");
    expect(addrLabel(addr("symbol", { symbol: "Foo::bar", path: "a.rs" }))).toBe("Foo::bar");
    expect(addrLabel(addr("entity", { entity: "Shop::Order" }))).toBe("Shop::Order");
    expect(addrLabel(addr("commit", { commit: "abc" }))).toBe("abc");
  });
  it("uses scalars.title, then path, then id, then the kind name for comment/fact/finding/node", () => {
    expect(addrLabel(addr("comment", { scalars: { title: "TODO: fix" } }))).toBe("TODO: fix");
    expect(addrLabel(addr("fact", { path: "a.rs" }))).toBe("a.rs");
    expect(addrLabel(addr("finding", { id: "f-1" }))).toBe("f-1");
    expect(addrLabel(addr("node"))).toBe("node");
  });
});

describe("addrToSetSpan / addrsToSetSpans", () => {
  it("a path-only address becomes a bare span", () => {
    expect(addrToSetSpan(addr("file", { path: "a.rs" }))).toEqual({ path: "a.rs" });
  });
  it("a path+line address carries line_start === line_end", () => {
    expect(addrToSetSpan(addr("line", { path: "a.rs", line: 5 }))).toEqual({
      path: "a.rs",
      line_start: 5,
      line_end: 5,
    });
  });
  it("an address with no path is not representable — null", () => {
    expect(addrToSetSpan(addr("entity", { entity: "Shop::Order" }))).toBeNull();
  });
  it("addrsToSetSpans reports skipped count honestly rather than dropping silently", () => {
    const rows = [addr("file", { path: "a.rs" }), addr("entity", { entity: "X" }), addr("commit", { commit: "c1" })];
    expect(addrsToSetSpans(rows)).toEqual({ spans: [{ path: "a.rs" }], skipped: 2 });
  });
});

describe("censusExplain — mirrors StepCensus::explain() for all 11 reasons", () => {
  const reasons: KbcStepCensus["empty_reason"][] = [
    "no-inputs",
    "upstream-empty",
    "filtered-out",
    "scope-excluded",
    "lane-disabled",
    "lane-unknown",
    "lane-unavailable",
    "no-index",
    "not-a-rails-app",
    "param-empty",
    "budget-exhausted",
  ];

  it("renders a non-empty sentence for every one of the 11 reasons", () => {
    for (const empty_reason of reasons) {
      const sentence = censusExplain({ empty_reason });
      expect(sentence.length).toBeGreaterThan(0);
    }
  });

  it("returns an empty string when there is no empty_reason (a non-empty step)", () => {
    expect(censusExplain({})).toBe("");
  });

  it("filtered-out cites the largest input count", () => {
    const sentence = censusExplain({
      empty_reason: "filtered-out",
      inputs: { files: 3, symbols: 12 },
    });
    expect(sentence).toContain("12 row(s) existed");
  });

  it("appends filters_applied, joined with '; '", () => {
    const sentence = censusExplain({
      empty_reason: "filtered-out",
      inputs: { files: 5 },
      filters_applied: ["kind=todo", "state=open"],
    });
    expect(sentence).toContain("(kind=todo; state=open)");
  });
});

describe("intent taxonomy", () => {
  it("has exactly the five closed groups, in section order", () => {
    expect(RECIPE_INTENT_ORDER).toEqual(["orienting", "reviewing", "checking-tests", "rails", "hygiene"]);
  });
  it("labels every known group with the operator's own phrasing where recorded", () => {
    expect(recipeIntentLabel("orienting")).toBe("I'm getting oriented");
    expect(recipeIntentLabel("reviewing")).toBe("I'm reviewing a PR");
    expect(recipeIntentLabel("checking-tests")).toBe("I'm checking the e2e tests");
  });
  it("falls back to the raw string for an unrecognized/future intent", () => {
    expect(recipeIntentLabel("something-new")).toBe("something-new");
  });
});
