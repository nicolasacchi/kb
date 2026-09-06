import { describe, expect, it } from "vitest";
import type { BranchOut, LadderAttribution } from "../api/types";
import { branchHasOpenReview, reasonChips, sortByAuthorTimeDesc } from "./branchReasons";

const NONE: LadderAttribution = {
  schema: "join/1",
  confidence: "none",
  via: "none",
  sha: "0".repeat(40),
};

function branch(over: Partial<BranchOut> = {}): BranchOut {
  return {
    name: "topic",
    target_sha: "a".repeat(40),
    is_head: false,
    ahead: 0,
    behind: 0,
    attribution: NONE,
    ...over,
  };
}

describe("reasonChips", () => {
  it("maps suggest.terms into the V4.L2 chip set", () => {
    const chips = reasonChips(
      branch({
        ahead: 3,
        suggest: {
          score: 3,
          terms: {
            recency: 0.98,
            has_open_review: 1,
            attribution: 0.5,
            ahead: 0.2,
          },
        },
      }),
    );
    expect(chips.map((c) => c.label)).toEqual([
      "active today",
      "open review",
      "agent session",
      "ahead 3",
    ]);
  });

  it("labels recency just above 0.5 as this week, and skips <= 0.5", () => {
    expect(
      reasonChips(branch({ suggest: { score: 0.6, terms: { recency: 0.6 } } })).map((c) => c.label),
    ).toEqual(["this week"]);
    expect(reasonChips(branch({ suggest: { score: 0.4, terms: { recency: 0.4 } } }))).toEqual([]);
  });

  it("does not emit ahead 0 even when the term is present", () => {
    expect(
      reasonChips(branch({ ahead: 0, suggest: { score: 0.1, terms: { ahead: 0 } } })),
    ).toEqual([]);
  });

  it("degrades to author_time recency when suggest is absent", () => {
    const now = 1_700_000_000;
    expect(
      reasonChips(branch({ last: { subject: "x", author_time: now - 3_600 } }), now).map((c) => c.label),
    ).toEqual(["active today"]);
    expect(
      reasonChips(branch({ last: { subject: "x", author_time: now - 3 * 86_400 } }), now).map(
        (c) => c.label,
      ),
    ).toEqual(["this week"]);
    expect(reasonChips(branch({ last: { subject: "x", author_time: now - 20 * 86_400 } }), now)).toEqual(
      [],
    );
  });
});

describe("sortByAuthorTimeDesc", () => {
  it("matches RepoCard's author_time desc", () => {
    const a = branch({ name: "a", last: { subject: "a", author_time: 10 } });
    const b = branch({ name: "b", last: { subject: "b", author_time: 30 } });
    const c = branch({ name: "c" });
    expect(sortByAuthorTimeDesc([a, c, b]).map((r) => r.name)).toEqual(["b", "a", "c"]);
  });
});

describe("branchHasOpenReview", () => {
  it("prefers the server field and joins only when it is absent", () => {
    expect(branchHasOpenReview(branch({ has_open_review: true }), [])).toBe(true);
    expect(branchHasOpenReview(branch({ has_open_review: false }), [{ head_ref: "topic" }])).toBe(
      false,
    );
    expect(branchHasOpenReview(branch(), [{ head_ref: "topic" }])).toBe(true);
    expect(branchHasOpenReview(branch(), [{ head_ref: "other" }])).toBe(false);
  });
});
