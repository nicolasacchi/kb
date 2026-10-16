import { docKey } from "../lib/dedupeDocs";
import {
  forwardRef,
  type CSSProperties,
  useEffect,
  useImperativeHandle,
  useRef,
  useState,
} from "react";
import { useWindowVirtualizer } from "@tanstack/react-virtual";
import ListRow from "./ListRow";
import type { DocSummary } from "../api/client";
import { useUrl } from "../hooks/useUrl";
import { useCodeRefCounts } from "../hooks/useCodeRefs";
import { galleryRefCount } from "../lib/codeRefCounts";
import { parseSortParams } from "../lib/sort";
import {
  LIST_COLUMNS,
  ariaSort,
  columnsStorageKey,
  gridTemplate,
  nextSort,
  parseColumns,
  serializeColumns,
  toggleColumn,
  type ListColumnId,
} from "../lib/listColumns";

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
  const { params, setMany } = useUrl();
  const { sort, dir } = parseSortParams(params);
  const [columns, setColumns] = useState<ListColumnId[]>(() =>
    parseColumns(readStored(kb)),
  );
  // The choice is per kb: reload it when the corpus changes.
  const [columnsKb, setColumnsKb] = useState(kb);
  if (columnsKb !== kb) {
    setColumnsKb(kb);
    setColumns(parseColumns(readStored(kb)));
  }
  const [chooserOpen, setChooserOpen] = useState(false);
  const chooserRef = useRef<HTMLDivElement>(null);
  const applyColumns = (next: ListColumnId[]) => {
    setColumns(next);
    try {
      localStorage.setItem(columnsStorageKey(kb), serializeColumns(next));
    } catch {
      // storage denied/full: the choice holds for this session only.
    }
  };
  useEffect(() => {
    if (!chooserOpen) return;
    const onDown = (e: MouseEvent) => {
      if (!chooserRef.current?.contains(e.target as Node)) setChooserOpen(false);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setChooserOpen(false);
    };
    document.addEventListener("mousedown", onDown);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDown);
      document.removeEventListener("keydown", onKey);
    };
  }, [chooserOpen]);
  // The shared coderef feed walk only runs while the Refs column is visible.
  const showRefs = columns.includes("refs");
  const refCounts = useCodeRefCounts(showRefs ? kb : null);
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
      className="virtual-list list"
      aria-label={`${docs.length} artifacts in ${kb}`}
      style={{ "--list-cols": gridTemplate(columns) } as CSSProperties}
    >
      <div className="list-row list-row--head" role="row" data-testid="index-head">
        {LIST_COLUMNS.filter((c) => columns.includes(c.id)).map((c) => {
          const next = nextSort(c, sort, dir);
          const label = next ? (
            <button
              type="button"
              className="list-head__sort"
              data-testid={`index-sort-${c.id}`}
              onClick={() => setMany({ sort: next.sort, dir: next.dir })}
            >
              {c.label}
              {c.sort === sort && (
                <span className="list-head__dir" aria-hidden="true">
                  {dir === "asc" ? "\u2191" : "\u2193"}
                </span>
              )}
            </button>
          ) : (
            <span className="list-head__label">{c.label}</span>
          );
          return (
            <div
              key={c.id}
              className={`list-head__cell list-head__cell--${c.align}`}
              data-col={c.id}
              role="columnheader"
              aria-sort={ariaSort(c, sort, dir)}
            >
              {label}
              {c.id === "title" && (
                <div className="list-head__chooser" ref={chooserRef}>
                  <button
                    type="button"
                    className="list-head__columns"
                    aria-haspopup="true"
                    aria-expanded={chooserOpen}
                    data-testid="index-columns"
                    onClick={() => setChooserOpen((o) => !o)}
                  >
                    Columns
                  </button>
                  {chooserOpen && (
                    <ul className="kb-popover list-head__menu" aria-label="Columns">
                      {LIST_COLUMNS.map((col) => (
                        <li key={col.id}>
                          <label className="kb-popover__item">
                            <span>{col.label}</span>
                            <input
                              type="checkbox"
                              checked={columns.includes(col.id)}
                              disabled={col.locked}
                              data-testid={`index-col-${col.id}`}
                              onChange={() => applyColumns(toggleColumn(columns, col.id))}
                            />
                          </label>
                        </li>
                      ))}
                    </ul>
                  )}
                </div>
              )}
            </div>
          );
        })}
        <span aria-hidden="true" />
      </div>
      <div
        ref={parentRef}
        className="virtual-list__body"
        style={{
          position: "relative",
          height: rowVirtualizer.getTotalSize(),
          width: "100%",
        }}
      >
        {items.map((row) => {
          const d = docs[row.index];
          const isFocused = focusedIndex === row.index;
          return (
            <div
              key={docKey(kb, d.id)}
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
              <ListRow
                doc={d}
                kb={kb}
                columns={columns}
                refs={showRefs ? galleryRefCount(refCounts.data, d.id) : 0}
              />
            </div>
          );
        })}
      </div>
    </div>
  );
});

function readStored(kb: string): string | null {
  try {
    return localStorage.getItem(columnsStorageKey(kb));
  } catch {
    return null;
  }
}

export default VirtualList;
