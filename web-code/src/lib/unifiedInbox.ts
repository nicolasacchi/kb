// S2-A — kb-code v6.0 "One Inbox" (design-s2.md §S2-A): pure view-model
// helpers for `routes/Inbox.tsx`. No fetching, no React — mirrors
// `lib/reviewInbox.ts`'s own "pure functions over already-fetched wire
// rows" discipline, so every branch here is testable with zero mocking
// (`unifiedInbox.test.ts`).

import { codeUrl } from "./codeUrl";
import type { KbLaneReason, UnifiedInboxAnnotationRow, UnifiedInboxKb } from "../api/types";

// --- Badge math --------------------------------------------------------

/// Badge count = reviews.length + annotations.length (+ kb attention when
/// the kb lane is available AND carries a desk sub-object) — design doc's
/// own formula, verbatim. `kb.desk` is `null` on an older/degraded
/// response even when `available` is somehow `true` (defensive; the wire
/// contract pairs them, but this function never assumes it).
export function computeInboxBadge(data: {
  reviews: readonly unknown[];
  annotations: readonly unknown[];
  kb: Pick<UnifiedInboxKb, "available" | "desk">;
}): number {
  const kbAttention = data.kb.available && data.kb.desk ? data.kb.desk.attention : 0;
  return data.reviews.length + data.annotations.length + kbAttention;
}

// --- kb-lane degradation -------------------------------------------------

export type KbLaneState =
  | { kind: "available" }
  | { kind: "unavailable"; reason: KbLaneReason | string; label: string };

/// Closed-vocab label for each of the three documented `kb.reason` values,
/// plus an honest fallback for anything else (a future daemon's new reason
/// string) — never thrown, never a blank chip.
const KB_REASON_LABELS: Record<KbLaneReason, string> = {
  disabled: "kb integration disabled",
  unreachable: "kb unreachable",
  sibling_mismatch: "kb/kb-code version mismatch",
};

/// Pure derivation of the "From kb" section's degradation state. A missing
/// `reason` on an `available:false` payload (shouldn't happen per the wire
/// contract, but this is a relay of another process's output) still
/// produces an honest, non-empty label rather than a blank section.
export function kbLaneState(kb: Pick<UnifiedInboxKb, "available" | "reason">): KbLaneState {
  if (kb.available) return { kind: "available" };
  const reason = kb.reason ?? "unreachable";
  const label = (KB_REASON_LABELS as Record<string, string>)[reason] ?? `kb unavailable (${reason})`;
  return { kind: "unavailable", reason, label };
}

// --- Truncation captions --------------------------------------------------

/// `truncated:true` on a kb sub-object → the honest "showing first N"
/// caption; anything else (`false`/`undefined`) → `null` (render nothing).
export function truncationCaption(truncated: boolean | undefined, cap = 50): string | null {
  return truncated ? `showing first ${cap}` : null;
}

// --- Grouping (reviews lane → per-repo InboxList reuse) --------------------

/// `InboxList` (reused VERBATIM, no internal changes) builds every row's
/// link from its own `repo` PROP, not the individual row's `repo` field —
/// fine for its original repo-scoped call site, wrong for a federated,
/// all-repos list. Grouping rows by `repo` at the CALL SITE (one
/// `<InboxList repo={r} rows={...} />` per group) keeps `InboxList.tsx`
/// untouched while still linking each row into the right repo's Room.
/// Group order = first-appearance order in `rows` (stable, matches the
/// server's own `sort_inbox_rows` ranking — a row is never reordered
/// relative to its neighbors, only bucketed).
export function groupByRepo<T extends { repo: string }>(rows: readonly T[]): Array<{ repo: string; rows: T[] }> {
  const order: string[] = [];
  const byRepo = new Map<string, T[]>();
  for (const row of rows) {
    let bucket = byRepo.get(row.repo);
    if (!bucket) {
      bucket = [];
      byRepo.set(row.repo, bucket);
      order.push(row.repo);
    }
    bucket.push(row);
  }
  return order.map((repo) => ({ repo, rows: byRepo.get(repo) as T[] }));
}

// --- Link builders ----------------------------------------------------------

/// Working-tree questions lane → the Reader at path/line, via the existing
/// one-builder-rule helper (`lib/codeUrl.ts`). `line` absent/`null`
/// (defensive — see `UnifiedInboxAnnotationRow.line`'s doc) omits the
/// `line=` param rather than emitting a bogus `0`.
export function annotationReaderUrl(row: Pick<UnifiedInboxAnnotationRow, "repo" | "path" | "line">): string {
  return codeUrl({ repo: row.repo, path: row.path, line: row.line ?? undefined });
}

/// The kb lane's OUT link: `{kbBase}/a/{kb}/{rel}` — kb's own path-form
/// artifact permalink grammar (`web/src/lib/artifactHref.ts`'s header doc:
/// "`/a/<kb>/<rel>`", each path segment percent-encoded individually so a
/// literal `/` inside a filename can't be confused with a separator, same
/// discipline `lib/codeUrl.ts`'s `encodePathSegments` uses on this side).
/// `kbBase` is the caller's resolved `kb_public_url` (identity, S2-A's own
/// pull — see `routes/Inbox.tsx`); a trailing slash on it is trimmed so the
/// join never double-slashes.
export function kbArtifactUrl(kbBase: string, kb: string, rel: string): string {
  const trimmed = kbBase.replace(/\/$/, "");
  const encRel = rel
    .split("/")
    .filter((s) => s !== "")
    .map(encodeURIComponent)
    .join("/");
  return `${trimmed}/a/${encodeURIComponent(kb)}/${encRel}`;
}

/// Same permalink, `?panel=comments` appended so the deep link opens
/// straight to the thread — kb's own `ArtifactHrefOpts.panel` grammar
/// (`web/src/lib/artifactHref.ts`), reproduced here rather than imported
/// (kb-code has no cross-package import of kb's `web/` — separate npm
/// packages, same boundary `components/icons.tsx`'s header doc notes for
/// the analogous EmptyState case).
export function kbCommentUrl(kbBase: string, kb: string, rel: string): string {
  return `${kbArtifactUrl(kbBase, kb, rel)}?panel=comments`;
}
