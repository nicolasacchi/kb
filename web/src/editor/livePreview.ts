// Inline live-preview decorations — the Obsidian-style "render in place" layer.
//
// A ViewPlugin walks the Lezer markdown syntax tree over the VISIBLE ranges and
// produces a DecorationSet that, on lines the cursor/selection is NOT on:
//   - hides inline markup (`**`, `*`, `~~`, `` ` ``, `#`, `>`, link brackets/URL)
//   - turns `- [ ]`/`- [x]` into a real, toggleable checkbox (editing the source)
//   - turns `---` into a divider rule
//   - turns `![](attachment:<aid>)` into an inline thumbnail (via the kb/id facet)
// On the active line the raw markdown is REVEALED so it can be edited — exactly
// Obsidian's behaviour. Rebuilds skip while IME composition is active.
//
// This whole module is isolated + gated behind the `livePreview` config flag
// (extensions.ts), so it can be reshaped or turned off without touching the
// rest of the editor.

import {
  Decoration,
  type DecorationSet,
  EditorView,
  ViewPlugin,
  type ViewUpdate,
  WidgetType,
} from "@codemirror/view";
import { Facet, type Range } from "@codemirror/state";
import { syntaxTree } from "@codemirror/language";
import { attachmentServeUrl } from "../lib/attachmentUrl";

export type AttachmentCtx = { kb: string; id: string };

/// kb/id needed to resolve inline `attachment:<aid>` thumbnails. Null in
/// contexts with no artifact (a bare preview) → thumbnails fall back to raw.
export const attachmentCtxFacet = Facet.define<AttachmentCtx, AttachmentCtx | null>(
  {
    combine: (vals) => (vals.length ? vals[vals.length - 1] : null),
  },
);

// --- widgets ---------------------------------------------------------------

class CheckboxWidget extends WidgetType {
  constructor(
    readonly checked: boolean,
    readonly pos: number,
  ) {
    super();
  }
  eq(o: CheckboxWidget) {
    return o.checked === this.checked && o.pos === this.pos;
  }
  toDOM(view: EditorView) {
    const box = document.createElement("input");
    box.type = "checkbox";
    box.checked = this.checked;
    box.className = "cm-kb-task";
    box.setAttribute("aria-label", "toggle task");
    // mousedown would move the caret onto this (inactive) line and reveal the
    // raw markup before our click fires — preventDefault keeps it inert.
    box.addEventListener("mousedown", (e) => e.preventDefault());
    box.addEventListener("click", (e) => {
      e.preventDefault();
      view.dispatch({
        changes: { from: this.pos, to: this.pos + 1, insert: this.checked ? " " : "x" },
        userEvent: "input.task.toggle",
      });
    });
    return box;
  }
  ignoreEvent() {
    return false; // let the checkbox receive its own click
  }
}

class DividerWidget extends WidgetType {
  eq() {
    return true;
  }
  toDOM() {
    const hr = document.createElement("span");
    hr.className = "cm-kb-hr";
    hr.setAttribute("aria-hidden", "true");
    return hr;
  }
}

class ThumbnailWidget extends WidgetType {
  constructor(
    readonly url: string,
    readonly alt: string,
  ) {
    super();
  }
  eq(o: ThumbnailWidget) {
    return o.url === this.url && o.alt === this.alt;
  }
  toDOM() {
    const img = document.createElement("img");
    img.src = this.url;
    img.alt = this.alt;
    img.className = "cm-kb-thumb";
    img.loading = "lazy";
    return img;
  }
}

// --- decoration construction ----------------------------------------------

const hideMark = Decoration.replace({});

// Mark nodes whose text is pure markup → hidden on inactive lines.
const HIDE_MARK_NODES = new Set([
  "EmphasisMark",
  "CodeMark",
  "StrikethroughMark",
  "QuoteMark",
  "LinkMark",
  "URL",
  "SubscriptMark",
  "SuperscriptMark",
]);

