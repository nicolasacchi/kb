// V76-R2c.4 — compose highlight/1 syntax spans with the token-diff marks.
// Syntax colour lives on the TEXT (`.kbc-hl-*`); add/del emphasis lives on
// BACKGROUND / UNDERLINE. The two range sets are intersected so a keyword
// that is also an added token carries both classes on the same run.
//
// Pure: the panel paints immediately from token ops and layers syntax on
// when the batch returns. In-flight / refused / mismatched paint degrades
// to unpainted-but-still-diffed.

import type { HighlightOut } from "../api/types";
import type { PaintedSegment } from "./diffHighlight";
import { paintSpans } from "./paintSpans";
import {
  suggestionDiff,
  suggestionRenderRows,
  type SuggestionDiffMode,
  type SuggestionRenderSide,
  type TokenOp,
} from "./tokenDiff";

export interface SuggestionHighlightItem {
  id: "old" | "new";
  lang: null;
  text: string;
  path?: string;
}

export interface ComposedSeg {
  text: string;
  tokKind: TokenOp["kind"];
  trailing?: boolean;
  /// `.kbc-hl-*` class from paintSpans; absent when unpainted.
  hlCls?: string;
}

export interface ComposedRow {
  side: SuggestionRenderSide;
  text: string;
  trailing: boolean;
  segs: ComposedSeg[];
  /// Inspectable highlight tier for this row (`pending` while in flight).
  tier: string;
}

export interface ComposedSuggestionView {
  mode: SuggestionDiffMode;
  caption: string;
  rows: ComposedRow[];
}

/// ONE batch payload per panel: old text + new text. `lang: null` so the
/// server derives the language from `path`. Empty sides are omitted.
export function suggestionHighlightItems(
  original: string,
  replacement: string,
  path?: string,
): SuggestionHighlightItem[] {
  const items: SuggestionHighlightItem[] = [];
  if (original) items.push({ id: "old", lang: null, text: original, path });
  if (replacement) items.push({ id: "new", lang: null, text: replacement, path });
  return items;
}

/// `null` means "do not paint" — in-flight, tier none, or no spans.
export function paintedLinesOrNull(
  text: string,
  result: HighlightOut | undefined,
): PaintedSegment[][] | null {
  if (!result || result.tier === "none" || result.spans.length === 0) return null;
  return paintSpans(text, result.spans);
}

/// Intersect token-diff ops with paintSpans segments over one line.
/// Segment texts concatenate to the ops' verbatim line. When `painted` is
/// missing, unclassed, or does not cover the line, ops render unpainted.
export function composeTokenAndSyntax(
  ops: TokenOp[],
  painted: PaintedSegment[] | undefined,
): ComposedSeg[] {
  const line = ops.map((o) => o.text).join("");
  if (ops.length === 0) return [];
  if (line.length === 0) {
    return ops.map((op) => ({
      text: op.text,
      tokKind: op.kind,
      ...(op.trailing ? { trailing: true as const } : {}),
    }));
  }

  const paintedText = painted?.map((s) => s.text).join("");
  const usePaint =
    painted != null &&
    painted.length > 0 &&
    paintedText === line &&
    painted.some((s) => Boolean(s.cls));

  const tokCover: TokenOp[] = new Array(line.length);
  let off = 0;
  for (const op of ops) {
    for (let i = 0; i < op.text.length; i++) tokCover[off + i] = op;
    off += op.text.length;
  }

  const hlCover: (string | undefined)[] = new Array(line.length);
  if (usePaint && painted) {
    off = 0;
    for (const seg of painted) {
      for (let i = 0; i < seg.text.length; i++) hlCover[off + i] = seg.cls;
      off += seg.text.length;
    }
  }

  const out: ComposedSeg[] = [];
  let i = 0;
  while (i < line.length) {
    const op = tokCover[i];
    const cls = hlCover[i];
    let j = i + 1;
    while (j < line.length && tokCover[j] === op && hlCover[j] === cls) j += 1;
    out.push({
      text: line.slice(i, j),
      tokKind: op.kind,
      ...(op.trailing ? { trailing: true as const } : {}),
      ...(cls ? { hlCls: cls } : {}),
    });
    i = j;
  }
  return out;
}

function lineTier(
  side: SuggestionRenderSide,
  original: string,
  replacement: string,
  oldResult: HighlightOut | undefined,
  newResult: HighlightOut | undefined,
): string {
  if (side === "old") return original ? (oldResult?.tier ?? "pending") : "pending";
  if (side === "new") return replacement ? (newResult?.tier ?? "pending") : "pending";
  return replacement
    ? (newResult?.tier ?? "pending")
    : original
      ? (oldResult?.tier ?? "pending")
      : "pending";
}

/// Walk the suggestion view and attach per-row paint from the old/new
/// highlight results. Does not wait on the batch — missing results just
/// leave `hlCls` unset.
export function composePaintedRows(
  original: string,
  replacement: string,
  oldResult: HighlightOut | undefined,
  newResult: HighlightOut | undefined,
): ComposedSuggestionView {
  const view = suggestionDiff(original, replacement);
  const oldPainted = paintedLinesOrNull(original, oldResult);
  const newPainted = paintedLinesOrNull(replacement, newResult);
  let oi = 0;
  let ni = 0;
  const rows: ComposedRow[] = [];
  for (const line of view.lines) {
    const paintedOld = line.oldText != null ? (oldPainted?.[oi++] ?? undefined) : undefined;
    const paintedNew = line.newText != null ? (newPainted?.[ni++] ?? undefined) : undefined;
    for (const row of suggestionRenderRows(line)) {
      const painted =
        row.side === "old" ? paintedOld : row.side === "new" ? paintedNew : (paintedNew ?? paintedOld);
      rows.push({
        side: row.side,
        text: row.text,
        trailing: row.trailing,
        segs: composeTokenAndSyntax(row.ops, painted),
        tier: lineTier(row.side, original, replacement, oldResult, newResult),
      });
    }
  }
  return { mode: view.mode, caption: view.caption, rows };
}
