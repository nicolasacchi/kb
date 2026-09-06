// Occurrence highlighting for the read-only CM6 reader.
//
// On cursor rest (300 ms debounce), extract the identifier under the
// cursor and decorate every exact-word match in the document with
// `cm-kbc-occurrence`. Pure helpers (`identifierAt`, `findOccurrenceRanges`)
// are vitest-covered; the CM6 glue follows the same StateField +
// ViewPlugin shape as `ageOverlay.ts` / `highlightField.ts` and is
// mounted unconditionally from `CodeView.tsx` (renders nothing when idle).
//
// Layering: this field provides `EditorView.decorations` the same way
// server-span highlights do — CM6 composes multiple decoration sources.
// The CSS class is distinct from `.cm-searchMatch` so `/`-search and
// occurrence tint never share a color.

import { StateEffect, StateField, type Extension } from "@codemirror/state";
import {
  Decoration,
  EditorView,
  ViewPlugin,
  type DecorationSet,
  type ViewUpdate,
} from "@codemirror/view";

export const OCCURRENCE_DEBOUNCE_MS = 300;

const WORD_CHAR_RE = /[A-Za-z0-9_]/;

export interface IdentifierAt {
  word: string;
  start: number;
  end: number;
}

/// Identifier under `col` (UTF-16 offset into `lineText`). Requires:
/// word chars `[A-Za-z0-9_]`, must start with a non-digit, length ≥ 2.
/// Returns `null` when the cursor sits on whitespace/punctuation or the
/// extracted token fails those rules. Non-ASCII is treated as non-word
/// (a boundary) — no unicode identifier support on purpose.
export function identifierAt(lineText: string, col: number): IdentifierAt | null {
  const at = (i: number) => (i >= 0 && i < lineText.length ? lineText[i] : "");
  const isWord = (i: number) => WORD_CHAR_RE.test(at(i));

  let anchor = -1;
  if (isWord(col)) anchor = col;
  else if (isWord(col - 1)) anchor = col - 1;
  if (anchor === -1) return null;

  let start = anchor;
  while (start > 0 && isWord(start - 1)) start--;
  let end = anchor + 1;
  while (end < lineText.length && isWord(end)) end++;

  const word = lineText.slice(start, end);
  if (word.length < 2) return null;
  if (/^[0-9]/.test(word)) return null;
  return { word, start, end };
}

export interface OccurrenceRange {
  from: number;
  to: number;
}

/// Exact-word matches of `word` in `docText` (whole document as one
/// string). A match is accepted only when both edges sit on a non-word
/// boundary (or the document edge) — so `foo` does not match inside
/// `foobar` or `afoo`.
export function findOccurrenceRanges(docText: string, word: string): OccurrenceRange[] {
  if (word.length < 2) return [];
  const out: OccurrenceRange[] = [];
  let from = 0;
  while (from <= docText.length - word.length) {
    const i = docText.indexOf(word, from);
    if (i < 0) break;
    const before = i === 0 ? "" : docText[i - 1];
    const after = i + word.length >= docText.length ? "" : docText[i + word.length];
    const leftOk = before === "" || !WORD_CHAR_RE.test(before);
    const rightOk = after === "" || !WORD_CHAR_RE.test(after);
    if (leftOk && rightOk) {
      out.push({ from: i, to: i + word.length });
    }
    from = i + 1;
  }
  return out;
}

// --- CM6 extension --------------------------------------------------------

const setOccurrenceRanges = StateEffect.define<OccurrenceRange[]>();

const occurrenceMark = Decoration.mark({ class: "cm-kbc-occurrence" });

function buildDeco(ranges: readonly OccurrenceRange[]): DecorationSet {
  if (ranges.length === 0) return Decoration.none;
  return Decoration.set(
    ranges.map((r) => occurrenceMark.range(r.from, r.to)),
    true,
  );
}

const occurrenceField = StateField.define<DecorationSet>({
  create: () => Decoration.none,
  update(deco, tr) {
    for (const e of tr.effects) {
      if (e.is(setOccurrenceRanges)) return buildDeco(e.value);
    }
    // Clear immediately on cursor move / selection change / doc swap so a
    // stale tint never lingers while the 300 ms debounce waits out. The
    // ViewPlugin re-schedules a recompute on the same triggers.
    if (tr.docChanged || tr.selection) return Decoration.none;
    return deco.map(tr.changes);
  },
  provide: (field) => EditorView.decorations.from(field),
});

function rangesForView(view: EditorView): OccurrenceRange[] {
  const sel = view.state.selection.main;
  // Non-collapsed selection → no occurrence highlight (the operator is
  // mid-visual-mode, not resting on a word).
  if (sel.from !== sel.to) return [];
  const head = sel.head;
  const line = view.state.doc.lineAt(head);
  const id = identifierAt(line.text, head - line.from);
  if (!id) return [];
  return findOccurrenceRanges(view.state.doc.sliceString(0), id.word);
}

const occurrencePlugin = ViewPlugin.fromClass(
  class {
    private timer: ReturnType<typeof setTimeout> | null = null;

    constructor(view: EditorView) {
      this.schedule(view);
    }

    update(update: ViewUpdate) {
      if (!update.selectionSet && !update.docChanged) return;
      this.schedule(update.view);
    }

    destroy() {
      if (this.timer != null) clearTimeout(this.timer);
    }

    private schedule(view: EditorView) {
      if (this.timer != null) clearTimeout(this.timer);
      this.timer = setTimeout(() => {
        this.timer = null;
        // View may have been destroyed during the wait.
        try {
          if (!view.dom.isConnected) return;
        } catch {
          return;
        }
        const ranges = rangesForView(view);
        view.dispatch({ effects: setOccurrenceRanges.of(ranges) });
      }, OCCURRENCE_DEBOUNCE_MS);
    }
  },
);

/// Always present in `CodeView`'s extension set — idle = no decorations.
export const occurrenceHighlightExtension: Extension = [occurrenceField, occurrencePlugin];
