import { useMemo, useState } from "react";
import { Link } from "react-router";
import { Icon } from "../icons";
import DiffFileAnnotations from "../annotations/DiffFileAnnotations";
import DiffFile from "../diff/DiffFile";
import { useDiff } from "../../hooks/useDiff";
import { useDiffHighlights } from "../../hooks/useDiffHighlights";
import { readerUrl } from "../../lib/breadcrumbs";
import { parseUnifiedDiff } from "../../lib/diff";
import { loadDiffMode, saveDiffMode } from "../../lib/prefs";
import type { DiffMode } from "../../lib/prefs";
import type { FileChange } from "../../api/types";

export interface FileChangeRowProps {
  repo: string;
  file: FileChange;
  /// The per-file diff's `from`/`to` revspecs (`GET /api/diff?path&from=&
  /// to=`) — omit BOTH and set `disabledNote` instead to disable expansion
  /// entirely (a root commit has no `<sha>^` to diff against).
  from?: string;
  to?: string;
  /// When set, the expand toggle is disabled and this note renders inline
  /// next to it (Commit page's root-commit case).
  disabledNote?: string;
  /// The ref to browse this file's CONTENT at when its path is clicked —
  /// the commit page's own sha, or a compare's `to` side.
  browseRef: string;
}

/// One changed-file row shared by the commit page (`routes/Commit.tsx`) and
/// the compare page (`routes/Compare.tsx`): status letter, path (linking
/// into the reader at `browseRef`), insertions/deletions, and an
/// EXPANDABLE inline diff fetched on demand (`useDiff`, gated on
/// `expanded`). Hunk rendering is `DiffFile` (V4.D1) so unified and
/// split share one parsed model. `to` doubles as the diff-comment
/// anchor sha, forwarded to UnifiedHunks. `DiffFileAnnotations` stays
/// below the renderer (independent of unified/split).
export default function FileChangeRow({ repo, file, from, to, disabledNote, browseRef }: FileChangeRowProps) {
  const [expanded, setExpanded] = useState(false);
  const [mode, setMode] = useState<DiffMode>(() => loadDiffMode());
  const disabled = !!disabledNote;
  const diff = useDiff(repo, file.path, expanded && !disabled ? from : undefined, expanded && !disabled ? to : undefined);
  const parsed = diff.data ? parseUnifiedDiff(diff.data.diff) : null;
  const hasRemoves = useMemo(
    () => (parsed ? parsed.hunks.some((h) => h.lines.some((l) => l.kind === "remove")) : false),
    [parsed],
  );
  const highlights = useDiffHighlights(
    parsed ? repo : undefined,
    parsed ? file.path : undefined,
    { oldSha: from, newSha: to },
    { hasRemoves, parsed },
  );

  return (
    <div className="kbc-filechange" data-kbc-filechange={file.path}>
      <div className="kbc-filechange__row">
        <button
          type="button"
          className="kbc-filechange__expand"
          onClick={() => setExpanded((v) => !v)}
          disabled={disabled}
          aria-expanded={expanded}
          aria-label={disabled ? disabledNote : expanded ? "Collapse diff" : "Expand diff"}
          title={disabled ? disabledNote : expanded ? "Collapse diff" : "Expand diff"}
          data-kbc-filechange-toggle
        >
          {disabled ? "—" : <Icon.Chevron className={expanded ? "kbc-twisty is-open" : "kbc-twisty"} />}
        </button>
        <span className="kbc-filechange__status" data-kbc-filechange-letter={file.status}>
          {file.status}
        </span>
        <Link className="kbc-filechange__path" to={readerUrl(repo, file.path, browseRef)}>
          {file.old_path ? `${file.old_path} → ${file.path}` : file.path}
        </Link>
        {disabled && (
          <span className="kbc-filechange__note" data-kbc-filechange-note>
            {disabledNote}
          </span>
        )}
        <span className="kbc-filechange__stats">
          {file.binary ? (
            <span className="kbc-filechange__binary">binary</span>
          ) : (
            <>
              <span className="kbc-filechange__additions">+{file.insertions}</span>{" "}
              <span className="kbc-filechange__deletions">-{file.deletions}</span>
            </>
          )}
        </span>
      </div>
      {expanded &&
        !disabled &&
        (diff.isLoading ? (
          <div className="kbc-diff kbc-diff--loading">Loading diff…</div>
        ) : diff.error ? (
          <div className="kbc-diff kbc-diff--error">Failed to load diff</div>
        ) : parsed ? (
          <>
            <DiffFile
              repo={repo}
              path={file.path}
              parsed={parsed}
              mode={mode}
              onModeChange={(next) => {
                saveDiffMode(next);
                setMode(next);
              }}
              sha={to}
              highlights={highlights}
            />
            {to && <DiffFileAnnotations repo={repo} path={file.path} sha={to} />}
          </>
        ) : null)}
    </div>
  );
}
