// V76-R2c — token-level suggestion renderer.
//
// Finding suggestions, comment suggestions, and the apply-preview confirm
// card share this view. The CM6 suggestion EDITOR (edit mode) does not —
// it is untouched and still paints via UnifiedHunks.
//
// Syntax paint: one `useHighlight` batch (`id: "old"` / `id: "new"`,
// `lang: null`, `path` from the thread) composed with token marks in
// `lib/suggestionPaint.ts`. Never blocks the diff on the paint.

import { useMemo } from "react";
import type { ReviewComment } from "../../api/types";
import { useFile } from "../../hooks/useFile";
import { useHighlight } from "../../hooks/useHighlight";
import { sliceAnchoredLines, splitSuggestionLines } from "../../lib/suggestions";
import {
  composePaintedRows,
  suggestionHighlightItems,
  type ComposedSeg,
} from "../../lib/suggestionPaint";

export function SuggestionDiffPanel({
  original,
  replacement,
  path,
}: {
  original: string;
  replacement: string;
  path?: string;
}) {
  const items = useMemo(
    () => suggestionHighlightItems(original, replacement, path),
    [original, replacement, path],
  );
  const { byId } = useHighlight(items);
  const oldResult = byId.get("old");
  const newResult = byId.get("new");
  const composed = useMemo(
    () => composePaintedRows(original, replacement, oldResult, newResult),
    [original, replacement, oldResult, newResult],
  );
  const none = items.some((it) => byId.get(it.id)?.tier === "none");
  const pending = items.some((it) => !byId.has(it.id));
  const panelTier = pending
    ? "pending"
    : none
      ? "none"
      : (oldResult?.tier ?? newResult?.tier ?? "pending");

  return (
    <div
      className="kbc-sugdiff"
      data-kbc-sugdiff
      data-kbc-sugdiff-mode={composed.mode}
      data-kbc-hl-tier={panelTier}
    >
      <p className="kbc-sugdiff__caption" data-kbc-sugdiff-caption>
        {composed.caption}
      </p>
      <div className="kbc-sugdiff__body">
        {composed.rows.map((row, i) => (
          <div
            key={i}
            className={`kbc-sugdiff__line kbc-sugdiff__line--${row.side}`}
            data-kbc-sugdiff-line={row.side}
            data-kbc-sugdiff-trailing={row.trailing ? "1" : "0"}
            data-kbc-hl-tier={row.tier}
          >
            <span className="kbc-sugdiff__gutter" aria-hidden>
              {row.side === "new" ? "+" : row.side === "old" ? "−" : " "}
            </span>
            <span className="kbc-sugdiff__text">
              {row.segs.map((seg, j) => (
                <TokenSpan key={j} seg={seg} />
              ))}
            </span>
          </div>
        ))}
      </div>
      {none && (
        <span className="kbc-hl-no-grammar" data-kbc-hl-no-grammar>
          {oldResult?.honesty.reason ?? newResult?.honesty.reason ?? "no grammar"}
        </span>
      )}
    </div>
  );
}

function TokenSpan({ seg }: { seg: ComposedSeg }) {
  const cls = ["kbc-sugdiff__tok", `kbc-sugdiff__tok--${seg.tokKind}`];
  if (seg.hlCls) cls.push(seg.hlCls);
  return (
    <span
      className={cls.join(" ")}
      data-kbc-sugdiff-tok={seg.tokKind}
      data-kbc-sugdiff-trailing={seg.trailing ? "1" : "0"}
      {...(seg.hlCls ? { "data-kbc-hl": "" } : {})}
    >
      {seg.text === "" ? "\u00a0" : seg.text}
    </span>
  );
}

/// Apply-confirm body: token diff plus the target lines' current working-tree
/// state. `pinned` / `drifted` is the wire's `resolution.orphaned` — not a
/// client guess. The WT slice is shown beside it so a 409 is not a surprise.
export function ApplySuggestionPreview({
  repo,
  thread,
  resolveRef,
}: {
  repo: string;
  thread: ReviewComment;
  resolveRef: { current: boolean };
}) {
  const suggestion = thread.suggestion;
  const start = thread.resolution.line ?? 1;
  const end = thread.resolution.line_end ?? start;
  const wt = useFile(repo || undefined, thread.path, undefined);
  const currentLines =
    wt.data?.encoding === "utf8" ? sliceAnchoredLines(wt.data.content, start, end) : null;
  const original = suggestion?.original ?? "";
  const blobState = thread.resolution.orphaned ? "drifted" : "pinned";
  const wtMatches =
    currentLines != null ? currentLines.join("\n") === splitSuggestionLines(original).join("\n") : null;

  return (
    <div className="kbc-sugdiff-apply" data-kbc-suggestion-apply-preview>
      <p>Apply this suggestion to the working tree?</p>
      {suggestion && (
        <SuggestionDiffPanel
          original={suggestion.original}
          replacement={suggestion.replacement}
          path={thread.path}
        />
      )}
      <p className="kbc-sugdiff-apply__blob" data-kbc-suggestion-blob={blobState}>
        target lines {blobState}
        {wt.isLoading
          ? " · loading working tree…"
          : wt.error
            ? " · working tree unread"
            : wtMatches === true
              ? " · working tree still matches original"
              : wtMatches === false
                ? " · working tree no longer matches original"
                : ""}
      </p>
      {currentLines && (
        <pre className="kbc-sugdiff-apply__wt" data-kbc-suggestion-wt>
          {currentLines.join("\n")}
        </pre>
      )}
      <label className="kbc-suggestion__resolve">
        <input
          type="checkbox"
          defaultChecked={false}
          onChange={(e) => {
            resolveRef.current = e.target.checked;
          }}
          data-kbc-suggestion-apply-resolve
        />
        Also resolve the thread
      </label>
    </div>
  );
}
