import { useEffect, useMemo, useRef, useState } from "react";
import type { Symbol } from "../api/types";
import { buildOutline, flattenOutline } from "../lib/outline";
import { highlightSegments, speedFilterItems } from "../lib/speedSearch";

export interface OutlineRailProps {
  symbols: Symbol[];
  onJump: (line: number) => void;
}

/// Right rail — the current file's symbol outline (functions/methods/
/// classes for the seven "tags" languages, or the hierarchical dotted
/// key-path outline for YAML/TOML/JSON — both ride the same `Symbol` shape,
/// see `api/types.ts`'s doc). Click → scroll `CodeView` to that symbol's
/// start line.
///
/// V3.N1 — speed search: when the rail has focus, typing opens an inline
/// filter box (substring-first / subsequence fallback via
/// `lib/speedSearch.ts`); Esc clears; Enter jumps the first/selected match.
export default function OutlineRail({ symbols, onJump }: OutlineRailProps) {
  const flat = useMemo(() => flattenOutline(buildOutline(symbols)), [symbols]);
  const [filter, setFilter] = useState("");
  const [filterOpen, setFilterOpen] = useState(false);
  const [focusedIndex, setFocusedIndex] = useState(0);
  const inputRef = useRef<HTMLInputElement | null>(null);
  const rootRef = useRef<HTMLDivElement | null>(null);

  const hits = useMemo(
    () => speedFilterItems(flat, filter, (r) => r.symbol.name),
    [flat, filter],
  );

  useEffect(() => {
    setFocusedIndex((i) => Math.max(0, Math.min(i, Math.max(0, hits.length - 1))));
  }, [hits.length]);

  useEffect(() => {
    // Reset filter when the symbol set swaps (file change).
    setFilter("");
    setFilterOpen(false);
    setFocusedIndex(0);
  }, [symbols]);

  function openFilter(seed = "") {
    setFilterOpen(true);
    setFilter(seed);
    requestAnimationFrame(() => inputRef.current?.focus());
  }

  function clearFilter() {
    setFilter("");
    setFilterOpen(false);
    rootRef.current?.focus();
  }

  function onRootKeyDown(e: React.KeyboardEvent) {
    // When the filter input is focused its own handler owns the keys.
    if (e.target instanceof HTMLInputElement) return;
    if (e.metaKey || e.ctrlKey || e.altKey) return;

    if (e.key === "Escape" && filterOpen) {
      e.preventDefault();
      clearFilter();
      return;
    }
    if (e.key === "j" || e.key === "ArrowDown") {
      e.preventDefault();
      setFocusedIndex((i) => Math.min(hits.length - 1, i + 1));
      return;
    }
    if (e.key === "k" || e.key === "ArrowUp") {
      e.preventDefault();
      setFocusedIndex((i) => Math.max(0, i - 1));
      return;
    }
    if (e.key === "Enter") {
      e.preventDefault();
      const hit = hits[focusedIndex];
      if (hit) onJump(hit.item.symbol.line_start);
      return;
    }
    if (e.key === "/" || e.key === "f") {
      e.preventDefault();
      openFilter("");
      return;
    }
    // Type-ahead: a single printable char opens the speed-search box.
    if (e.key.length === 1 && !e.metaKey && !e.ctrlKey && !e.altKey) {
      e.preventDefault();
      openFilter(e.key);
    }
  }

  if (symbols.length === 0) {
    return <div className="kbc-outline kbc-outline--empty">No symbols in this file</div>;
  }

  return (
    <div
      ref={rootRef}
      className="kbc-outline-wrap"
      tabIndex={0}
      onKeyDown={onRootKeyDown}
      data-kbc-outline
    >
      {filterOpen && (
        <input
          ref={inputRef}
          className="kbc-outline__filter"
          type="text"
          value={filter}
          placeholder="Filter symbols…"
          aria-label="Filter symbols"
          onChange={(e) => setFilter(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Escape") {
              e.preventDefault();
              e.stopPropagation();
              clearFilter();
              return;
            }
            if (e.key === "Enter") {
              e.preventDefault();
              const hit = hits[focusedIndex] ?? hits[0];
              if (hit) onJump(hit.item.symbol.line_start);
              return;
            }
            if (e.key === "ArrowDown") {
              e.preventDefault();
              setFocusedIndex((i) => Math.min(hits.length - 1, i + 1));
              return;
            }
            if (e.key === "ArrowUp") {
              e.preventDefault();
              setFocusedIndex((i) => Math.max(0, i - 1));
            }
          }}
        />
      )}
      <ul className="kbc-outline">
        {hits.map(({ item, ranges }, i) => {
          const { symbol, depth } = item;
          const focused = i === focusedIndex;
          return (
            <li key={`${symbol.ordinal}`} style={{ paddingLeft: `${depth * 12}px` }}>
              <button
                type="button"
                className={`kbc-outline__item${focused ? " kbc-outline__item--focused" : ""}`}
                onClick={() => onJump(symbol.line_start)}
                onMouseEnter={() => setFocusedIndex(i)}
                title={symbol.container ? `${symbol.container} · ${symbol.kind}` : symbol.kind}
              >
                <span className="kbc-outline__kind">{symbol.kind}</span>
                <span className="kbc-outline__name">
                  {ranges.length === 0
                    ? symbol.name
                    : highlightSegments(symbol.name, ranges).map((seg, si) =>
                        seg.hit ? (
                          <mark key={si} className="kbc-speedsearch__mark">
                            {seg.text}
                          </mark>
                        ) : (
                          <span key={si}>{seg.text}</span>
                        ),
                      )}
                </span>
              </button>
            </li>
          );
        })}
        {hits.length === 0 && (
          <li className="kbc-outline--empty" style={{ listStyle: "none" }}>
            No matches
          </li>
        )}
      </ul>
    </div>
  );
}
