import { Fragment, useEffect, useRef, useState } from "react";
import type { GithubThread } from "../../api/types";
import type { DiffLine, ParsedDiff } from "../../lib/diff";
import type { DiagnosticGutterMark } from "../../lib/diagnostics";
import { paintLine, spansForLine, type DiffHighlights, type LineSpan } from "../../lib/diffHighlight";
import { severityRank, threadVisibleInOverlay } from "../../lib/diffFindings";
import {
  orphansAt,
  threadLineKey,
  threadsAt,
  type DiffCommentsApi,
  type DiffSide,
} from "../../lib/reviewComments";
import HunkStrip, { type HunkView } from "./HunkStrip";
import DiffLineComposer from "../annotations/DiffLineComposer";
import DiffLineComposerV2 from "./DiffLineComposerV2";
import DiffThread from "./DiffThread";
import GithubDiffCard from "../reviews/GithubDiffCard";

function useCoarsePointer(): boolean {
  const query = "(pointer: coarse)";
  const [coarse, setCoarse] = useState<boolean>(() =>
    typeof window !== "undefined" && typeof window.matchMedia === "function"
      ? window.matchMedia(query).matches
      : false,
  );
  useEffect(() => {
    if (typeof window === "undefined" || typeof window.matchMedia !== "function") return;
    const mql = window.matchMedia(query);
    const onChange = () => setCoarse(mql.matches);
    onChange();
    mql.addEventListener("change", onChange);
    return () => mql.removeEventListener("change", onChange);
  }, []);
  return coarse;
}

/// Unified (flat) hunk renderer. Extracted from `DiffView` (Phase C-SPA)
/// and moved under `components/diff/` (V4.D1) so `DiffFile` can swap it
/// with `SplitHunks`. Header/stats live on `DiffFileHeader` — this file
/// is hunk rows only. Comment-button pattern (`data-kbc-diff-comment-line`
/// on the NEW-side gutter) is byte-compatible with the previous
/// `DiffHunks.tsx` when `comments` is absent.
export interface UnifiedHunksProps {
  path: string;
  parsed: ParsedDiff;
  /// When BOTH are given (`FileChangeRow`'s own call, which always diffs
  /// against ONE resolved commit), every NEW-side line number becomes a
  /// "Comment at this commit" affordance. Omitted by `DiffView` (arbitrary
  /// from/to, not tied to one resolved commit).
  repo?: string;
  sha?: string;
  highlights?: DiffHighlights | null;
  /// V4.C4 — review-scoped threads + both-gutter compose. When set, the
  /// legacy repo/sha composer is not used.
  comments?: DiffCommentsApi | null;
  /// PRR-U9 — NEW-side-line → severity mark, passed ONLY while the diff's
  /// overlay selector is in the `"diagnostics"` lane (`DiffFile`'s own
  /// doc); `null`/absent renders no marks, same "no structural difference,
  /// just an empty map" posture the thread marks above use.
  diagnosticsByLine?: Map<number, DiagnosticGutterMark> | null;
  /// PRR-F (design-addendum-2.md §A) — GitHub-origin threads for this file,
  /// keyed the SAME way `comments.byLine` is (`threadLineKey`), passed ONLY
  /// while the overlay is in the `"github"` lane (`DiffFile`'s own doc).
  githubByLine?: Map<string, GithubThread[]> | null;
  githubOrphans?: GithubThread[];
  /// V73-K2a — one entry per `parsed.hunks` entry, index-aligned. When
  /// absent (Commit/Compare/SessionDiff, which have no review behind
  /// them) this renderer behaves exactly as it did before diff v2: the
  /// bare `@@ … @@` header row and the wire's own lines.
  hunkViews?: readonly HunkView[] | null;
  onHunkFold?: (hunkIdx: number) => void;
  onHunkViewed?: (hunkIdx: number) => void;
  onHunkExpand?: (hunkIdx: number, dir: "up" | "down") => void;
  /// The cursor's hunk index — `j`/`k` and `?hunk=` both land here.
  currentHunk?: number | null;
}

function lineSpans(highlights: DiffHighlights | null | undefined, line: DiffLine): LineSpan[] | undefined {
  if (line.kind === "remove") return spansForLine(highlights, "old", line.oldLine, line.text);
  return spansForLine(highlights, "new", line.newLine, line.text);
}

