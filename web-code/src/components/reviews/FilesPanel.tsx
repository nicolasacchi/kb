import { useMemo } from "react";
import type { ReviewFileRow, ReviewRiskFile } from "../../api/types";
import EmptyState from "../EmptyState";
import { Icon } from "../icons";
import ReviewFileItem from "./ReviewFileItem";
import { useReviewRisk } from "../../hooks/useBehavioral";
import { speedFilterItems } from "../../lib/speedSearch";

export type FileSort = "diff" | "risk" | "path";

export interface FilesPanelProps {
  repo: string;
  reviewId: number;
  files: ReviewFileRow[];
  loading: boolean;
  error: Error | null;
  expanded: string | null;
  onOpenFile: (path: string) => void;
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
  onOpenFile,
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
        {filtered.map(({ item: f, ranges }) => (
          <ReviewFileItem
            key={f.path}
            repo={repo}
            reviewId={reviewId}
            ps={ps}
            file={f}
            ranges={ranges}
            isOpen={expanded === f.path}
            riskAvailable={riskAvailable}
            riskRow={riskAvailable ? riskByPath.get(f.path) : undefined}
            baseSha={baseSha}
            tipSha={tipSha}
            onOpen={onOpenFile}
          />
        ))}
      </div>
    </>
  );
}
