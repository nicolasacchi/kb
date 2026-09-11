import { Fragment, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import type { GithubThread } from "../../api/types";
import type { DiffLine, ParsedDiff } from "../../lib/diff";
import type { DiagnosticGutterMark } from "../../lib/diagnostics";
import { paintLine, spansForLine, type DiffHighlights, type LineSpan } from "../../lib/diffHighlight";
import { severityRank, threadVisibleInOverlay } from "../../lib/diffFindings";
import { buildSplitPairs, buildSplitRows, type SplitRow } from "../../lib/diffRows";
import {
  orphansAt,
  threadLineKey,
  threadsAt,
  type DiffCommentsApi,
  type DiffSide,
} from "../../lib/reviewComments";
import HunkStrip, { type HunkView } from "./HunkStrip";
import DiffLineComposerV2 from "./DiffLineComposerV2";
import DiffThread from "./DiffThread";
import GithubDiffCard from "../reviews/GithubDiffCard";

/// PRR-U3 — same gutter-mark logic as `UnifiedHunks.tsx` (duplicated, not
/// shared — the two hunk renderers are already independent implementations
/// by design; only `DiffThread` itself is the "no forked component" unit).
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

/// PRR-U9 — same diagnostics gutter mark as `UnifiedHunks.tsx` (duplicated,
/// not shared — see that file's own note on why the two hunk renderers
/// stay independent implementations).
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

export interface SplitHunksProps {
  path: string;
  parsed: ParsedDiff;
  highlights?: DiffHighlights | null;
  comments?: DiffCommentsApi | null;
  /// PRR-U9 — NEW-side-line → severity mark, passed ONLY while the diff's
  /// overlay selector is in the `"diagnostics"` lane (`DiffFile`'s doc).
  diagnosticsByLine?: Map<number, DiagnosticGutterMark> | null;
  /// PRR-F (design-addendum-2.md §A) — GitHub-origin threads for this file,
  /// keyed the SAME way `comments.byLine` is (`threadLineKey`), passed ONLY
  /// while the overlay is in the `"github"` lane (`DiffFile`'s own doc).
  /// PRR-U8 — the recorded cut: `DiffFile` already forwards these two props
  /// to `UnifiedHunks` but silently dropped them for the split renderer, so
  /// a GitHub-origin thread on a file viewed in split mode simply never
  /// rendered. Mirrors `UnifiedHunks.tsx`'s own props + rendering exactly.
  githubByLine?: Map<string, GithubThread[]> | null;
  githubOrphans?: GithubThread[];
  /// V73-K2a — see `UnifiedHunks`'s twin: one entry per `parsed.hunks`
  /// entry, index-aligned; absent on the non-review diff surfaces, which
  /// then render exactly as they did before diff v2.
  hunkViews?: readonly HunkView[] | null;
  onHunkFold?: (hunkIdx: number) => void;
  onHunkViewed?: (hunkIdx: number) => void;
  onHunkExpand?: (hunkIdx: number, dir: "up" | "down") => void;
  currentHunk?: number | null;
  /// V73-K2c — see `UnifiedHunks`'s twin doc; identical contract.
  onHunkTurns?: (hunkIdx: number) => void;
  turnsOpenId?: string | null;
  turnsPanel?: ReactNode;
}

/// Context lines: paint the NEW side on both cells (text is identical;
/// old-side spans would match but we reuse new so a missing old fetch
/// still colors context). Remove → old map; add → new map.
function sideSpans(
  highlights: DiffHighlights | null | undefined,
  line: DiffLine | null,
  side: "old" | "new",
): LineSpan[] | undefined {
  if (!line) return undefined;
  if (side === "new" || line.kind === "context") {
    return spansForLine(highlights, "new", line.newLine, line.text);
  }
  return spansForLine(highlights, "old", line.oldLine, line.text);
}

function PaintedText({ text, spans }: { text: string; spans: LineSpan[] | undefined }) {
  const segs = paintLine(text, spans);
  if (segs.length === 1 && !segs[0].cls) {
    return <span className="kbc-sdiff__text">{text}</span>;
  }
  return (
    <span className="kbc-sdiff__text">
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

function sideClass(side: "old" | "new", line: DiffLine | null): string {
  const base = `kbc-sdiff__cell kbc-sdiff__${side}`;
  if (line === null) return `${base} kbc-sdiff__spacer`;
  if (side === "old" && line.kind === "remove") return `${base} kbc-sdiff__cell--del`;
  if (side === "new" && line.kind === "add") return `${base} kbc-sdiff__cell--add`;
  return base;
}

function gutterClass(side: "old" | "new", line: DiffLine | null): string {
  const base = `kbc-sdiff__gutter kbc-sdiff__gutter-${side}`;
  if (line === null) return `${base} kbc-sdiff__spacer`;
  if (side === "old" && line.kind === "remove") return `${base} kbc-sdiff__cell--del`;
  if (side === "new" && line.kind === "add") return `${base} kbc-sdiff__cell--add`;
  return base;
}

type ComposeAt = { side: DiffSide; line: number };

/// Side-by-side hunk renderer. When `comments` is set (V4.C4), each
/// column's gutter composes for its own side; thread rows span
/// `grid-column: 1 / -1`.
export default function SplitHunks({
  path,
  parsed,
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
  onHunkTurns,
  turnsOpenId,
  turnsPanel,
}: SplitHunksProps) {
  // V73-K2a — with hunk views in hand the row stream is rebuilt PER HUNK
  // (each hunk owns a strip, a fold and its own context-expanded lines),
  // through the very same `buildSplitPairs` `buildSplitRows` delegates to
  // — one pairing implementation, two entry points. `rowHunk[i]` is the
  // hunk index row `i` belongs to, so the header row can render its own
  // strip without `SplitRow` growing a field the pre-v2 golden pins.
  const { rows, rowHunk } = useMemo((): { rows: SplitRow[]; rowHunk: number[] } => {
    if (!hunkViews) {
      const flat = buildSplitRows(parsed);
      let hi = -1;
      const idx = flat.map((r) => {
        if (r.kind === "hunk") hi += 1;
        return hi;
      });
      return { rows: flat, rowHunk: idx };
    }
    const out: SplitRow[] = [];
    const idx: number[] = [];
    parsed.hunks.forEach((hunk, hi) => {
      const view = hunkViews[hi];
      out.push({ kind: "hunk", header: view?.header ?? hunk.header });
      idx.push(hi);
      if (view?.collapsed) return;
      // Prefer the view's expanded lines, but never render an empty body
      // when the wire hunk itself has rows (added-file + ctx-dial miss).
      const lines =
        view && view.lines.length > 0 ? view.lines : hunk.lines;
      for (const row of buildSplitPairs(lines)) {
        out.push(row);
        idx.push(hi);
      }
    });
    return { rows: out, rowHunk: idx };
  }, [parsed, hunkViews]);
  const [compose, setCompose] = useState<ComposeAt | null>(null);
  const seenComposeToken = useRef<number | undefined>(undefined);

  useEffect(() => {
    const req = comments?.compose;
    if (!req) return;
    if (req.token !== undefined && req.token === seenComposeToken.current) return;
    seenComposeToken.current = req.token;
    setCompose({ side: req.side, line: req.line });
  }, [comments?.compose]);

  // PRR-U3 — orphans respect the overlay filter too.
  const orphans = comments
    ? orphansAt(comments, path).filter((t) =>
        threadVisibleInOverlay(comments.findingsById.has(t.id), comments.overlay),
      )
    : [];

  return (
    <div className="kbc-sdiff" data-kbc-sdiff>
      {rows.map((row, i) => {
        if (row.kind === "hunk") {
          const hi = rowHunk[i] ?? 0;
          const view = hunkViews?.[hi] ?? null;
          return (
            <div className="kbc-sdiff__hunk" data-kbc-sdiff-row="hunk" key={i}>
              {view ? (
                <HunkStrip
                  view={view}
                  current={currentHunk === hi}
                  reviewMode={!!comments}
                  onToggleFold={() => onHunkFold?.(hi)}
                  onToggleViewed={() => onHunkViewed?.(hi)}
                  onExpand={(dir) => onHunkExpand?.(hi, dir)}
                  onToggleTurns={onHunkTurns ? () => onHunkTurns(hi) : undefined}
                  turnsOpen={turnsOpenId === view.id}
                  turnsPanel={turnsOpenId === view.id ? turnsPanel : undefined}
                />
              ) : (
                row.header
              )}
            </div>
          );
        }
        const oldLine = row.old?.oldLine ?? null;
        const newLine = row.new?.newLine ?? null;
        const oldThreads = comments && oldLine !== null ? threadsAt(comments, path, "old", oldLine) : [];
        const newThreads = comments && newLine !== null ? threadsAt(comments, path, "new", newLine) : [];
        const seen = new Set<string>();
        const lineThreadsAll = [...oldThreads, ...newThreads].filter((t) => {
          if (seen.has(t.id)) return false;
          seen.add(t.id);
          return true;
        });
        // PRR-U3 — overlay filter applies to both the rendered thread rows
        // AND the gutter mark.
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
        const diagMark = diagnosticsByLine && newLine !== null ? diagnosticsByLine.get(newLine) : null;
        const composeHere =
          compose &&
          ((compose.side === "old" && compose.line === oldLine) ||
            (compose.side === "new" && compose.line === newLine));

        return (
          <Fragment key={i}>
            <div
              className="kbc-sdiff__row"
              data-kbc-sdiff-row="pair"
              data-old-line={oldLine ?? undefined}
              data-new-line={newLine ?? undefined}
            >
              {comments && oldLine !== null ? (
                <button
                  type="button"
                  className={`${gutterClass("old", row.old)} kbc-sdiff__gutter--commentable`}
                  title="Comment on old side"
                  onClick={() =>
                    setCompose((cur) =>
                      cur?.side === "old" && cur.line === oldLine ? null : { side: "old", line: oldLine },
                    )
                  }
                  data-kbc-review-compose-old={oldLine}
                >
                  <GutterMark mark={oldMark} />
                  {oldLine}
                </button>
              ) : (
                <span className={gutterClass("old", row.old)}>
                  <GutterMark mark={oldMark} />
                  {row.old?.oldLine ?? ""}
                </span>
              )}
              <span className={sideClass("old", row.old)}>
                <PaintedText text={row.old?.text ?? ""} spans={sideSpans(highlights, row.old, "old")} />
              </span>
              {comments && newLine !== null ? (
                <button
                  type="button"
                  className={`${gutterClass("new", row.new)} kbc-sdiff__gutter--commentable`}
                  title="Comment on new side"
                  onClick={() =>
                    setCompose((cur) =>
                      cur?.side === "new" && cur.line === newLine ? null : { side: "new", line: newLine },
                    )
                  }
                  data-kbc-diff-comment-line={newLine}
                  data-kbc-review-compose-new={newLine}
                >
                  <GutterMark mark={newMark} />
                  <DiagGutterMark mark={diagMark} />
                  {newLine}
                </button>
              ) : (
                <span className={gutterClass("new", row.new)}>
                  <GutterMark mark={newMark} />
                  <DiagGutterMark mark={diagMark} />
                  {row.new?.newLine ?? ""}
                </span>
              )}
              <span className={sideClass("new", row.new)}>
                <PaintedText text={row.new?.text ?? ""} spans={sideSpans(highlights, row.new, "new")} />
              </span>
            </div>
            {lineThreads.map((t) => (
              <div className="kbc-sdiff__thread" key={t.id}>
                <DiffThread thread={t} comments={comments!} />
              </div>
            ))}
            {githubByLine &&
              [
                ...(oldLine !== null ? githubByLine.get(threadLineKey(path, "old", oldLine)) ?? [] : []),
                ...(newLine !== null ? githubByLine.get(threadLineKey(path, "new", newLine)) ?? [] : []),
              ].map((t) => (
                <div className="kbc-sdiff__thread" key={`gh-${t.id}`}>
                  <GithubDiffCard thread={t} />
                </div>
              ))}
            {composeHere && compose && comments && (
              <div className="kbc-sdiff__thread">
                <DiffLineComposerV2
                  side={compose.side}
                  line={compose.line}
                  onSubmit={(body, intent) =>
                    comments.onCreate(compose.side, compose.line, undefined, body, intent)
                  }
                  onSubmitFinding={(draft) => comments.onCreateFinding(compose.side, compose.line, draft)}
                  onDone={() => setCompose(null)}
                />
              </div>
            )}
          </Fragment>
        );
      })}
      {orphans.length > 0 && comments && (
        <div className="kbc-sdiff__thread kbc-rthread-orphans" data-kbc-review-orphans={path}>
          <div className="kbc-rthread-orphans__head">Orphaned ({orphans.length})</div>
          {orphans.map((t) => (
            <DiffThread key={t.id} thread={t} comments={comments} orphaned />
          ))}
        </div>
      )}
      {githubOrphans && githubOrphans.length > 0 && (
        <div
          className="kbc-sdiff__thread kbc-rthread-orphans kbc-rthread-orphans--github"
          data-kbc-review-orphans-github={path}
        >
          <div className="kbc-rthread-orphans__head">GitHub — general / orphaned ({githubOrphans.length})</div>
          {githubOrphans.map((t) => (
            <GithubDiffCard key={`gh-orphan-${t.id}`} thread={t} showPositionFoot />
          ))}
        </div>
      )}
    </div>
  );
}
