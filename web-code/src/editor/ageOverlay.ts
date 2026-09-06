// CM6 extension: tints each line's BACKGROUND by its blame region's
// author-age bucket (Wave C — the "age" state of the reader's 3-state
// Provenance overlay, alongside `off`/`dots`). A `StateField`/`StateEffect`
// pair at MODULE scope — unlike `editor/lineGutter.ts`'s per-call factory
// (needed there because TWO distinct gutters, blame + annotations, each
// need their own field identity), there is only ever one age overlay per
// `CodeView`, so this mirrors `highlightField.ts`/`editor/linkify.ts`'s own
// module-level singleton fields instead.
//
// Read-only reader (same note as `linkify.ts`'s `linkifyField`): `tr.
// changes` is always empty in practice since this buffer never accepts a
// document edit — `.map` on the non-effect branch keeps this correct
// rather than merely convenient if that invariant is ever relaxed.
//
// Churn heatmap (per-line historical edit VOLUME, not just recency) is OUT
// OF SCOPE — see `lib/ageHeatmap.ts`'s module doc for why.

import { RangeSetBuilder, StateEffect, StateField, type EditorState, type Extension } from "@codemirror/state";
import { Decoration, EditorView, type DecorationSet } from "@codemirror/view";
import { AGE_BUCKET_COUNT, type AgeLineInfo } from "../lib/ageHeatmap";

export const setAgeLines = StateEffect.define<Map<number, AgeLineInfo>>();

/// One `Decoration.line` per bucket, built once (they're immutable value
/// objects — no reason to recreate them per file load).
const BUCKET_DECORATIONS: readonly Decoration[] = Array.from({ length: AGE_BUCKET_COUNT }, (_, b) =>
  Decoration.line({ class: `kbc-age-line kbc-age-line--${b}` }),
);

function buildDecorations(state: EditorState, lines: Map<number, AgeLineInfo>): DecorationSet {
  if (lines.size === 0) return Decoration.none;
  const builder = new RangeSetBuilder<Decoration>();
  const doc = state.doc;
  for (let ln = 1; ln <= doc.lines; ln++) {
    const info = lines.get(ln);
    if (!info) continue;
    const line = doc.line(ln);
    builder.add(line.from, line.from, BUCKET_DECORATIONS[info.bucket]);
  }
  return builder.finish();
}

interface AgeOverlayState {
  lines: Map<number, AgeLineInfo>;
  deco: DecorationSet;
}

const ageOverlayField = StateField.define<AgeOverlayState>({
  create: () => ({ lines: new Map(), deco: Decoration.none }),
  update(value, tr) {
    let lines = value.lines;
    let changed = false;
    for (const e of tr.effects) {
      if (e.is(setAgeLines)) {
        lines = e.value;
        changed = true;
      }
    }
    if (changed) return { lines, deco: buildDecorations(tr.state, lines) };
    return { lines, deco: value.deco.map(tr.changes) };
  },
  provide: (field) => EditorView.decorations.from(field, (v) => v.deco),
});

/// Always present in `CodeView`'s extension set (like the blame/annotation
/// gutters) — renders nothing when its line map is empty (`age` mode off,
/// or blame hasn't loaded yet), never a structural difference that would
/// force a view recreation just to toggle the overlay.
export const ageOverlayExtension: Extension = ageOverlayField;

export function setAgeOverlayLines(view: EditorView, lines: Map<number, AgeLineInfo>): void {
  view.dispatch({ effects: setAgeLines.of(lines) });
}
