import {
  forwardRef,
  useCallback,
  useEffect,
  useImperativeHandle,
  useMemo,
  useRef,
  useState,
} from "react";
import { useQueries } from "@tanstack/react-query";
import { useVirtualizer } from "@tanstack/react-virtual";
import { Icon } from "./icons";
import { fetchTree } from "../api/client";
import { rungForMouse, type RampRung } from "../nav/ramp";
import type { EntryKind, HotspotRow, TreeEntry } from "../api/types";
import { useHotspotsMap } from "../hooks/useBehavioral";
import {
  ancestorDirs,
  buildTreeRows,
  filterTreeRows,
  toggleExpanded,
  type TreeRow,
} from "../lib/tree";
import { attentionTintStyle } from "../lib/attentionRamp";
import { loadAttentionOverlay, saveAttentionOverlay } from "../lib/prefs";
import { highlightSegments, matchSpeedSearch } from "../lib/speedSearch";

export interface FileTreeHandle {
  /// `/` — jump focus into the quick-filter input.
  focusFilter: () => void;
  /// j/k — move the row cursor by `delta` rows.
  moveFocus: (delta: number) => void;
  /// Enter — open the focused row (expand a dir, select a file) into
  /// whichever pane the caller currently considers focused. Shift+Enter
  /// (Wave E) — open a FILE row specifically into pane2 regardless of which
  /// pane is focused; a no-op distinction for a directory row (still just
  /// expands/collapses either way).
  activateFocused: (target?: "focused" | "pane2") => void;
  /// V70-H1 — `Ctrl-w h`'s "return to the tree" gesture. A bare
  /// `document.activeElement?.blur()` with nothing else explicitly focused
  /// leaves `document.activeElement` reporting `<body>` as the spec's
  /// FALLBACK query result, but fires no `focus`/`focusin` EVENT anywhere
  /// (per the DOM spec — `<body>` isn't natively focusable) — so
  /// `Reader.tsx`'s `focusin` listener (the only thing that flips
  /// `bufferFocused`, which is what actually gates `CommandRoot`'s scope
  /// to `"tree"`) never fires, and every tree-scope key silently no-ops
  /// forever, for a real user in a real browser, not just under test.
  /// `focusContainer` fires a REAL, event-generating focus transfer so
  /// that listener runs — see the implementation's own doc for why a
  /// natively-focusable element is used rather than a `tabIndex=-1` div
  /// (empirically unreliable here: a plain div's `.focus()` did not
  /// always dispatch a `focus`/`focusin` event, even fully visible and
  /// correctly `tabIndex`ed, while a real `<input>`'s always did).
  focusContainer: () => void;
  /// V70-A6 — the focused row's path + kind, for the Ramp's keyboard rungs
  /// (`o`/`O`/`Ctrl-Enter`), which open a row ELSEWHERE rather than
  /// activating it here. `null` when the focused row is a directory or the
  /// tree is empty: only a file has somewhere else to be opened.
  focusedFile: () => { path: string; kind: EntryKind } | null;
}

export interface FileTreeProps {
  repo: string;
  /// Named `gitRef`, not `ref` — `ref` is React's own reserved forwarding
  /// prop (this component already takes one, for `FileTreeHandle`).
  gitRef?: string;
  selectedPath: string;
  /// Wave E — `target` is `"pane2"` for a Shift+Enter/Shift+click open,
  /// otherwise `undefined` (open into whichever pane `Reader.tsx` currently
  /// considers focused — the pre-Wave-E default). Directory rows ignore it
  /// (expand/collapse has no pane).
  onSelect: (path: string, kind: EntryKind, target?: "focused" | "pane2") => void;
  /// V70-A6 — the Ramp (§P7). A tree row was a `<div onClick>` with no href,
  /// so Cmd-click and middle-click silently did NOTHING (the recon's table of
  /// six such surfaces). They now route through the one shared handler.
  /// Absent ⇒ pre-A6 behaviour exactly.
  onRamp?: (rung: RampRung, path: string, kind: EntryKind) => void;
}

const ROW_HEIGHT = 24;

