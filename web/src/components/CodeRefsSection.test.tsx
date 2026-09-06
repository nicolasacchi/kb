// @vitest-environment jsdom
//
// DCB-W1.D.R #3 — the review flagged three pure/near-pure helpers inside
// CodeRefsSection.tsx as the subtlest logic in the whole Code section, with
// zero direct coverage (everything rode Playwright's full-page fixtures):
//
//   (a) degradeMessage — the reason-PRESENT vs reason-ABSENT branch (state 3
//       vs state 2 of 13-w1d-kb-spa.md §6's degrade matrix)
//   (b) LineBadge — all four line-state branches, including R18's
//       "moved +N · git-verified" special case
//   (c) groupRefs — grouped / ungrouped / dangling-FK bucketing
//
// All three are exported from CodeRefsSection.tsx specifically for this file
// (W1.D.R #3's "export/lightly refactor the pure helpers if needed").
import { afterEach, describe, expect, it } from "vitest";
import { cleanup, render } from "@testing-library/react";
import {
  COMPOUND_UNAVAILABLE,
  degradeMessage,
  freshnessState,
  groupRefs,
  LineBadge,
} from "./CodeRefsSection";
import type { DocLensOut, ResolvedCodeRef } from "../api/doclens";

afterEach(() => {
  cleanup();
});

function reasoned(message: string, reason: string): Error & { reason?: string } {
  const e = new Error(message) as Error & { reason?: string };
  e.reason = reason;
  return e;
}

// --- (a) degradeMessage -----------------------------------------------

describe("degradeMessage (R12 discriminator)", () => {
  it("renders the server-authored message verbatim when `.reason` is kb_not_allowlisted", () => {
    expect(
      degradeMessage(reasoned("this kb isn't allowlisted", "kb_not_allowlisted")),
    ).toBe("this kb isn't allowlisted");
  });

  it("renders the server-authored message verbatim when `.reason` is doclens_disabled", () => {
    expect(
      degradeMessage(reasoned("doc-lens is disabled on this daemon", "doclens_disabled")),
    ).toBe("doc-lens is disabled on this daemon");
  });

  it("passes repo_unavailable through the reason branch, not the compound message", () => {
    const msg = degradeMessage(
      reasoned('repo "app": read the file index: boom', "repo_unavailable"),
    );
    expect(msg).toBe('repo "app": read the file index: boom');
    expect(msg).not.toBe(COMPOUND_UNAVAILABLE);
  });

  it("passes kb_upstream_error through the reason branch, not the compound message", () => {
    const msg = degradeMessage(
      reasoned("kb responded 502", "kb_upstream_error"),
    );
    expect(msg).toBe("kb responded 502");
    expect(msg).not.toBe(COMPOUND_UNAVAILABLE);
  });

  it("collapses a reason-LESS fetch failure to the one honest compound message", () => {
    // A browser cannot distinguish "not CORS-allowlisted" from "kb-code
    // down" from "network unreachable" — all three throw an opaque error
    // with no `.reason`. TypeError is the real shape `fetch()` throws.
    expect(degradeMessage(new TypeError("Failed to fetch"))).toBe(
      COMPOUND_UNAVAILABLE,
    );
  });

  it("collapses a reason-LESS AbortError (timeout) to the same compound message", () => {
    expect(degradeMessage(new DOMException("The operation was aborted", "AbortError"))).toBe(
      COMPOUND_UNAVAILABLE,
    );
  });

  it("collapses a completely absent error value to the compound message", () => {
    expect(degradeMessage(undefined)).toBe(COMPOUND_UNAVAILABLE);
  });
});

// --- (b) LineBadge -------------------------------------------------------

function baseRef(overrides: Partial<ResolvedCodeRef> = {}): ResolvedCodeRef {
  return {
    ordinal: 0,
    group: null,
    kind: "path_line",
    raw: "ref",
    declared: false,
    path_hint: null,
    line_hint: null,
    line_hint_end: null,
    symbol_container: null,
    symbol_member: null,
    context: null,
    path_state: "present",
    resolved_path: null,
    candidate_count: 0,
    candidates: [],
    issue: null,
    line_state: "absent",
    line_evidence: "none",
    confirm_token: null,
    token_line: null,
    resolved_line: null,
    line_hint_delta: null,
    file_lines: 0,
    line_reason: null,
    spans: [],
    symbol_state: "no_symbol",
    symbol_hit_count: 0,
    symbol_hits: [],
    reader: null,
    search: null,
    note: null,
    ...overrides,
  };
}

