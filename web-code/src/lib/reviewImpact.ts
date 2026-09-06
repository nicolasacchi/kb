// PRR-F — Reviewer X-ray chip text (design-ui.md §12.2). Pure derivation,
// same "small lib module, one state-matrix-shaped fn" precedent
// `lib/diagnostics.ts`'s `diagnosticsChipText` establishes for its own
// sibling file-header chip.

import type { ReviewImpactChangedSymbol, ReviewImpactFileOut } from "../api/types";

/// "12 callers · 2 in this diff" — `null` when there's nothing worth
/// showing (unsupported language, zero callers, absent/still loading) —
/// same "named absence, not a fabricated zero" posture `diagnosticsChipText`
/// documents for its own chip.
export function impactChipText(data: ReviewImpactFileOut | null | undefined): string | null {
  if (!data || !data.lang_supported || data.callers_total === 0) return null;
  const callers = `${data.callers_total} caller${data.callers_total === 1 ? "" : "s"}`;
  return `${callers} · ${data.callers_in_diff} in this diff`;
}

/// The symbol the chip's click targets — the one with the MOST total
/// callers (ties broken by line ascending, a full deterministic order).
/// "click → the usages view" (design-ui.md §12.2) needs exactly ONE
/// landing symbol even when a file's diff touched several.
export function topChangedSymbol(
  data: ReviewImpactFileOut | null | undefined,
): ReviewImpactChangedSymbol | null {
  if (!data || data.changed_symbols.length === 0) return null;
  return [...data.changed_symbols].sort(
    (a, b) => b.callers_total - a.callers_total || a.line - b.line,
  )[0];
}