function PathWithHighlight({ path, name, query }: { path: string; name: string; query: string }) {
  if (!query.trim()) return <>{name}</>;
  // Match against the full path (same text `filterTreeRows` uses), then
  // map ranges that fall inside the leaf `name` suffix so the visible
  // label lights up without re-deriving the match on a shorter string.
  const m = matchSpeedSearch(path, query);
  if (!m || m.ranges.length === 0) return <>{name}</>;
  const nameStart = path.length - name.length;
  const nameRanges = m.ranges
    .map((r) => ({
      start: Math.max(0, r.start - nameStart),
      end: Math.min(name.length, r.end - nameStart),
    }))
    .filter((r) => r.end > r.start);
  if (nameRanges.length === 0) return <>{name}</>;
  return (
    <>
      {highlightSegments(name, nameRanges).map((seg, i) =>
        seg.hit ? (
          <mark key={i} className="kbc-speedsearch__mark">
            {seg.text}
          </mark>
        ) : (
          <span key={i}>{seg.text}</span>
        ),
      )}
    </>
  );
}

/// Left rail: repo-relative directory listings loaded LAZILY per
/// directory (`GET /api/tree`, one query per expanded dir — via
/// `useQueries` so SSE-driven cache invalidation, `api/queryClient.ts`'s
/// bridge, transparently refetches any currently-expanded directory when
/// `mirror.updated` names this repo), flattened + virtualized
/// (`@tanstack/react-virtual`, same dependency `web/`'s own gallery uses).
///
/// V3.N1 — the permanent filter input is the speed-search box: substring-
/// first / subsequence-fallback ranking (`lib/speedSearch.ts`) with match
/// ranges highlighted on the leaf name. Esc clears; Enter opens the
/// focused (first) match via the existing handle.
function hotspotTitle(row: HotspotRow): string {
  const t = row.hotspot.terms;
  const parts = [
    `score ${row.hotspot.score.toFixed(2)}`,
    `churn_rank ${t.churn_rank}`,
    `complexity_rank ${t.complexity_rank}`,
  ];
  if (t.pain != null) parts.push(`pain ${t.pain.toFixed(2)}`);
  return parts.join(" · ");
}

