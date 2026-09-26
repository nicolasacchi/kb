import { useMemo } from "react";
import { Link } from "react-router";
import { UNCOMMITTED_SHA } from "../../api/types";
import DiffFile from "../diff/DiffFile";
import { useDiff } from "../../hooks/useDiff";
import { useDiffHighlights } from "../../hooks/useDiffHighlights";
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
  // The rendered diff is the SLICE, not the full parse — hand the hook
  // that same projection so the base side is fetched only when a remove
  // line actually survives the cap.
  const sliced = useMemo(() => (slice ? slicedAsParsed(slice) : null), [slice]);
  const hasRemoves = useMemo(
    () => (sliced ? sliced.hunks.some((h) => h.lines.some((l) => l.kind === "remove")) : false),
    [sliced],
  );
  // V80-H1 — BOTH refs are real: `sha` is the blame region's commit and
  // `from` is either `BlameRegion.previous_sha` or `<sha>^` — a `Revspec`
  // the `/api/file` reader resolves the same way `/api/diff` does, so the
  // committed arm reads two pinned blobs. `hasAdds` does not touch that
  // arm (a pinned tip is never gated on it).
  //
  // `hasAdds: false` is what closes the UNCOMMITTED arm: its `newSha` is
  // `undefined`, and the tip of a diff with no `to` is the WORKING TREE
  // (`tipSideEnabled`) — an `UNCOMMITTED_SHA` "commit" has no blob, so a
  // read there would paint today's on-disk text, not this change. The
  // hook's `hasAdds` is opt-OUT, so omitting it would open that path
  // silently. Today `sliced` is ALSO `null` on that arm, which makes
  // `repo`/`path` undefined and stops the query before it runs — incidental
  // to the diff fetch being disabled, so the opt carries the guarantee
  // on its own.
  const highlights = useDiffHighlights(
    sliced ? repo : undefined,
    sliced ? path : undefined,
    { oldSha: enabled ? from : undefined, newSha: enabled ? sha : undefined },
    { hasRemoves, hasAdds: false, parsed: sliced },
  );

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
            {/* `slice` non-null above ⇔ `sliced` non-null (one derivation). */}
            <DiffFile path={path} parsed={sliced!} mode="unified" highlights={highlights} />
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