function PaintedText({ text, spans }: { text: string; spans: LineSpan[] | undefined }) {
  const segs = paintLine(text, spans);
  if (segs.length === 1 && !segs[0].cls) {
    return <span className="kbc-diff__text">{text}</span>;
  }
  return (
    <span className="kbc-diff__text">
      {segs.map((s, i) =>
        s.cls ? (
          <span key={i} className={s.cls} data-kbc-hl>
            {s.text}
          </span>
        ) : (
          s.text
        ),
      )}
    </span>
  );
}

/// PRR-U3 — the gutter mark for a line's (already overlay-filtered) thread
/// set: `null` (nothing to show), `"comment"` (neutral dot), or a severity
/// string (colored tick, the WORST among the line's visible findings).
function lineMark(
  threads: readonly { id: string }[],
  comments: DiffCommentsApi,
): "comment" | string | null {
  if (threads.length === 0) return null;
  let bestSeverity: string | null = null;
  let hasComment = false;
  for (const t of threads) {
    const f = comments.findingsById.get(t.id);
    if (f) {
      if (bestSeverity === null || severityRank(f.severity) < severityRank(bestSeverity)) {
        bestSeverity = f.severity;
      }
    } else {
      hasComment = true;
    }
  }
  return bestSeverity ?? (hasComment ? "comment" : null);
}

function GutterMark({ mark }: { mark: "comment" | string | null }) {
  if (!mark) return null;
  const isFinding = mark !== "comment";
  return (
    <span
      className={
        "kbc-diff__thread-mark" +
        (isFinding ? ` kbc-diff__thread-mark--${mark}` : " kbc-diff__thread-mark--comment")
      }
      aria-hidden="true"
      data-kbc-thread-mark={mark}
    />
  );
}

/// PRR-U9 — the diagnostics gutter mark, NEW side only (lip diagnostics
/// have no notion of an "old side" — a live-tree provider result, see
/// `api/types.ts`'s `DiagnosticsOut` doc). Rendered ALONGSIDE `GutterMark`
/// (both can be present on the same line), not swapped in for it.
function DiagGutterMark({ mark }: { mark: DiagnosticGutterMark | null | undefined }) {
  if (!mark) return null;
  return (
    <span
      className={`kbc-diff__diag-mark kbc-diff__diag-mark--${mark.severity}`}
      title={mark.title}
      aria-hidden="true"
      data-kbc-diag-mark={mark.severity}
    />
  );
}

function lineClass(kind: DiffLine["kind"]): string {
  switch (kind) {
    case "add":
      return "kbc-diff__line kbc-diff__line--add";
    case "remove":
      return "kbc-diff__line kbc-diff__line--remove";
    default:
      return "kbc-diff__line";
  }
}

type ComposeAt = { side: DiffSide; line: number };

