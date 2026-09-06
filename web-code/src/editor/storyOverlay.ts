// CM6 extension: tints a set of line RANGES with the story player's own
// distinct `--accent`-based background (Phase C7, "watch this file being
// made") — the changed-line highlight for the CURRENT playback step. Same
// module-scoped `StateField`/`StateEffect` shape as `editor/ageOverlay.ts`
// (only one story overlay per `CodeView`, same reasoning that module's doc
// gives for why this isn't a per-call factory like `lineGutter.ts`'s
// gutters) — deliberately a SEPARATE field rather than reusing the age
// overlay's, since the two are mutually exclusive in practice (the age
// heatmap is a Provenance-toggle concept; story mode never renders the
// Provenance toggle at all, see `Reader.tsx`) but visually and semantically
// distinct enough (age = "how old", story = "changed in THIS step") to
// deserve their own CSS class rather than overloading one meaning onto the
// other.
//
// Every dispatch of `setStoryLines` is treated as a STEP CHANGE, not just a
// "ranges happened to update" — so it always drives the CSS
// `kbc-story-line--flash` animation class (`styles/story.css`), which
// self-terminates via `animation` (no `animation-fill-mode: forwards`, so
// the steady `.kbc-story-line` tint is what's left once the flash keyframe
// ends — no JS timer needed to "turn off" the flash). Reduced-motion users
// get the steady tint with no animation at all, purely via a CSS media
// query — this module never needs to know about that preference.

import { RangeSetBuilder, StateEffect, StateField, type EditorState, type Extension } from "@codemirror/state";
import { Decoration, EditorView, type DecorationSet } from "@codemirror/view";

/// A 1-based, inclusive line range to tint — mirrors `lib/diff.ts`'s
/// `ChangedRange` shape (kept as a separate type here so this editor module
/// has no dependency on the diff-parsing lib beyond the shape itself).
export interface StoryLineRange {
  start: number;
  end: number;
}

export const setStoryLines = StateEffect.define<StoryLineRange[]>();

const STORY_LINE_DECO = Decoration.line({ class: "kbc-story-line kbc-story-line--flash" });

function buildDecorations(state: EditorState, ranges: StoryLineRange[]): DecorationSet {
  if (ranges.length === 0) return Decoration.none;
  const builder = new RangeSetBuilder<Decoration>();
  const doc = state.doc;
  for (const r of ranges) {
    const lo = Math.max(1, Math.min(r.start, r.end));
    const hi = Math.min(doc.lines, Math.max(r.start, r.end));
    for (let ln = lo; ln <= hi; ln++) {
      const line = doc.line(ln);
      builder.add(line.from, line.from, STORY_LINE_DECO);
    }
  }
  return builder.finish();
}

interface StoryOverlayState {
  ranges: StoryLineRange[];
  deco: DecorationSet;
}

const storyOverlayField = StateField.define<StoryOverlayState>({
  create: () => ({ ranges: [], deco: Decoration.none }),
  update(value, tr) {
    let ranges = value.ranges;
    let changed = false;
    for (const e of tr.effects) {
      if (e.is(setStoryLines)) {
        ranges = e.value;
        changed = true;
      }
    }
    if (changed) return { ranges, deco: buildDecorations(tr.state, ranges) };
    // Read-only reader (same note as `ageOverlay.ts`): `tr.changes` is
    // always empty in practice since this buffer never accepts a document
    // edit — `.map` on the non-effect branch keeps this correct rather than
    // merely convenient if that invariant is ever relaxed.
    return { ranges, deco: value.deco.map(tr.changes) };
  },
  provide: (field) => EditorView.decorations.from(field, (v) => v.deco),
});

/// Always present in `CodeView`'s extension set (like the age overlay) —
/// renders nothing when its range list is empty (no step tinted yet, or
/// story mode isn't in use for this view).
export const storyOverlayExtension: Extension = storyOverlayField;

export function setStoryOverlayLines(view: EditorView, ranges: StoryLineRange[]): void {
  view.dispatch({ effects: setStoryLines.of(ranges) });
}