describe("LineBadge", () => {
  it("renders nothing for a kind with no line hint at all (path/symbol/issue/external)", () => {
    const { container } = render(
      <LineBadge r={baseRef({ kind: "path", line_state: "confirmed" })} />,
    );
    expect(container.firstChild).toBeNull();
  });

  it("renders a plain confirmed badge for a non-rev_remap confirmation", () => {
    const { getByText } = render(
      <LineBadge
        r={baseRef({
          kind: "path_line",
          line_state: "confirmed",
          line_evidence: "context_token",
          line_hint_delta: 0,
        })}
      />,
    );
    expect(getByText("✓ confirmed")).toBeTruthy();
  });

  it("renders drifted with the resolved-now line number", () => {
    const { getByText } = render(
      <LineBadge
        r={baseRef({
          kind: "path_range",
          line_state: "drifted",
          resolved_line: 88,
        })}
      />,
    );
    expect(getByText("≈ now :88")).toBeTruthy();
  });

  it("renders drifted with a `?` fallback when resolved_line is null", () => {
    const { getByText } = render(
      <LineBadge
        r={baseRef({ kind: "path_range", line_state: "drifted", resolved_line: null })}
      />,
    );
    expect(getByText("≈ now :?")).toBeTruthy();
  });

  it("renders unverifiable", () => {
    const { getByText } = render(
      <LineBadge r={baseRef({ kind: "path_list", line_state: "unverifiable" })} />,
    );
    expect(getByText("? unverifiable")).toBeTruthy();
  });

  it("renders nothing for line_state absent", () => {
    const { container } = render(
      <LineBadge r={baseRef({ kind: "path_line", line_state: "absent" })} />,
    );
    expect(container.firstChild).toBeNull();
  });

  // R18 — the case the review named directly: a git-verified rev_remap that
  // ALSO moved the line renders the honest "moved" badge, not a bare
  // "confirmed" that silently hides the move.
  it("R18 — renders 'moved +N · git-verified' for a rev_remap confirmation with a positive delta", () => {
    const { getByText } = render(
      <LineBadge
        r={baseRef({
          kind: "path_line",
          line_state: "confirmed",
          line_evidence: "rev_remap",
          line_hint_delta: 8,
        })}
      />,
    );
    expect(getByText("✓ moved +8 · git-verified")).toBeTruthy();
  });

  it("R18 — renders the negative delta without a doubled sign", () => {
    const { getByText } = render(
      <LineBadge
        r={baseRef({
          kind: "path_line",
          line_state: "confirmed",
          line_evidence: "rev_remap",
          line_hint_delta: -3,
        })}
      />,
    );
    expect(getByText("✓ moved -3 · git-verified")).toBeTruthy();
  });

  it("R18 — a rev_remap confirmation with a ZERO delta is a plain confirmed badge, not 'moved +0'", () => {
    const { getByText, queryByText } = render(
      <LineBadge
        r={baseRef({
          kind: "path_line",
          line_state: "confirmed",
          line_evidence: "rev_remap",
          line_hint_delta: 0,
        })}
      />,
    );
    expect(getByText("✓ confirmed")).toBeTruthy();
    expect(queryByText(/moved/)).toBeNull();
  });
});

// --- (c) groupRefs ---------------------------------------------------

