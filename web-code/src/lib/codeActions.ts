// S2-C (B4) — quick fixes. Pure derivation for `POST /api/code-actions`
// (`code-actions/1`, design-s2.md Addendum — pinned wire shape, B1 emits/B4
// consumes) and the conversion of a returned `TextEdit` into the EXISTING
// atomic `POST /api/annotations/batch` `add_comment(+suggestion)` op (the
// audited creation path — `api/client.ts`'s `postAnnotationsBatch`). Kept
// free of React/DOM/fetch concerns, same split `lib/diagnostics.ts`
// establishes for its own card — `components/provenance/QuickFixes.tsx` is
// the thin renderer + the only place that actually calls `fetch`.

import type {
  AnchorKind,
  CodeActionEdit,
  CodeActionFileEdit,
  CodeActionRow,
  CodeActionsDropped,
  CodeActionsOut,
} from "../api/types";
import type { AnnotationBatchAddCommentOp, CodeActionsRequest } from "../api/client";

// --- request builder ------------------------------------------------------

export interface CodeActionsRange {
  start_line: number;
  start_col: number;
  /// Omitted (or `<= start_line`) ⇒ a point range — mirrors kb-lip's own
  /// "end_* optional → defaults to start_*" convention (design-s2.md §S2-C).
  end_line?: number;
  end_col?: number;
}

/// Builds the `POST /api/code-actions` body for `range` — a diagnostic
/// row's own `line/col/end_line/end_col` are ALREADY 0-based byte cols (the
/// SAME codec every lip/1 endpoint uses, `api/types.ts`'s `DiagnosticRow`
/// doc / `kb-lip::http`'s own doc) and map straight across, no adjustment.
export function buildCodeActionsRequest(
  repo: string,
  path: string,
  range: CodeActionsRange,
  kinds?: readonly string[],
): CodeActionsRequest {
  const hasEnd = range.end_line !== undefined && range.end_line >= range.start_line;
  const req: CodeActionsRequest = {
    repo,
    path,
    start_line: range.start_line,
    start_col: range.start_col,
  };
  if (hasEnd) {
    req.end_line = range.end_line;
    req.end_col = range.end_col ?? range.start_col;
  }
  if (kinds && kinds.length > 0) req.kinds = [...kinds];
  return req;
}

/// A `DiagnosticRow`'s range as a `CodeActionsRange` — the "Fixes" button's
/// own conversion (`DiagnosticsCard`'s affordance always requests actions
/// scoped to the row it's attached to). `end_line === 0` (or `< line`) is
/// the SAME "unreported range end" convention `lib/diagnostics.ts`'s
/// `diagnosticGutterMarks` treats `DiagnosticRow.end_line` with.
export function rangeFromDiagnostic(row: {
  line: number;
  col: number;
  end_line: number;
  end_col: number;
}): CodeActionsRange {
  const hasEnd = row.end_line >= row.line && row.end_line !== 0;
  return {
    start_line: row.line,
    start_col: row.col,
    end_line: hasEnd ? row.end_line : undefined,
    end_col: hasEnd ? row.end_col : undefined,
  };
}

// --- view-state matrix (absent|loading|reason|empty|actions) --------------

export type CodeActionsViewKind = "absent" | "loading" | "reason" | "empty" | "actions";

export interface CodeActionsView {
  kind: CodeActionsViewKind;
  reason?: string;
  actions?: CodeActionRow[];
  provider?: string | null;
  dropped?: CodeActionsDropped;
}

const REASON_LABELS: Record<string, string> = {
  unknown_language: "file type not recognized",
  no_provider_configured: "no code-actions provider configured for this file",
  file_unreadable: "file could not be read",
  provider_unavailable: "code-actions provider unavailable",
  blob_stale: "file changed while fetching fixes — try again",
  capability_absent: "provider doesn't support code actions",
};

/// An unrecognized reason (a newer daemon) degrades to its own verbatim
/// text — same forward-compat posture `lib/diagnostics.ts`'s
/// `unavailableReasonLabel` uses for its own closed vocabulary.
export function unavailableCodeActionsReasonLabel(reason: string | null | undefined): string {
  if (!reason) return "quick fixes unavailable";
  return REASON_LABELS[reason] ?? reason;
}

