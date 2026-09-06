// DCB W2.B — pure tier-selection logic for `components/lens/RefRow.tsx`,
// extracted for direct unit coverage (`refTier.test.ts`) — same "pure
// helpers live in `lib/`, network I/O + JSX stay in the component" split
// `lib/hierarchyState.ts` already establishes for the hierarchy panel.
//
// Decision 3's tiered UX: every tier here is chosen from the RESOLUTION
// overlay (`path_state`/`candidate_count`/`symbol_state`/`symbol_hits`),
// NEVER from `path_hint`/`line_hint` (the doc's own unverified citation).

import type { CodeLensRef } from "../api/types";

/// Mirrors `crate::doclens::AMBIGUITY_INLINE_MAX` (Rust `usize = 3`) —
/// Decision 3's tiering boundary: >3 candidates renders as a count + a
/// search deep-link instead of an inline list.
export const AMBIGUITY_INLINE_MAX = 3;

export type RefTier =
  | "unique"
  | "ambiguous-inline"
  | "ambiguous-search"
  | "absent"
  // DCB-W2.B.R fix 6 — its OWN tier, split out of "absent": a `path_state:
  // "external"` ref (a gem/vendor path) is a DELIBERATE citation the daemon
  // never even attempts to resolve, not a failed lookup — folding it into
  // "absent" told the same lie the base spec's own `refTier` originally
  // did ("didn't resolve" implies a citation that SHOULD have matched).
  // Parity with kb's `web/src/components/CodeRefsSection.tsx` `RefRow`'s
  // own `is-external` treatment.
  | "external"
  | "issue"
  // (deviation, recorded — the base spec's own `refTier` union omitted a
  // symbol arm, silently assuming every non-issue/non-external ref carries
  // a path_hint. A `symbol_method`/`symbol_const` ref with NO path_hint at
  // all (D5 — `path_state: null`) is a real, common case: doc-lens still
  // resolves it against the repo's symbol table via `symbol_state`/
  // `symbol_hits`. Split into three sub-tiers, mirroring kb's OWN
  // already-reviewed treatment of the identical wire (`web/src/components/
  // CodeRefsSection.tsx`'s `RefRow`, W1.D.R) rather than inventing a
  // second, possibly-diverging shape.)
  | "symbol-unique"
  | "symbol-ambiguous"
  | "symbol-none";

/// Pure tier-selection logic.
export function refTier(r: CodeLensRef): RefTier {
  if (r.kind === "issue") return "issue";
  // `"indexing"` is a per-repo SCORECARD state (`ScorecardRepoRow.state`)
  // and can never appear as a `path_state` — no branch here checks for it.
  if (r.path_state === "external") return "external";
  if (r.path_state === "present") return "unique";
  if (r.path_state === "ambiguous") {
    return r.candidate_count <= AMBIGUITY_INLINE_MAX ? "ambiguous-inline" : "ambiguous-search";
  }
  if (r.path_state === "absent") return "absent";
  // `path_state === null` (D5) — no path hint at all.
  if (
    (r.symbol_state === "hit_unique" || r.symbol_state === "hit_container_matched") &&
    r.symbol_hits.length === 1
  ) {
    return "symbol-unique";
  }
  if (r.symbol_state === "hit_ambiguous" && r.symbol_hits.length > 0) return "symbol-ambiguous";
  return "symbol-none";
}
