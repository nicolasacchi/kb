// The `kbc-review/1` document model (V73-K2b).
//
// The whole numeric and addressing half of the Document tab lives in
// `lib/reviewDoc.ts` precisely so it can be pinned here, in the `node`
// environment this SPA's suite runs in. What these cases are really
// defending is the honesty contract: a count that comes off the wire, an
// absence that is stated, an orphan that is visible, and a ref this side
// cannot resolve that says so instead of disappearing.
import { describe, expect, it } from "vitest";
import type { ReviewDocCard, ReviewDocOut } from "../api/types";
import {
  actChipClass,
  blockLabel,
  bodyOf,
  cardAddress,
  cardCensus,
  cardCensusText,
  cardHref,
  cardIndex,
  cardStateClass,
  cardStateLabel,
  composeCommandLine,
  docBlocks,
  docCommandLine,
  findingCites,
  findingFacetText,
  findingFacets,
  findingTombstoneText,
  lintCandidatesText,
  lintCensusText,
  lintSeverityClass,
  omittedIsSection,
  omittedLabel,
  orderedBlocks,
  readingOrderIsDerived,
  refSpanFor,
  splitRefRuns,
} from "./reviewDoc";

function card(overrides: Partial<ReviewDocCard> = {}): ReviewDocCard {
  return {
    ref: "code:app/a.rb:12",
    scheme: "code",
    state: "pinned",
    trust: "exact",
    path: "app/a.rb",
    line: 12,
    caption: "the bytes the author cited are the bytes this card shows",
    ...overrides,
  };
}

function doc(overrides: Partial<ReviewDocOut> = {}): ReviewDocOut {
  return {
    schema: "kbc-review/1",
    review_id: 7,
    repo: "fixture",
    ps_number: 2,
    revision: 3,
    revisions: 3,
    tier: "standard",
    created_at: 100,
    doc_md: "---\nsummary_md: hi\n---\nbody text\n",
    summary_md: "hi",
    reading_order: { source: "authored", caption: "the reading order this document declares", chapters: [] },
    blocks: {},
    flows: [],
    questions: [],
    findings: [],
    omitted: [],
    cards: [],
    cards_resolved: true,
    ...overrides,
  };
}

const BUILDERS = {
  codeUrl: (loc: { repo: string; path: string; line?: { start: number; end: number } | number }) =>
    `/r/${loc.repo}/${loc.path}` +
    (loc.line === undefined
      ? ""
      : typeof loc.line === "number"
        ? `?line=${loc.line}`
        : `?line=${loc.line.start}-${loc.line.end}`),
  reviewDiffHref: (repo: string, id: number, file?: string) =>
    `/r/${repo}/~reviews/${id}/diff${file ? `/${file}` : ""}`,
  findingUrl: (repo: string, id: number, slug: string) => `/r/${repo}/~reviews/${id}/f/${slug}`,
};

describe("cardIndex", () => {
  it("keys on the ref body exactly as the author wrote it", () => {
    const idx = cardIndex([card({ ref: "sym:Order#total", scheme: "sym" }), card()]);
    expect(idx.get("sym:Order#total")?.scheme).toBe("sym");
    expect(idx.get("code:app/a.rb:12")?.line).toBe(12);
  });

  it("keeps the FIRST card for a duplicated ref, so a render is deterministic", () => {
    const idx = cardIndex([card({ caption: "first" }), card({ caption: "second" })]);
    expect(idx.get("code:app/a.rb:12")?.caption).toBe("first");
  });

  it("is empty for a read that did not resolve cards", () => {
    expect(cardIndex(null).size).toBe(0);
    expect(cardIndex(undefined).size).toBe(0);
  });
});

