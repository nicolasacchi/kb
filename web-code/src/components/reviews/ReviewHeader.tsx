import { Link, useNavigate } from "react-router-dom";
import { ApiError } from "../../api/client";
import type { ReviewDetail, ReviewDetailPr, ReviewFileRow, ReviewPatchset, ReviewReport } from "../../api/types";
import { useConfirm } from "../ConfirmProvider";
import {
  useDeleteReview,
  usePatchReview,
  useSnapshotReview,
} from "../../hooks/useReviews";
import { reviewDiffHref, reviewsUrl } from "../../lib/codeUrl";
import { shortSha } from "../../lib/format";
import { toast } from "../../lib/toast";
import AgentVerdictCard from "./AgentVerdictCard";
import DialecticLedger from "./DialecticLedger";
import PrChip from "./PrChip";
import StalenessBanner from "./StalenessBanner";
import VerdictBar from "./VerdictBar";

// V70-A3S — this used to be a SECOND, weaker copy of `codeUrl.ts`'s
// `reviewDiffHref` (no `opts`/`finding=`/`overlay=` support), predating
// PRR-U3's canonicalization into `codeUrl.ts` (see that module's own
// header comment on the duplication's history). Re-exported here (rather
// than inlined at each call site) so the four existing `from
// "./ReviewHeader"` importers (`ReviewFileItem.tsx`, `FindingCard.tsx`,
// `ReviewThreadsCard.tsx`, `ReadingOrderPanel.tsx`) keep working unchanged
// — ONE implementation, same import path. Imported above (not just
// re-exported) so this file's OWN JSX below can still call it directly.
export { reviewDiffHref };

export function isLoopbackRefusal(e: unknown): boolean {
  return e instanceof ApiError && e.status === 404;
}

export const LOOPBACK_HINT =
  "This action is loopback-only — open kb-code from the machine running kb-code-server.";

export function msg(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

export interface ReviewHeaderProps {
  repo: string;
  id: number;
  review: ReviewDetail;
  activePs: ReviewPatchset | undefined;
  files: ReviewFileRow[];
  /// PRR-U2 — the authored report, when one exists (`undefined` otherwise);
  /// `AgentVerdictCard` renders nothing when neither this nor a verdict
  /// signal is present, so passing `undefined` degrades to today's
  /// plain-`VerdictBar` header exactly (§2 S2: "No report → strip collapses
  /// to the plain VerdictBar exactly as today").
  report?: ReviewReport;
}

export default function ReviewHeader({ repo, id, review, activePs, files, report }: ReviewHeaderProps) {
  const navigate = useNavigate();
  const confirm = useConfirm();
  const snapshot = useSnapshotReview(repo);
  const patch = usePatchReview(repo);
  const del = useDeleteReview(repo);

  const viewedCount = files.filter((f) => f.viewed && !f.viewed_stale).length;
  const filesCount = files.length;
  const pct = filesCount > 0 ? Math.round((viewedCount / filesCount) * 100) : 0;
  const title = review.title?.trim() || review.head_ref;
  const reviewPr = review as ReviewDetailPr;
  const latestPs = review.patchsets.length > 0 ? review.patchsets[review.patchsets.length - 1].ps_number : null;

  async function onSnapshot() {
    try {
      await snapshot.mutateAsync(id);
      toast.ok("Patchset captured");
    } catch (e) {
      toast.err(isLoopbackRefusal(e) ? LOOPBACK_HINT : `snapshot failed: ${msg(e)}`);
    }
  }

  async function onToggleState() {
    const next = review.state === "open" ? "closed" : "open";
    try {
      await patch.mutateAsync({ id, input: { state: next } });
      toast.ok(next === "closed" ? "Review closed" : "Review reopened");
    } catch (e) {
      toast.err(isLoopbackRefusal(e) ? LOOPBACK_HINT : `couldn't update state: ${msg(e)}`);
    }
  }

  async function onDelete() {
    const ok = await confirm({
      title: `Delete review #${id}?`,
      body: "Removes the review row, patchset refs, and viewed tracking. This can't be undone.",
      confirmLabel: "Delete",
    });
    if (!ok) return;
    try {
      await del.mutateAsync(id);
      toast.ok("Review deleted");
      navigate(reviewsUrl(repo));
    } catch (e) {
      toast.err(isLoopbackRefusal(e) ? LOOPBACK_HINT : `couldn't delete: ${msg(e)}`);
    }
  }

  return (
    <header className="kbc-review__head">
      <div className="kbc-review__head-row">
        <Link to={reviewsUrl(repo)} className="kbc-review__back" data-kbc-review-back>
          ← Reviews
        </Link>
        <h1 className="kbc-review__title" data-kbc-review-title>
          {title}
        </h1>
        <span className={`kbc-review__state kbc-review__state--${review.state}`} data-kbc-review-state>
          {review.state}
        </span>
        <div
          className="kbc-review__progress"
          title={`${viewedCount}/${filesCount} viewed`}
          data-kbc-review-progress={`${viewedCount}/${filesCount}`}
        >
          <div className="kbc-review__progress-bar" style={{ width: `${pct}%` }} />
          <span className="kbc-review__progress-label">
            {viewedCount}/{filesCount}
          </span>
        </div>
      </div>
      <div className="kbc-review__refs">
        <code>{review.head_ref}</code>
        <span aria-hidden="true">→</span>
        <code>{review.base_ref}</code>
        {activePs && (
          <span className="kbc-reviews__row-time">
            · ps{activePs.ps_number} @ {shortSha(activePs.tip_sha_full || activePs.tip_sha)} ·{" "}
            {activePs.commit_count} commit{activePs.commit_count === 1 ? "" : "s"}
          </span>
        )}
      </div>
      <PrChip repo={repo} reviewId={id} review={reviewPr} />
      <StalenessBanner review={review} latestPs={latestPs} />
      <div className="kbc-review__actions">
        <Link
          to={reviewDiffHref(repo, id)}
          className="kbc-review__action"
          data-kbc-rdiff-open
        >
          Open full page
        </Link>
        <Link
          to={`${reviewDiffHref(repo, id)}?tour=1`}
          className="kbc-review__action"
          title="Guided tour — walk reading-order files interleaved with severity-ordered findings"
          data-kbc-review-tour-start
        >
          Guided tour
        </Link>
        <button
          type="button"
          className="kbc-review__action"
          onClick={() => void onSnapshot()}
          disabled={snapshot.isPending || review.state !== "open"}
          data-kbc-review-snapshot
        >
          {snapshot.isPending ? "Capturing…" : "Snapshot now"}
        </button>
        <button
          type="button"
          className="kbc-review__action"
          onClick={() => void onToggleState()}
          disabled={patch.isPending}
          data-kbc-review-toggle-state
        >
          {review.state === "open" ? "Close" : "Reopen"}
        </button>
        <button
          type="button"
          className="kbc-review__action kbc-review__action--danger"
          onClick={() => void onDelete()}
          disabled={del.isPending}
          data-kbc-review-delete
        >
          Delete
        </button>
      </div>
      {report ? (
        <div className="kbc-verdicts" data-kbc-verdict-dialectic>
          <AgentVerdictCard report={report} />
          <VerdictBar repo={repo} reviewId={id} review={review} />
        </div>
      ) : (
        <VerdictBar repo={repo} reviewId={id} review={review} />
      )}
      {report && <DialecticLedger repo={repo} reviewId={id} review={review} report={report} />}
    </header>
  );
}
