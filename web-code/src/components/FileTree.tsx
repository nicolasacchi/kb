import {
  forwardRef,
  useCallback,
  useEffect,
  useImperativeHandle,
  useMemo,
  useRef,
  useState,
} from "react";
import { useQueries, useQuery } from "@tanstack/react-query";
import { useVirtualizer } from "@tanstack/react-virtual";
import { Icon } from "./icons";
import { fetchTree, fetchTreeV2 } from "../api/client";
import { rungForMouse, type RampRung } from "../nav/ramp";
import type {
  EntryKind,
  HotspotRow,
  TreeEntry,
  TreeLane,
  TreeRow,
  TreeV2Response,
  TreeView,
} from "../api/types";
import { useHotspotsMap } from "../hooks/useBehavioral";
import { ancestorDirs, buildTreeRows, legacyRowsToTreeRows, toggleExpanded } from "../lib/tree";
import { fromWire, highlightSegments } from "../lib/matchRanges";
import {
  dirKey,
  expandParam,
  nextRowWith,
  routeFilterBox,
  selectionCli,
  stickyAncestors,
  treeCli,
  SELECTION_ACTIONS,
  type TreeMode,
} from "../lib/treeQuery";
import { attentionTintStyle } from "../lib/attentionRamp";
import { loadAttentionOverlay, saveAttentionOverlay } from "../lib/prefs";

export interface FileTreeHandle {
  /// `/` — jump focus into the quick-filter input.
  focusFilter: () => void;
  /// j/k — move the row cursor by `delta` rows.
  moveFocus: (delta: number) => void;
  /// Enter — open the focused row (expand a dir/group, select a file) into
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