/// The state matrix `QuickFixes.tsx` renders off of. `requested`/`loading`
/// are the CALLER's own imperative-fetch state (this is an on-demand POST
/// triggered by clicking "Fixes", never an eager query like
/// `useDiagnostics`) — `"absent"` before the user has ever clicked,
/// `"loading"` mid-flight, `"reason"` for `available:false`, `"empty"` for
/// a `[]` result, `"actions"` otherwise.
export function buildCodeActionsView(
  requested: boolean,
  loading: boolean,
  data: CodeActionsOut | null | undefined,
): CodeActionsView {
  if (!requested) return { kind: "absent" };
  if (loading || !data) return { kind: "loading" };
  if (!data.available) {
    return { kind: "reason", reason: unavailableCodeActionsReasonLabel(data.reason) };
  }
  if (data.actions.length === 0) {
    return { kind: "empty", provider: data.provider, dropped: data.dropped };
  }
  return { kind: "actions", actions: data.actions, provider: data.provider, dropped: data.dropped };
}

// --- byte-column slice math (the wire's codec, not JS UTF-16 indices) -----

/// Encodes `line` to UTF-8 bytes and returns the text BEFORE byte offset
/// `col` (clamped to the line's byte length). Every lip/1 endpoint's `col`
/// is a 0-based BYTE offset (`kb-lip::http`'s own doc) — a naive JS
/// `String.slice` operates on UTF-16 code units and would cut a multibyte
/// character in the wrong place for any non-ASCII line.
export function bytePrefix(line: string, col: number): string {
  const bytes = new TextEncoder().encode(line);
  const c = Math.max(0, Math.min(col, bytes.length));
  return new TextDecoder().decode(bytes.slice(0, c));
}

/// The text AFTER byte offset `col` — see `bytePrefix`'s doc.
export function byteSuffix(line: string, col: number): string {
  const bytes = new TextEncoder().encode(line);
  const c = Math.max(0, Math.min(col, bytes.length));
  return new TextDecoder().decode(bytes.slice(c));
}

/// The text BETWEEN byte offsets `[startCol, endCol)` — see `bytePrefix`'s
/// doc. `endCol < startCol` clamps to an empty slice rather than throwing.
export function byteSlice(line: string, startCol: number, endCol: number): string {
  const bytes = new TextEncoder().encode(line);
  const s = Math.max(0, Math.min(startCol, bytes.length));
  const e = Math.max(s, Math.min(endCol, bytes.length));
  return new TextDecoder().decode(bytes.slice(s, e));
}

/// The ORIGINAL text an edit's range covers, sliced out of `fileContent` —
/// the "before" half of a quick-fix preview ("original-range slice math for
/// display", design-s2.md §S2-C). `null` when the range falls outside
/// `fileContent`'s line count (a stale/mismatched blob — the caller is
/// expected to have already gated on a fresh fetch; this is a defensive
/// fallback, never a thrown error).
export function originalRangeText(fileContent: string, edit: CodeActionEdit): string | null {
  const lines = fileContent.split("\n");
  const startIdx = edit.start_line - 1;
  const endIdx = edit.end_line - 1;
  if (startIdx < 0 || endIdx < startIdx || endIdx >= lines.length) return null;
  if (startIdx === endIdx) return byteSlice(lines[startIdx], edit.start_col, edit.end_col);
  const first = byteSuffix(lines[startIdx], edit.start_col);
  const middle = lines.slice(startIdx + 1, endIdx);
  const last = bytePrefix(lines[endIdx], edit.end_col);
  return [first, ...middle, last].join("\n");
}

/// The FULL line-range replacement text after applying `edit` to
/// `fileContent` — `prefix (before start_col) + new_text + suffix (after
/// end_col)`, the standard TextEdit-application algorithm. This is what
/// `editToAnnotationOp` sends as `suggestion.replacement`: the annotation
/// system anchors whole LINES (`AnchorKind` has no column granularity,
/// `api/types.ts`'s own doc), so the suggestion must cover the SAME
/// line-range the annotation anchors, not just the edit's own (possibly
/// column-scoped) `new_text`. `null` for the same out-of-range reason
/// `originalRangeText` returns `null`.
export function spliceEditReplacement(fileContent: string, edit: CodeActionEdit): string | null {
  const lines = fileContent.split("\n");
  const startIdx = edit.start_line - 1;
  const endIdx = edit.end_line - 1;
  if (startIdx < 0 || endIdx < startIdx || endIdx >= lines.length) return null;
  const prefix = bytePrefix(lines[startIdx], edit.start_col);
  const suffix = byteSuffix(lines[endIdx], edit.end_col);
  return prefix + edit.new_text + suffix;
}

// --- TextEdit → annotations/batch op conversion ----------------------------

