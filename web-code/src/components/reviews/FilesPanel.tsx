import { useMemo } from "react";
import type { ReviewFileRow, ReviewRiskFile } from "../../api/types";
import EmptyState from "../EmptyState";
import { Icon } from "../icons";
import { ReviewAwareDiff } from "./ReviewFileItem";
import ReviewFileTree from "./ReviewFileTree";
import BlastRadiusStrip from "./BlastRadiusStrip";
import { useReviewRisk } from "../../hooks/useBehavioral";
import { useDeleteReviewViewed, usePutReviewViewed } from "../../hooks/useReviews";
import { useSyntax } from "../../hooks/useSyntax";
import { toast } from "../../lib/toast";
import { isLoopbackRefusal, LOOPBACK_HINT, msg } from "./ReviewHeader";
import { speedFilterItems } from "../../lib/speedSearch";
import { codeUrl, reviewDiffHref } from "../../lib/codeUrl";
import { useNavigate } from "react-router";
import type { MapRowState } from "../../lib/reviewMapColumn";

export type FileSort = "diff" | "risk" | "path";

export interface FilesPanelProps {
  repo: string;
  reviewId: number;
  files: ReviewFileRow[];
  loading: boolean;
  error: Error | null;
  /// V80-F1 — still externally driven ONLY (the rail's per-thread-group
  /// "open file" quick action, `ReviewDetail.tsx`'s `openFile`/`expanded`):
  /// a click on the tree ROW ITSELF no longer sets this — see `openInDiff`
  /// below — so `expanded` can only ever be set from outside this
  /// component now. Kept (not retired) because that rail surface still
  /// uses the inline preview.
  expanded: string | null;
  pathFilter: string;
  onPathFilter: (q: string) => void;
  fileSort: FileSort;
  onFileSort: (s: FileSort) => void;
  baseSha: string | undefined;
  tipSha: string | undefined;
  ps: string;
}

export default function FilesPanel({
  repo,
  reviewId,
  files,
  loading,
  error,
  expanded,
  pathFilter,
  onPathFilter,
  fileSort,
  onFileSort,
  baseSha,
  tipSha,
  ps,
}: FilesPanelProps) {
  const riskQ = useReviewRisk(repo, reviewId);
  const riskAvailable = riskQ.isSuccess && riskQ.data != null;
  const riskByPath = useMemo(() => {
    const m = new Map<string, ReviewRiskFile>();
    for (const f of riskQ.data?.files ?? []) m.set(f.path, f);
    return m;
  }, [riskQ.data]);

  const navigate = useNavigate();
  const syntaxQ = useSyntax();
  const putViewed = usePutReviewViewed(repo, reviewId);
  const delViewed = useDeleteReviewViewed(repo, reviewId);
  // V80-F1 — the Files tab's own row click NAVIGATES to the full-page diff
  // (design of record, `web-code/CLAUDE.md`'s Review diff v2 section) —
  // the inline cockpit expansion below (`expandedPath`/`expandedContent`)
  // is retired from THIS interaction; it survives only for the rail's
  // externally-driven `expanded` (see the prop doc above). `ps` is a
  // single-patchset selector here (never a range, `ReviewDetail.tsx`'s
  // `psQuery`), so `"latest"` is the only non-numeric value it ever holds.
  function openInDiff(path: string) {
    navigate(reviewDiffHref(repo, reviewId, path, ps === "latest" ? undefined : { ps: Number(ps) }));
  }
  const stateByPath = useMemo(() => {
    const m = new Map<string, MapRowState>();
    for (const f of files) {
      m.set(f.path, {
        path: f.path,
        viewed: f.viewed,
        viewedStale: f.viewed_stale,
        openComments: f.open_annotations,
        findings: 0,
        drafts: 0,
        noise: [],
      });
    }
    return m;
  }, [files]);
  const filtered = useMemo(() => {
    const hits = speedFilterItems(
      files,
      pathFilter,
      (f) => f.path + (f.old_path ? ` ${f.old_path}` : ""),
    );
    if (fileSort === "diff" || !riskAvailable) return hits;
    if (fileSort === "path") {
      return [...hits].sort((a, b) => a.item.path.localeCompare(b.item.path));
    }
    // risk first: higher score first; null risk last (never treated as 0).
    return [...hits].sort((a, b) => {
      const ra = riskByPath.get(a.item.path)?.risk?.score;
      const rb = riskByPath.get(b.item.path)?.risk?.score;
      const aNull = ra == null;
      const bNull = rb == null;
      if (aNull && bNull) return a.item.path.localeCompare(b.item.path);
      if (aNull) return 1;
      if (bNull) return -1;
      if (rb !== ra) return (rb as number) - (ra as number);
      return a.item.path.localeCompare(b.item.path);
    });
  }, [files, pathFilter, fileSort, riskAvailable, riskByPath]);

  if (loading) {
    return <div className="kbc-reader__hint">Loading files…</div>;
  }
  if (error) {
    return (
      <div className="kbc-reader__hint kbc-reader__hint--error">
        {error.message}
      </div>
    );
  }
  if (files.length === 0) {
    return <EmptyState icon={<Icon.List />} title="No files changed" hint="This patchset is empty." />;
  }

  return (
    <>
      <div className="kbc-review__files-tools">
        <input
          type="search"
          className="kbc-review__filter"
          value={pathFilter}
          onChange={(e) => onPathFilter(e.target.value)}
          placeholder="Filter paths…"
          aria-label="filter file paths"
          data-kbc-review-filter
        />
        {riskAvailable && (
          <label className="kbc-review__sort-lab">
            Sort
            <select
              className="kbc-review__sort"
              value={fileSort}
              onChange={(e) => onFileSort(e.target.value as FileSort)}
              aria-label="file sort order"
              data-kbc-review-sort
            >
              <option value="diff">diff order</option>
              <option value="risk">risk first</option>
              <option value="path">path</option>
            </select>
          </label>
        )}
      </div>
      <div className="kbc-review__files" data-kbc-review-files>
        <ReviewFileTree
          files={filtered.map((h) => h.item)}
          stateByPath={stateByPath}
          currentPath={expanded ?? ""}
          syntaxRows={syntaxQ.data?.rows}
          onPick={openInDiff}
          rowAttr="files"
          riskAvailable={riskAvailable}
          riskByPath={riskByPath}
          onToggleViewed={(f) => {
            void (async () => {
              try {
                if (f.viewed && !f.viewed_stale) await delViewed.mutateAsync(f.path);
                else await putViewed.mutateAsync({ path: f.path, blob_sha: f.blob_sha });
              } catch (err) {
                toast.err(
                  isLoopbackRefusal(err) ? LOOPBACK_HINT : `couldn't update viewed: ${msg(err)}`,
                );
              }
            })();
          }}
          expandedPath={expanded}
          expandedContent={
            expanded && baseSha && tipSha ? (
              <div className="kbc-review__file-diff" data-kbc-review-file-diff={expanded}>
                <BlastRadiusStrip
                  repo={repo}
                  path={expanded}
                  baseSha={baseSha}
                  tipSha={tipSha}
                  onOpenImpact={(sym) => {
                    navigate(codeUrl({ repo, path: sym.path, ref: tipSha, line: sym.line }));
                  }}
                />
                <ReviewAwareDiff
                  repo={repo}
                  reviewId={reviewId}
                  ps={ps}
                  path={expanded}
                  from={baseSha}
                  to={tipSha}
                />
              </div>
            ) : null
          }
        />
      </div>
    </>
  );
}
