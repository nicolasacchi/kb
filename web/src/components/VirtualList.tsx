import {
  forwardRef,
  useEffect,
  useImperativeHandle,
  useRef,
  useState,
} from "react";
import { useWindowVirtualizer } from "@tanstack/react-virtual";
import ListRow from "./ListRow";
import type { DocSummary } from "../api/client";

// S-milestone S5 — virtualised list rendering. Mirrors VirtualGrid:
// window-scroll virtualisation so the SPA scroll model is preserved.
//
// W2.6a — same roving-cursor plumbing as VirtualGrid (`focusedIndex` +
// imperative `scrollToIndex`), minus the column math (a list is always 1
// col). The focused row's class rides the EXISTING per-row positioning
// `<div>` — no extra wrapper needed here.
export interface VirtualListHandle {
  scrollToIndex: (index: number) => void;
}

type Props = {
  docs: DocSummary[];
  kb: string;
  onEnd?: () => void;
  hasMore?: boolean;
  /// W2.6a — the flat doc index the roving cursor currently focuses.
  focusedIndex?: number;
};

const ESTIMATED_ROW_HEIGHT = 32;

const VirtualList = forwardRef<VirtualListHandle, Props>(function VirtualList(
  { docs, kb, onEnd, hasMore, focusedIndex },
  handleRef,
) {
  const parentRef = useRef<HTMLDivElement>(null);
  const [scrollMargin, setScrollMargin] = useState(0);

  useEffect(() => {
    const el = parentRef.current;
    if (!el) return;
    const update = () => {
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

  const rowVirtualizer = useWindowVirtualizer({
    count: docs.length,
    estimateSize: () => ESTIMATED_ROW_HEIGHT,
    overscan: 12,
    scrollMargin,
  });

  useImperativeHandle(
    handleRef,
    () => ({
      scrollToIndex: (index: number) => {
        rowVirtualizer.scrollToIndex(index, { align: "auto" });
      },
    }),
    [rowVirtualizer],
  );

  const items = rowVirtualizer.getVirtualItems();
  const lastIndex = items.length > 0 ? items[items.length - 1].index : -1;
  useEffect(() => {
    if (hasMore && onEnd && lastIndex >= docs.length - 12) {
      onEnd();
    }
  }, [hasMore, onEnd, lastIndex, docs.length]);

  return (
    <div
      ref={parentRef}
      className="virtual-list list"
      aria-label={`${docs.length} artifacts in ${kb}`}
      style={{
        position: "relative",
        height: rowVirtualizer.getTotalSize(),
        width: "100%",
      }}
    >
      <div className="list-row list-row--head" aria-hidden="true">
        <span>id</span>
        <span>title</span>
        <span>folder</span>
        <span>file</span>
        <span>category</span>
        <span></span>
        <span></span>
      </div>
      {items.map((row) => {
        const d = docs[row.index];
        const isFocused = focusedIndex === row.index;
        return (
          <div
            key={d.id}
            className={isFocused ? "list-row--focused" : undefined}
            role="option"
            aria-selected={isFocused}
            data-kb-cursor={isFocused ? "true" : undefined}
            style={{
              position: "absolute",
              top: 0,
              left: 0,
              width: "100%",
              transform: `translateY(${row.start - scrollMargin}px)`,
            }}
          >
            <ListRow doc={d} kb={kb} />
          </div>
        );
      })}
    </div>
  );
});

export default VirtualList;
