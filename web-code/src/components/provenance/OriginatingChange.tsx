import { useMemo } from "react";
import { Link } from "react-router-dom";
import { UNCOMMITTED_SHA } from "../../api/types";
import DiffFile from "../diff/DiffFile";
import { useDiff } from "../../hooks/useDiff";
import { commitUrl } from "../../lib/codeUrl";
import { parseUnifiedDiff } from "../../lib/diff";
import { shortSha } from "../../lib/format";
import { sliceHunksForLine, slicedAsParsed } from "../../lib/hunkSlice";

export interface OriginatingChangeProps {
  repo: string;
  path: string;
  /// Blame region's commit sha (new side of the originating diff).
  sha: string;
  /// Prefer `BlameRegion.previous_sha` when present; falls back to `sha^`.
  previousSha: string | null | undefined;
  /// 1-based NEW-side line (blame `final` line) to slice hunks around.
  line: number;
}

/// R12 — "originating change" section for the why-panel: lazily fetches
/// `GET /api/diff` for (parent → sha, path) and renders just the hunk(s)
/// overlapping the hovered line, capped (~40 lines) with an "open commit"
/// link for the rest.
export default function OriginatingChange({
  repo,
  path,
  sha,
  previousSha,
  line,
}: OriginatingChangeProps) {
  const isUncommitted = sha === UNCOMMITTED_SHA;
  const from = previousSha && previousSha.length > 0 ? previousSha : `${sha}^`;
  const enabled = !isUncommitted && !!sha;

  const diff = useDiff(repo, path, enabled ? from : undefined, enabled ? sha : undefined);
  const slice = useMemo(() => {
    if (!diff.data) return null;
    const parsed = parseUnifiedDiff(diff.data.diff);
    return sliceHunksForLine(parsed, { newLine: line });
  }, [diff.data, line]);

  if (isUncommitted) {
    return (
      <div className="kbc-why__origin" data-kbc-why-origin="uncommitted">
        <div className="kbc-why__origin-title">Originating change</div>
        <p className="kbc-why__absence">uncommitted — no originating commit diff</p>
      </div>
    );
  }

  return (
    <div className="kbc-why__origin" data-kbc-why-origin>
      <div className="kbc-why__origin-title">
        Originating change{" "}
        <Link to={commitUrl(repo, sha)} className="kbc-why__sha" data-kbc-why-origin-commit>
          {shortSha(sha)}
        </Link>
      </div>
      {diff.isLoading ? (
        <div className="kbc-why__timeline-loading" data-kbc-why-origin-loading>
          Loading diff…
        </div>
      ) : diff.isError ? (
        <p className="kbc-why__absence" data-kbc-why-origin-error>
          Couldn&apos;t load originating diff
        </p>
      ) : slice && slice.hunks.length > 0 ? (
        <>
          <div className="kbc-why__origin-diff" data-kbc-why-origin-diff>
            <DiffFile path={path} parsed={slicedAsParsed(slice)} mode="unified" />
          </div>
          {slice.truncated && (
            <p className="kbc-why__origin-more">
              Truncated to {slice.keptLines} lines —{" "}
              <Link to={commitUrl(repo, sha)} data-kbc-why-origin-open>
                open commit
              </Link>{" "}
              for the rest.
            </p>
          )}
        </>
      ) : (
        <p className="kbc-why__absence" data-kbc-why-origin-empty>
          No overlapping hunk for line {line}
        </p>
      )}
    </div>
  );
}
