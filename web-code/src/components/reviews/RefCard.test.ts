// `RefCard`'s pure half (V73-K2b).
//
// The suite runs in the `node` environment and mounts no React (see
// `vitest.config.ts`), so what is pinned here is what the component DECIDES:
// which address it shows, where it links, and how a snippet's lines are
// numbered. The rendering itself is proved by the Playwright spec.
import { describe, expect, it } from "vitest";
import type { ReviewDocCard } from "../../api/types";
import { refCardHref, refCardSummary, snippetLines } from "./RefCard";

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

describe("refCardSummary", () => {
  it("names the scheme and the address, so a FOLDED card still says what it points at", () => {
    expect(refCardSummary(card())).toBe("code: app/a.rb:12");
    expect(refCardSummary(card({ line: 12, line_end: 20 }))).toBe("code: app/a.rb:12-20");
  });

  it("falls back to the author's own ref when there is no resolved position", () => {
    expect(refCardSummary(card({ state: "orphan", path: null, line: null }))).toBe(
      "code: code:app/a.rb:12",
    );
  });
});

describe("refCardHref", () => {
  it("builds a reader URL for a resolved code card", () => {
    expect(refCardHref(card(), "fixture", 7)).toBe("/r/fixture/app/a.rb?line=12");
  });

  it("builds a finding permalink for a finding card", () => {
    const c = card({ ref: "finding:f-a", scheme: "finding", path: null, line: null });
    expect(refCardHref(c, "fixture", 7)).toBe("/r/fixture/~reviews/7/f/f-a");
  });

  it("returns null for an orphan and for an inert link", () => {
    expect(refCardHref(card({ state: "orphan", path: null, line: null }), "fixture", 7)).toBeNull();
    expect(
      refCardHref(
        card({ ref: "kb:research/9f8b", scheme: "kb", state: "inert", trust: null, path: null, line: null }),
        "fixture",
        7,
      ),
    ).toBeNull();
  });
});

describe("snippetLines", () => {
  it("numbers the gutter from the snippet's own start line", () => {
    const c = card({ snippet: "a\nb\nc", snippet_start: 12 });
    expect(snippetLines(c)).toEqual([
      { n: 12, text: "a" },
      { n: 13, text: "b" },
      { n: 14, text: "c" },
    ]);
  });

  it("leaves the gutter BLANK when the daemon sent no start line", () => {
    // A guessed line number in a gutter is the same class of lie as a
    // guessed anchor: it looks authoritative and is not.
    const c = card({ snippet: "a\nb" });
    expect(snippetLines(c)).toEqual([
      { n: null, text: "a" },
      { n: null, text: "b" },
    ]);
  });

  it("is empty when there is no snippet at all", () => {
    expect(snippetLines(card())).toEqual([]);
  });
});
