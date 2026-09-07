import { describe, expect, it } from "vitest";
import type { ReviewFinding } from "../../api/types";
import {
  findingAct,
  findingActClass,
  findingAuthorDisplay,
  findingCitesOf,
  findingDiffHref,
  findingLastTouched,
  findingLocationLabel,
  findingTombstone,
  severityRank,
  severityStripeClass,
} from "./FindingCard";

function finding(overrides: Partial<ReviewFinding> = {}): ReviewFinding {
  return {
    slug: "f-dedup-race",
    severity: "concern",
    category: "Concurrency",
    location: { kind: "single", path: "app/models/order.rb", lines: [88], removed: false },
    title: "Duplicate order rows possible under concurrent checkout",
    rationale: "Verified against db/schema.rb:41 — no unique index.",
    recommendation: null,
    evidence: null,
    origin: "import",
    author: "claude",
    disposition: null,
    published_state: "unpublished",
    published_at: null,
    published_url: null,
    superseded: false,
    superseded_reason: null,
    content_updated_at: null,
    annotation_id: "ann1",
    import_batch_id: "batch_1",
    created_at: 1000,
    updated_at: 1000,
    resolution: { line: 88, line_end: null, orphaned: false, confidence: "exact" },
    thread_count: 0,
    unresolved_count: 0,
    ...overrides,
  };
}

describe("severityRank", () => {
  it("ranks blocker first, concern second, ok last", () => {
    expect(severityRank("blocker")).toBeLessThan(severityRank("concern"));
    expect(severityRank("concern")).toBeLessThan(severityRank("ok"));
  });
});

describe("severityStripeClass", () => {
  it("builds a kbc-finding--<severity> class for each of the 3 severities", () => {
    expect(severityStripeClass("blocker")).toBe("kbc-finding--blocker");
    expect(severityStripeClass("concern")).toBe("kbc-finding--concern");
    expect(severityStripeClass("ok")).toBe("kbc-finding--ok");
  });
});

describe("findingAuthorDisplay", () => {
  it("marks an import-origin finding as agent-authored regardless of the literal author string", () => {
    const f = finding({ origin: "import", author: "claude" });
    expect(findingAuthorDisplay(f)).toEqual({ isAgent: true, label: "claude" });
  });

  it("marks a manual-origin finding as human-authored ('you' chip, not the ✳ agent mark)", () => {
    const f = finding({ origin: "manual", author: "you" });
    expect(findingAuthorDisplay(f)).toEqual({ isAgent: false, label: "you" });
  });

  it("still trusts origin over a surprising author string on either side", () => {
    // A manual finding authored under a real username, not literally "you".
    const f = finding({ origin: "manual", author: "carol" });
    expect(findingAuthorDisplay(f).isAgent).toBe(false);
  });
});

describe("findingLocationLabel", () => {
  it("renders single as path:line", () => {
    const f = finding({ location: { kind: "single", path: "a.rb", lines: [225], removed: false } });
    expect(findingLocationLabel(f.location)).toBe("a.rb:225");
  });

  it("renders range as path:start-end", () => {
    const f = finding({ location: { kind: "range", path: "a.rb", lines: [5, 9], removed: false } });
    expect(findingLocationLabel(f.location)).toBe("a.rb:5-9");
  });

  it("renders multi as path:comma,separated,lines", () => {
    const f = finding({
      location: { kind: "multi", path: "trade.rb", lines: [13, 30, 33, 36], removed: false },
    });
    expect(findingLocationLabel(f.location)).toBe("trade.rb:13,30,33,36");
  });

  it("renders whole_file as the bare path with no line suffix", () => {
    const f = finding({ location: { kind: "whole_file", path: "README.md", lines: null, removed: false } });
    expect(findingLocationLabel(f.location)).toBe("README.md");
  });

  it("appends (removed) when the cited line/file was deleted by this diff", () => {
    const f = finding({ location: { kind: "single", path: "a.rb", lines: [1], removed: true } });
    expect(findingLocationLabel(f.location)).toBe("a.rb:1 (removed)");
  });
});

describe("findingDiffHref", () => {
  it("links to the file with no ?line= when the finding is orphaned", () => {
    const f = finding({ resolution: { line: null, line_end: null, orphaned: true, confidence: "orphaned" } });
    const href = findingDiffHref("myrepo", 7, f);
    expect(href).not.toContain("line=");
    expect(href).toContain("/order.rb");
  });

  it("appends ?line=N when the finding resolved to a concrete line", () => {
    const f = finding({ resolution: { line: 42, line_end: null, orphaned: false, confidence: "exact" } });
    const href = findingDiffHref("myrepo", 7, f);
    expect(href).toContain("line=42");
  });

  it("appends ?ps= only for a non-latest patchset", () => {
    const f = finding();
    expect(findingDiffHref("myrepo", 7, f, "latest")).not.toContain("ps=");
    expect(findingDiffHref("myrepo", 7, f, "2")).toContain("ps=2");
  });

  it("never emits a finding= param — that's a sibling unit's (U3) upgrade", () => {
    const href = findingDiffHref("myrepo", 7, finding());
    expect(href).not.toContain("finding=");
  });
});

describe("findingLastTouched", () => {
  it("prefers content_updated_at over updated_at/created_at when it is the newest", () => {
    const f = finding({ created_at: 1000, updated_at: 1000, content_updated_at: 5000 });
    // now=5005s -> 5s ago from content_updated_at, not "just now" territory confusion
    expect(findingLastTouched(f, 5010_000)).toContain("second");
  });

  it("falls back to updated_at when content_updated_at is null", () => {
    const f = finding({ created_at: 1000, updated_at: 2000, content_updated_at: null });
    expect(findingLastTouched(f, 2010_000)).toContain("second");
  });
});

// --- findings v2 (V73-K2b, design D9) -------------------------------------
//
// V73-K1 landed `act`/`blocking`/`cites`/`fingerprint`/`superseded_by` as
// storage columns; K2b put them on the wire and on this card. Every field is
// OPTIONAL on the TS type on purpose — an older daemon omits them entirely,
// and each default below is what a pre-V0034 row always meant.
describe("findings v2 on the card", () => {
  it("reads a finding with no act as an ISSUE", () => {
    expect(findingAct(finding())).toBe("issue");
    expect(findingAct(finding({ act: "question" }))).toBe("question");
  });

  it("puts the act in a class of its own, never in the severity's", () => {
    expect(findingActClass(finding({ act: "nitpick" }))).toBe(
      "kbc-finding__act kbc-finding__act--nitpick",
    );
    // The act is a LABEL beside severity; it must not reuse the severity
    // stripe, whose whole job is the other axis.
    expect(findingActClass(finding({ act: "issue" }))).not.toBe(
      severityStripeClass(finding().severity),
    );
  });

  it("renders superseded_by as a tombstone naming its successor", () => {
    expect(findingTombstone(finding({ superseded: true, superseded_by: "f-newer" }))).toBe(
      "superseded by f-newer",
    );
    // `superseded` without a declared successor still says so — the daemon
    // never INFERS which new finding replaced an old one.
    expect(findingTombstone(finding({ superseded: true }))).toBe("superseded");
    expect(findingTombstone(finding())).toBeNull();
  });

  it("treats an unreadable cites blob the same as none, for RENDERING only", () => {
    expect(findingCitesOf(finding({ cites: ["code:a.rb:1"] }))).toEqual(["code:a.rb:1"]);
    expect(findingCitesOf(finding({ cites: null }))).toEqual([]);
    expect(findingCitesOf(finding())).toEqual([]);
  });
});
