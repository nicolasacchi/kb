// V70-A6 — the inline peek WIDGET (§P7's "inline expansion", first slice).
//
// A CM6 block widget rendered under the caret line, showing the destination's
// own source with the server's highlight spans. The state machine is pure and
// lives in `lib/inlinePeek.ts`; this file is the CM6 half: one `StateField`
// holding a rendered view, one `StateEffect` to push a new one, and a
// `Decoration.widget({ block: true, side: 1 })` at the end of the host line.
//
// TWO THINGS IT OWNS THAT THE PURE HALF CANNOT:
//
//   1. **Focus.** The widget takes real DOM focus while open and stops every
//      keydown from bubbling — the same discipline `PeekPanel` uses, and the
//      reason `dismiss.peek` / `peek.context-more` / `peek.context-less` are
//      `dispatch: surface` rows in the registry. Without it the vim layer
//      underneath would also see `+`/`-`/`Esc`.
//   2. **Scroll restoration on close.** The stack records the host buffer's
//      `scrollTop` when it opens and puts it back when it closes, which is
//      the design's literal requirement ("Esc closes and restores the exact
//      prior scroll"). The widget is inserted BELOW the caret line, so
//      nothing above it moves and the saved offset is still the right one.
//
// It renders DOM directly rather than through React: a block widget's DOM is
// owned by CM6's view plugin lifecycle, and a React portal into it would put
// two reconcilers on one node for no gain — the card has no state of its own.

import { StateEffect, StateField, type Extension } from "@codemirror/state";
import { Decoration, EditorView, WidgetType, type DecorationSet } from "@codemirror/view";
import { buildLineSpans, paintLine, splitContentLines, type LineSpan } from "../lib/diffHighlight";
import {
  breadcrumb,
  excerptWindow,
  topFrame,
  type InlinePeekState,
} from "../lib/inlinePeek";
import type { Span } from "../api/types";

export interface InlinePeekHandlers {
  onClose?: () => void;
  /// `+` / `-` — the context dial.
  onContext?: (delta: 1 | -1) => void;
  /// `Enter` inside a peek — promote it to a real pane (the ONLY thing that
  /// creates a provisional pane, §P7).
  onPromote?: () => void;
  /// A click on a line inside the excerpt — nest one deeper.
  onNest?: (line: number) => void;
}

/// What the host hands the widget: the pure state plus the fetched spans for
/// each open frame's file (spans live outside `InlinePeekState` because they
/// are a big array the reducer has no business copying on every action).
export interface InlinePeekRender {
  state: InlinePeekState;
  spansByPath: Record<string, Span[] | undefined>;
}

export const setInlinePeek = StateEffect.define<InlinePeekRender | null>();

function el(tag: string, cls?: string, text?: string): HTMLElement {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (text !== undefined) e.textContent = text;
  return e;
}

class InlinePeekWidget extends WidgetType {
  constructor(
    readonly render: InlinePeekRender,
    readonly handlers: { current: InlinePeekHandlers },
  ) {
    super();
  }

  /// CM6 re-uses a widget's DOM when `eq` says the two are the same. Compare
  /// the fields that actually change the rendered card — a deep compare of
  /// the whole span array would be O(file) on every transaction.
  eq(other: InlinePeekWidget): boolean {
    const a = this.render.state;
    const b = other.render.state;
    if (a.frames.length !== b.frames.length || a.hostLine !== b.hostLine) return false;
    return a.frames.every((f, i) => {
      const g = b.frames[i];
      return (
        f.path === g.path &&
        f.line === g.line &&
        f.context === g.context &&
        f.loading === g.loading &&
        f.error === g.error &&
        (f.content?.length ?? -1) === (g.content?.length ?? -1)
      );
    });
  }