const IMAGE_RE = /^!\[([^\]]*)\]\(attachment:([A-Za-z0-9_]+)\)$/;

function buildDecorations(view: EditorView): DecorationSet {
  const ctx = view.state.facet(attachmentCtxFacet);
  const { state } = view;

  // Lines touched by any selection range → revealed (raw markdown shown).
  const activeLines = new Set<number>();
  for (const r of state.selection.ranges) {
    const a = state.doc.lineAt(r.from).number;
    const b = state.doc.lineAt(r.to).number;
    for (let n = a; n <= b; n++) activeLines.add(n);
  }
  const isActive = (pos: number) => activeLines.has(state.doc.lineAt(pos).number);

  const ranges: Range<Decoration>[] = [];

  for (const { from, to } of view.visibleRanges) {
    syntaxTree(state).iterate({
      from,
      to,
      enter: (node) => {
        if (node.from === node.to) return;
        const name = node.name;

        // Image → inline attachment thumbnail (only for resolvable refs).
        if (name === "Image") {
          if (isActive(node.from) || !ctx) return; // raw on active line / no ctx
          const text = state.sliceDoc(node.from, node.to);
          const m = text.match(IMAGE_RE);
          if (!m) return; // non-attachment image → let marks hide (descend)
          ranges.push(
            Decoration.replace({
              widget: new ThumbnailWidget(
                attachmentServeUrl(ctx.kb, ctx.id, m[2]),
                m[1] || "attachment",
              ),
            }).range(node.from, node.to),
          );
          return false; // don't descend into the image's marks
        }

        // Horizontal rule → divider widget.
        if (name === "HorizontalRule") {
          if (isActive(node.from)) return;
          ranges.push(
            Decoration.replace({ widget: new DividerWidget() }).range(node.from, node.to),
          );
          return false;
        }

        // Task checkbox → interactive widget (always; clicking edits source).
        if (name === "TaskMarker") {
          if (isActive(node.from)) return;
          const inner = state.sliceDoc(node.from + 1, node.from + 2);
          ranges.push(
            Decoration.replace({
              widget: new CheckboxWidget(/[xX]/.test(inner), node.from + 1),
            }).range(node.from, node.to),
          );
          return;
        }

        // ATX heading marker (`##`) + its trailing spaces.
        if (name === "HeaderMark") {
          if (isActive(node.from)) return;
          let end = node.to;
          while (end < state.doc.length && state.sliceDoc(end, end + 1) === " ") end++;
          ranges.push(hideMark.range(node.from, end));
          return;
        }

        // Generic inline markup → hide on inactive lines.
        if (HIDE_MARK_NODES.has(name)) {
          if (isActive(node.from)) return;
          ranges.push(hideMark.range(node.from, node.to));
          return;
        }
        return;
      },
    });
  }

  ranges.sort((a, b) => a.from - b.from || a.value.startSide - b.value.startSide);
  return Decoration.set(ranges, true);
}

/// The live-preview ViewPlugin. Decorations rebuild on doc/viewport/selection
/// change, but NOT mid-IME-composition (atomic-range churn during composition
/// is the sharp edge — invariant in the plan).
export function livePreview() {
  return ViewPlugin.fromClass(
    class {
      decorations: DecorationSet;
      constructor(view: EditorView) {
        this.decorations = buildDecorations(view);
      }
      update(u: ViewUpdate) {
        if (u.view.composing) return; // leave decorations stable during IME
        if (u.docChanged || u.viewportChanged || u.selectionSet) {
          this.decorations = buildDecorations(u.view);
        }
      }
    },
    {
      decorations: (v) => v.decorations,
      // Replaced ranges are atomic so the caret jumps over hidden markup
      // instead of landing inside a zero-width gap. Active lines carry no
      // decorations, so the line being edited is never atomic.
      provide: (plugin) =>
        EditorView.atomicRanges.of((view) => view.plugin(plugin)?.decorations ?? Decoration.none),
    },
  );
}
