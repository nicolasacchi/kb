import { useMemo, useState } from "react";
import type { ReviewInterdiffFile, ReviewInterdiffOut } from "../../api/types";
import DiffFile from "../diff/DiffFile";
import RangeDiffTable from "./RangeDiffTable";
import { statusGlyph } from "./ReviewFileItem";
import { useDiff } from "../../hooks/useDiff";
import { parseUnifiedDiff } from "../../lib/diff";
import { loadDiffMode, saveDiffMode } from "../../lib/prefs";
import type { DiffMode } from "../../lib/prefs";

export interface InterdiffPanelProps {
  repo: string;
  loading: boolean;
  error: Error | null;
  data: ReviewInterdiffOut | undefined;
  fromTipSha: string | undefined;
  toTipSha: string | undefined;
}

export default function InterdiffPanel({
  repo,
  loading,
  error,
  data,
  fromTipSha,
  toTipSha,
}: InterdiffPanelProps) {
  if (loading) return <div className="kbc-reader__hint">Loading interdiff…</div>;
  if (error) return <div className="kbc-reader__hint kbc-reader__hint--error">{error.message}</div>;
  if (!data) return null;
  return (
    <div data-kbc-review-interdiff>
      <h2 className="kbc-review__card-title">
        Files changed (ps{data.from} → ps{data.to})
      </h2>
      {data.files.length === 0 ? (
        <p className="kbc-review__card-empty">No file-level changes between these tips.</p>
      ) : (
        <div className="kbc-review__files" data-kbc-review-interdiff-files>
          {data.files.map((f) => (
            <InterdiffFileRow
              key={f.path}
              repo={repo}
              file={f}
              fromTipSha={fromTipSha}
              toTipSha={toTipSha}
            />
          ))}
        </div>
      )}
      <h2 className="kbc-review__card-title" style={{ marginTop: 16 }}>
        Range-diff
      </h2>
      {data.range_diff.pairs.length === 0 ? (
        <p className="kbc-review__card-empty">No range-diff pairs.</p>
      ) : (
        <RangeDiffTable
          repo={repo}
          pairs={data.range_diff.pairs}
          truncated={data.range_diff.truncated}
        />
      )}
    </div>
  );
}

function InterdiffFileRow({
  repo,
  file: f,
  fromTipSha,
  toTipSha,
}: {
  repo: string;
  file: ReviewInterdiffFile;
  fromTipSha: string | undefined;
  toTipSha: string | undefined;
}) {
  const [expanded, setExpanded] = useState(false);
  return (
    <>
      <div className="kbc-review__file-row" data-kbc-review-interdiff-file={f.path}>
        <button
          type="button"
          className="kbc-filechange__expand"
          onClick={() => setExpanded((v) => !v)}
          aria-expanded={expanded}
          title={expanded ? "Collapse diff" : "Expand diff"}
          data-kbc-interdiff-file={f.path}
        >
          {expanded ? "▾" : "▸"}
        </button>
        <span className="kbc-review__file-status">{statusGlyph(f.status)}</span>
        <span className="kbc-review__file-path">
          {f.old_path ? `${f.old_path} → ${f.path}` : f.path}
        </span>
        <span className="kbc-review__file-stats">
          <span className="kbc-review__file-add">+{f.additions}</span>{" "}
          <span className="kbc-review__file-del">−{f.deletions}</span>
        </span>
      </div>
      {expanded && fromTipSha && toTipSha && (
        <div className="kbc-review__file-diff">
          <InterdiffFileDiff repo={repo} path={f.path} from={fromTipSha} to={toTipSha} />
        </div>
      )}
    </>
  );
}

function InterdiffFileDiff({
  repo,
  path,
  from,
  to,
}: {
  repo: string;
  path: string;
  from: string;
  to: string;
}) {
  const diff = useDiff(repo, path, from, to);
  const parsed = useMemo(() => (diff.data ? parseUnifiedDiff(diff.data.diff) : null), [diff.data]);
  const [mode, setMode] = useState<DiffMode>(() => loadDiffMode());

  if (diff.isLoading) return <div className="kbc-diff kbc-diff--loading">Loading diff…</div>;
  if (diff.error) return <div className="kbc-diff kbc-diff--error">Failed to load diff</div>;
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
    />
  );
}
