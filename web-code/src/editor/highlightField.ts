// Server highlight spans → CodeMirror 6 decorations. A `Facet` carries the
// current file's already-UTF16-mapped ranges (`lib/decorations.ts`'s
// `spansToDecorationRanges` — pure, vitest-covered); a `StateField` derived
// from that facet builds the actual `DecorationSet` CM6 renders. Because
// `CodeView` creates a FRESH `EditorState` per file/ref load (see that
// component), the field's `create()` is what does the real work — the
// `update()` arm exists for API completeness (CM6 requires one) and to
// keep the field's ranges correctly repositioned if the document were ever
// edited, which never happens here (the reader is read-only — invariant:
// kb-code has no editor, ever, per the design ruling in the milestone plan).
//
// **Deferred (documented, not implemented):** the decoration set below
// covers the WHOLE file in one pass, not just the visible viewport. For
// v1's typical source-file sizes (a handful of KB to low hundreds of KB)
// this is fast enough to not matter; a genuinely huge file would want a
// viewport-aware field (recomputing only the visible line range on
// scroll, via `EditorView.decorations.compute` reading `view.viewport`)
// — that's a real optimization, left for a later wave if a slow-file
// report ever justifies it.

import { Facet, StateField } from "@codemirror/state";
import { Decoration, EditorView, type DecorationSet } from "@codemirror/view";
import type { DecorationRange } from "../lib/decorations";
import { cssClassFor } from "../lib/decorations";

export const highlightRangesFacet = Facet.define<DecorationRange[], DecorationRange[]>({
  combine: (values) => values[0] ?? [],
});

function buildDecorationSet(ranges: DecorationRange[]): DecorationSet {
  // `Decoration.set`'s second arg (`sort: true`) — `spansToDecorationRanges`
  // already trusts the server's own non-overlapping, start-ascending sweep,
  // but a defensive sort costs nothing here (built once per file load, not
  // per keystroke) and protects against a future spans source that isn't
  // pre-sorted.
  const marks = ranges.map((r) =>
    Decoration.mark({ class: cssClassFor(r.class) }).range(r.from, r.to),
  );
  return Decoration.set(marks, true);
}

export const highlightField = StateField.define<DecorationSet>({
  create(state) {
    return buildDecorationSet(state.facet(highlightRangesFacet));
  },
  update(deco, tr) {
    // The reader never edits (`EditorState.readOnly` is always on — see
    // `CodeView`), so `tr.changes` is always empty in practice; `.map`
    // keeps this correct rather than merely convenient if that invariant
    // is ever relaxed.
    return deco.map(tr.changes);
  },
  provide: (field) => EditorView.decorations.from(field),
});
