import { describe, expect, it } from "vitest";
import type { CodeLensGroup, CodeLensOut, CodeLensRef } from "../api/types";
import {
  lensEntryByPathUrl,
  lensEntryUrl,
  lensUrl,
  pinCorrection,
  refsForGroup,
  stepGroup,
  truncationCaption,
  UNGROUPED_SEL,
} from "./docLensUrl";

// Golden strings, same discipline as `setsUrl.test.ts`/`codeUrl.test.ts`.

describe("lensUrl", () => {
  it("builds the repo-scoped lens URL", () => {
    expect(lensUrl("kb", "platform", "9f8b7182d433")).toBe("/r/kb/~lens/platform/9f8b7182d433");
  });

  it("percent-encodes each segment", () => {
    expect(lensUrl("my repo", "my kb", "doc id")).toBe("/r/my%20repo/~lens/my%20kb/doc%20id");
  });
});

describe("lensEntryUrl", () => {
  it("builds the repo-less, id-addressed ramp URL", () => {
    expect(lensEntryUrl("platform", "9f8b7182d433")).toBe("/~lens/platform/9f8b7182d433");
  });

  it("percent-encodes each segment", () => {
    expect(lensEntryUrl("my kb", "doc id")).toBe("/~lens/my%20kb/doc%20id");
  });
});

describe("lensEntryByPathUrl", () => {
  it("builds the repo-less, path-addressed ramp URL", () => {
    expect(lensEntryByPathUrl("platform", "features/algolia/piano.html")).toBe(
      "/~lens/platform/by-path/features/algolia/piano.html",
    );
  });

  it("encodes each path segment individually, keeping `/` as a literal separator", () => {
    expect(lensEntryByPathUrl("kb", "a b/c#d.html")).toBe("/~lens/kb/by-path/a%20b/c%23d.html");
  });

  it("drops empty segments from a stray leading/trailing/doubled slash", () => {
    expect(lensEntryByPathUrl("kb", "/a//b/")).toBe("/~lens/kb/by-path/a/b");
  });
});

function group(overrides: Partial<CodeLensGroup>): CodeLensGroup {
  return { key: "g", label: "G", anchor: "g", ordinal: 0, ref_count: 1, ...overrides };
}

describe("stepGroup", () => {
  const groups = [group({ key: "a", ordinal: 0 }), group({ key: "b", ordinal: 1 })];

  it("steps forward through [null, a, b] with no ungrouped trailer", () => {
    expect(stepGroup(groups, 0, null, 1)).toBe("a");
    expect(stepGroup(groups, 0, "a", 1)).toBe("b");
    // Wraps back to "All".
    expect(stepGroup(groups, 0, "b", 1)).toBe(null);
  });

  it("steps backward, wrapping the other way", () => {
    expect(stepGroup(groups, 0, null, -1)).toBe("b");
    expect(stepGroup(groups, 0, "b", -1)).toBe("a");
    expect(stepGroup(groups, 0, "a", -1)).toBe(null);
  });

  it("includes the Ungrouped trailer only when ungroupedCount > 0", () => {
    expect(stepGroup(groups, 3, "b", 1)).toBe(UNGROUPED_SEL);
    expect(stepGroup(groups, 3, UNGROUPED_SEL, 1)).toBe(null); // wraps to All
    expect(stepGroup(groups, 3, null, -1)).toBe(UNGROUPED_SEL);
  });

  it("an empty groups list with no ungrouped trailer only ever yields All", () => {
    expect(stepGroup([], 0, null, 1)).toBe(null);
    expect(stepGroup([], 0, null, -1)).toBe(null);
  });

  it("an unrecognized current selection is treated as starting from the ring's head", () => {
    expect(stepGroup(groups, 0, "not-a-real-group", 1)).toBe("a");
  });
});

function ref(overrides: Partial<CodeLensRef>): CodeLensRef {
  return {
    ordinal: 1,
    group: null,
    kind: "path",
    raw: "a.rb",
    declared: false,
    path_hint: "a.rb",
    line_hint: null,
    line_hint_end: null,
    symbol_container: null,
    symbol_member: null,
    context: null,
    path_state: "present",
    resolved_path: "a.rb",
    candidate_count: 1,
    candidates: ["a.rb"],
    issue: null,
    line_state: "absent",
    line_evidence: "none",
    confirm_token: null,
    token_line: null,
    resolved_line: null,
    resolved_line_end: null,
    line_hint_delta: null,
    file_lines: 1,
    line_reason: null,
    remap: null,
    symbol_state: "no_symbol",
    symbol_hit_count: 0,
    symbol_hits: [],
    spans: [],
    reader: { repo: "kb", path: "a.rb", line: null },
    search: null,
    note: null,
    when_written: null,
    ...overrides,
  };
}

