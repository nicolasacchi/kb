import { useEffect, useMemo, useRef, useState } from "react";
import type { Symbol } from "../api/types";
import { buildOutline, flattenOutline } from "../lib/outline";
import { highlightSegments, speedFilterItems } from "../lib/speedSearch";
import { buildSym, langIdForPath, symbolPermalinkFor } from "../lib/codeUrl";
import { copyToClipboard } from "../editor/vimReader";
import { toast } from "../lib/toast";
import { Icon } from "./icons";

export interface StructurePopupProps {
  open: boolean;
  onClose: () => void;
  symbols: Symbol[];
  /// Jump to a symbol's start line (same path OutlineRail uses).
  onJump: (line: number) => void;
  /// T1 (design-ui.md §9.2) — the copy-symbol-link affordance's `sym=`
  /// coordinates: the CURRENT file's own repo/path. Both optional so a
  /// caller that doesn't (yet) know them degrades to no copy button at all,
  /// rather than a broken link.
  repo?: string;
  path?: string;
  /// `FileOut.lang` when known — falls back to `langIdForPath(path)` when
  /// omitted/`null` (an older file response, or a path whose extension
  /// `lib/codeUrl.ts`'s own small mirror doesn't recognize).
  lang?: string | null;
}

/// V3.N2 — File structure popup (`gO`). Modal over the CURRENT file's
/// symbols (same data OutlineRail uses). Speed-search filter + j/k/Enter.
export default function StructurePopup({
  open,
  onClose,
  symbols,
  onJump,
  repo,
  path,
  lang,
}: StructurePopupProps) {
  const [q, setQ] = useState("");
  const [cursor, setCursor] = useState(0);
  const inputRef = useRef<HTMLInputElement | null>(null);

  const flat = useMemo(() => flattenOutline(buildOutline(symbols)), [symbols]);

  useEffect(() => {
    if (!open) return;
    setQ("");
    setCursor(0);
    requestAnimationFrame(() => inputRef.current?.focus());
  }, [open]);

  const hits = useMemo(
    () => speedFilterItems(flat, q, (r) => `${r.symbol.kind} ${r.symbol.name}`),
    [flat, q],
  );

  useEffect(() => {
    setCursor((c) => {
      if (hits.length === 0) return 0;
      return Math.min(c, hits.length - 1);
    });
  }, [hits.length]);

  function activate(index: number) {
    const hit = hits[index];
    if (!hit) return;
    onJump(hit.item.symbol.line_start);
    onClose();
  }

  function onKeyDown(e: React.KeyboardEvent) {
    if (e.key === "Escape") {
      e.preventDefault();
      e.stopPropagation();
      onClose();
      return;
    }
    if (e.key === "ArrowDown" || (e.key === "j" && q === "")) {
      e.preventDefault();
      if (hits.length === 0) return;
      setCursor((c) => Math.min(hits.length - 1, c + 1));
      return;
    }
    if (e.key === "ArrowUp" || (e.key === "k" && q === "")) {
      e.preventDefault();
      if (hits.length === 0) return;
      setCursor((c) => Math.max(0, c - 1));
      return;
    }
    if (e.key === "Enter") {
      e.preventDefault();
      activate(cursor);
    }
  }

  if (!open) return null;

  return (
    <div
      className="kbc-omnibox-backdrop"
      role="presentation"
      data-kbc-structure-popup
      onClick={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
    >
      <div
        className="kbc-omnibox kbc-structure-popup"
        role="dialog"
        aria-modal="true"
        aria-label="file structure"
        onKeyDown={onKeyDown}
      >
        <div className="kbc-omnibox__head">
          <input
            ref={inputRef}
            value={q}
            onChange={(e) => setQ(e.target.value)}
            placeholder="Filter symbols…"
            className="kbc-omnibox__input"
            aria-label="filter symbols"
            data-kbc-structure-filter
          />
        </div>
        <div className="kbc-omnibox__body" role="listbox" aria-label="file symbols">
          {hits.length === 0 && (
            <div className="kbc-omnibox__hint">
              {symbols.length === 0 ? "No symbols in this file." : "No matches."}
            </div>
          )}
          {hits.map(({ item, ranges }, i) => {
            const { symbol, depth } = item;
            const active = i === cursor;
            const canCopy = !!repo && !!path;
            return (
              // A `<div role="option">` (not a `<button>`) — a copy-symbol-
              // link affordance sits alongside the row's own click target,
              // and HTML forbids nesting `<button>`s; keyboard nav here has
              // always routed entirely through the filter `<input>`'s own
              // `onKeyDown` (Up/Down/Enter), so no row has ever been
              // individually Tab-focused — this loses nothing.
              <div
                key={`${symbol.ordinal}`}
                role="option"
                aria-selected={active}
                className={`kbc-recent-locs__row kbc-structure-popup__row-wrap${active ? " is-active" : ""}`}
                style={{ paddingLeft: 14 + depth * 12 }}
                onMouseEnter={() => setCursor(i)}
                data-kbc-structure-row
                data-kbc-structure-line={symbol.line_start}
              >
                <button
                  type="button"
                  className="kbc-structure-popup__row-btn"
                  onClick={() => activate(i)}
                >
                  <span className="kbc-structure-popup__row">
                    <span className="kbc-outline__kind">{symbol.kind}</span>
                    <span className="kbc-structure-popup__name">
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
                    <span className="kbc-structure-popup__line">:{symbol.line_start}</span>
                  </span>
                </button>
                {canCopy && (
                  <button
                    type="button"
                    className="kbc-structure-popup__copy-sym"
                    title="copy symbol link"
                    aria-label="copy symbol link"
                    data-kbc-structure-copy-sym
                    onClick={(e) => {
                      e.preventDefault();
                      e.stopPropagation();
                      const sym = buildSym({
                        namespace: lang ?? langIdForPath(path!) ?? "code",
                        name: symbol.name,
                        container: symbol.container,
                        kind: symbol.kind,
                      });
                      copyToClipboard(
                        symbolPermalinkFor(window.location.origin, repo!, sym, {
                          fallbackPath: path!,
                          fallbackLine: symbol.line_start,
                        }),
                      );
                      toast.ok("symbol link copied");
                    }}
                  >
                    <Icon.Copy width={12} height={12} aria-hidden />
                  </button>
                )}
              </div>
            );
          })}
        </div>
        <div className="kbc-omnibox__foot">
          <span className="kbc-omnibox__keys">
            <kbd>↑↓</kbd>/<kbd>j k</kbd> move <kbd>Enter</kbd> jump <kbd>Esc</kbd> close
          </span>
        </div>
      </div>
    </div>
  );
}