export interface EditToOpOptions {
  /// `CodeActionsOut.provider` — folded into the provenance line.
  provider?: string | null;
  /// Per-touched-file content, keyed by the SAME `path` a `CodeActionFileEdit`
  /// carries — lets the conversion splice a REAL full-line replacement
  /// (`spliceEditReplacement`) rather than falling back to the edit's bare
  /// `new_text`. Absent/missing-key is an honest best-effort degrade (the
  /// server still captures `original` itself at create time either way,
  /// design-s2.md §S2-C — this only affects what the SPA proposes as the
  /// suggestion's `replacement` before that capture).
  fileContents?: Readonly<Record<string, string>>;
  /// Review scope, when converting inside a review-diff context (omitted
  /// for a plain working-tree Reader conversion).
  reviewId?: number;
  ps?: number;
  side?: "old" | "new";
}

/// `anchor_kind`/`line_end` for a single edit's range — `"line"` (no
/// `line_end`) for a single-line edit, `"range"` otherwise. Exported for
/// the view layer's own preview math (same range the batch op will anchor).
export function anchorForEdit(edit: CodeActionEdit): { anchor_kind: AnchorKind; line: number; line_end?: number } {
  if (edit.end_line > edit.start_line) {
    return { anchor_kind: "range", line: edit.start_line, line_end: edit.end_line };
  }
  return { anchor_kind: "line", line: edit.start_line };
}

function provenanceLine(provider: string | null | undefined): string {
  return provider ? `via ${provider} (lsp-live)` : "via lsp-live";
}

/// One `add_comment(+suggestion)` op per (file, edit) — "one
/// annotation+suggestion per (file, contiguous TextEdit)" (design-s2.md
/// §S2-C). Body: `Quick fix: <title>` + a provenance line; intent `"note"`;
/// anchor kind line/range on the edit's own range (`anchorForEdit`).
export function editToAnnotationOp(
  actionTitle: string,
  fileEdit: CodeActionFileEdit,
  edit: CodeActionEdit,
  opts: EditToOpOptions = {},
): AnnotationBatchAddCommentOp {
  const anchor = anchorForEdit(edit);
  const content = opts.fileContents?.[fileEdit.path];
  const replacement = (content !== undefined ? spliceEditReplacement(content, edit) : null) ?? edit.new_text;
  const op: AnnotationBatchAddCommentOp = {
    op: "add_comment",
    path: fileEdit.path,
    line: anchor.line,
    anchor_kind: anchor.anchor_kind,
    body: `Quick fix: ${actionTitle}\n\n${provenanceLine(opts.provider)}`,
    intent: "note",
    suggestion: { replacement },
  };
  if (anchor.line_end !== undefined) op.line_end = anchor.line_end;
  if (opts.reviewId !== undefined) op.review_id = opts.reviewId;
  if (opts.ps !== undefined) op.ps = opts.ps;
  if (opts.side !== undefined) op.side = opts.side;
  return op;
}

/// Every `add_comment` op a whole `CodeActionRow` expands into — one per
/// (file, edit) pair, in the SAME order the action's own `edits` arrays are
/// given (relay order — "ordering is the consumer's concern", design-s2.md
/// §S2-C). `AnnotationsBatchResult.created_ids[i]` corresponds to
/// `ops[i]` (`api/client.ts`'s own doc), so callers can map a created id
/// back to the (path, line) it landed on positionally.
export function actionToAnnotationOps(
  action: CodeActionRow,
  opts: EditToOpOptions = {},
): AnnotationBatchAddCommentOp[] {
  const ops: AnnotationBatchAddCommentOp[] = [];
  for (const fileEdit of action.edits) {
    for (const edit of fileEdit.edits) {
      ops.push(editToAnnotationOp(action.title, fileEdit, edit, opts));
    }
  }
  return ops;
}

// --- badge / count helpers -------------------------------------------------

export function totalActionCount(data: CodeActionsOut | null | undefined): number {
  return data?.actions.length ?? 0;
}

export function droppedTotal(dropped: CodeActionsDropped | null | undefined): number {
  if (!dropped) return 0;
  return dropped.command_only + dropped.unsupported;
}

/// Number of distinct files an action touches.
export function editFileCount(action: CodeActionRow): number {
  return action.edits.length;
}

/// Total number of individual TextEdits across every file an action
/// touches (== the number of annotation+suggestion ops `actionToAnnotationOps`
/// will create for it).
export function editTotalCount(action: CodeActionRow): number {
  return action.edits.reduce((sum, f) => sum + f.edits.length, 0);
}
