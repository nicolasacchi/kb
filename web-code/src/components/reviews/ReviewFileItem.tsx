import { useMemo, useState } from "react";
import { codeUrl } from "../../lib/codeUrl";
import { Link, useNavigate } from "react-router-dom";
import type { ReviewFileRow, ReviewRiskFile } from "../../api/types";
import DiffFile from "../diff/DiffFile";
import BlastRadiusStrip from "./BlastRadiusStrip";
import RiskBadge from "./RiskBadge";
import { reviewDiffHref, isLoopbackRefusal, LOOPBACK_HINT, msg } from "./ReviewHeader";
import { useDiff } from "../../hooks/useDiff";
import { useDiffHighlights } from "../../hooks/useDiffHighlights";
import { useReviewDiffComments } from "../../hooks/useReviewComments";
import { useDeleteReviewViewed, usePutReviewViewed } from "../../hooks/useReviews";
import { parseUnifiedDiff } from "../../lib/diff";
import { loadDiffMode, saveDiffMode, type DiffMode } from "../../lib/prefs";
import { highlightSegments, type MatchRange } from "../../lib/speedSearch";
import { toast } from "../../lib/toast";

export function statusGlyph(status: string): string {
  const s = status.toUpperCase();
  if (s.startsWith("A") || s === "ADDED") return "A";
  if (s.startsWith("D") || s === "DELETED") return "D";
  if (s.startsWith("R") || s === "RENAMED") return "R";
  if (s.startsWith("C") || s === "COPIED") return "C";
  if (s.startsWith("M") || s === "MODIFIED") return "M";
  return status.slice(0, 1) || "?";
}

export interface ReviewFileItemProps {
  repo: string;
  reviewId: number;
  ps: string;
  file: ReviewFileRow;
  ranges: readonly MatchRange[];
  isOpen: boolean;
  riskAvailable: boolean;
  riskRow: ReviewRiskFile | undefined;
  baseSha: string | undefined;
  tipSha: string | undefined;
  onOpen: (path: string) => void;
}

