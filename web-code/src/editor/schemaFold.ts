// V72-I2 — the annotaterb schema FOLD.
//
// One block `Decoration.replace` over the `# == Schema Information` banner's
// line range, swapped in and out through a `Compartment` (`CodeView`'s own
// `wrapCompartment` pattern) so toggling it never remounts the view and
// never loses scroll position or the live selection.
//
// WHY NOT `@codemirror/language`'s `foldService`/`codeFolding`. That package
// is not a direct dependency of `web-code` (only `@codemirror/{commands,
// search,state,view}` are — it is present transitively at 6.12.4), and
// adding one to fold exactly ONE known range would buy a fold GUTTER, a fold
// keymap and a second fold state machine this reader does not want: the
// banner's range is already known exactly (`lib/annotaterb.ts` parsed it),
// there is nothing to discover, and the buffer is read-only so no edit can
// invalidate it. A replace decoration is the whole mechanism the feature
// needs. If a general "fold any block" reader feature ever lands, THAT is
// the unit that should take the dependency — and this extension should be
// deleted in favour of it, not kept beside it.
import { StateField, type Extension } from "@codemirror/state";
import { foldRangeIsValid } from "../lib/annotaterb";
import { Decoration, EditorView, WidgetType, type DecorationSet } from "@codemirror/view";

export interface SchemaFoldSpec {
  /// 1-based, inclusive.
  startLine: number;
  endLine: number;
  /// What the placeholder says in place of the block.
  label: string;
  /// Clicking the placeholder unfolds. Optional — the rail button and
  /// `rails.schema-fold` are the other two doors onto the same state.
  onUnfold?: () => void;
}

class SchemaFoldWidget extends WidgetType {
  constructor(
    readonly label: string,
    readonly onUnfold?: () => void,
  ) {
    super();
  }

  eq(other: SchemaFoldWidget): boolean {
    return other.label === this.label;
  }

  toDOM(): HTMLElement {
    const el = document.createElement("div");
    el.className = "kbc-schemafold";
    el.setAttribute("data-kbc-schema-fold-widget", "");
    const btn = document.createElement("button");
    btn.type = "button";
    btn.className = "kbc-schemafold__btn";
    btn.textContent = this.label;
    btn.title = "Unfold the annotaterb schema banner";
    btn.addEventListener("click", (e) => {
      e.preventDefault();
      this.onUnfold?.();
    });
    el.appendChild(btn);
    return el;
  }

  /// The placeholder is INTERACTIVE, so CM6 must let its events through
  /// rather than treating them as editor input.
  ignoreEvent(): boolean {
    return false;
  }
}

/// The extension, or `[]` when there is nothing to fold. `null` is the
/// ordinary state — most files carry no banner.
export function schemaFoldExtension(spec: SchemaFoldSpec | null): Extension {
  if (!spec) return [];
  const build = (doc: { lines: number; line: (n: number) => { from: number; to: number } }): DecorationSet => {
    if (!foldRangeIsValid(doc.lines, spec.startLine, spec.endLine)) return Decoration.none;
    const from = doc.line(spec.startLine).from;
    const to = doc.line(spec.endLine).to;
    return Decoration.set([
      Decoration.replace({
        block: true,
        widget: new SchemaFoldWidget(spec.label, spec.onUnfold),
      }).range(from, to),
    ]);
  };
  const field = StateField.define<DecorationSet>({
    create: (state) => build(state.doc),
    update: (value, tr) => (tr.docChanged ? build(tr.state.doc) : value),
    provide: (f) => EditorView.decorations.from(f),
  });
  return [field];
}
