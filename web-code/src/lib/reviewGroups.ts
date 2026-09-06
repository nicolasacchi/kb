// Session-grouped branch review (Phase G3 — the headline of the
// review-workflow milestone): reviewing an agent branch as the narrative of
// the CONVERSATIONS that made it, not a flat commit list. Pure grouping
// logic only — no fetch, no React — the Compare page (`routes/Compare.tsx`)
// is the sole caller, feeding it `ComparePageResponse.commits` fetched with
// `?attribution=true` (`CompareCommitOut[]`, `api/types.ts`).

import type { Confidence, CompareCommitOut, LadderAttribution } from "../api/types";

/// One session's (or the unattributed bucket's) commits, in review order.
export interface CommitGroup {
  /// `null` = the "no recorded session" bucket — every commit with no
  /// attributed session (either `attribution` wasn't resolved at all, or it
  /// came back an honest `confidence: "none"` miss) pools HERE rather than
  /// being silently dropped or scattered as one singleton group per commit.
  sessionId: string | null;
  /// The group's header label — an attributed session's `display_name`
  /// (falling back to its own `session_id` when no display name was
  /// resolved), `null` for the unattributed bucket (nothing to name).
  displayName: string | null;
  /// The STRONGEST confidence (`trailer` > `exact` > `fuzzy` > `none`)
  /// among the group's own commits — the header's confidence badge.
  /// `null` for the unattributed bucket (no badge to show).
  confidence: Confidence | null;
  /// Kept in the SAME relative order `commits` arrived in (the compare
  /// page's own newest-first convention) — grouping only decides which
  /// bucket a commit lands in and how buckets order against each other,
  /// never re-sorts within one.
  commits: CompareCommitOut[];
  /// The earliest (oldest) `author_time` among the group's own commits —
  /// what `groupCommitsBySession` orders groups by, see that function's doc.
  earliestAuthorTime: number;
}

const CONFIDENCE_RANK: Record<Confidence, number> = { trailer: 3, exact: 2, fuzzy: 1, none: 0 };

/// The attributed session key for one commit — an attribution only counts
/// once it's both NOT an honest `"none"` miss AND actually carries a
/// `session_id` (a `fuzzy`/`squash-*` hit built from the commit-map feed can
/// in principle omit it — see `join::ladder::Attribution`'s own doc); every
/// commit failing either check pools into the shared `null` bucket.
function sessionKeyOf(c: CompareCommitOut): string | null {
  const a = c.attribution;
  if (a && a.confidence !== "none" && a.session_id) return a.session_id;
  return null;
}

/// The number of DISTINCT attributed sessions across `commits` — the input
/// to `defaultGroupedView`'s threshold.
export function distinctAttributedSessionCount(commits: CompareCommitOut[]): number {
  const ids = new Set<string>();
  for (const c of commits) {
    const key = sessionKeyOf(c);
    if (key !== null) ids.add(key);
  }
  return ids.size;
}

/// Session-grouped review is the DEFAULT presentation once there's an
/// actual narrative to tell apart — two or more DISTINCT attributed
/// sessions. Below that (zero, or exactly one, session — nothing to
/// distinguish a "narrative" from a plain list) flat is the honest default;
/// the operator can still opt into grouped view via the toggle either way.
export function defaultGroupedView(commits: CompareCommitOut[]): boolean {
  return distinctAttributedSessionCount(commits) >= 2;
}

/// Group `commits` (a compare page's `commits[]`, fetched with
/// `?attribution=true`) by attributed session — see `CommitGroup`'s own doc
/// for the bucketing rule and `sessionKeyOf` for what counts as
/// "attributed." Groups are ordered by their own EARLIEST commit's
/// `author_time` ascending — the group containing the oldest work opens the
/// review, the conversations-in-order narrative the headline promises.
export function groupCommitsBySession(commits: CompareCommitOut[]): CommitGroup[] {
  const order: Array<string | null> = [];
  const buckets = new Map<string | null, CompareCommitOut[]>();
  for (const c of commits) {
    const key = sessionKeyOf(c);
    let bucket = buckets.get(key);
    if (!bucket) {
      bucket = [];
      buckets.set(key, bucket);
      order.push(key);
    }
    bucket.push(c);
  }

  const groups: CommitGroup[] = order.map((key) => {
    const groupCommits = buckets.get(key) as CompareCommitOut[];
    let earliest = groupCommits[0].author_time;
    let best: LadderAttribution | null = null;
    for (const c of groupCommits) {
      if (c.author_time < earliest) earliest = c.author_time;
      if (key !== null && c.attribution) {
        if (!best || CONFIDENCE_RANK[c.attribution.confidence] > CONFIDENCE_RANK[best.confidence]) {
          best = c.attribution;
        }
      }
    }
    return {
      sessionId: key,
      displayName: best ? (best.display_name ?? best.session_id ?? null) : null,
      confidence: best ? best.confidence : null,
      commits: groupCommits,
      earliestAuthorTime: earliest,
    };
  });

  return groups.slice().sort((a, b) => a.earliestAuthorTime - b.earliestAuthorTime);
}