const FileTree = forwardRef<FileTreeHandle, FileTreeProps>(function FileTree(
  { repo, gitRef, selectedPath, onSelect, onRamp },
  handleRef,
) {
  const [expanded, setExpanded] = useState<Set<string>>(() => new Set(ancestorDirs(selectedPath)));
  const [filter, setFilter] = useState("");
  const [focusedIndex, setFocusedIndex] = useState(0);
  // V3.2-B3 — attention overlay default OFF, pref-persisted.
  const [attentionOn, setAttentionOn] = useState(() => loadAttentionOverlay());
  const filterInputRef = useRef<HTMLInputElement | null>(null);
  const scrollParentRef = useRef<HTMLDivElement | null>(null);

  const hotspotsMap = useHotspotsMap(repo, attentionOn);

  // Re-expand the new selection's ancestors when the selected path changes
  // out from under us (e.g. a breadcrumb click, or a search-result jump)
  // and isn't already visible.
  useEffect(() => {
    setExpanded((prev) => {
      const need = ancestorDirs(selectedPath);
      if (need.every((d) => prev.has(d))) return prev;
      const next = new Set(prev);
      for (const d of need) next.add(d);
      return next;
    });
  }, [selectedPath]);

  const dirsToLoad = useMemo(() => ["", ...Array.from(expanded)], [expanded]);
  const dirQueries = useQueries({
    queries: dirsToLoad.map((dir) => ({
      queryKey: ["tree", repo, dir, gitRef ?? null],
      queryFn: () => fetchTree(repo, dir, gitRef),
      enabled: repo !== "",
    })),
  });

  // `useQueries` doesn't give a stable memo boundary across a variable-length
  // query set, so this recomputes every render rather than reaching for
  // `useMemo` with a variable-length dependency array (a real React
  // footgun — the hooks rule requires a FIXED-length deps array). Cheap:
  // at most a few dozen expanded directories for any file tree a human is
  // actually browsing.
  const entriesByPath = new Map<string, TreeEntry[]>();
  dirsToLoad.forEach((dir, i) => {
    const data = dirQueries[i]?.data;
    if (data) entriesByPath.set(dir, data.entries);
  });

  const allRows = useMemo(() => buildTreeRows(entriesByPath, expanded), [entriesByPath, expanded]);
  const rows = useMemo(() => filterTreeRows(allRows, filter), [allRows, filter]);

  useEffect(() => {
    setFocusedIndex((i) => Math.max(0, Math.min(i, rows.length - 1)));
  }, [rows.length]);

  const virtualizer = useVirtualizer({
    count: rows.length,
    getScrollElement: () => scrollParentRef.current,
    estimateSize: () => ROW_HEIGHT,
    overscan: 12,
  });

  const activate = useCallback(
    (row: TreeRow, target?: "focused" | "pane2") => {
      if (row.kind === "dir") {
        setExpanded((prev) => toggleExpanded(prev, row.path));
      } else {
        onSelect(row.path, row.kind, target);
      }
    },
    [onSelect],
  );

  useImperativeHandle(
    handleRef,
    () => ({
      focusFilter: () => filterInputRef.current?.focus(),
      moveFocus: (delta: number) => {
        setFocusedIndex((i) => Math.max(0, Math.min(i + delta, rows.length - 1)));
      },
      activateFocused: (target) => {
        const row = rows[focusedIndex];
        if (row) activate(row, target);
      },
      // V70-H1 — a real `<input>`'s `.focus()` reliably dispatches a
      // `focus`/`focusin` event (unlike a `tabIndex=-1` div's, empirically
      // — see the interface doc); blur it right back so the operator
      // isn't left with a cursor in the filter box (the fleeting focus
      // is enough to flip `Reader.tsx`'s `bufferFocused`, which is what
      // actually matters — where `document.activeElement` ends up
      // afterward is irrelevant to that listener having already run).
      //
      // Polls for REAL visibility (`offsetParent`) rather than trusting a
      // fixed number of frames: this tree is always MOUNTED regardless of
      // the dock's collapsed state (`Desk.tsx` renders `{dock}`
      // unconditionally inside the `<aside>`, only the PANEL's size
      // changes), so a ref-truthiness check alone proves nothing — and
      // the panel's actual resize is a SEPARATE, LATER `useEffect` in
      // `Desk.tsx` (calling the resizable-panels library's own imperative
      // `.resize()`), one more async layer past React's own commit. A
      // caller invoking this before that has landed would silently do
      // nothing (a hidden input structurally cannot take focus) — retry
      // on a real interval instead of guessing how many layers deep the
      // asynchrony goes.
      // V70-H1 — best-effort REAL DOM focus for accessibility (a
      // screen-reader/keyboard user needs a genuine focus target, not
      // just an internal scope flag) — see `FileTreeHandle.focusContainer`'s
      // doc. Nothing in the app's OWN keyboard routing depends on this
      // succeeding: `Reader.tsx`'s `keyboardRegion` is set synchronously
      // by the caller regardless. A real `<input>`'s `.focus()` dispatches
      // a `focus`/`focusin` event reliably (a `tabIndex=-1` div's did not,
      // empirically); blur it right back so the operator isn't left with
      // a cursor in the filter box. Polls briefly for the filter input to
      // be laid out (`offsetParent`) since the dock's expand can still be
      // settling when this fires.
      focusContainer: () => {
        let attempts = 0;
        const tryFocus = () => {
          attempts += 1;
          const el = filterInputRef.current;
          if (el && el.offsetParent !== null) {
            el.focus();
            el.blur();
            return;
          }
          if (attempts < 25) window.setTimeout(tryFocus, 20);
        };
        tryFocus();
      },
      focusedFile: () => {
        const row = rows[focusedIndex];
        return row && row.kind === "file" ? { path: row.path, kind: row.kind } : null;
      },
    }),
    [rows, focusedIndex, activate],
  );

  useEffect(() => {
    virtualizer.scrollToIndex(focusedIndex, { align: "auto" });
  }, [focusedIndex, virtualizer]);

  return (
    <div className="kbc-tree" data-kbc-tree data-kbc-attention-overlay={attentionOn ? "on" : "off"}>
      <div className="kbc-tree__head">
        <label className="kbc-tree__overlay-toggle" title="Tint files by hotspot attention score (history-derived, not quality)">
          <input
            type="checkbox"
            checked={attentionOn}
            onChange={(e) => {
              const on = e.target.checked;
              setAttentionOn(on);
              saveAttentionOverlay(on);
            }}
            data-kbc-attention-toggle
          />
          <span>attention overlay</span>
        </label>
      </div>
      <input
        ref={filterInputRef}
        className="kbc-tree__filter"
        type="text"
        placeholder="Filter files…"
        aria-label="Filter files"
        value={filter}
        onChange={(e) => setFilter(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Escape") {
            e.preventDefault();
            setFilter("");
            (e.target as HTMLInputElement).blur();
            return;
          }
          if (e.key === "Enter") {
            e.preventDefault();
            // Open the first / currently-focused match.
            const row = rows[focusedIndex] ?? rows[0];
            if (row) activate(row, e.shiftKey ? "pane2" : undefined);
            return;
          }
          if (e.key === "ArrowDown") {
            e.preventDefault();
            setFocusedIndex((i) => Math.max(0, Math.min(i + 1, rows.length - 1)));
            return;
          }
          if (e.key === "ArrowUp") {
            e.preventDefault();
            setFocusedIndex((i) => Math.max(0, i - 1));
          }
        }}
      />
      <div className="kbc-tree__scroll" ref={scrollParentRef}>
        <div style={{ height: virtualizer.getTotalSize(), position: "relative" }}>
          {virtualizer.getVirtualItems().map((vi) => {
            const row = rows[vi.index];
            const isSelected = row.path === selectedPath;
            const isFocused = vi.index === focusedIndex;
            const hot =
              attentionOn && row.kind === "file"
                ? hotspotsMap.data?.get(row.path)
                : undefined;
            // Absence of a behavioral row ⇒ untinted (≠ zero score).
            const tint = attentionOn && hot ? attentionTintStyle(hot.hotspot.score) : undefined;
            return (
              <div
                key={row.path}
                className={
                  "kbc-tree__row" +
                  (isSelected ? " kbc-tree__row--selected" : "") +
                  (isFocused ? " kbc-tree__row--focused" : "") +
                  (row.kind === "dir" ? " kbc-tree__row--dir" : "") +
                  (hot ? " kbc-tree__row--attention" : "")
                }
                style={{
                  position: "absolute",
                  top: 0,
                  left: 0,
                  width: "100%",
                  height: ROW_HEIGHT,
                  transform: `translateY(${vi.start}px)`,
                  paddingLeft: `${8 + row.depth * 14}px`,
                  ...(tint ?? {}),
                }}
                title={hot ? hotspotTitle(hot) : undefined}
                onMouseDown={(e) => {
                  const rung = rungForMouse(e);
                  if (!rung || rung === "here" || rung === "other" || row.kind !== "file" || !onRamp) return;
                  e.preventDefault();
                  setFocusedIndex(vi.index);
                  onRamp(rung, row.path, row.kind);
                }}
                onClick={(e) => {
                  setFocusedIndex(vi.index);
                  activate(row, e.shiftKey ? "pane2" : undefined);
                }}
                data-kbc-kind={row.kind}
                data-kbc-attention-score={hot ? String(hot.hotspot.score) : undefined}
              >
                <span className="kbc-tree__twisty" aria-hidden>
                  {row.kind === "dir" ? (
                    <Icon.Chevron
                      className={expanded.has(row.path) ? "kbc-twisty is-open" : "kbc-twisty"}
                    />
                  ) : null}
                </span>
                <PathWithHighlight path={row.path} name={row.name} query={filter} />
              </div>
            );
          })}
        </div>
      </div>
    </div>
  );
});

export default FileTree;