describe("refSpanFor", () => {
  const cards = cardIndex([card()]);

  it("returns the daemon's card when there is one", () => {
    const s = refSpanFor("code:app/a.rb:12", cards, true);
    expect(s.kind).toBe("card");
  });

  it("reports a malformed ref with its reason rather than degrading it to a wikilink", () => {
    const s = refSpanFor("code:", cards, true);
    expect(s.kind).toBe("malformed");
    if (s.kind === "malformed") expect(s.reason).toContain("empty path");
  });

  it("leaves a bare wikilink to kb (root invariant #29)", () => {
    expect(refSpanFor("Order", cards, true).kind).toBe("wikilink");
  });

  it("says a well-formed ref with no card was NOT scanned as prose", () => {
    const s = refSpanFor("ent:Shop::Order", cards, true);
    expect(s.kind).toBe("unresolved");
    if (s.kind === "unresolved") expect(s.reason).toContain("not scanned as prose");
  });

  it("blames the REQUEST, not the daemon, when the read asked for no cards", () => {
    const s = refSpanFor("ent:Shop::Order", cards, false);
    expect(s.kind).toBe("unresolved");
    if (s.kind === "unresolved") expect(s.reason).toContain("resolve=true");
  });
});

describe("splitRefRuns", () => {
  const cards = cardIndex([card()]);

  it("splits a text run at its ref, keeping the prose either side", () => {
    const runs = splitRefRuns(
      { kind: "text", text: "see [[code:app/a.rb:12]] for the guard" },
      cards,
      true,
    );
    expect(runs).toHaveLength(3);
    expect(runs[0]).toEqual({ kind: "text", text: "see " });
    expect(runs[1].kind).toBe("ref");
    expect(runs[2]).toEqual({ kind: "text", text: " for the guard" });
  });

  it("NEVER scans a code run — a ref in a code span is a ref being talked about", () => {
    const run = { kind: "code" as const, text: "[[code:app/a.rb:12]]" };
    expect(splitRefRuns(run, cards, true)).toEqual([run]);
  });

  it("renders a wikilink back as the literal text the author typed", () => {
    const runs = splitRefRuns({ kind: "text", text: "see [[Order]] here" }, cards, true);
    expect(runs).toEqual([
      { kind: "text", text: "see " },
      { kind: "text", text: "[[Order]]" },
      { kind: "text", text: " here" },
    ]);
  });

  it("returns the run untouched when it holds no ref at all", () => {
    const run = { kind: "text" as const, text: "no refs here" };
    expect(splitRefRuns(run, cards, true)).toEqual([run]);
  });

  it("handles two refs on one line without losing the text between them", () => {
    const runs = splitRefRuns(
      { kind: "text", text: "[[code:app/a.rb:12]] and [[code:]] end" },
      cards,
      true,
    );
    expect(runs.map((r) => r.kind)).toEqual(["ref", "text", "ref", "text"]);
    expect(runs[1]).toEqual({ kind: "text", text: " and " });
    expect(runs[3]).toEqual({ kind: "text", text: " end" });
  });
});

describe("docBlocks", () => {
  const cards = cardIndex([card()]);

  it("renders headings and fences, which a report summary deliberately does not", () => {
    const blocks = docBlocks("# Title\n\ntext\n\n```rb\nx = 1\n```\n", cards, true);
    expect(blocks.map((b) => b.kind)).toEqual(["heading", "paragraph", "code"]);
  });

  it("does not turn a ref inside a fence into a card", () => {
    const blocks = docBlocks("```\n[[code:app/a.rb:12]]\n```\n", cards, true);
    expect(blocks).toHaveLength(1);
    expect(blocks[0].kind).toBe("code");
  });
});

describe("bodyOf", () => {
  it("renders the body, never the front matter", () => {
    expect(bodyOf(doc())).toBe("body text\n");
  });
});

