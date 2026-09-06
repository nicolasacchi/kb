// CM6 extension: tints a line whenever it starts a real git conflict marker
// (`<<<<<<< `, `=======`, `>>>>>>> `) — Phase G2's reader-chrome half of the
// repo-state banner: the reader ALREADY renders working-tree conflict
// markers as plain text (it's just the file's own bytes), so this overlay
// is purely a visual "here's where the conflict actually is" aid, active
// only while the CURRENTLY open file is named in `GET /api/repo-state`'s
// own `conflicted` list (`Reader.tsx` wires `active` from that).
//
// A `StateField`/`StateEffect` pair at MODULE scope, mirroring `editor/
// ageOverlay.ts`'s own shape/rationale (one age overlay per `CodeView`, so
// no per-call factory is needed the way `editor/lineGutter.ts`'s two
// distinct gutters need one each) — the SAME "reusing the overlay
// machinery" this module's own doc comment (phase brief) points at.
//
// Read-only reader (same note as `ageOverlay.ts`/`linkify.ts`): `tr.changes`
// is always empty in practice since this buffer never accepts a document
// edit — `.map` on the non-effect branch keeps this correct rather than
// merely convenient if that invariant is ever relaxed.

import { RangeSetBuilder, StateEffect, StateField, type EditorState, type Extension } from "@codemirror/state";
import { Decoration, EditorView, type DecorationSet } from "@codemirror/view";

export const setConflictActive = StateEffect.define<boolean>();

const CONFLICT_LINE_DECORATION = Decoration.line({ class: "kbc-conflict-line" });

// Real git conflict marker lines (`git merge`/`git rebase`'s own output,
// NOT `git merge-tree`'s dry-run — that call never touches the working
// tree, see `history::merge_check`'s module doc): the "ours" start, the
// separator, and the "theirs" end. `=======` never carries trailing content
// (it's always the bare 7-character marker on its own line); `<<<<<<< `/
// `>>>>>>> ` are always followed by a ref/label, hence the trailing space in
// those two patterns but not the separator's.
const CONFLICT_START = /^<<<<<<< /;
const CONFLICT_SEP = /^=======\s*$/;
const CONFLICT_END = /^>>>>>>> /;

function isConflictMarkerLine(text: string): boolean {
  return CONFLICT_START.test(text) || CONFLICT_SEP.test(text) || CONFLICT_END.test(text);
}

function buildDecorations(state: EditorState, active: boolean): DecorationSet {
  if (!active) return Decoration.none;
  const builder = new RangeSetBuilder<Decoration>();
  const doc = state.doc;
  for (let ln = 1; ln <= doc.lines; ln++) {
    const line = doc.line(ln);
    if (isConflictMarkerLine(line.text)) {
      builder.add(line.from, line.from, CONFLICT_LINE_DECORATION);
    }
  }
  return builder.finish();
}

interface ConflictOverlayState {
  active: boolean;
  deco: DecorationSet;
}

const conflictOverlayField = StateField.define<ConflictOverlayState>({
  create: () => ({ active: false, deco: Decoration.none }),
  update(value, tr) {
    let active = value.active;
    for (const e of tr.effects) {
      if (e.is(setConflictActive)) active = e.value;
    }
    if (active !== value.active) {
      return { active, deco: buildDecorations(tr.state, active) };
    }
    return { active, deco: value.deco.map(tr.changes) };
  },
  provide: (field) => EditorView.decorations.from(field, (v) => v.deco),
});

/// Always present in `CodeView`'s extension set (like the age overlay) —
/// renders nothing when inactive (the open file isn't in `conflicted`, or
/// `GET /api/repo-state` hasn't resolved yet), never a structural
/// difference that would force a view recreation just to toggle it.
export const conflictOverlayExtension: Extension = conflictOverlayField;

export function setConflictOverlayActive(view: EditorView, active: boolean): void {
  view.dispatch({ effects: setConflictActive.of(active) });
}
