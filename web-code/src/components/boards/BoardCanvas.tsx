// `kbc-canvas/1` — the SURFACE (V74-L2, D10).
//
// Cards at the positions `lib/boardLayout.ts` computed, edges drawn between
// them, and a camera. Three rules:
//
// **This component computes NO geometry.** Every `x`/`y` it renders came out of
// the layout engine (or out of a PIN, which is the one authored coordinate on
// the wire). If a position has to be worked out, it is worked out there.
//
// **Dragging does not write a position.** D10 stores boards coordinate-free
// plus pins, and the only way a coordinate reaches the daemon is the explicit
// "pin here" action on a card — so the pointer here pans the CAMERA and
// nothing else. There is no freehand drawing either (D21's refusal list).
//
// **A derived edge shows its class in LINE STYLE; an authored edge has one
// stroke.** `lib/boards.ts`'s `edgeClass` owns that mapping, over kbc-theme/1's
// Lane Budget — trust is line style, never a hue this feature picks, so a
// hand-drawn arrow can never be mistaken for a SCIP-verified one.
//
// `prefers-reduced-motion` is honoured: the walkthrough camera JUMPS rather
// than animating when the operator has asked for less motion.

import { useCallback, useEffect, useRef, useState, type PointerEvent as ReactPointerEvent, type WheelEvent as ReactWheelEvent } from "react";
import type { BoardEdge, BoardNode } from "../../api/types";
import { edgeClass, edgeTitle } from "../../lib/boards";
import { columnFromLineClick } from "../../lib/identResolve";
import type { BoardLayout, Camera } from "../../lib/boardLayout";
import BoardCard from "./BoardCard";

export const BOARD_MIN_ZOOM = 0.35;
export const BOARD_MAX_ZOOM = 2;

export interface BoardCanvasProps {
  repo: string;
  nodes: BoardNode[];
  edges: BoardEdge[];
  layout: BoardLayout;
  folded: ReadonlySet<string>;
  expanded: ReadonlySet<string>;
  focusedId: string | null;
  /// When set, the surface moves to this camera (a walkthrough step). `null`
  /// leaves the camera under the human's own control.
  camera: Camera | null;
  onToggleFold(id: string): void;
  onThread(node: BoardNode): void;
  onPin?(node: BoardNode, at: { x: number; y: number }): void;
  onUnpin?(node: BoardNode): void;
  onFocus(id: string): void;
  /// A click that landed on a snippet line inside a code card.
  onSnippetClick(node: BoardNode, lineIndex: number, lineText: string, col: number, at: { top: number; bottom: number; left: number }): void;
}

function prefersReducedMotion(): boolean {
  if (typeof window === "undefined" || !window.matchMedia) return false;
  return window.matchMedia("(prefers-reduced-motion: reduce)").matches;
}

