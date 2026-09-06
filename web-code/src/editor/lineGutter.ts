// A generic, hoverable/clickable CM6 line-gutter factory — the shared
// plumbing behind both the blame gutter (dots, W4.4) and the annotations
// gutter (existing-annotation markers + a hover "+" affordance, W4.6). Each
// call to `createLineGutter` defines its OWN `StateField`/`StateEffect`
// pair (CM6 identity requires that — see the doc below), so this is a
// factory invoked ONCE per `CodeView` mount (via a `useRef` initializer),
// never per-render.
//
// Markers are supplied from React via `setMarkers` (an imperative
// `view.dispatch`, called from a `useEffect` whenever the underlying data —
// blame attributions, the annotations list — changes); hover/click state
// lives INSIDE the field (driven by `domEventHandlers`) so a marker
// redraws on hover without React re-rendering the whole editor. Click/hover
// callbacks are read from a caller-owned `{ current: LineGutterHandlers }`
// ref rather than captured at `createLineGutter` call time — the ref's
// `.current` is reassigned every render, so the extension always calls the
// LATEST closure without needing to be recreated (which would break CM6's
// "one gutter, one identity" contract above).

import { StateEffect, StateField, type Extension } from "@codemirror/state";
import { EditorView, gutter, GutterMarker, type BlockInfo } from "@codemirror/view";

export interface LineMarkerSpec {
  className: string;
  title: string;
}

export interface LineGutterHandlers {
  onHover?: (line: number, rect: DOMRect) => void;
  onUnhover?: (line: number) => void;
  onClick?: (line: number, rect: DOMRect) => void;
}

export interface LineGutterHandle {
  extension: Extension;
  /// Replace the gutter's marker map (1-based line → spec). Lines absent
  /// from the map render no marker (besides the `hoverFallback`, if any).
  setMarkers: (view: EditorView, markers: Map<number, LineMarkerSpec>) => void;
}

class LineDotMarker extends GutterMarker {
  constructor(
    readonly spec: LineMarkerSpec,
    readonly hovered: boolean,
  ) {
    super();
  }
  eq(other: LineDotMarker): boolean {
    return (
      other.spec.className === this.spec.className &&
      other.spec.title === this.spec.title &&
      other.hovered === this.hovered
    );
  }
  toDOM(): Node {
    const el = document.createElement("span");
    el.className =
      `kbc-linegutter-dot ${this.spec.className}` + (this.hovered ? " kbc-linegutter-dot--hover" : "");
    el.title = this.spec.title;
    return el;
  }
}

function lineNumberAt(view: EditorView, block: BlockInfo): number {
  return view.state.doc.lineAt(block.from).number;
}

/// `gutterClass` distinguishes this gutter's DOM wrapper (`.cm-gutter` gets
/// an extra class) from any other gutter on the same view — CM6 orders
/// gutters by extension priority, not by this string, but it's a useful
/// hook for e2e/CSS. `hoverFallback`, when given, renders on whichever line
/// is currently hovered AND has no marker of its own (the annotations
/// gutter's "+ add annotation" affordance; the blame gutter passes none —
/// every line is still clickable there via `domEventHandlers`, it just
/// shows no marker when there's nothing to preview, per W4.4's "honest
/// absence").
export function createLineGutter(
  gutterClass: string,
  handlersRef: { current: LineGutterHandlers },
  hoverFallback?: LineMarkerSpec,
): LineGutterHandle {
  const setMarkersEffect = StateEffect.define<Map<number, LineMarkerSpec>>();
  const setHoverEffect = StateEffect.define<number | null>();

  interface FieldState {
    markers: Map<number, LineMarkerSpec>;
    hoverLine: number | null;
  }

  const field = StateField.define<FieldState>({
    create: () => ({ markers: new Map(), hoverLine: null }),
    update(value, tr) {
      let { markers, hoverLine } = value;
      let changed = false;
      for (const e of tr.effects) {
        if (e.is(setMarkersEffect)) {
          markers = e.value;
          changed = true;
        } else if (e.is(setHoverEffect)) {
          hoverLine = e.value;
          changed = true;
        }
      }
      return changed ? { markers, hoverLine } : value;
    },
  });

  const gutterExt = gutter({
    class: gutterClass,
    // `true`, deliberately: CM6's default `false` skips creating a DOM
    // element ENTIRELY for a line with no marker (`SingleGutterView.line`'s
    // `localMarkers.length == 0 && !renderEmptyElements` early return). W4.4
    // needs "click ANY line to open the honest-absence panel" to work even
    // when NOT ONE line in the whole file has a dot (a repo with zero join
    // hits — see the module doc's `domEventHandlers` delegation note) — a
    // gutter with zero rendered children can collapse to zero height,
    // which would make its own event-delegation fallback (`event.clientY`
    // → `lineBlockAtHeight`) unreachable since there's nothing to click.
    // Always rendering an (empty, when there's no marker) `.cm-gutterElement`
    // per line keeps the gutter's height matching the content's, so every
    // line stays clickable/hoverable regardless of how many carry a marker.
    renderEmptyElements: true,
    lineMarker(view, block) {
      const { markers, hoverLine } = view.state.field(field);
      const line = lineNumberAt(view, block);
      const spec = markers.get(line) ?? (hoverFallback && hoverLine === line ? hoverFallback : null);
      if (!spec) return null;
      return new LineDotMarker(spec, hoverLine === line);
    },
    lineMarkerChange: (update) =>
      update.transactions.some((tr) =>
        tr.effects.some((e) => e.is(setMarkersEffect) || e.is(setHoverEffect)),
      ),
    domEventHandlers: {
      mouseover(view, block, event) {
        const line = lineNumberAt(view, block);
        const { hoverLine } = view.state.field(field);
        if (hoverLine !== line) view.dispatch({ effects: setHoverEffect.of(line) });
        const target = event.target as HTMLElement;
        handlersRef.current.onHover?.(line, target.getBoundingClientRect());
        return false;
      },
      mouseout(view, block) {
        const line = lineNumberAt(view, block);
        view.dispatch({ effects: setHoverEffect.of(null) });
        handlersRef.current.onUnhover?.(line);
        return false;
      },
      mousedown(view, block, event) {
        const line = lineNumberAt(view, block);
        const target = event.target as HTMLElement;
        handlersRef.current.onClick?.(line, target.getBoundingClientRect());
        return true;
      },
    },
  });

  return {
    extension: [field, gutterExt],
    setMarkers: (view, markers) => view.dispatch({ effects: setMarkersEffect.of(markers) }),
  };
}