  // ── V71-F1 additions ────────────────────────────────────────────────
  /// `t`/`T` — cycle the PROJECTION (physical → role → namespace →
  /// change). The vocabulary comes from the daemon's own
  /// `views_available`, never a list hardcoded here.
  cycleView: (delta: number) => void;
  /// `Ctrl-Alt-f` — flip the filter box between VS Code's two modes.
  toggleFilterMode: () => void;
  /// `d` — cycle the decoration-lane preset (reading / reviewing /
  /// archaeology). Three lanes is a budget, not a starting point.
  cycleLanes: () => void;
  /// `Space` — toggle the focused row into the selection.
  toggleSelect: () => void;
  /// `a` — open the selection's action menu (every action prints its CLI).
  openActions: () => void;
  /// `]c`/`[c` and `]a`/`[a` — walk to the next row carrying a change or
  /// an open annotation. Returns `false` when there is none, so the caller
  /// can say so instead of silently doing nothing.
  jump: (lane: "change" | "annot", dir: 1 | -1) => boolean;
  /// `g R` — reveal a path in the tree from anywhere in the app: expand
  /// its ancestors and focus its row.
  reveal: (path: string) => void;
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

/// Rows one request asks for. Well under the daemon's own
/// `MAX_TREE_ROWS`; the daemon reports `truncated` either way and this
/// component renders that verbatim rather than hiding it.
const ROW_LIMIT = 2000;

/// `d` cycles these. Three lanes is the daemon's hard budget; the DEFAULT
/// is one cheap lane (`annot` is a single grouped sqlite read), because
/// `git` costs a `git diff` subprocess per request and nobody should pay
/// for it until they ask.
const LANE_PRESETS: { id: string; lanes: TreeLane[]; title: string }[] = [
  { id: "reading", lanes: ["annot"], title: "reading — open questions only" },
  { id: "reviewing", lanes: ["git", "review", "findings"], title: "reviewing — change · viewed · findings" },
  { id: "archaeology", lanes: ["git", "annot", "todo"], title: "archaeology — change · questions · TODOs" },
  { id: "off", lanes: [], title: "off — no decorations" },
];

function LabelWithHighlight({ row }: { row: TreeRow }) {
  const ranges = fromWire(row.match_ranges);
  if (ranges.length === 0) return <>{row.label}</>;
  return (
    <>
      {highlightSegments(row.label, ranges).map((seg, i) =>
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

/// The three decoration slots, rendered from the daemon's `facts`. A lane
/// that was not requested is ABSENT from `facts` and renders nothing; a
/// lane that WAS requested and found nothing renders nothing either, which
/// is why the header names the active preset — the row cannot say "I asked
/// and the answer was zero" in one character.
function FactsCells({ facts }: { facts: TreeRow["facts"] }) {
  if (!facts) return null;
  const cells: { key: string; text: string; title: string; cls: string }[] = [];
  if (facts.git) {
    cells.push({ key: "git", text: facts.git, title: `git: ${facts.git} vs the base`, cls: "git" });
  } else if (facts.git_changed) {
    cells.push({
      key: "git",
      text: String(facts.git_changed),
      title: `${facts.git_changed} changed file(s) in this subtree`,
      cls: "git",
    });
  }
  if (facts.review) {
    cells.push({ key: "rv", text: facts.review === "viewed" ? "✓" : "○", title: `review: ${facts.review}`, cls: "review" });
  } else if (facts.review_total) {
    cells.push({
      key: "rv",
      text: `${facts.review_viewed ?? 0}/${facts.review_total}`,
      title: "review: viewed / in this review",
      cls: "review",
    });
  }
  if (facts.findings) {
    cells.push({
      key: "fd",
      text: facts.findings === "blocker" ? "▲" : "▪",
      title: `worst finding here: ${facts.findings}`,
      cls: `findings findings--${facts.findings}`,
    });
  }
  if (facts.annot) {
    cells.push({ key: "an", text: String(facts.annot), title: `${facts.annot} open annotation(s)`, cls: "annot" });
  }
  if (facts.todo) {
    cells.push({ key: "td", text: String(facts.todo), title: `${facts.todo} TODO/FIXME`, cls: "todo" });
  }
  if (facts.bookmark) {
    cells.push({ key: "bm", text: "⚑", title: `${facts.bookmark} bookmark(s)`, cls: "bookmark" });
  }
  if (cells.length === 0) return null;
  return (
    <span className="kbc-tree__lanes" aria-hidden>
      {cells.map((c) => (
        <span key={c.key} className={`kbc-tree__lane kbc-tree__lane--${c.cls}`} title={c.title}>
          {c.text}
        </span>
      ))}
    </span>
  );
}

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

/// Left rail: the PROJECTED tree (`GET /api/tree/2`, kbc-tree/1). The
/// projection, its decoration aggregates and its match ranges are all
/// computed ONCE server-side — this component renders rows it did not
/// compute and never re-derives a count from them (the evidence report's
/// "two renderers of one projection will diverge" risk).
///
/// One exception, stated rather than hidden: kbc-tree/1 projects the mirror
/// INDEX, so while the reader is browsing a non-default ref the dock keeps
/// the pre-existing per-directory `GET /api/tree` listing, adapted into the
/// same row shape by `lib/tree.ts`'s `legacyRowsToTreeRows`. Only the data
/// SOURCE branches; the render, the keyboard model and the handle are one.
/// Tree-as-of-a-ref proper is F18 and is deferred.
const FileTree = forwardRef<FileTreeHandle, FileTreeProps>(function FileTree(
  { repo, gitRef, selectedPath, onSelect, onRamp },
  handleRef,
) {
  const [expanded, setExpanded] = useState<Set<string>>(() => new Set(ancestorDirs(selectedPath)));
  const [openGroups, setOpenGroups] = useState<Set<string>>(() => new Set());
  const [box, setBox] = useState("");
  const [mode, setMode] = useState<TreeMode>("filter");
  const [view, setView] = useState<TreeView>("physical");
  const [lanePreset, setLanePreset] = useState(0);
  const [focusedIndex, setFocusedIndex] = useState(0);
  const [selected, setSelected] = useState<Set<string>>(() => new Set());
  const [actionsOpen, setActionsOpen] = useState(false);
  const [firstVisible, setFirstVisible] = useState(0);
  // V3.2-B3 — attention overlay default OFF, pref-persisted.
  const [attentionOn, setAttentionOn] = useState(() => loadAttentionOverlay());
  const filterInputRef = useRef<HTMLInputElement | null>(null);
  const scrollParentRef = useRef<HTMLDivElement | null>(null);

  const hotspotsMap = useHotspotsMap(repo, attentionOn);
  const lanes = LANE_PRESETS[lanePreset % LANE_PRESETS.length];

  // kbc-tree/1 is index-only — see the component doc.
  const atRef = !!gitRef && gitRef !== "HEAD";

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

  const routed = useMemo(() => routeFilterBox(box), [box]);
  const expandCsv = useMemo(
    () => expandParam([...Array.from(expanded).map(dirKey), ...Array.from(openGroups)]),
    [expanded, openGroups],
  );

  const projected = useQuery({
    queryKey: [
      "tree2",
      repo,
      view,
      expandCsv,
      routed.scope ?? "",
      routed.filter ?? "",
      mode,
      lanes.id,
    ],
    queryFn: () =>
      fetchTreeV2({
        repo,
        view,
        depth: 1,
        expand: expandCsv || undefined,
        scope: routed.scope,
        filter: routed.filter,
        mode,
        decorate: lanes.lanes.length > 0 ? lanes.lanes.join(",") : undefined,
        limit: ROW_LIMIT,
      }),
    enabled: repo !== "" && !atRef,
  });

  // --- the ref-browsing fallback (see the component doc) ----------------
  const dirsToLoad = useMemo(() => ["", ...Array.from(expanded)], [expanded]);
  const dirQueries = useQueries({
    queries: dirsToLoad.map((dir) => ({
      queryKey: ["tree", repo, dir, gitRef ?? null],
      queryFn: () => fetchTree(repo, dir, gitRef),
      enabled: repo !== "" && atRef,
    })),
  });
  const entriesByPath = new Map<string, TreeEntry[]>();
  dirsToLoad.forEach((dir, i) => {
    const data = dirQueries[i]?.data;
    if (data) entriesByPath.set(dir, data.entries);
  });

  const response: TreeV2Response | undefined = atRef ? undefined : projected.data;
  const rows: TreeRow[] = useMemo(() => {
    if (atRef) return legacyRowsToTreeRows(buildTreeRows(entriesByPath, expanded), expanded);
    return response?.rows ?? [];
    // `entriesByPath` is rebuilt every render (a variable-length
    // `useQueries` set has no stable memo boundary — the pre-V71-F1 code
    // said so and this keeps the same trade), so the legacy branch keys on
    // the two things that actually change.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [atRef, response, expanded, dirQueries.map((q) => q.dataUpdatedAt).join(",")]);

  useEffect(() => {
    setFocusedIndex((i) => Math.max(0, Math.min(i, rows.length - 1)));
  }, [rows.length]);

  const virtualizer = useVirtualizer({
    count: rows.length,
    getScrollElement: () => scrollParentRef.current,
    estimateSize: () => ROW_HEIGHT,
    overscan: 12,
  });

  const virtualItems = virtualizer.getVirtualItems();
  useEffect(() => {
    const top = virtualItems[0]?.index ?? 0;
    setFirstVisible(top);
  }, [virtualItems]);

  const sticky = useMemo(() => stickyAncestors(rows, firstVisible), [rows, firstVisible]);

  const activate = useCallback(
    (row: TreeRow, target?: "focused" | "pane2") => {
      if (row.kind === "file") {
        if (row.path) onSelect(row.path, "file", target);
        return;
      }
      if (row.kind === "dir" && row.path) {
        setExpanded((prev) => toggleExpanded(prev, row.path as string));
        return;
      }
      // A `group`/`entity` row is not a directory — its open-state key is
      // the daemon's own row id, never a path this component invents.
      setOpenGroups((prev) => {
        const next = new Set(prev);
        if (next.has(row.id)) next.delete(row.id);
        else next.add(row.id);
        return next;
      });
    },
    [onSelect],
  );

  const revealPath = useCallback(
    (path: string) => {
      setExpanded((prev) => {
        const next = new Set(prev);
        for (const d of ancestorDirs(path)) next.add(d);
        return next;
      });
      const i = rows.findIndex((r) => r.kind === "file" && r.path === path);
      if (i >= 0) setFocusedIndex(i);
    },
    [rows],
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
      // isn't left with a cursor in the filter box. Polls for REAL
      // visibility (`offsetParent`) rather than trusting a fixed number
      // of frames: this tree is always MOUNTED regardless of the dock's
      // collapsed state (`Desk.tsx` renders `{dock}` unconditionally
      // inside the `<aside>`, only the PANEL's size changes), and the
      // panel's actual resize is a SEPARATE, LATER `useEffect` there.
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
        return row && row.kind === "file" && row.path
          ? { path: row.path, kind: "file" as EntryKind }
          : null;
      },
      cycleView: (delta: number) => {
        const available = response?.views_available ?? ["physical"];
        const at = Math.max(0, available.indexOf(view));
        const next = available[(at + delta + available.length) % available.length];
        setView(next);
        setFocusedIndex(0);
      },
      toggleFilterMode: () => setMode((m) => (m === "filter" ? "highlight" : "filter")),
      cycleLanes: () => setLanePreset((p) => (p + 1) % LANE_PRESETS.length),
      toggleSelect: () => {
        const row = rows[focusedIndex];
        if (!row || row.kind !== "file" || !row.path) return;
        setSelected((prev) => {
          const next = new Set(prev);
          if (next.has(row.path as string)) next.delete(row.path as string);
          else next.add(row.path as string);
          return next;
        });
      },
      openActions: () => setActionsOpen(true),
      jump: (lane, dir) => {
        const i = nextRowWith(rows, focusedIndex, dir, lane);
        if (i < 0) return false;
        setFocusedIndex(i);
        return true;
      },
      reveal: revealPath,
    }),
    [rows, focusedIndex, activate, response, view, revealPath],
  );

  useEffect(() => {
    virtualizer.scrollToIndex(focusedIndex, { align: "auto" });
  }, [focusedIndex, virtualizer]);

  const selectedPaths = useMemo(() => Array.from(selected).sort(), [selected]);
  const cliLine = treeCli({
    repo,
    view,
    scope: response?.scope_applied ? response?.scope ?? undefined : undefined,
    filter: routed.filter,
    mode,
    decorate: lanes.lanes,
  });

  return (
    <div
      className="kbc-tree"
      data-kbc-tree
      data-kbc-tree-view={view}
      data-kbc-tree-mode={mode}
      data-kbc-attention-overlay={attentionOn ? "on" : "off"}
    >
      <div className="kbc-tree__head">
        <div className="kbc-tree__views" role="group" aria-label="Tree projection">
          {(response?.views_available ?? ["physical"]).map((v) => (
            <button
              key={v}
              type="button"
              className={"kbc-tree__view" + (v === view ? " is-active" : "")}
              data-kbc-tree-view-btn={v}
              aria-pressed={v === view}
              onClick={() => {
                setView(v);
                setFocusedIndex(0);
              }}
            >
              {v}
            </button>
          ))}
        </div>
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
        placeholder={mode === "filter" ? "Filter files…" : "Highlight files…"}
        aria-label="Filter files"
        value={box}
        onChange={(e) => setBox(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Escape") {
            e.preventDefault();
            setBox("");
            (e.target as HTMLInputElement).blur();
            return;
          }
          if (e.key === "f" && e.ctrlKey && e.altKey) {
            e.preventDefault();
            setMode((m) => (m === "filter" ? "highlight" : "filter"));
            return;
          }
          if (e.key === "Enter") {
            e.preventDefault();
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
      <div className="kbc-tree__filter-meta">
        <button
          type="button"
          className="kbc-tree__mode"
          data-kbc-tree-mode-btn
          aria-label={`filter mode: ${mode}`}
          title="Ctrl-Alt-f — highlight keeps every row and badges the ancestors; filter prunes non-matches but keeps a match's ancestry"
          onClick={() => setMode((m) => (m === "filter" ? "highlight" : "filter"))}
        >
          {mode}
        </button>
        <button
          type="button"
          className="kbc-tree__lanes-btn"
          data-kbc-tree-lanes={lanes.id}
          title={`decorations: ${lanes.title} (press d to cycle)`}
          onClick={() => setLanePreset((p) => (p + 1) % LANE_PRESETS.length)}
        >
          {lanes.id}
        </button>
        {response?.counts.matched !== undefined && response?.counts.matched !== null ? (
          <span className="kbc-tree__count">{response.counts.matched} matched</span>
        ) : null}
      </div>

      {/* Honesty strip — every degrade, cap and refusal the daemon
          reported, rendered verbatim. Never summarised, never hidden. */}
      {atRef ? (
        <p className="kbc-tree__note" data-kbc-tree-note>
          browsing <code>{gitRef}</code> — showing the ref's own tree; projections and
          decorations are index-only (tree-as-of-a-ref is deferred)
        </p>
      ) : null}
      {response && response.scope_applied === false && response.scope ? (
        <p className="kbc-tree__note kbc-tree__note--warn" data-kbc-tree-note>
          scope not applied — showing the unscoped tree
        </p>
      ) : null}
      {(response?.notes ?? []).map((n) => (
        <p key={n} className="kbc-tree__note" data-kbc-tree-note>
          {n}
        </p>
      ))}
      {response?.truncated ? (
        <p className="kbc-tree__note kbc-tree__note--warn" data-kbc-tree-note>
          truncated: {response.truncated.returned} of {response.truncated.total ?? "?"} rows —{" "}
          {response.truncated.reason}
        </p>
      ) : null}

      {sticky.length > 0 ? (
        <div className="kbc-tree__sticky" data-kbc-tree-sticky aria-hidden>
          {sticky.map((r) => (
            <div
              key={r.id}
              className="kbc-tree__sticky-row"
              style={{ paddingLeft: `${8 + r.depth * 14}px`, height: ROW_HEIGHT }}
            >
              <span className="kbc-tree__twisty" aria-hidden>
                <Icon.Chevron className="kbc-twisty is-open" />
              </span>
              {r.label}
            </div>
          ))}
        </div>
      ) : null}

      <div className="kbc-tree__scroll" ref={scrollParentRef}>
        <div style={{ height: virtualizer.getTotalSize(), position: "relative" }}>
          {virtualItems.map((vi) => {
            const row = rows[vi.index];
            if (!row) return null;
            const isSelected = !!row.path && row.path === selectedPath;
            const isFocused = vi.index === focusedIndex;
            const isPicked = !!row.path && selected.has(row.path);
            const isOpen =
              row.kind === "dir" && row.path
                ? expanded.has(row.path)
                : openGroups.has(row.id);
            const hot =
              attentionOn && row.kind === "file" && row.path
                ? hotspotsMap.data?.get(row.path)
                : undefined;
            // Absence of a behavioral row ⇒ untinted (≠ zero score).
            const tint = attentionOn && hot ? attentionTintStyle(hot.hotspot.score) : undefined;
            return (
              <div
                key={row.id}
                className={
                  "kbc-tree__row" +
                  (isSelected ? " kbc-tree__row--selected" : "") +
                  (isFocused ? " kbc-tree__row--focused" : "") +
                  (isPicked ? " kbc-tree__row--picked" : "") +
                  (row.kind !== "file" ? " kbc-tree__row--dir" : "") +
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
                  if (!rung || rung === "here" || rung === "other" || row.kind !== "file" || !row.path || !onRamp)
                    return;
                  e.preventDefault();
                  setFocusedIndex(vi.index);
                  onRamp(rung, row.path, "file");
                }}
                onClick={(e) => {
                  setFocusedIndex(vi.index);
                  if ((e.ctrlKey || e.metaKey) && row.kind === "file" && row.path) {
                    const p = row.path;
                    setSelected((prev) => {
                      const next = new Set(prev);
                      if (next.has(p)) next.delete(p);
                      else next.add(p);
                      return next;
                    });
                    return;
                  }
                  activate(row, e.shiftKey ? "pane2" : undefined);
                }}
                data-kbc-kind={row.kind === "file" ? "file" : "dir"}
                data-kbc-row-kind={row.kind}
                data-kbc-attention-score={hot ? String(hot.hotspot.score) : undefined}
              >
                <span className="kbc-tree__twisty" aria-hidden>
                  {row.kind !== "file" ? (
                    <Icon.Chevron className={isOpen ? "kbc-twisty is-open" : "kbc-twisty"} />
                  ) : null}
                </span>
                <LabelWithHighlight row={row} />
                {row.trust ? (
                  <span
                    className={`kbc-tree__trust kbc-tree__trust--${row.trust}`}
                    title={`this grouping is an inference: ${row.trust}`}
                  >
                    {row.trust}
                  </span>
                ) : null}
                {mode === "highlight" && row.kind !== "file" && (row.match_count ?? 0) > 0 ? (
                  <span className="kbc-tree__matchcount">{row.match_count}</span>
                ) : null}
                <FactsCells facts={row.facts} />
              </div>
            );
          })}
        </div>
      </div>

      {response && response.unplaced_total > 0 ? (
        <details className="kbc-tree__unplaced" data-kbc-tree-unplaced>
          <summary>
            {response.unplaced_total} unplaced file(s) — the `{view}` projection could not place
            them
          </summary>
          {response.unplaced.map((r) => (
            <button
              key={r.id}
              type="button"
              className="kbc-tree__unplaced-row"
              onClick={() => r.path && onSelect(r.path, "file")}
            >
              {r.label}
            </button>
          ))}
        </details>
      ) : null}

      {selectedPaths.length > 0 ? (
        <div className="kbc-tree__selection" data-kbc-tree-selection>
          <span>{selectedPaths.length} selected</span>
          <button type="button" onClick={() => setActionsOpen((o) => !o)} data-kbc-tree-actions>
            Actions…
          </button>
          <button type="button" onClick={() => setSelected(new Set())}>
            Clear
          </button>
        </div>
      ) : null}

      {actionsOpen && selectedPaths.length > 0 ? (
        <div className="kbc-tree__actions" role="dialog" aria-label="Selection actions">
          {SELECTION_ACTIONS.map((a) => {
            const cli = selectionCli(a.id, { repo, paths: selectedPaths });
            return (
              <div key={a.id} className="kbc-tree__action">
                <div className="kbc-tree__action-label">
                  {a.label}
                  {a.exact ? null : <span className="kbc-tree__action-warn"> (template)</span>}
                </div>
                {a.note ? <div className="kbc-tree__action-note">{a.note}</div> : null}
                <code className="kbc-tree__action-cli">{cli}</code>
                <button
                  type="button"
                  onClick={() => void navigator.clipboard?.writeText(cli)}
                  title="copy the command"
                >
                  Copy CLI
                </button>
              </div>
            );
          })}
          <button type="button" onClick={() => setActionsOpen(false)}>
            Close
          </button>
        </div>
      ) : null}

      <details className="kbc-tree__cli" data-kbc-tree-cli>
        <summary>CLI</summary>
        <code>{cliLine}</code>
      </details>
    </div>
  );
});

export default FileTree;