describe("card presentation", () => {
  it("names the state in words on every card, orphan included", () => {
    expect(cardStateLabel(card({ state: "pinned" }))).toBe("pinned");
    expect(cardStateLabel(card({ state: "carried" }))).toBe("carried");
    expect(cardStateLabel(card({ state: "orphan" }))).toBe("no honest match");
    expect(cardStateLabel(card({ state: "inert" }))).toBe("inert");
  });

  it("puts the state in the class so the four are addressable", () => {
    expect(cardStateClass(card({ state: "orphan" }))).toBe("kbc-refcard--orphan");
  });

  it("shows a range address, a bare-path address, and the REF for an orphan", () => {
    expect(cardAddress(card({ line: 12, line_end: 20 }))).toBe("app/a.rb:12-20");
    expect(cardAddress(card({ line: null, line_end: null }))).toBe("app/a.rb");
    expect(cardAddress(card({ state: "orphan", path: null, line: null }))).toBe("code:app/a.rb:12");
  });

  it("censuses a mixed card set and states every non-zero lane", () => {
    const c = cardCensus([
      card(),
      card({ ref: "b", state: "carried" }),
      card({ ref: "c", state: "orphan" }),
      card({ ref: "d", state: "inert" }),
    ]);
    expect(c).toEqual({ total: 4, pinned: 1, carried: 1, orphan: 1, inert: 1 });
    expect(cardCensusText(c)).toBe("4 refs · 1 pinned · 1 carried · 1 no honest match · 1 inert");
    expect(cardCensusText(cardCensus([]))).toBe("no refs");
  });
});

describe("cardHref", () => {
  it("links a code card at its line range through the shared builder", () => {
    expect(cardHref(card({ line: 12, line_end: 20 }), BUILDERS, "fixture", 7)).toBe(
      "/r/fixture/app/a.rb?line=12-20",
    );
  });

  it("links a finding card at the review's own finding permalink", () => {
    const c = card({ ref: "finding:f-dedup-race", scheme: "finding", path: null, line: null });
    expect(cardHref(c, BUILDERS, "fixture", 7)).toBe("/r/fixture/~reviews/7/f/f-dedup-race");
  });

  it("links a hunk card into the review diff at that file", () => {
    const c = card({ ref: "hunk:app/a.rb@2#1", scheme: "hunk", path: "app/a.rb", line: null });
    expect(cardHref(c, BUILDERS, "fixture", 7)).toBe("/r/fixture/~reviews/7/diff/app/a.rb");
  });

  it("gives an ORPHAN no link — a guessed destination is worse than none", () => {
    expect(cardHref(card({ state: "orphan", path: null }), BUILDERS, "fixture", 7)).toBeNull();
  });

  it("gives an INERT card no link — kb-code names no host it could resolve", () => {
    const c = card({ ref: "gh:comment/12345", scheme: "gh", state: "inert", trust: null, path: null });
    expect(cardHref(c, BUILDERS, "fixture", 7)).toBeNull();
  });
});

describe("the document's own facts", () => {
  it("says when a reading order is the daemon's rather than the author's", () => {
    expect(readingOrderIsDerived(doc())).toBe(false);
    expect(
      readingOrderIsDerived(
        doc({ reading_order: { source: "derived", caption: "composed by the daemon", chapters: [] } }),
      ),
    ).toBe(true);
  });

  it("labels an omitted block and says which are named sections", () => {
    expect(omittedLabel("reading_order")).toBe("reading order");
    expect(omittedLabel("blocks.alternatives_considered")).toBe("alternatives considered");
    expect(omittedIsSection("blocks.tests")).toBe(true);
    expect(omittedIsSection("risk")).toBe(false);
  });

  it("renders blocks in the DECLARED order, not the wire's alphabetical one", () => {
    const blocks = { tests: "t", context: "c", approach: "a" };
    expect(orderedBlocks(blocks).map((b) => b.name)).toEqual(["context", "approach", "tests"]);
  });

  it("renders an unknown block LAST rather than hiding a server-side widening", () => {
    expect(orderedBlocks({ zzz_future: "f", context: "c" }).map((b) => b.name)).toEqual([
      "context",
      "zzz_future",
    ]);
  });

  it("drops an empty block rather than rendering an empty heading", () => {
    expect(orderedBlocks({ context: "   ", tests: "t" }).map((b) => b.name)).toEqual(["tests"]);
  });

  it("labels a block in the author's own vocabulary", () => {
    expect(blockLabel("open_questions")).toBe("open questions");
  });
});