export default function UnifiedHunks({
  path,
  parsed,
  repo,
  sha,
  highlights,
  comments,
  diagnosticsByLine,
  githubByLine,
  githubOrphans,
  hunkViews,
  onHunkFold,
  onHunkViewed,
  onHunkExpand,
  currentHunk,
}: UnifiedHunksProps) {
  const reviewMode = !!comments;
  const canComment = !reviewMode && !!repo && !!sha;
  // Which NEW-side line currently has its inline composer open — at most
  // one at a time across the WHOLE file's diff (opening a second one
  // closes the first), mirroring the reader's own single-composer-at-a-
  // time posture.
  const [composerLine, setComposerLine] = useState<number | null>(null);
  const [compose, setCompose] = useState<ComposeAt | null>(null);
  const [picked, setPicked] = useState<{ hi: number; li: number } | null>(null);
  const coarse = useCoarsePointer();
  const rowTap = coarse && reviewMode;
  const seenComposeToken = useRef<number | undefined>(undefined);

  useEffect(() => {
    const req = comments?.compose;
    if (!req) return;
    if (req.token !== undefined && req.token === seenComposeToken.current) return;
    seenComposeToken.current = req.token;
    setCompose({ side: req.side, line: req.line });
  }, [comments?.compose]);

  // PRR-U3 — orphans respect the overlay filter too (design-ui.md §S3:
  // "Orphaned findings keep the stripe in the existing orphan section").
  const orphans = comments
    ? orphansAt(comments, path).filter((t) =>
        threadVisibleInOverlay(comments.findingsById.has(t.id), comments.overlay),
      )
    : [];

  return (
    <>
      {parsed.hunks.map((hunk, hi) => {
        const view = hunkViews?.[hi] ?? null;
        // The context dial / expand buttons hand back a WIDER line list;
        // with no view (the non-review surfaces) it is the wire's own.
        const lines = view ? view.lines : hunk.lines;
        return (
        <div
          className={"kbc-diff__hunk" + (view?.collapsed ? " kbc-diff__hunk--collapsed" : "")}
          key={hi}
          data-kbc-diff-hunk={view?.id}
        >
          {view ? (
            <HunkStrip
              view={view}
              current={currentHunk === hi}
              reviewMode={reviewMode}
              onToggleFold={() => onHunkFold?.(hi)}
              onToggleViewed={() => onHunkViewed?.(hi)}
              onExpand={(dir) => onHunkExpand?.(hi, dir)}
            />
          ) : (
            <div className="kbc-diff__hunk-header">{hunk.header}</div>
          )}
          {view?.collapsed
            ? null
            : lines.map((line, li) => {
            const oldThreads =
              comments && line.oldLine !== null ? threadsAt(comments, path, "old", line.oldLine) : [];
            const newThreads =
              comments && line.newLine !== null ? threadsAt(comments, path, "new", line.newLine) : [];
            const seen = new Set<string>();
            const lineThreadsAll = [...oldThreads, ...newThreads].filter((t) => {
              if (seen.has(t.id)) return false;
              seen.add(t.id);
              return true;
            });
            // PRR-U3 — overlay filter applies to both the rendered thread
            // rows AND the gutter mark (design-ui.md §S3: "Ticks respect
            // the overlay filter").
            const lineThreads = comments
              ? lineThreadsAll.filter((t) =>
                  threadVisibleInOverlay(comments.findingsById.has(t.id), comments.overlay),
                )
              : lineThreadsAll;
            const oldMark = comments
              ? lineMark(
                  lineThreads.filter((t) => oldThreads.some((o) => o.id === t.id)),
                  comments,
                )
              : null;
            const newMark = comments
              ? lineMark(
                  lineThreads.filter((t) => newThreads.some((n) => n.id === t.id)),
                  comments,
                )
              : null;
            const diagMark =
              diagnosticsByLine && line.newLine !== null ? diagnosticsByLine.get(line.newLine) : null;
            const composeHere =
              compose &&
              ((compose.side === "old" && compose.line === line.oldLine) ||
                (compose.side === "new" && compose.line === line.newLine));
            const isPicked = picked?.hi === hi && picked?.li === li;
            const canOld = reviewMode && line.oldLine !== null;
            const canNew = reviewMode && line.newLine !== null;

            return (
              <Fragment key={li}>
                <div
                  className={lineClass(line.kind) + (isPicked ? " is-picked" : "")}
                  data-old-line={line.oldLine ?? undefined}
                  data-new-line={line.newLine ?? undefined}
                  onClick={
                    rowTap && (canOld || canNew)
                      ? () =>
                          setPicked((cur) =>
                            cur?.hi === hi && cur?.li === li ? null : { hi, li },
                          )
                      : undefined
                  }
                >
                  {rowTap && isPicked && (canOld || canNew) && (
                    <div
                      className="kbc-diff-pill"
                      data-kbc-diff-pill
                      onClick={(e) => e.stopPropagation()}
                    >
                      {canNew && (
                        <button
                          type="button"
                          data-kbc-diff-pill-side="new"
                          onClick={() => {
                            setCompose({ side: "new", line: line.newLine as number });
                            setPicked(null);
                          }}
                        >
                          Comment (new side)
                        </button>
                      )}
                      {canOld && (
                        <button
                          type="button"
                          data-kbc-diff-pill-side="old"
                          onClick={() => {
                            setCompose({ side: "old", line: line.oldLine as number });
                            setPicked(null);
                          }}
                        >
                          Comment (old side)
                        </button>
                      )}
                    </div>
                  )}
                  {reviewMode && line.oldLine !== null ? (
                    <button
                      type="button"
                      className="kbc-diff__gutter-old kbc-diff__gutter-old--commentable"
                      title="Comment on old side"
                      onClick={() =>
                        setCompose((cur) =>
                          cur?.side === "old" && cur.line === line.oldLine
                            ? null
                            : { side: "old", line: line.oldLine as number },
                        )
                      }
                      data-kbc-review-compose-old={line.oldLine}
                    >
                      <GutterMark mark={oldMark} />
                      {line.oldLine}
                    </button>
                  ) : (
                    <span className="kbc-diff__gutter-old">
                      <GutterMark mark={oldMark} />
                      {line.oldLine ?? ""}
                    </span>
                  )}
                  {reviewMode && line.newLine !== null ? (
                    <button
                      type="button"
                      className="kbc-diff__gutter-new kbc-diff__gutter-new--commentable"
                      title="Comment on new side"
                      onClick={() =>
                        setCompose((cur) =>
                          cur?.side === "new" && cur.line === line.newLine
                            ? null
                            : { side: "new", line: line.newLine as number },
                        )
                      }
                      data-kbc-diff-comment-line={line.newLine}
                      data-kbc-review-compose-new={line.newLine}
                    >
                      <GutterMark mark={newMark} />
                      <DiagGutterMark mark={diagMark} />
                      {line.newLine}
                    </button>
                  ) : canComment && line.newLine !== null ? (
                    <button
                      type="button"
                      className="kbc-diff__gutter-new kbc-diff__gutter-new--commentable"
                      title="Comment at this commit"
                      onClick={() => setComposerLine((cur) => (cur === line.newLine ? null : line.newLine))}
                      data-kbc-diff-comment-line={line.newLine}
                    >
                      <DiagGutterMark mark={diagMark} />
                      {line.newLine}
                    </button>
                  ) : (
                    <span className="kbc-diff__gutter-new">
                      <GutterMark mark={newMark} />
                      <DiagGutterMark mark={diagMark} />
                      {line.newLine ?? ""}
                    </span>
                  )}
                  <span className="kbc-diff__marker">
                    {line.kind === "add" ? "+" : line.kind === "remove" ? "-" : " "}
                  </span>
                  <PaintedText text={line.text} spans={lineSpans(highlights, line)} />
                </div>
                {lineThreads.map((t) => (
                  <DiffThread key={t.id} thread={t} comments={comments!} />
                ))}
                {githubByLine &&
                  [
                    ...(line.oldLine !== null
                      ? githubByLine.get(threadLineKey(path, "old", line.oldLine)) ?? []
                      : []),
                    ...(line.newLine !== null
                      ? githubByLine.get(threadLineKey(path, "new", line.newLine)) ?? []
                      : []),
                  ].map((t) => <GithubDiffCard key={`gh-${t.id}`} thread={t} />)}
                {composeHere && compose && comments && (
                  <DiffLineComposerV2
                    side={compose.side}
                    line={compose.line}
                    onSubmit={(body, intent) => comments.onCreate(compose.side, compose.line, undefined, body, intent)}
                    onSubmitFinding={(draft) => comments.onCreateFinding(compose.side, compose.line, draft)}
                    onDone={() => setCompose(null)}
                  />
                )}
                {canComment && line.newLine !== null && composerLine === line.newLine && (
                  <DiffLineComposer
                    repo={repo!}
                    path={path}
                    sha={sha!}
                    line={line.newLine}
                    onDone={() => setComposerLine(null)}
                  />
                )}
              </Fragment>
            );
          })}
        </div>
        );
      })}
      {orphans.length > 0 && comments && (
        <div className="kbc-rthread-orphans" data-kbc-review-orphans={path}>
          <div className="kbc-rthread-orphans__head">Orphaned ({orphans.length})</div>
          {orphans.map((t) => (
            <DiffThread key={t.id} thread={t} comments={comments} orphaned />
          ))}
        </div>
      )}
      {githubOrphans && githubOrphans.length > 0 && (
        <div className="kbc-rthread-orphans kbc-rthread-orphans--github" data-kbc-review-orphans-github={path}>
          <div className="kbc-rthread-orphans__head">GitHub — general / orphaned ({githubOrphans.length})</div>
          {githubOrphans.map((t) => (
            <GithubDiffCard key={`gh-orphan-${t.id}`} thread={t} showPositionFoot />
          ))}
        </div>
      )}
    </>
  );
}
