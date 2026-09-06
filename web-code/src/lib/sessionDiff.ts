// Pure rendering transforms over `SessionDiff` (W3.5's wire shape, W4.4's
// `/session/:sid/diff` route + the why-panel's "sibling files" section).
// `routes/SessionDiff.tsx` and `components/provenance/WhyPanel.tsx` are the
// two callers — kept here, not inline, so the transforms are testable
// without mounting either.

import type { CommitEntryOut, CommitFileOut, Segment, SessionDiff } from "../api/types";

function relativize(path: string, repoRoot?: string): string {
  if (!repoRoot) return path;
  const prefix = repoRoot.endsWith("/") ? repoRoot : `${repoRoot}/`;
  return path.startsWith(prefix) ? path.slice(prefix.length) : path;
}

/// Every file this session touched (diffed commits' numstat files, unioned
/// with uncommitted evidence groups' files), deduped and excluding
/// `excludePath` — the why-panel's best-effort "sibling files" section
/// (neither `/api/why` nor `/api/story` carry a files list of their own, so
/// this is sourced from the session's own change-set instead). Uncommitted
/// segment files travel as ABSOLUTE paths server-side (`sessiondiff`'s
/// module doc); `repoRoot` (the configured repo's absolute working-tree
/// path, from `GET /api/repos`) relativizes them so they dedupe/exclude
/// correctly against the (repo-relative) committed files and `excludePath`
/// — omitted, they're left absolute (a degraded but still honest render,
/// never silently dropped).
export function siblingFiles(diff: SessionDiff, excludePath: string, repoRoot?: string): string[] {
  const seen = new Set<string>();
  for (const seg of diff.segments) {
    if (seg.kind === "commits") {
      // `c.files` is OMITTED (not `[]`) on the wire whenever it's empty —
      // true for unresolved commits AND diffed ones with zero changed files
      // (server's `skip_serializing_if = "Vec::is_empty"`, `types.ts`'s
      // `CommitEntryOut` doc) — default it here rather than trust presence.
      for (const c of seg.commits) for (const f of c.files ?? []) seen.add(f.path);
    } else if (seg.kind === "uncommitted") {
      for (const f of seg.files) seen.add(relativize(f, repoRoot));
    }
  }
  seen.delete(excludePath);
  return Array.from(seen).sort();
}

/// One file's line-delta, formatted for a compact list row.
export function commitFileStat(file: CommitFileOut): string {
  if (file.binary) return "binary";
  return `+${file.insertions} -${file.deletions}`;
}

export function commitShortSha(commit: CommitEntryOut): string {
  return commit.sha.slice(0, 7);
}

/// The session-diff page's header stat line.
export function totalsSummary(totals: SessionDiff["totals"]): string {
  const commitPart =
    totals.commits_diffed === totals.commits
      ? `${totals.commits} commit${totals.commits === 1 ? "" : "s"}`
      : `${totals.commits_diffed}/${totals.commits} commits diffed`;
  return `${commitPart} · ${totals.files} file${totals.files === 1 ? "" : "s"} · +${totals.insertions} -${totals.deletions}`;
}

export function isPromptSegment(seg: Segment): seg is Extract<Segment, { kind: "prompt" }> {
  return seg.kind === "prompt";
}

export function isCommitsSegment(seg: Segment): seg is Extract<Segment, { kind: "commits" }> {
  return seg.kind === "commits";
}

export function isUncommittedSegment(seg: Segment): seg is Extract<Segment, { kind: "uncommitted" }> {
  return seg.kind === "uncommitted";
}