describe("findingFacets", () => {
  it("derives act and blocking counts from the rows and nothing else", () => {
    const f = findingFacets([
      { act: "issue", blocking: true, category: "correctness" },
      { act: "question", blocking: false, category: "design" },
      { act: "issue", blocking: false, category: "correctness" },
    ]);
    expect(f.total).toBe(3);
    expect(f.blocking).toBe(1);
    expect(f.byAct).toEqual([
      { act: "issue", count: 2 },
      { act: "question", count: 1 },
    ]);
    expect(f.byCategory).toEqual([
      { category: "correctness", count: 2 },
      { category: "design", count: 1 },
    ]);
  });

  it("reads a row with no act as an ISSUE — what every pre-V0034 row meant", () => {
    expect(findingFacets([{}]).byAct).toEqual([{ act: "issue", count: 1 }]);
  });

  it("states the facets in one line, and says 'no findings' honestly", () => {
    expect(findingFacetText(findingFacets([{ act: "issue", blocking: true }]))).toBe(
      "1 finding · 1 blocking · 1 issue",
    );
    expect(findingFacetText(findingFacets([]))).toBe("no findings");
  });
});

describe("findings v2 helpers", () => {
  it("renders superseded_by as a tombstone naming the successor", () => {
    expect(findingTombstoneText({ superseded: true, superseded_by: "f-b" })).toBe("superseded by f-b");
    expect(findingTombstoneText({ superseded: true })).toBe("superseded");
    expect(findingTombstoneText({ superseded: false })).toBeNull();
  });

  it("treats an absent cites list as EMPTY for rendering, without claiming it is empty", () => {
    expect(findingCites({ cites: ["code:a.rb:1"] })).toEqual(["code:a.rb:1"]);
    expect(findingCites({ cites: null })).toEqual([]);
    expect(findingCites({})).toEqual([]);
  });

  it("puts the act in the class, defaulting to issue", () => {
    expect(actChipClass("question")).toBe("kbc-finding__act kbc-finding__act--question");
    expect(actChipClass(undefined)).toBe("kbc-finding__act kbc-finding__act--issue");
  });
});

describe("the CLI lines", () => {
  it("shows the compose line an AGENT would run — composing stays loopback-only", () => {
    expect(composeCommandLine(7)).toBe("kb-code review compose 7 --doc review.md");
    expect(composeCommandLine(7, "full")).toBe("kb-code review compose 7 --doc review.md --tier full");
    expect(composeCommandLine(7, "standard")).toBe("kb-code review compose 7 --doc review.md");
  });

  it("shows the read this tab is rendering", () => {
    expect(docCommandLine(7, 2)).toBe("kb-code review doc 7 --ps 2 --resolve");
    expect(docCommandLine(7)).toBe("kb-code review doc 7 --resolve");
  });
});

describe("lint", () => {
  it("states the census, including the all-clear", () => {
    expect(lintCensusText({ schema: "kbc-review/1", errors: 2, warnings: 1, infos: 0, rows: [{ rule: "r", severity: "error", message: "m" }] })).toBe(
      "2 errors · 1 warning",
    );
    expect(lintCensusText({ schema: "kbc-review/1", errors: 0, warnings: 0, infos: 0, rows: [] })).toBe(
      "lints clean",
    );
    expect(lintCensusText(null)).toBe("");
  });

  it("classes the three weights, folding anything unknown to info", () => {
    expect(lintSeverityClass("error")).toBe("kbc-doclint__row--error");
    expect(lintSeverityClass("warning")).toBe("kbc-doclint__row--warning");
    expect(lintSeverityClass("whatever")).toBe("kbc-doclint__row--info");
  });

  it("suggests, never fixes", () => {
    expect(lintCandidatesText(["code:app/a.rb:12", "code:app/b.rb:3"])).toBe(
      "did you mean [[code:app/a.rb:12]] or [[code:app/b.rb:3]]?",
    );
    expect(lintCandidatesText([])).toBe("");
    expect(lintCandidatesText(undefined)).toBe("");
  });
});