export default function BoardCanvas(props: BoardCanvasProps) {
  const { layout, camera } = props;
  const surfaceRef = useRef<HTMLDivElement | null>(null);
  const [view, setView] = useState<Camera>({ x: 0, y: 0, zoom: 1 });
  const panRef = useRef<{ x: number; y: number; vx: number; vy: number } | null>(null);

  // The walkthrough's camera is applied here rather than being held as a
  // second source of truth: `camera` is derived from the step in the URL and
  // from the layout, so a re-layout can never leave it pointing at nothing.
  useEffect(() => {
    if (!camera) return;
    setView(camera);
  }, [camera]);

  const onWheel = useCallback((e: ReactWheelEvent) => {
    if (!e.ctrlKey && !e.metaKey) return;
    e.preventDefault();
    setView((v) => {
      const next = Math.min(BOARD_MAX_ZOOM, Math.max(BOARD_MIN_ZOOM, v.zoom * (e.deltaY < 0 ? 1.1 : 1 / 1.1)));
      return { ...v, zoom: next };
    });
  }, []);

  const onPointerDown = useCallback((e: ReactPointerEvent) => {
    // Only a drag on the BACKGROUND pans; a drag that starts on a card is the
    // browser's own text selection, which a reading surface must keep.
    if ((e.target as HTMLElement).closest("[data-kbc-board-node]")) return;
    panRef.current = { x: e.clientX, y: e.clientY, vx: view.x, vy: view.y };
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
  }, [view.x, view.y]);

  const onPointerMove = useCallback((e: ReactPointerEvent) => {
    const p = panRef.current;
    if (!p) return;
    setView((v) => ({ ...v, x: p.vx + (e.clientX - p.x), y: p.vy + (e.clientY - p.y) }));
  }, []);

  const onPointerUp = useCallback((e: ReactPointerEvent) => {
    panRef.current = null;
    if ((e.currentTarget as HTMLElement).hasPointerCapture?.(e.pointerId)) {
      (e.currentTarget as HTMLElement).releasePointerCapture(e.pointerId);
    }
  }, []);

  // Focus follows the focused card, so `j`/`k` scroll it into view without a
  // scroll position this component would have to own.
  useEffect(() => {
    if (!props.focusedId) return;
    const el = surfaceRef.current?.querySelector<HTMLElement>(
      `[data-kbc-board-node="${CSS.escape(props.focusedId)}"]`,
    );
    el?.scrollIntoView({
      block: "nearest",
      inline: "nearest",
      behavior: prefersReducedMotion() ? "auto" : "smooth",
    });
  }, [props.focusedId]);

  const onClickCapture = useCallback(
    (e: React.MouseEvent) => {
      const target = e.target as HTMLElement;
      const lineEl = target.closest<HTMLElement>(".kbc-refcard__text");
      if (!lineEl) return;
      const cardEl = target.closest<HTMLElement>("[data-kbc-board-node]");
      const id = cardEl?.dataset.kbcBoardNode;
      const node = props.nodes.find((n) => n.id === id);
      if (!node || !node.code) return;
      const lineRow = lineEl.closest<HTMLElement>(".kbc-refcard__line");
      const snippetEl = lineRow?.parentElement;
      if (!lineRow || !snippetEl) return;
      const lineIndex = Array.prototype.indexOf.call(snippetEl.children, lineRow);
      if (lineIndex < 0) return;
      const col = caretColumn(lineEl, e.clientX, e.clientY);
      if (col === null) return;
      const rect = lineRow.getBoundingClientRect();
      props.onSnippetClick(node, lineIndex, lineEl.textContent ?? "", col, {
        top: rect.top,
        bottom: rect.bottom,
        left: rect.left,
      });
    },
    [props],
  );

  return (
    <div
      className="kbc-board__surface"
      data-kbc-board-surface
      ref={surfaceRef}
      onWheel={onWheel}
      onPointerDown={onPointerDown}
      onPointerMove={onPointerMove}
      onPointerUp={onPointerUp}
      onPointerCancel={onPointerUp}
      onClickCapture={onClickCapture}
    >
      <div
        className="kbc-board__world"
        style={{
          transform: `translate(${view.x}px, ${view.y}px) scale(${view.zoom})`,
          width: layout.width,
          height: layout.height,
        }}
        data-kbc-board-world
      >
        <svg
          className="kbc-board__edges"
          width={layout.width}
          height={layout.height}
          aria-hidden="true"
        >
          {props.edges.map((e, i) => {
            const placed = layout.edges.find((p) => p.from === e.from && p.to === e.to);
            if (!placed) return null;
            return (
              <line
                key={`${e.from}-${e.to}-${e.kind}-${i}`}
                className={edgeClass(e)}
                x1={placed.x1}
                y1={placed.y1}
                x2={placed.x2}
                y2={placed.y2}
                data-kbc-board-edge={`${e.from}->${e.to}`}
                data-kbc-board-edge-kind={e.kind}
                data-kbc-board-edge-provenance={e.provenance}
              >
                <title>{edgeTitle(e)}</title>
              </line>
            );
          })}
        </svg>
        {props.nodes.map((n) => {
          const p = layout.byId.get(n.id);
          if (!p) return null;
          return (
            <div
              key={n.id}
              className="kbc-board__slot"
              style={{ left: p.x, top: p.y, width: p.w }}
              onFocusCapture={() => props.onFocus(n.id)}
            >
              <BoardCard
                node={n}
                repo={props.repo}
                folded={props.folded.has(n.id)}
                expanded={props.expanded.has(n.id)}
                focused={props.focusedId === n.id}
                onToggleFold={props.onToggleFold}
                onThread={props.onThread}
                onPin={props.onPin ? (node) => props.onPin?.(node, { x: p.x, y: p.y }) : undefined}
                onUnpin={props.onUnpin}
              />
            </div>
          );
        })}
      </div>
      {layout.truncated > 0 && (
        <p className="kbc-board__truncated" data-kbc-board-truncated>
          {layout.truncated} node(s) exceeded the layout cap and are not placed — the board says
          so rather than dropping them quietly.
        </p>
      )}
    </div>
  );
}

/// The 0-based UTF-16 column a click landed on inside a rendered line, using
/// the platform's caret-from-point API. `null` when the platform has neither
/// (an honest miss, not column 0) — the card then simply does not resolve,
/// which is better than resolving the wrong identifier.
function caretColumn(lineEl: HTMLElement, x: number, y: number): number | null {
  const doc = lineEl.ownerDocument;
  // Both APIs are feature-DETECTED rather than assumed: `caretPositionFromPoint`
  // is the standard one (Firefox, and Chromium since 128) and
  // `caretRangeFromPoint` is WebKit/older-Chromium's. Typed structurally
  // through `unknown` because lib.dom's own signatures differ between the two
  // and neither is optional in its declaration.
  const d = doc as unknown as {
    caretPositionFromPoint?: (x: number, y: number) => { offsetNode: Node; offset: number } | null;
    caretRangeFromPoint?: (x: number, y: number) => Range | null;
  };
  let node: Node | null = null;
  let offset = 0;
  if (typeof d.caretPositionFromPoint === "function") {
    const pos = d.caretPositionFromPoint(x, y);
    if (!pos) return null;
    node = pos.offsetNode;
    offset = pos.offset;
  } else if (typeof d.caretRangeFromPoint === "function") {
    const range = d.caretRangeFromPoint(x, y);
    if (!range) return null;
    node = range.startContainer;
    offset = range.startOffset;
  } else {
    return null;
  }
  if (!node || !lineEl.contains(node)) return null;
  // The offset-within-the-line arithmetic lives in `lib/identResolve.ts` with
  // the rest of the "which identifier is this" rule; all this function adds is
  // the platform call that turns a POINT into a (node, offset) pair.
  return columnFromLineClick(lineEl, node, offset);
}
