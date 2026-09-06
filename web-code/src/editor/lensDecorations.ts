// Code Vision lens line-widgets (V3.1-H3b).
//
// A block widget ABOVE each declaration line from `/api/lenses`, rendering
// compact chips: usage count (sum of three classes, tooltip breakdown),
// implementors (types only), and author (`agent · <session short>` or
// `human · <label>`). Click handlers fire host callbacks via a ref so the
// extension identity stays stable across renders.
//
// **No pain chip while `pain` is null** — the server always sends null in
// this wave; absence is the signal (no placeholder, no grey dot).
//
// Widgets are block decorations above the line so they do not fight sticky
// context, occurrence highlight, or param hints (gutter untouched).

import { StateEffect, StateField, type Extension } from "@codemirror/state";
import {
  Decoration,
  EditorView,
  WidgetType,
  type DecorationSet,
} from "@codemirror/view";
import type { LensDeclaration } from "../api/types";

export interface LensClickHandlers {
  onUsages?(decl: LensDeclaration): void;
  onImpls?(decl: LensDeclaration): void;
  onAuthor?(decl: LensDeclaration): void;
}

export interface LensDecorationPayload {
  declarations: LensDeclaration[];
  enabled: boolean;
}

export const setLensDecorations = StateEffect.define<LensDecorationPayload>();

function usageSum(d: LensDeclaration): number {
  return (d.usages?.exact ?? 0) + (d.usages?.likely ?? 0) + (d.usages?.candidate ?? 0);
}

function authorLabel(d: LensDeclaration): string | null {
  if (!d.author && !d.session) return null;
  const kind = (d.author?.kind || "human").toLowerCase();
  if (kind === "agent" && d.session?.short) {
    return `agent · ${d.session.short}`;
  }
  if (d.author?.label) {
    // Prefer a short sha-ish tail when label looks long.
    const lab = d.author.label;
    const short = lab.length > 12 ? lab.slice(0, 7) : lab;
    return `${kind} · ${short}`;
  }
  if (d.session?.short) return `${kind} · ${d.session.short}`;
  return kind;
}

class LensWidget extends WidgetType {
  constructor(
    readonly decl: LensDeclaration,
    readonly handlers: LensClickHandlers,
  ) {
    super();
  }
  eq(other: LensWidget) {
    const a = this.decl;
    const b = other.decl;
    return (
      a.line === b.line &&
      a.name === b.name &&
      usageSum(a) === usageSum(b) &&
      a.implementors === b.implementors &&
      authorLabel(a) === authorLabel(b) &&
      a.pain === b.pain
    );
  }
  toDOM() {
    const root = document.createElement("div");
    root.className = "kbc-lens";
    root.setAttribute("data-kbc-lens", this.decl.name);
    root.setAttribute("data-kbc-lens-line", String(this.decl.line));

    const u = usageSum(this.decl);
    const usagesChip = document.createElement("button");
    usagesChip.type = "button";
    usagesChip.className = "kbc-lens__chip kbc-lens__chip--usages";
    usagesChip.textContent = `${u} usage${u === 1 ? "" : "s"}`;
    usagesChip.title = `exact ${this.decl.usages.exact} · likely ${this.decl.usages.likely} · candidate ${this.decl.usages.candidate}`;
    usagesChip.setAttribute("data-kbc-lens-usages", this.decl.name);
    usagesChip.addEventListener("mousedown", (e) => {
      e.preventDefault();
      e.stopPropagation();
      this.handlers.onUsages?.(this.decl);
    });
    root.appendChild(usagesChip);

    if (this.decl.implementors != null) {
      const n = this.decl.implementors;
      const impls = document.createElement("button");
      impls.type = "button";
      impls.className = "kbc-lens__chip kbc-lens__chip--impls";
      impls.textContent = `${n} impl${n === 1 ? "" : "s"}`;
      impls.title = "Open type hierarchy";
      impls.setAttribute("data-kbc-lens-impls", this.decl.name);
      impls.addEventListener("mousedown", (e) => {
        e.preventDefault();
        e.stopPropagation();
        this.handlers.onImpls?.(this.decl);
      });
      root.appendChild(impls);
    }

    const author = authorLabel(this.decl);
    if (author) {
      const a = document.createElement("button");
      a.type = "button";
      a.className = "kbc-lens__chip kbc-lens__chip--author";
      a.textContent = author;
      a.title = "Open Provenance tab";
      a.setAttribute("data-kbc-lens-author", this.decl.name);
      a.addEventListener("mousedown", (e) => {
        e.preventDefault();
        e.stopPropagation();
        this.handlers.onAuthor?.(this.decl);
      });
      root.appendChild(a);
    }

    // pain is ALWAYS null this wave — deliberately no chip.
    return root;
  }
  ignoreEvent() {
    // Clicks handled by the chip listeners above.
    return false;
  }
}

function buildDeco(
  docLines: number,
  declarations: LensDeclaration[],
  enabled: boolean,
  handlers: LensClickHandlers,
  lineAt: (n: number) => { from: number } | null,
): DecorationSet {
  if (!enabled || declarations.length === 0) return Decoration.none;
  // Sort by line; decorations must be ordered by from.
  const sorted = [...declarations]
    .filter((d) => d.line >= 1 && d.line <= docLines)
    .sort((a, b) => a.line - b.line || a.name.localeCompare(b.name));
  const ranges = sorted.map((d) => {
    const line = lineAt(d.line);
    const from = line?.from ?? 0;
    return Decoration.widget({
      widget: new LensWidget(d, handlers),
      block: true,
      side: -1,
    }).range(from);
  });
  return Decoration.set(ranges, true);
}

interface LensFieldState {
  payload: LensDecorationPayload;
  deco: DecorationSet;
}

function emptyPayload(): LensDecorationPayload {
  return { declarations: [], enabled: true };
}

export function createLensDecorationsExtension(
  getHandlers: () => LensClickHandlers,
): Extension {
  const field = StateField.define<LensFieldState>({
    create: () => ({ payload: emptyPayload(), deco: Decoration.none }),
    update(value, tr) {
      let payload = value.payload;
      let changed = false;
      for (const e of tr.effects) {
        if (e.is(setLensDecorations)) {
          payload = e.value;
          changed = true;
        }
      }
      if (changed || tr.docChanged) {
        const handlers = getHandlers();
        const deco = buildDeco(
          tr.state.doc.lines,
          payload.declarations,
          payload.enabled,
          handlers,
          (n) => {
            try {
              return tr.state.doc.line(n);
            } catch {
              return null;
            }
          },
        );
        return { payload, deco };
      }
      return { payload, deco: value.deco.map(tr.changes) };
    },
    provide: (f) => EditorView.decorations.from(f, (v) => v.deco),
  });
  return field;
}

export function applyLensDecorations(
  view: EditorView,
  payload: LensDecorationPayload,
): void {
  view.dispatch({ effects: setLensDecorations.of(payload) });
}