describe("refsForGroup", () => {
  const refs = [ref({ ordinal: 1, group: "a" }), ref({ ordinal: 2, group: "b" }), ref({ ordinal: 3, group: null })];

  it("null selection (All) returns every ref", () => {
    expect(refsForGroup(refs, null)).toEqual(refs);
  });

  it("UNGROUPED_SEL returns only group === null refs", () => {
    expect(refsForGroup(refs, UNGROUPED_SEL).map((r) => r.ordinal)).toEqual([3]);
  });

  it("a real group key returns only that group's refs", () => {
    expect(refsForGroup(refs, "a").map((r) => r.ordinal)).toEqual([1]);
    expect(refsForGroup(refs, "b").map((r) => r.ordinal)).toEqual([2]);
  });
});

// DCB-W2.B.R fix 4 — all four cases the review named, table-driven.
describe("pinCorrection", () => {
  it("pinned !== current, not yet seeded ⇒ the pinned repo (a real correction)", () => {
    expect(pinCorrection("beta", "alpha", false)).toBe("beta");
  });

  it("pinned === current ⇒ null (the URL already names the pin)", () => {
    expect(pinCorrection("alpha", "alpha", false)).toBe(null);
  });

  it("already seeded this doc ⇒ null, even if pinned still disagrees with current", () => {
    expect(pinCorrection("beta", "alpha", true)).toBe(null);
  });

  it("no scorecard resolved yet (pinned undefined) ⇒ null", () => {
    expect(pinCorrection(undefined, "alpha", false)).toBe(null);
  });

  it("the doc genuinely has no pin (pinned null) ⇒ null", () => {
    expect(pinCorrection(null, "alpha", false)).toBe(null);
  });
});

// DCB-W2.B.R fix 3 — the ported truncation/partial-resolution caption.
function lens(overrides: Partial<CodeLensOut>): CodeLensOut {
  return {
    schema: "codelens/1",
    kb: "platform",
    doc_id: "doc1",
    moved_from: null,
    doc_path: "a.html",
    doc_href: null,
    doc_hash: null,
    doc_title: null,
    doc_extracted_at: null,
    doc_code_rev: null,
    rev_remap: null,
    never_scanned: false,
    repo: {
      name: "alpha",
      root: "/repo",
      state: "ready",
      head_sha: null,
      head_branch: null,
      dirty: false,
      source: "param",
    },
    resolved_unix: 0,
    truncated: false,
    partial: false,
    partial_reason: null,
    counts: {
      total: 0,
      resolved: 0,
      present: 0,
      ambiguous: 0,
      absent: 0,
      external: 0,
      confirmed: 0,
      drifted: 0,
      unverifiable: 0,
      line_absent: 0,
      declared_but_absent: 0,
    },
    ungrouped_count: 0,
    groups: [],
    refs: [],
    era: "none",
    note: "",
    ...overrides,
  };
}

describe("truncationCaption", () => {
  it("undefined lens (no repo picked yet / still loading) ⇒ null", () => {
    expect(truncationCaption(undefined)).toBe(null);
  });

  it("neither truncated nor partial ⇒ null", () => {
    expect(truncationCaption(lens({}))).toBe(null);
  });

  it("truncated ⇒ 'showing N of M', honest about the shrink", () => {
    expect(
      truncationCaption(
        lens({
          truncated: true,
          refs: [{} as CodeLensRef, {} as CodeLensRef],
          counts: { ...lens({}).counts, total: 20 },
        }),
      ),
    ).toBe("showing 2 of 20");
  });

  it("partial ⇒ 'resolution incomplete (<reason>)'", () => {
    expect(truncationCaption(lens({ partial: true, partial_reason: "deadline" }))).toBe(
      "resolution incomplete (deadline)",
    );
  });

  it("partial with no reason ⇒ falls back to 'budget'", () => {
    expect(truncationCaption(lens({ partial: true, partial_reason: null }))).toBe(
      "resolution incomplete (budget)",
    );
  });

  it("both truncated and partial ⇒ joined with · ", () => {
    expect(
      truncationCaption(
        lens({
          truncated: true,
          refs: [{} as CodeLensRef],
          counts: { ...lens({}).counts, total: 5 },
          partial: true,
          partial_reason: "deadline",
        }),
      ),
    ).toBe("showing 1 of 5 · resolution incomplete (deadline)");
  });
});