  toDOM(): HTMLElement {
    const { state, spansByPath } = this.render;
    const frame = topFrame(state);
    const root = el("div", "kbc-inpeek");
    root.setAttribute("data-kbc-inpeek", "");
    root.setAttribute("data-kbc-inpeek-depth", String(state.frames.length));
    root.tabIndex = -1;
    if (!frame) return root;

    // --- breadcrumb -------------------------------------------------------
    const crumbs = el("div", "kbc-inpeek__crumbs");
    crumbs.setAttribute("data-kbc-inpeek-crumbs", "");
    breadcrumb(state).forEach((label, i) => {
      if (i > 0) crumbs.appendChild(el("span", "kbc-inpeek__sep", "›"));
      crumbs.appendChild(el("span", "kbc-inpeek__crumb", label));
    });
    if (frame.trust) {
      const t = el("span", `kbc-inpeek__trust kbc-trust-${frame.trust}`, frame.trust);
      crumbs.appendChild(t);
    }
    const dial = el("span", "kbc-inpeek__dial");
    const less = el("button", "kbc-inpeek__dialbtn", "−");
    less.setAttribute("type", "button");
    less.setAttribute("data-cmd", "peek.context-less");
    less.setAttribute("data-kbc-inpeek-less", "");
    less.title = "less context (-)";
    less.onclick = (e) => {
      e.preventDefault();
      e.stopPropagation();
      this.handlers.current.onContext?.(-1);
    };
    const more = el("button", "kbc-inpeek__dialbtn", "+");
    more.setAttribute("type", "button");
    more.setAttribute("data-cmd", "peek.context-more");
    more.setAttribute("data-kbc-inpeek-more", "");
    more.title = "more context (+)";
    more.onclick = (e) => {
      e.preventDefault();
      e.stopPropagation();
      this.handlers.current.onContext?.(1);
    };
    dial.appendChild(less);
    dial.appendChild(more);
    const close = el("button", "kbc-inpeek__close", "✕");
    close.setAttribute("type", "button");
    close.setAttribute("aria-label", "close peek");
    close.setAttribute("data-kbc-inpeek-close", "");
    close.title = "close (Esc)";
    close.onclick = (e) => {
      e.preventDefault();
      e.stopPropagation();
      this.handlers.current.onClose?.();
    };
    crumbs.appendChild(dial);
    crumbs.appendChild(close);
    root.appendChild(crumbs);

    // --- body -------------------------------------------------------------
    const body = el("div", "kbc-inpeek__body");
    body.setAttribute("data-kbc-inpeek-body", "");
    if (frame.error) {
      body.appendChild(el("div", "kbc-inpeek__msg kbc-inpeek__msg--error", frame.error));
    } else if (frame.loading || frame.content === undefined) {
      body.appendChild(el("div", "kbc-inpeek__msg", "Loading…"));
    } else {
      const lines = splitContentLines(frame.content);
      const spans = spansByPath[frame.path] ?? [];
      const lineSpans: Map<number, LineSpan[]> = buildLineSpans(frame.content, spans);
      const win = excerptWindow(frame.line, frame.context, lines.length);
      for (let n = win.start; n <= win.end; n++) {
        const row = el("div", "kbc-inpeek__line" + (n === frame.line ? " kbc-inpeek__line--target" : ""));
        row.setAttribute("data-kbc-inpeek-line", String(n));
        const num = el("span", "kbc-inpeek__num", String(n));
        row.appendChild(num);
        const code = el("span", "kbc-inpeek__code");
        for (const seg of paintLine(lines[n - 1] ?? "", lineSpans.get(n))) {
          code.appendChild(el("span", seg.cls, seg.text));
        }
        row.appendChild(code);
        row.onclick = (e) => {
          e.preventDefault();
          e.stopPropagation();
          this.handlers.current.onNest?.(n);
        };
        body.appendChild(row);
      }
      if (win.end < win.start) {
        body.appendChild(el("div", "kbc-inpeek__msg", "empty file"));
      }
    }
    root.appendChild(body);

    const foot = el("div", "kbc-inpeek__foot");
    foot.appendChild(el("span", "kbc-inpeek__where", `${frame.path}:${frame.line}`));
    foot.appendChild(
      el("span", "kbc-inpeek__keys", "Esc close · +/− context · Enter open in a pane · gd nest"),
    );
    root.appendChild(foot);

    // Focus + key ownership (see the module doc). `keydown` is handled HERE
    // so the vim layer underneath never also sees these keys.
    root.addEventListener("keydown", (e: KeyboardEvent) => {
      if (e.metaKey || e.altKey || e.ctrlKey) return;
      switch (e.key) {
        case "Escape":
          e.preventDefault();
          e.stopPropagation();
          this.handlers.current.onClose?.();
          return;
        case "+":
        case "=":
          e.preventDefault();
          e.stopPropagation();
          this.handlers.current.onContext?.(1);
          return;
        case "-":
          e.preventDefault();
          e.stopPropagation();
          this.handlers.current.onContext?.(-1);
          return;
        case "Enter":
          e.preventDefault();
          e.stopPropagation();
          this.handlers.current.onPromote?.();
          return;
      }
    });
    // A widget's DOM is created before it is in the document; focus on the
    // next frame, and only if it is still there.
    requestAnimationFrame(() => {
      if (root.isConnected) root.focus({ preventScroll: true });
    });
    return root;
  }

  /// Keep CM6's own event handling out of the card entirely — every key and
  /// click inside it is the widget's.
  ignoreEvent(): boolean {
    return true;
  }
}

/// The extension. `handlersRef.current` is re-read at call time (the same
/// ref idiom `editor/lineGutter.ts` documents) so the host can hand it fresh
/// closures every render without recreating the extension.
export function inlinePeekExtension(handlersRef: { current: InlinePeekHandlers }): Extension {
  const field = StateField.define<{ render: InlinePeekRender | null; decos: DecorationSet }>({
    create() {
      return { render: null, decos: Decoration.none };
    },
    update(value, tr) {
      let next = value.render;
      for (const e of tr.effects) if (e.is(setInlinePeek)) next = e.value;
      if (next === value.render && !tr.docChanged) {
        return value;
      }
      if (!next || next.state.frames.length === 0) {
        return { render: null, decos: Decoration.none };
      }
      const lineCount = tr.state.doc.lines;
      const hostLine = Math.min(Math.max(1, next.state.hostLine), lineCount);
      const at = tr.state.doc.line(hostLine).to;
      const deco = Decoration.widget({
        widget: new InlinePeekWidget(next, handlersRef),
        block: true,
        side: 1,
      });
      return { render: next, decos: Decoration.set([deco.range(at)]) };
    },
    provide: (f) => EditorView.decorations.from(f, (v) => v.decos),
  });
  return [field];
}

/// Push a rendered stack into `view` (or `null` to close). Returns the host
/// buffer's CURRENT scroll offset, which the caller stores on the state when
/// opening — the value `restoreInlinePeekScroll` puts back on close.
export function applyInlinePeek(view: EditorView, render: InlinePeekRender | null): number {
  const top = view.scrollDOM.scrollTop;
  view.dispatch({ effects: setInlinePeek.of(render) });
  return top;
}

/// Put the host buffer back exactly where it was when the peek opened.
/// Deferred one frame: the widget's removal changes layout, and setting
/// `scrollTop` before CM6 has re-measured would be immediately overwritten.
export function restoreInlinePeekScroll(view: EditorView, scrollTop: number): void {
  requestAnimationFrame(() => {
    view.scrollDOM.scrollTop = scrollTop;
  });
}