function baseLens(overrides: Partial<DocLensOut> = {}): DocLensOut {
  return {
    schema: "codelens/1",
    kb: "demo",
    doc_id: "d1",
    moved_from: null,
    doc_path: "file.md",
    doc_href: null,
    doc_hash: "hash",
    doc_title: "Doc",
    doc_extracted_at: 0,
    doc_code_rev: null,
    never_scanned: false,
    repo: {
      name: "app",
      root: "/tmp/app",
      state: "ready",
      head_sha: "abc1234567",
      head_branch: "main",
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
    note: "note",
    ...overrides,
  };
}

describe("groupRefs", () => {
  it("buckets refs into their matching group, in group-ordinal order", () => {
    const lens = baseLens({
      groups: [
        { key: "g2", label: "Second", anchor: "second", ordinal: 1, ref_count: 1 },
        { key: "g1", label: "First", anchor: "first", ordinal: 0, ref_count: 1 },
      ],
      refs: [
        baseRef({ ordinal: 0, group: "g2", raw: "in-g2" }),
        baseRef({ ordinal: 1, group: "g1", raw: "in-g1" }),
      ],
    });
    const out = groupRefs(lens);
    expect(out.map((b) => b.key)).toEqual(["g1", "g2"]);
    expect(out[0].refs.map((r) => r.raw)).toEqual(["in-g1"]);
    expect(out[1].refs.map((r) => r.raw)).toEqual(["in-g2"]);
  });

  it("omits a group bucket that ended up with zero refs", () => {
    const lens = baseLens({
      groups: [{ key: "empty", label: "Empty", anchor: "empty", ordinal: 0, ref_count: 0 }],
      refs: [],
    });
    expect(groupRefs(lens)).toEqual([]);
  });

  it("appends an Ungrouped trailer LAST when ungrouped_count > 0", () => {
    const lens = baseLens({
      groups: [{ key: "g1", label: "First", anchor: "first", ordinal: 0, ref_count: 1 }],
      ungrouped_count: 2,
      refs: [
        baseRef({ ordinal: 0, group: "g1", raw: "grouped" }),
        baseRef({ ordinal: 1, group: null, raw: "loose-1" }),
        baseRef({ ordinal: 2, group: null, raw: "loose-2" }),
      ],
    });
    const out = groupRefs(lens);
    expect(out.map((b) => b.label)).toEqual(["First", "Ungrouped"]);
    expect(out[1].refs.map((r) => r.raw)).toEqual(["loose-1", "loose-2"]);
  });

  it("W1.D.R #6 — a dangling-FK ref (group key absent from groups[]) still renders under Ungrouped even when ungrouped_count is 0", () => {
    const lens = baseLens({
      groups: [{ key: "g1", label: "First", anchor: "first", ordinal: 0, ref_count: 1 }],
      ungrouped_count: 0, // the server never counted this ref as "ungrouped"
      refs: [
        baseRef({ ordinal: 0, group: "g1", raw: "grouped" }),
        // Names a group key that ISN'T in `groups[]` — a dangling FK.
        baseRef({ ordinal: 1, group: "phantom-group", raw: "orphaned" }),
      ],
    });
    const out = groupRefs(lens);
    const trailer = out.find((b) => b.key === null);
    expect(trailer).toBeDefined();
    expect(trailer!.refs.map((r) => r.raw)).toEqual(["orphaned"]);
  });

  it("renders no Ungrouped trailer when there is nothing ungrouped", () => {
    const lens = baseLens({
      groups: [{ key: "g1", label: "First", anchor: "first", ordinal: 0, ref_count: 1 }],
      ungrouped_count: 0,
      refs: [baseRef({ ordinal: 0, group: "g1", raw: "grouped" })],
    });
    expect(groupRefs(lens).some((b) => b.key === null)).toBe(false);
  });
});

// --- (d) freshnessState (CT-E3) ------------------------------------------

// Fixed clock: extraction happened exactly 3 days before "now", so the
// tier-(a) wording is byte-pinned as "extracted 3d".
const NOW_MS = 1_754_822_400_000; // unix 1_754_822_400
const EXTRACTED_3D_AGO = 1_754_822_400 - 3 * 86_400;

function freshArgs(
  overrides: Partial<Parameters<typeof freshnessState>[0]> = {},
): Parameters<typeof freshnessState>[0] {
  return {
    neverScanned: false,
    refCount: 2,
    extractedAt: EXTRACTED_3D_AGO,
    hasCodeUrl: true,
    pinned: false,
    scorecardOk: false,
    lens: undefined,
    crossErr: null,
    nowMs: NOW_MS,
    ...overrides,
  };
}

describe("freshnessState (CT-E3 honest-staleness ladder)", () => {
  // -- tier (a): the three header states ---------------------------------

  it("never_scanned wins over everything — exact wording, no count, no time", () => {
    const out = freshnessState(
      freshArgs({ neverScanned: true, refCount: 0, extractedAt: null }),
    );
    expect(out).toEqual({
      kind: "never-scanned",
      text: "code refs never scanned",
      drifted: null,
      title: null,
    });
  });

  it("a scanned zero-ref doc yields NO line — a doc citing nothing makes no claim that can go stale", () => {
    expect(freshnessState(freshArgs({ refCount: 0 }))).toBeNull();
  });

  it("N refs with no code_url renders the plain tier-(a) line", () => {
    const out = freshnessState(freshArgs({ hasCodeUrl: false }));
    expect(out).toEqual({
      kind: "hints-only",
      text: "cites 2 code refs · extracted 3d",
      drifted: null,
      title: null,
    });
  });

  it("pluralizes honestly: 'cites 1 code ref'", () => {
    const out = freshnessState(freshArgs({ refCount: 1, hasCodeUrl: false }));
    expect(out!.text).toBe("cites 1 code ref · extracted 3d");
  });

  it("a missing extracted_at (defensive) drops the time segment rather than fabricating one", () => {
    const out = freshnessState(
      freshArgs({ extractedAt: null, hasCodeUrl: false }),
    );
    expect(out!.text).toBe("cites 2 code refs");
  });

  // -- tier (b): the four cross-daemon states ----------------------------

  it("verified: lens data upgrades to 'cites N code locations · M drifted · checked <rel>'", () => {
    const lens = baseLens({
      resolved_unix: Math.floor(NOW_MS / 1000), // "now"
      counts: { ...baseLens().counts, total: 12, drifted: 4 },
    });
    const out = freshnessState(
      freshArgs({ refCount: 12, pinned: true, scorecardOk: true, lens }),
    );
    expect(out).toEqual({
      kind: "verified",
      text: "cites 12 code locations · 4 drifted · checked now",
      drifted: 4,
      title: null,
    });
  });

  it("verified with zero drift still SAYS '0 drifted' (a checked verdict, not silence)", () => {
    const lens = baseLens({
      resolved_unix: Math.floor(NOW_MS / 1000),
      counts: { ...baseLens().counts, total: 2, drifted: 0 },
    });
    const out = freshnessState(
      freshArgs({ pinned: true, scorecardOk: true, lens }),
    );
    expect(out!.kind).toBe("verified");
    expect(out!.text).toContain("0 drifted");
    expect(out!.drifted).toBe(0);
  });

  it("no-pin: scorecard reachable but no checkout pinned — 'freshness unchecked', never a drift number", () => {
    const out = freshnessState(freshArgs({ scorecardOk: true, pinned: false }));
    expect(out!.kind).toBe("no-pin");
    expect(out!.text).toBe(
      "cites 2 code refs · extracted 3d · freshness unchecked — no checkout pinned",
    );
    expect(out!.drifted).toBeNull();
  });

  it("refusal: a `.reason`-bearing failure renders the server-authored message verbatim", () => {
    const out = freshnessState(
      freshArgs({
        crossErr: reasoned("this kb isn't allowlisted", "kb_not_allowlisted"),
      }),
    );
    expect(out!.kind).toBe("refusal");
    expect(out!.text).toBe(
      "cites 2 code refs · extracted 3d · kb-code: this kb isn't allowlisted",
    );
    expect(out!.title).toBe("this kb isn't allowlisted");
  });

  it("unreachable DEGRADES to the exact tier-(a) wording — never a fake 'fresh', never a drift claim", () => {
    const out = freshnessState(
      freshArgs({ crossErr: new TypeError("Failed to fetch") }),
    );
    expect(out!.kind).toBe("degraded");
    // Byte-identical to the tier-(a) line: the kb-side extraction facts are
    // the only thing an unreached kb-code leaves us authoritative for.
    expect(out!.text).toBe("cites 2 code refs · extracted 3d");
    expect(out!.text).not.toContain("drifted");
    expect(out!.drifted).toBeNull();
    expect(out!.title).toBe(COMPOUND_UNAVAILABLE);
  });

  it("an error outranks stale lens data (a failed re-check must not keep rendering the old verdict)", () => {
    const lens = baseLens({
      resolved_unix: Math.floor(NOW_MS / 1000),
      counts: { ...baseLens().counts, total: 2, drifted: 1 },
    });
    const out = freshnessState(
      freshArgs({
        pinned: true,
        scorecardOk: true,
        lens,
        crossErr: new TypeError("Failed to fetch"),
      }),
    );
    expect(out!.kind).toBe("degraded");
    expect(out!.text).not.toContain("drifted");
  });

  it("pinned with the lens still in flight holds the tier-(a) line (upgrades in place when it lands)", () => {
    const out = freshnessState(
      freshArgs({ pinned: true, scorecardOk: true, lens: undefined }),
    );
    expect(out!.kind).toBe("hints-only");
    expect(out!.text).toBe("cites 2 code refs · extracted 3d");
  });
});
