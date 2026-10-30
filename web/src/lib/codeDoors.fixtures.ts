// Shared fixtures for the citation-door tests (builder + parity). Not imported
// by any app code.
import type { CodeRefOut } from "../api/generated/CodeRefOut";
import type { DocLensOut, ResolvedCodeRef } from "../api/doclens";

export function ref(p: Partial<CodeRefOut> & Pick<CodeRefOut, "ordinal" | "kind" | "raw">): CodeRefOut {
  return {
    group: null,
    path_hint: null,
    line_start: null,
    line_end: null,
    line_spans: null,
    symbol_container: null,
    symbol_member: null,
    context: "",
    context_tokens: [],
    declared: false,
    ...p,
  };
}

export function row(p: Partial<ResolvedCodeRef> & Pick<ResolvedCodeRef, "ordinal" | "kind" | "raw">): ResolvedCodeRef {
  return {
    group: null,
    declared: false,
    path_hint: null,
    line_hint: null,
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
    ...p,
  };
}

export function lensOf(refs: ResolvedCodeRef[], repo = "app"): DocLensOut {
  return { repo: { name: repo }, refs } as unknown as DocLensOut;
}

