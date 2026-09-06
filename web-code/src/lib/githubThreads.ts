// PRR-F — GitHub thread import UI (design-addendum-2.md §A). Pure helpers
// over `GET /api/reviews/{id}/github-threads` (`kbc-github-threads/1`):
// per-path filtering/indexing (mirrors `lib/reviewComments.ts`'s
// `indexThreads` shape so the diff renderer's by-line lookup is the SAME
// key grammar), and the Timeline-tab interleave (`lib/reviewTimeline.ts`'s
// module doc: "the timeline route doesn't know GitHub; merge client-side").

import type { GithubThread, GithubThreadComment } from "../api/types";
import { threadLineKey, type DiffSide } from "./reviewComments";
import type { TimelineRow } from "./reviewTimeline";

/// GitHub's `created_at` is an ISO-8601 string (the raw GitHub API value,
/// never converted server-side — `PrCommentOut.created_at: String`), unlike
/// every other timestamp in this app's wire types (unix seconds). `null`
/// on anything `Date.parse` can't read — an honest "unknown time" rather
/// than a fabricated `0` (which would sort first, before everything real).
export function parseGithubTimestamp(iso: string | null | undefined): number | null {
  if (!iso) return null;
  const ms = Date.parse(iso);
  return Number.isFinite(ms) ? Math.floor(ms / 1000) : null;
}

/// Every root thread in `threads` whose OWN `path` is `path` — replies
/// don't carry their own `path` (they inherit the root's), so filtering is
/// done on the root only; the whole thread (root + replies) travels
/// together into whichever bucket the root belongs to.
export function githubThreadsForPath(threads: readonly GithubThread[], path: string): GithubThread[] {
  return threads.filter((t) => t.path === path);
}

/// Index a path's resolved threads by the SAME `threadLineKey` grammar
/// `lib/reviewComments.ts`'s `indexThreads` uses, so a diff renderer can
/// look GitHub threads up next to local ones with one shared key builder.
/// A thread with neither `resolved` nor a recognizable `side` degrades to
/// "new" (GitHub's own default when `side` is absent — same convention the
/// server's `position_for` maps `Some("LEFT") → "old", _ => "new"`).
export function indexGithubThreadsByLine(
  threads: readonly GithubThread[],
  path: string,
): Map<string, GithubThread[]> {
  const byLine = new Map<string, GithubThread[]>();
  for (const t of githubThreadsForPath(threads, path)) {
    if (!t.resolved) continue;
    const side: DiffSide = t.side === "LEFT" ? "old" : "new";
    const key = threadLineKey(path, side, t.resolved.line);
    const list = byLine.get(key);
    if (list) list.push(t);
    else byLine.set(key, [t]);
  }
  return byLine;
}

/// General (issue-style) + orphaned (couldn't re-resolve) threads for one
/// path — the same "honest orphan, never a guessed line" ladder the local
/// comment orphan section uses, rendered in its own foot section.
export function githubOrphansForPath(threads: readonly GithubThread[], path: string): GithubThread[] {
  return githubThreadsForPath(threads, path).filter((t) => t.general || t.orphaned);
}

/// Total root-thread count for a path (ThreadsCard's "GitHub (N)" chip
/// count, and the diff file-header rollup) — root threads only, replies
/// don't count twice.
export function githubThreadCountForPath(threads: readonly GithubThread[], path: string): number {
  return githubThreadsForPath(threads, path).length;
}

export function githubThreadCountTotal(threads: readonly GithubThread[]): number {
  return threads.length;
}

/// Timeline row — one PER comment (root + each reply), each carrying its
/// own author/time/link. `icon: "github"` (an ADDITIVE `TimelineIconKind`
/// value this module owns — `TimelinePanel.tsx`'s icon map gains one entry,
/// no change to `lib/reviewTimeline.ts`'s closed server-kind switch).
function githubCommentRow(c: GithubThreadComment, isReply: boolean): TimelineRow {
  const at = parseGithubTimestamp(c.created_at) ?? 0;
  const author = c.author ?? "someone";
  return {
    at,
    kind: "github_comment",
    icon: "github",
    label: isReply ? `${author} replied on GitHub` : `${author} commented on GitHub`,
    detail: c.body.length > 140 ? `${c.body.slice(0, 140)}…` : c.body,
    external: c.html_url ?? undefined,
  };
}

/// Flatten every thread's root + replies into `TimelineRow`s — pure, no
/// sorting here (the caller merges against the server rows and sorts once,
/// `mergeTimelineRows` below).
export function githubTimelineRows(threads: readonly GithubThread[]): TimelineRow[] {
  const rows: TimelineRow[] = [];
  for (const t of threads) {
    rows.push(githubCommentRow(t, false));
    for (const r of t.replies) rows.push(githubCommentRow(r, true));
  }
  return rows;
}

/// Interleave server timeline rows with client-derived GitHub rows by `at`
/// ascending (newest LAST, matching `TimelinePanel`'s own "reading order =
/// story order" doc) — a stable merge (`Array.sort` is stable per spec) so
/// two rows sharing the same `at` keep their relative construction order,
/// same determinism posture `review_timeline.rs`'s own server-side sort
/// documents for its stable tiebreak.
export function mergeTimelineRows(
  serverRows: readonly TimelineRow[],
  githubRows: readonly TimelineRow[],
): TimelineRow[] {
  return [...serverRows, ...githubRows].sort((a, b) => a.at - b.at);
}
