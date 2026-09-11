// V76-R2c — token-level suggestion renderer.
//
// Finding suggestions, comment suggestions, and the apply-preview confirm
// card share this view. The CM6 suggestion EDITOR (edit mode) does not —
// it is untouched and still paints via UnifiedHunks.
//
// TODO(V76-C1): when `hooks/useHighlight.ts` lands, paint old/new text with
// server spans here. useHighlight.ts is absent on this base; do not invent
// a C1 hook.

import { useMemo } from "react";
import type { ReviewComment } from "../../api/types";
import { useFile } from "../../hooks/useFile";
import { sliceAnchoredLines, splitSuggestionLines } from "../../lib/suggestions";
import {
  suggestionDiff,
  suggestionRenderRows,
  type SuggestionDiffLine,
  type TokenOp,
} from "../../lib/tokenDiff";

export function SuggestionDiffPanel({
  original,
  replacement,
}: {
  original: string;
  replacement: string;
}) {
  const view = useMemo(() => suggestionDiff(original, replacement), [original, replacement]);
  return (
    <div className="kbc-sugdiff" data-kbc-sugdiff data-kbc-sugdiff-mode={view.mode}>
      <p className="kbc-sugdiff__caption" data-kbc-sugdiff-caption>
        {view.caption}
      </p>
      <div className="kbc-sugdiff__body">
        {view.lines.map((line, i) => (
          <SuggestionLineRow key={i} line={line} />
        ))}
      </div>
    </div>
  );
}

function SuggestionLineRow({ line }: { line: SuggestionDiffLine }) {
  const rows = suggestionRenderRows(line);
  return (
    <>
      {rows.map((row, i) => (
        <div
          key={i}
          className={`kbc-sugdiff__line kbc-sugdiff__line--${row.side}`}
          data-kbc-sugdiff-line={row.side}
          data-kbc-sugdiff-trailing={row.trailing ? "1" : "0"}
        >
          <span className="kbc-sugdiff__gutter" aria-hidden>
            {row.side === "new" ? "+" : row.side === "old" ? "−" : " "}
          </span>
          <span className="kbc-sugdiff__text">
            {row.ops.map((op, j) => (
              <TokenSpan key={j} op={op} />
            ))}
          </span>
        </div>
      ))}
    </>
  );
}

function TokenSpan({ op }: { op: TokenOp }) {
  return (
    <span
      className={`kbc-sugdiff__tok kbc-sugdiff__tok--${op.kind}`}
      data-kbc-sugdiff-tok={op.kind}
      data-kbc-sugdiff-trailing={op.trailing ? "1" : "0"}
    >
      {op.text === "" ? "\u00a0" : op.text}
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
        <SuggestionDiffPanel original={suggestion.original} replacement={suggestion.replacement} />
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
