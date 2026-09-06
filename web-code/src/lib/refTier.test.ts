import { describe, expect, it } from "vitest";
import type { CodeLensRef } from "../api/types";
import { refTier } from "./refTier";

// Table-driven over `path_state`/`kind`/`symbol_state` combos — the base
// plan's own W1.C verification list (present/ambiguous/absent/external ×
// confirmed/drifted/unverifiable), plus the >3-candidate/symbol/issue arms.

function ref(overrides: Partial<CodeLensRef>): CodeLensRef {
  return {
    ordinal: 1,
    group: null,
    kind: "path_line",
    raw: "a.rb:1",
    declared: false,
    path_hint: "a.rb",
    line_hint: 1,
    line_hint_end: null,
    symbol_container: null,
    symbol_member: null,
    context: null,
    path_state: null,
    resolved_path: null,
    candidate_count: 0,
    candidates: [],
    issue: null,
    line_state: "absent",
    line_evidence: "none",
    confirm_token: null,
    token_line: null,
    resolved_line: null,
    resolved_line_end: null,
    line_hint_delta: null,
    file_lines: 0,
    line_reason: null,
    remap: null,
    symbol_state: "no_symbol",
    symbol_hit_count: 0,
    symbol_hits: [],
    spans: [],
    reader: null,
    search: null,
    note: null,
    when_written: null,
    ...overrides,
  };
}

describe("refTier", () => {
  it("present ⇒ unique", () => {
    expect(refTier(ref({ path_state: "present" }))).toBe("unique");
  });

  it("ambiguous with <=3 candidates ⇒ ambiguous-inline", () => {
    expect(refTier(ref({ path_state: "ambiguous", candidate_count: 3 }))).toBe("ambiguous-inline");
    expect(refTier(ref({ path_state: "ambiguous", candidate_count: 1 }))).toBe("ambiguous-inline");
  });

  it("ambiguous with >3 candidates ⇒ ambiguous-search, tiered on candidate_count NEVER candidates.length", () => {
    // B2's named failure mode: candidate_count: 5, candidates: [] must
    // still render the >3 tier, not fall through to the empty ≤3 tier.
    expect(refTier(ref({ path_state: "ambiguous", candidate_count: 5, candidates: [] }))).toBe(
      "ambiguous-search",
    );
  });

  it("absent ⇒ absent", () => {
    expect(refTier(ref({ path_state: "absent" }))).toBe("absent");
  });

  it("external ⇒ external, its OWN tier — never folded into absent (a deliberate vendor citation is not a failed lookup)", () => {
    expect(refTier(ref({ path_state: "external" }))).toBe("external");
  });

  it("kind === issue ⇒ issue, regardless of path_state", () => {
    expect(refTier(ref({ kind: "issue", path_state: null }))).toBe("issue");
  });

  it("null path_state + symbol hit_unique with exactly one hit ⇒ symbol-unique", () => {
    expect(
      refTier(
        ref({
          kind: "symbol_method",
          path_state: null,
          symbol_state: "hit_unique",
          symbol_hits: [{ path: "a.rb", line_start: 1, line_end: 2, kind: "method", container: null }],
        }),
      ),
    ).toBe("symbol-unique");
  });

  it("null path_state + symbol hit_container_matched with exactly one hit ⇒ symbol-unique", () => {
    expect(
      refTier(
        ref({
          kind: "symbol_const",
          path_state: null,
          symbol_state: "hit_container_matched",
          symbol_hits: [{ path: "a.rb", line_start: 1, line_end: 1, kind: "const", container: "C" }],
        }),
      ),
    ).toBe("symbol-unique");
  });

  it("null path_state + symbol hit_ambiguous with hits ⇒ symbol-ambiguous", () => {
    expect(
      refTier(
        ref({
          kind: "symbol_method",
          path_state: null,
          symbol_state: "hit_ambiguous",
          symbol_hits: [
            { path: "a.rb", line_start: 1, line_end: 2, kind: "method", container: null },
            { path: "b.rb", line_start: 3, line_end: 4, kind: "method", container: null },
          ],
        }),
      ),
    ).toBe("symbol-ambiguous");
  });

  it("null path_state + no_symbol ⇒ symbol-none", () => {
    expect(
      refTier(ref({ kind: "symbol_method", path_state: null, symbol_state: "no_symbol" })),
    ).toBe("symbol-none");
  });

  it("null path_state + hit_unique but zero hits (producer disagreement) ⇒ symbol-none, never a crash", () => {
    expect(
      refTier(
        ref({ kind: "symbol_method", path_state: null, symbol_state: "hit_unique", symbol_hits: [] }),
      ),
    ).toBe("symbol-none");
  });
});
