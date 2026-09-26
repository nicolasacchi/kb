import { describe, expect, it } from "vitest";
import type { FindingTouchedIn, ReviewFinding, ReviewPatchset } from "../api/types";
import { reviewDiffHref } from "./codeUrl";
import { findingTouchTimelineRows, touchedInChips } from "./findingTouches";

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
    own_ps: 1,
    touched_in: [],
    touched_in_capped: false,
    ...overrides,
  };
}

function touch(overrides: Partial<FindingTouchedIn> = {}): FindingTouchedIn {
  return { ps: 2, hunks: 1, overlap: "exact", ...overrides };
}

function patchset(overrides: Partial<ReviewPatchset> = {}): ReviewPatchset {
  return {
    ps_number: 1,
    tip_sha: "a".repeat(40),
    tip_sha_full: "a".repeat(40),
    base_sha: "b".repeat(40),
    base_sha_full: "b".repeat(40),
    captured_at: 1000,
    commit_count: 1,
    ...overrides,
  };
}

describe("touchedInChips", () => {
  it("is empty for a finding with no touched_in entries", () => {
    expect(touchedInChips(finding({ touched_in: [] }), "acme/repo", 7)).toEqual([]);
  });

  it("is empty when own_ps is absent — a chip with nowhere honest to link", () => {
    const f = finding({ own_ps: null, touched_in: [touch()] });
    expect(touchedInChips(f, "acme/repo", 7)).toEqual([]);
  });

  it("builds one chip per entry, linking to the own_ps..ps interdiff on the finding's file", () => {
    const f = finding({
      own_ps: 1,
      touched_in: [touch({ ps: 2, overlap: "exact", hunks: 1 }), touch({ ps: 5, overlap: "adjacent", hunks: 2 })],
    });
    const chips = touchedInChips(f, "acme/repo", 7);
    expect(chips).toHaveLength(2);
    expect(chips[0]).toEqual({
      ps: 2,
      label: "lines changed in ps 2",
      title: "exact overlap — 1 hunk in ps 2's diff from ps 1",
      href: reviewDiffHref("acme/repo", 7, undefined, {
        ps: { from: 1, to: 2 },
        file: "app/models/order.rb",
      }),
    });
    expect(chips[1].label).toBe("lines changed in ps 5");
    expect(chips[1].title).toBe("adjacent overlap — 2 hunks in ps 5's diff from ps 1");
  });

  it("never says the word fixed", () => {
    const f = finding({ touched_in: [touch({ ps: 3, hunks: 4, overlap: "exact" })] });
    const chips = touchedInChips(f, "acme/repo", 7);
    for (const c of chips) {
      expect(c.label.toLowerCase()).not.toContain("fixed");
      expect(c.title.toLowerCase()).not.toContain("fixed");
    }
  });

  it("golden — pins the exact href shape for one canonical case", () => {
    const f = finding({ own_ps: 1, touched_in: [touch({ ps: 2 })] });
    const [chip] = touchedInChips(f, "acme/repo", 7);
    expect(chip.href).toBe("/r/acme%2Frepo/~reviews/7/diff?ps=1..2&file=app%2Fmodels%2Forder.rb");
  });
});

describe("findingTouchTimelineRows", () => {
  it("emits one row per (finding, touched_in entry), timestamped from the matching patchset", () => {
    const findings = [
      finding({
        slug: "f-a",
        own_ps: 1,
        touched_in: [touch({ ps: 2, overlap: "exact", hunks: 1 })],
      }),
      finding({
        slug: "f-b",
        own_ps: 1,
        touched_in: [
          touch({ ps: 2, overlap: "adjacent", hunks: 3 }),
          touch({ ps: 3, overlap: "exact", hunks: 1 }),
        ],
      }),
    ];
    const patchsets = [
      patchset({ ps_number: 1, captured_at: 1000 }),
      patchset({ ps_number: 2, captured_at: 2000 }),
      patchset({ ps_number: 3, captured_at: 3000 }),
    ];
    const rows = findingTouchTimelineRows(findings, patchsets, "acme/repo", 7);
    expect(rows).toHaveLength(3);
    expect(rows.map((r) => r.at)).toEqual([2000, 2000, 3000]);
    expect(rows[0].kind).toBe("finding_touch");
    expect(rows[0].icon).toBe("finding_touch");
    expect(rows[0].label).toBe("author touched f-a's lines in ps 2");
    expect(rows[1].label).toBe("author touched f-b's lines in ps 2");
    expect(rows[2].label).toBe("author touched f-b's lines in ps 3");
    for (const r of rows) {
      expect(r.label.toLowerCase()).not.toContain("fixed");
    }
  });

  it("degrades to at=0 for a ps missing from the given patchset list, never throwing", () => {
    const findings = [finding({ own_ps: 1, touched_in: [touch({ ps: 9 })] })];
    const rows = findingTouchTimelineRows(findings, [], "acme/repo", 7);
    expect(rows).toHaveLength(1);
    expect(rows[0].at).toBe(0);
  });

  it("is empty over an empty findings list", () => {
    expect(findingTouchTimelineRows([], [], "acme/repo", 7)).toEqual([]);
  });
});