export default function ReviewFileItem({
  repo,
  reviewId,
  ps,
  file: f,
  ranges,
  isOpen,
  riskAvailable,
  riskRow,
  baseSha,
  tipSha,
  onOpen,
}: ReviewFileItemProps) {
  const navigate = useNavigate();
  const putViewed = usePutReviewViewed(repo, reviewId);
  const delViewed = useDeleteReviewViewed(repo, reviewId);
  const checked = f.viewed && !f.viewed_stale;

  return (
    <div className="kbc-review__file" data-kbc-review-file={f.path}>
      <div
        className="kbc-review__file-row"
        onClick={() => onOpen(f.path)}
        role="button"
        tabIndex={0}
        onKeyDown={(e) => {
          if (e.key === "Enter" || e.key === " ") {
            e.preventDefault();
            onOpen(f.path);
          }
        }}
        data-kbc-review-file-row={f.path}
      >
        <span className="kbc-review__file-status" data-kbc-review-file-status={f.status}>
          {statusGlyph(f.status)}
        </span>
        <span className="kbc-review__file-path">
          {highlightSegments(
            f.old_path ? `${f.old_path} → ${f.path}` : f.path,
            ranges,
          ).map((seg, i) =>
            seg.hit ? (
              <mark key={i} className="kbc-speed-hit">
                {seg.text}
              </mark>
            ) : (
              <span key={i}>{seg.text}</span>
            ),
          )}
        </span>
        <span className="kbc-review__file-stats">
          <span className="kbc-review__file-add">+{f.additions}</span>{" "}
          <span className="kbc-review__file-del">−{f.deletions}</span>
        </span>
        {riskAvailable && (
          <RiskBadge file={f} riskRow={riskRow} />
        )}
        {f.viewed_stale && (
          <span className="kbc-review__stale" data-kbc-review-stale title="changed since viewed">
            changed
          </span>
        )}
        {f.open_annotations > 0 && (
          <span className="kbc-review__file-ann" data-kbc-review-file-ann>
            {f.open_annotations}
          </span>
        )}
        <Link
          to={reviewDiffHref(repo, reviewId, f.path)}
          className="kbc-rdiff__open-file"
          data-kbc-rdiff-open-file={f.path}
          onClick={(e) => e.stopPropagation()}
        >
          Open full page
        </Link>
        <label
          className="kbc-review__file-viewed"
          onClick={(e) => {
            e.stopPropagation();
          }}
        >
          <input
            type="checkbox"
            checked={checked}
            onChange={() => {
              void (async () => {
                try {
                  if (f.viewed && !f.viewed_stale) {
                    await delViewed.mutateAsync(f.path);
                  } else {
                    await putViewed.mutateAsync({
                      path: f.path,
                      blob_sha: f.blob_sha,
                    });
                  }
                } catch (err) {
                  toast.err(
                    isLoopbackRefusal(err)
                      ? LOOPBACK_HINT
                      : `couldn't update viewed: ${msg(err)}`,
                  );
                }
              })();
            }}
            aria-label={checked ? "mark unviewed" : "mark viewed"}
            data-kbc-review-viewed={f.path}
          />
        </label>
      </div>
      {isOpen && baseSha && tipSha && (
        <div className="kbc-review__file-diff" data-kbc-review-file-diff={f.path}>
          <BlastRadiusStrip
            repo={repo}
            path={f.path}
            baseSha={baseSha}
            tipSha={tipSha}
            onOpenImpact={(sym) => {
              // Navigate to the tip file at the symbol — the reader owns
              // `gi`; this is a deep-link affordance. V70-A6: through the ONE
              // builder (`lib/codeUrl.ts`, root CLAUDE.md #35). This was the
              // last hand-assembled `/r/${repo}/…` template literal outside
              // `lib/`, and `nav/rawUrls.test.ts` now keeps it that way.
              navigate(codeUrl({ repo, path: sym.path, ref: tipSha, line: sym.line }));
            }}
          />
          <ReviewAwareDiff
            repo={repo}
            reviewId={reviewId}
            ps={ps}
            path={f.path}
            from={baseSha}
            to={tipSha}
          />
        </div>
      )}
    </div>
  );
}

/// Same `useDiff` data DiffView used; DiffFile now receives review comments.
/// Exported so the Files-tab tree (V76-R2b) can mount the inline diff under
/// the picked row without re-rendering the whole `ReviewFileItem` chrome.
export function ReviewAwareDiff({
  repo,
  reviewId,
  ps,
  path,
  from,
  to,
}: {
  repo: string;
  reviewId: number;
  ps: string;
  path: string;
  from: string;
  to: string;
}) {
  const { data, isLoading, error } = useDiff(repo, path, from, to);
  const parsed = useMemo(() => (data ? parseUnifiedDiff(data.diff) : null), [data]);
  const [mode, setMode] = useState<DiffMode>(() => loadDiffMode());
  const hasRemoves = useMemo(
    () => (parsed ? parsed.hunks.some((h) => h.lines.some((l) => l.kind === "remove")) : false),
    [parsed],
  );
  const highlights = useDiffHighlights(
    parsed ? repo : undefined,
    parsed ? path : undefined,
    { oldSha: from, newSha: to },
    { hasRemoves },
  );
  const comments = useReviewDiffComments(repo, reviewId, ps, path);

  if (isLoading) return <div className="kbc-diff kbc-diff--loading">Loading diff…</div>;
  if (error) return <div className="kbc-diff kbc-diff--error">Failed to load diff</div>;
  if (!parsed) return null;

  return (
    <DiffFile
      path={path}
      parsed={parsed}
      mode={mode}
      onModeChange={(next) => {
        saveDiffMode(next);
        setMode(next);
      }}
      highlights={highlights}
      comments={comments}
    />
  );
}
