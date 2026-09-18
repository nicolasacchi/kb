import { useMemo } from "react";
import { Link } from "react-router";
import { useReviewComments } from "../../hooks/useReviewComments";
import { useReviewFindings } from "../../hooks/useReviews";
import { findingsByAnnotationId } from "../../lib/diffFindings";
import { formatUnixSeconds } from "../../lib/format";
import EmptyState from "../EmptyState";
import { FindingRow } from "./FindingCard";
import PromoteToFinding from "./PromoteToFinding";
import { threadHref } from "./ReviewThreadsCard";

export interface ReviewFileThreadsPanelProps {
  repo: string;
  reviewId: number;
  reviewTitle?: string;
  /// The reader's currently-open file. Empty string is treated as "no
  /// file open" (the same convention `path=""` carries elsewhere in this
  /// crate — an annotation with `path === ""` is the review-LEVEL general
  /// group, never a file's own).
  path: string;
  onOpenComposer: () => void;
}

/// V80-M2 — the reader rail's real Review tab body (replacing M3's
/// "threads arrive with M2" stub): the current review's threads for the
/// OPEN FILE, from the SAME `useReviewComments` query the Room's own
/// `ReviewThreadsCard`/diff renderers already share (this component fetches
/// no `ps` — the review's latest, matching the composer's own default), a
/// header caption naming whether the file is one the target patchset's
/// diff touches, and a "Comment here" door into `AnnotationsPanel`'s
/// composer (which preselects this same review).
export default function ReviewFileThreadsPanel({
  repo,
  reviewId,
  reviewTitle,
  path,
  onOpenComposer,
}: ReviewFileThreadsPanelProps) {
  const q = useReviewComments(repo, reviewId, undefined, true);
  const group = path ? q.data?.groups.find((g) => g.path === path) : undefined;
  const threads = group?.comments ?? [];
  const ps = q.data?.ps;
  // V80-M5 — the SAME `findingsByAnnotationId` join `ReviewThreadsCard`
  // uses, so a promoted (or composer-authored) manual finding renders
  // identically in the reader rail as it does in the Room.
  const findingsQ = useReviewFindings(repo, reviewId, {
    ps: ps !== undefined ? String(ps) : undefined,
  });
  const findingsByAnnId = useMemo(
    () => findingsByAnnotationId(findingsQ.data?.findings ?? []),
    [findingsQ.data],
  );

  const caption = q.isLoading
    ? "Loading…"
    : !path
      ? null
      : group
        ? group.in_diff
          ? "This file is in the review's diff."
          : `Not in the diff — comments here anchor to ps ${ps ?? "?"}'s tip.`
        : "No comments yet on this file — comment here to start one.";

  return (
    <div className="kbc-review-rail" data-kbc-review-rail>
      {caption && (
        <div className="kbc-review-rail__caption" data-kbc-review-rail-caption>
          {caption}
        </div>
      )}
      {threads.length === 0 ? (
        <EmptyState
          variant="rail"
          title="No threads on this file yet"
          hint={
            reviewTitle
              ? `Comments filed here show up in the Room of ${reviewTitle} too.`
              : "Comments filed here show up in the Room too."
          }
        />
      ) : (
        <ul className="kbc-review-rail__threads" data-kbc-review-rail-threads>
          {threads.map((t) => {
            // V80-M5 — same swap `ReviewThreadsCard` makes: a
            // human-promoted (or composer-authored) manual finding renders
            // as a compact `FindingRow` instead of the plain thread <li>.
            const linkedFinding = findingsByAnnId.get(t.id) ?? null;
            if (linkedFinding && linkedFinding.origin === "manual") {
              return (
                <li key={t.id} data-kbc-review-rail-thread={t.id}>
                  <FindingRow repo={repo} reviewId={reviewId} finding={linkedFinding} ps={ps !== undefined ? String(ps) : undefined} />
                </li>
              );
            }
            return (
              <li
                key={t.id}
                className={"kbc-review-rail__thread" + (t.resolved ? " is-resolved" : "")}
                data-kbc-review-rail-thread={t.id}
              >
                <p className="kbc-review-rail__body">{t.body}</p>
                <div className="kbc-review-rail__meta">
                  <span>
                    {t.author} · {formatUnixSeconds(t.updated_at)}
                  </span>
                  {t.resolution.orphaned && (
                    <span
                      className="kbc-review-rail__badge kbc-review-rail__badge--orphan"
                      data-kbc-review-rail-orphan
                    >
                      orphaned
                    </span>
                  )}
                  {t.resolved && (
                    <span className="kbc-review-rail__badge kbc-review-rail__badge--resolved">resolved</span>
                  )}
                </div>
                <Link
                  to={threadHref(repo, reviewId, t, ps !== undefined ? String(ps) : undefined)}
                  data-kbc-review-rail-open-diff
                >
                  Open in diff
                </Link>
                <PromoteToFinding repo={repo} reviewId={reviewId} thread={t} />
              </li>
            );
          })}
        </ul>
      )}
      {/* ONE "Comment here" door regardless of empty/non-empty — always
          the same `data-kbc-review-rail-compose` target, rather than
          `EmptyState`'s own generic `action` (which carries no attribute
          a caller could hang a selector off). */}
      <button
        type="button"
        className="kbc-review-rail__compose"
        onClick={onOpenComposer}
        data-kbc-review-rail-compose
      >
        Comment here
      </button>
    </div>
  );
}
