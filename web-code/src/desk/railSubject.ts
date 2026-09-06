// V70-A4 — what the right rail is ABOUT, and how it says so.
//
// docs/research/kb-code-v7-continuum-2026-09.html §P1: "The rail follows
// the caret (debounced ~250 ms) with an explicit pin: pinned, the header
// reads '📌 Order#total — caret is in Order#refund · …'; the subject is
// always named in a segmented chip (symbol | line | file) and a fallback
// is captioned, never blank."
//
// Two rules make that honest:
//
//  1. **The narrowest subject the file can support.** `symbol` if the
//     caret sits inside one, else `line` if a line is known, else
//     `file`. A degrade is CAPTIONED ("no symbol data — showing
//     file-level"), never a silently narrower-looking header.
//
//  2. **Pin freezes the subject, never the view.** A pinned rail keeps
//     rendering its subject while the caret travels, and the header
//     names BOTH — the pinned subject and where the caret actually is —
//     so a stale-looking rail is never mistaken for a live one.
//
// Pure: no React, no timers (the ~250 ms debounce lives in
// `useRailSubject`, which is the only thing here that needs a clock).

import type { Symbol as CodeSymbol } from "../api/types";

export type RailSubjectKind = "symbol" | "line" | "file";

export interface RailSubject {
  kind: RailSubjectKind;
  /// Always present — every subject belongs to a file.
  path: string;
  /// Set for `symbol` and `line`.
  line?: number;
  /// Set for `symbol` only: the symbol's own name, and its container
  /// when the extractor reported one (`Order#total` reads better than a
  /// bare `total`).
  symbol?: string;
  container?: string | null;
  /// Why the subject is not narrower than it is. `null` when nothing
  /// degraded. Rendered as the caption under the segmented chip.
  caption: string | null;
}

/// Basename-ish label for the `file` segment — the full path already
/// lives in the breadcrumbs above, so the chip stays short.
export function fileLabel(path: string): string {
  const i = path.lastIndexOf("/");
  return i === -1 ? path : path.slice(i + 1);
}

/// The symbol whose `[line_start, line_end]` contains `line` and which is
/// the INNERMOST such symbol — a method inside a class wins over the
/// class. Ties (identical ranges) break toward the later `ordinal`, the
/// extractor's own nesting order.
export function symbolAtLine(symbols: readonly CodeSymbol[], line: number): CodeSymbol | null {
  let best: CodeSymbol | null = null;
  for (const s of symbols) {
    if (line < s.line_start || line > s.line_end) continue;
    if (best === null) {
      best = s;
      continue;
    }
    const span = s.line_end - s.line_start;
    const bestSpan = best.line_end - best.line_start;
    if (span < bestSpan || (span === bestSpan && s.ordinal > best.ordinal)) best = s;
  }
  return best;
}

export function symbolLabel(s: Pick<CodeSymbol, "name" | "container">): string {
  return s.container ? `${s.container}#${s.name}` : s.name;
}

export interface RailSubjectInput {
  path: string | null;
  line: number | null;
  symbols: readonly CodeSymbol[];
  /// `false` while the file's own fetch is still in flight — the
  /// difference between "this file has no symbols" (a real, captioned
  /// fallback) and "we do not know yet" (no caption; the rail simply has
  /// not resolved).
  symbolsLoaded: boolean;
}

/// The subject cascade. Total — every input produces a subject, and the
/// no-file case is its own honest value rather than `null` (the rail
/// renders "no file open", which is a state, not an absence).
export function subjectFor(input: RailSubjectInput): RailSubject | null {
  if (!input.path) return null;
  const base: RailSubject = { kind: "file", path: input.path, caption: null };
  if (input.line === null || input.line < 1) {
    return input.symbolsLoaded && input.symbols.length === 0
      ? { ...base, caption: "no symbol data — showing file-level" }
      : base;
  }
  const sym = symbolAtLine(input.symbols, input.line);
  if (sym) {
    return {
      kind: "symbol",
      path: input.path,
      line: input.line,
      symbol: symbolLabel(sym),
      container: sym.container,
      caption: null,
    };
  }
  return {
    kind: "line",
    path: input.path,
    line: input.line,
    caption: input.symbolsLoaded
      ? input.symbols.length === 0
        ? "no symbol data — showing line-level"
        : "caret is outside every symbol — showing line-level"
      : null,
  };
}

export function subjectLabel(s: RailSubject): string {
  if (s.kind === "symbol") return s.symbol ?? fileLabel(s.path);
  if (s.kind === "line") return `${fileLabel(s.path)}:${s.line}`;
  return fileLabel(s.path);
}

/// Two subjects are "the same subject" when they name the same thing —
/// a caret moving WITHIN one symbol must not re-render the rail as a
/// change, and a caption change alone is not a subject change.
export function sameSubject(a: RailSubject | null, b: RailSubject | null): boolean {
  if (a === null || b === null) return a === b;
  if (a.kind !== b.kind || a.path !== b.path) return false;
  if (a.kind === "symbol") return a.symbol === b.symbol;
  if (a.kind === "line") return a.line === b.line;
  return true;
}

/// The pinned header line. §P1's exact shape, with "unpin to follow" as
/// the instruction (the `Space p` key it names is unit A5's command
/// registry — naming a key that does nothing yet would be a lie).
export function pinnedHeaderText(pinned: RailSubject, caret: RailSubject | null): string {
  if (!caret || sameSubject(pinned, caret)) return `📌 ${subjectLabel(pinned)}`;
  return `📌 ${subjectLabel(pinned)} — caret is in ${subjectLabel(caret)} · unpin to follow`;
}
