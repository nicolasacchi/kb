import {
  forwardRef,
  useEffect,
  useImperativeHandle,
  useMemo,
  useRef,
  useState,
} from "react";
import { useWindowVirtualizer } from "@tanstack/react-virtual";
import Card from "./Card";
import type { DocSummary } from "../api/client";
import type { Progress } from "../hooks/useReadingProgress";
import type { SessionRow } from "../api/sessions";

// S-milestone S5 — virtualised card grid for the gallery's
// scale-mode path. Uses window-scroll virtualisation so the existing
// SPA scroll model is preserved (the legacy `.grid` also scrolls the
// document; we don't introduce an inner overflow container that
// would change scroll bar / mouse-wheel ergonomics).
//
// Computes columns from the parent width to match the legacy
// `repeat(auto-fill, minmax(280px, 1fr))` grid behaviour. `onEnd` is
// called once when the viewport reaches the last ~3 rows; the caller
// (useDocs.loadMore) must be idempotent while a fetch is in flight.
//
// W2.6a — the roving cursor (`hooks/useRovingCursor.ts`, driven from
// `routes/gallery.tsx`) needs (a) the live `cols` count for its grid h/l
// column math, exposed via `onColsChange` rather than lifting `cols` state
// up wholesale (`cols` is otherwise this component's own concern), and (b)
// an imperative `scrollToIndex` so the virtualizer follows the cursor —
// FileTree.tsx's `scrollToIndex(focusedIndex, {align:"auto"})` precedent.
// `Card.tsx` isn't owned by this phase, so the focused card gets its
// outline via a thin wrapper div (`.kb-card--focused`), never a prop
// threaded into `Card` itself — CSS-class focus, never DOM focus theft.

export interface VirtualGridHandle {
  scrollToIndex: (index: number) => void;
}

type Props = {
  docs: DocSummary[];
  kb: string;
  progress: Map<string, Progress>;
  /// Sessions-gallery join — artifact_id → SessionRow (session-shaped cards).
  sessions?: Map<string, SessionRow>;
  onEnd?: () => void;
  hasMore?: boolean;
  /// W2.6a — the flat doc index the roving cursor currently focuses (-1 or
  /// undefined = none). The corresponding card's wrapper gets
  /// `.kb-card--focused`.
  focusedIndex?: number;
  /// Fires whenever the computed column count changes, so the caller's
  /// `useRovingCursor({cols})` stays in sync with this component's own
  /// ResizeObserver-derived layout.
  onColsChange?: (cols: number) => void;
};

// v0.10 G2 — design wants 5 cols at 1440×900. With the 240px sidebar +
// gallery padding the usable content area is ~1148px; a 210px min-width
// yields 5 cols at that size (floor((1148+14)/(210+14)) = 5). Step
// down to 4/3/2/1 below 1280 / 1024 / 768 / 480 respectively.
const CARD_MIN_WIDTH = 210;
const ROW_GAP = 14;
// Rows are positioned at a FIXED `index * ESTIMATED_ROW_HEIGHT` stride and
// never measured, so this must equal the real per-row height or rows
// overlap/gap. `.kb-card` is a fixed 246px (chrome.css) + this 14px
// ROW_GAP = 260 — keep the two coupled. (A card taller than the stride is
// what caused the gallery overflow; the fixed card height + content clamps
// guarantee it can't happen.)
const ESTIMATED_ROW_HEIGHT = 260;

const VirtualGrid = forwardRef<VirtualGridHandle, Props>(function VirtualGrid(
  { docs, kb, progress, sessions, onEnd, hasMore, focusedIndex, onColsChange },
  handleRef,
) {
  const parentRef = useRef<HTMLDivElement>(null);
  const [cols, setCols] = useState(1);
  const [scrollMargin, setScrollMargin] = useState(0);

  useEffect(() => {
    const el = parentRef.current;
    if (!el) return;
    const update = () => {
      const w = el.clientWidth;
      setCols(Math.max(1, Math.floor((w + ROW_GAP) / (CARD_MIN_WIDTH + ROW_GAP))));
      setScrollMargin(el.getBoundingClientRect().top + window.scrollY);
    };
    update();
    const ro = new ResizeObserver(update);
    ro.observe(el);
    window.addEventListener("resize", update);
    return () => {
      ro.disconnect();
      window.removeEventListener("resize", update);
    };
  }, []);

  useEffect(() => {
    onColsChange?.(cols);
  }, [cols, onColsChange]);

  const rowCount = useMemo(() => Math.ceil(docs.length / cols), [docs.length, cols]);

  const rowVirtualizer = useWindowVirtualizer({
    count: rowCount,
    estimateSize: () => ESTIMATED_ROW_HEIGHT,
    overscan: 4,
    scrollMargin,
  });

  useImperativeHandle(
    handleRef,
    () => ({
      scrollToIndex: (index: number) => {
        rowVirtualizer.scrollToIndex(Math.floor(index / cols), { align: "auto" });
      },
    }),
    [rowVirtualizer, cols],
  );

  const items = rowVirtualizer.getVirtualItems();
  const lastIndex = items.length > 0 ? items[items.length - 1].index : -1;
  useEffect(() => {
    if (hasMore && onEnd && lastIndex >= rowCount - 3) {
      onEnd();
    }
  }, [hasMore, onEnd, lastIndex, rowCount]);

  return (
    <div
      ref={parentRef}
      className="virtual-grid"
      aria-label={`${docs.length} artifacts in ${kb}`}
      style={{
        position: "relative",
        height: rowVirtualizer.getTotalSize(),
        width: "100%",
      }}
    >
      {items.map((row) => {
        const start = row.index * cols;
        const slice = docs.slice(start, start + cols);
        return (
          <div
            key={row.index}
            data-row={row.index}
            style={{
              position: "absolute",
              top: 0,
              left: 0,
              width: "100%",
              transform: `translateY(${row.start - scrollMargin}px)`,
              display: "grid",
              gridTemplateColumns: `repeat(${cols}, minmax(0, 1fr))`,
              gap: `${ROW_GAP}px`,
              paddingBottom: `${ROW_GAP}px`,
            }}
          >
            {slice.map((d, i) => {
              const flatIndex = start + i;
              const isFocused = focusedIndex === flatIndex;
              return (
                // Card.tsx isn't owned by this phase — the focus outline
                // rides a thin wrapper div instead of a prop threaded into
                // Card itself (CSS-class focus, never DOM focus theft).
                <div
                  key={d.id}
                  className={isFocused ? "kb-card--focused" : undefined}
                  role="option"
                  aria-selected={isFocused}
                  data-kb-cursor={isFocused ? "true" : undefined}
                >
                  <Card
                    doc={d}
                    kb={kb}
                    progress={progress.get(d.id)}
                    session={sessions?.get(d.id)}
                  />
                </div>
              );
            })}
          </div>
        );
      })}
    </div>
  );
});

export default VirtualGrid;
