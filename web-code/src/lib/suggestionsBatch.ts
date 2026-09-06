// PRR-U5+U6 (addendum-2 §F UI — suggestions batch card) — pure helpers over
// the already-fetched `review-comments/1` payload (`useReviewComments`,
// shared cache with `ReviewThreadsCard`/`ReviewSidePanel` — no new fetch)
// and the `POST /api/annotations/apply-batch` 409 verify-phase verdicts
// (`suggestions.rs`'s `BatchVerdict`).
import type { ApplyBatchVerdict, ReviewComment, ReviewCommentsOut } from "../api/types";

export interface UnappliedSuggestionRow {
  id: string;
  path: string;
  line: number | null;
  /// First line of the replacement, truncated — a compact preview, not the
  /// full suggestion text (the row is a selection list, not an editor).
  preview: string;
  orphaned: boolean;
}

const PREVIEW_MAX = 60;

function previewOf(replacement: string): string {
  const first = replacement.split("\n")[0] ?? "";
  return first.length > PREVIEW_MAX ? `${first.slice(0, PREVIEW_MAX)}…` : first;
}

/// Every unapplied suggestion across the whole review, flattened out of the
/// per-path groups. Only TOP-LEVEL comments can carry a suggestion
/// (`validate_suggestion_target` server-side rejects a reply) — the nested
/// `replies[]` are never scanned. Orphaned threads are still LISTED (an
/// honest row, not silently dropped) but flagged `orphaned: true` so the
/// card can warn that an apply attempt will likely 409.
export function unappliedSuggestionRows(response: ReviewCommentsOut): UnappliedSuggestionRow[] {
  const rows: UnappliedSuggestionRow[] = [];
  for (const group of response.groups) {
    for (const c of group.comments) {
      if (!c.suggestion || c.suggestion.applied) continue;
      rows.push({
        id: c.id,
        path: group.path,
        line: c.resolution.line,
        preview: previewOf(c.suggestion.replacement),
        orphaned: c.resolution.orphaned,
      });
    }
  }
  return rows;
}

/// Same source, keyed by id — used to resolve a 409 verdict's `id` back to
/// a display row (path/line) for the inline per-id error list.
export function unappliedSuggestionById(response: ReviewCommentsOut): Map<string, ReviewComment> {
  const byId = new Map<string, ReviewComment>();
  for (const group of response.groups) {
    for (const c of group.comments) {
      if (c.suggestion && !c.suggestion.applied) byId.set(c.id, c);
    }
  }
  return byId;
}

export interface BatchVerdictSummary {
  okCount: number;
  errCount: number;
  /// `error.kind` → count, so the card can render "2 drift · 1 overlap"
  /// instead of a flat error count.
  errorsByKind: Record<string, number>;
}

/// Pure aggregate over a 409 response's `verdicts[]` — never scored, just
/// counted (mirrors `CiChecksCard.tsx`'s `summarizeChecks` idiom: one pure
/// reducer the component renders from, no logic duplicated inline).
export function summarizeBatchVerdicts(verdicts: ApplyBatchVerdict[]): BatchVerdictSummary {
  let okCount = 0;
  let errCount = 0;
  const errorsByKind: Record<string, number> = {};
  for (const v of verdicts) {
    if (v.ok) {
      okCount += 1;
      continue;
    }
    errCount += 1;
    const kind = v.error?.kind ?? "unknown";
    errorsByKind[kind] = (errorsByKind[kind] ?? 0) + 1;
  }
  return { okCount, errCount, errorsByKind };
}

/// Only the FAILING verdicts, in their original order — what the inline
/// per-id error list actually renders (a passing verdict on a 409 response
/// carries no new information: the whole batch was rejected together).
export function failingVerdicts(verdicts: ApplyBatchVerdict[]): ApplyBatchVerdict[] {
  return verdicts.filter((v) => !v.ok);
}
