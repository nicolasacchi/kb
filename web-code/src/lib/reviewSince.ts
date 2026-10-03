// v0.44 F9 — pure helpers over `GET /api/reviews/{id}/since`
// (`kbc-review-since/1`). The daemon derives the author delta from each
// patchset's diff against ITS OWN base; everything here only words it and
// filters by it. Nothing here is a verdict: a label says what changed, never
// that anything is fixed or approved, and the daemon carries no verdict
// forward (D20).
import type { SinceReport } from "../api/types";

/// Hunks the author added or dropped between the two patchsets.
export function authorChangeCount(r: SinceReport): number {
  return r.author_delta.new_hunks + r.author_delta.gone_hunks;
}

/// "ps4 · rebase-only since your verdict (0 author changes)" — the
/// PatchsetStrip label. `from_source` says whether the start was the
/// verdict's patchset or an explicit one.
export function sinceLabel(r: SinceReport): string {
  const since = r.from_source === "verdict" ? "your verdict" : `ps${r.from.ps}`;
  const head = `ps${r.to.ps}`;
  if (r.from.ps === r.to.ps) return `${head} · same patchset as ${since}`;
  const n = authorChangeCount(r);
  if (r.rebase_only) {
    return r.bases.moved
      ? `${head} · rebase-only since ${since} (0 author changes)`
      : `${head} · no changes since ${since} (0 author changes)`;
  }
  return `${head} · ${n} author change${n === 1 ? "" : "s"} since ${since}${
    r.bases.moved ? " (rebased)" : ""
  }`;
}

/// Paths where the author added or dropped at least one hunk.
export function authorChangedPaths(r: SinceReport): Set<string> {
  const out = new Set<string>();
  for (const p of r.paths) {
    if (p.new > 0 || p.gone > 0) out.add(p.path);
  }
  return out;
}

/// The "author changes only" filter over interdiff file rows (tip-to-tip,
/// so upstream files are in them). A rename row matches on either name.
export function filterAuthorFiles<T extends { path: string; old_path: string | null }>(
  files: T[],
  r: SinceReport | undefined,
): T[] {
  if (!r) return files;
  const keep = authorChangedPaths(r);
  return files.filter((f) => keep.has(f.path) || (f.old_path != null && keep.has(f.old_path)));
}

/// Show the strip label only when it says something the verdict chip does
/// not: a verdict exists and a LATER patchset has landed.
export function sinceApplies(verdictPs: number | null | undefined, latestPs: number | null): boolean {
  return verdictPs != null && latestPs != null && verdictPs < latestPs;
}

/// The interdiff file rows to show for a `?ps=a..b` range: all of them, or —
/// with the author-only switch on AND the author delta loaded — only the
/// paths where the author added or dropped a hunk. Until the delta arrives
/// the switch shows everything rather than an empty list that would read as
/// "no author changes".
export function rangeFileRows<T extends { path: string; old_path: string | null }>(
  files: T[],
  since: SinceReport | undefined,
  authorOnly: boolean,
): T[] {
  return authorOnly ? filterAuthorFiles(files, since) : files;
}

/// "3 author changes · base moved" — the caption beside the switch.
export function authorOnlyCaption(r: SinceReport): string {
  const n = authorChangeCount(r);
  return `${n} author change${n === 1 ? "" : "s"}${r.bases.moved ? " · base moved" : ""}`;
}

/// v0.44 F9b — may the Room offer "Re-affirm on psN"? Only for a verdict
/// that went stale because a LATER patchset landed, when the daemon says that
/// patchset is rebase-only against the verdict's own (no author hunk new or
/// gone) AND the base actually moved. It is a prompt for a human click, never
/// an automatic carry-forward (D20): the click records an ordinary verdict.
export function canReaffirm(
  verdictPs: number | null | undefined,
  stale: boolean | undefined,
  latestPs: number | null,
  since: SinceReport | undefined,
): boolean {
  if (!stale || !sinceApplies(verdictPs, latestPs) || !since) return false;
  return since.rebase_only && since.bases.moved && since.from.ps === verdictPs && since.to.ps === latestPs;
}

/// "Re-affirm approved on ps4" - the button's label.
export function reaffirmLabel(state: string, latestPs: number): string {
  const word =
    state === "approve" ? "approval" : state === "request-changes" ? "changes requested" : "comment";
  return `Re-affirm ${word} on ps${latestPs}`;
}
