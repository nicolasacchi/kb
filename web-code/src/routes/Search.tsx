// V71-D2 — the results page, as a LENS over the shell (design P1/D3).
//
// Three regions and no modes: the facet rail WRITES the query, the middle
// column is the same `SearchSection` renderer the Omnibox uses (grouped by
// the daemon, narrowed for free by the refine box), and the right column is
// a peek-first CM6 preview that never takes focus.
//
// What is deliberately NOT here:
//
//   - **No second matcher.** Ranking is the daemon's (kbcq/1's one-matcher
//     rule); `lib/refine.ts` only REMOVES rows from the page the daemon
//     returned, and says "N of M" rather than replacing M.
//   - **No hidden filter state.** Every control writes a kbcq/1 clause into
//     the query string through `lib/kbcqEdit.ts`. If you cannot see it in
//     the box, it is not in force.
//   - **No new key dispatcher.** Every key on this page is a
//     `scope: "search"` row in `commands/registry.json`, resolved through
//     the SAME `dispatch.ts` resolver `CommandRoot` uses. They are
//     `dispatch: "surface"` because the query box holds focus and guard 1
//     (`isTypingTarget`) stops the window host before it ever sees them —
//     the palette's own pattern.
//   - **No server-side saved searches.** History and saved searches are
//     browser-local (`lib/searchHistory.ts`'s recorded cut); the parity
//     affordance is the copied `kb-code search '<q>'` line.

import { useEffect, useMemo, useReducer, useRef, useState } from "react";
import { useRamp } from "../nav/ramp";
import { useNavigate, useSearchParams } from "react-router";
import type { TranscriptHit } from "../api/types";
import { StateCard } from "@kb/ui/StateCard";
import { ViewBar, type Cluster } from "@kb/ui/ViewBar";
import EmptyState from "../components/EmptyState";
import MetaLine from "../components/MetaLine";
import { Icon } from "../components/iconSet";
import SearchSection from "../components/search/SearchSection";
import FacetRail from "../components/search/FacetRail";
import SearchPreview from "../components/search/SearchPreview";
import { useOmniSearch } from "../hooks/useOmniSearch";
import { useReviewFiles } from "../hooks/useReviews";
import { useScopes } from "../hooks/useScopes";
import { readerUrl } from "../lib/breadcrumbs";
import { useValidatedCurrentReview } from "../hooks/useValidatedCurrentReview";
import { sectionsToRowCounts } from "../lib/omniSearch";
import { initialPaletteState, paletteReducer } from "../lib/paletteReducer";
import { loadKeyPreset, loadSearchPreviewOpen, saveSearchPreviewOpen } from "../lib/prefs";
import { applyPrefixChip, PREFIX_CHIPS, stripLeadingPrefix } from "../lib/prefixChips";
import { resolveSearchTarget } from "../lib/searchTargets";
import { buildPageView, laneCounts } from "../lib/searchPage";
import { displayKey, tokenOf, resolve as resolveCommand } from "../commands/dispatch";
import { KBC_COMMANDS } from "../commands/registry.gen";
import { useCommandScope } from "../commands/CommandRoot";
import { SEARCH_COMMAND_IDS, type SearchHandlers } from "./searchCommands";
import { setFacets, setGroup } from "../lib/kbcqEdit";
import { parse as parseKbcq, GROUP_KEYS, type GroupKey } from "../lib/kbcq";
import {
  cliLineFor,
  loadHistory,
  loadSaved,
  pushHistory,
  saveSearch,
  type SavedSearch,
} from "../lib/searchHistory";
import {
  activeDrawerSet,
  drawerSetsReducer,
  drawerSetId,
  drawerTabOrder,
  initialDrawerSets,
  type DrawerRow,
} from "../desk/drawerSets";
import { copyToClipboard } from "../editor/vimReader";
import { toast } from "../lib/toast";
import { useListScrollRestoration } from "../hooks/useScrollRestoration";
import "../styles/search.css";

/// The key a `scope: "search"` row displays under the active preset, so the
/// ViewBar names the same chord the `?` sheet does (null = palette-only).
function keyFor(id: string): string | undefined {
  const c = KBC_COMMANDS.find((r) => r.id === id);
  return c ? (displayKey(c, loadKeyPreset()) ?? undefined) : undefined;
}

const GROUP_LABELS: Record<GroupKey, string> = {
  file: "File",
  kind: "Kind",
  lane: "Lane",
  dir: "Directory",
  none: "None",
};

const LIMIT = 20;
const URL_SYNC_MS = 120;
/// `GET /api/file` bumps the store generation on every read (recon R1), so
/// the preview waits for the cursor to settle before fetching. Long enough
/// to skip every intermediate row of a held arrow key, short enough to feel
/// attached to the cursor.
const PREVIEW_DEBOUNCE_MS = 250;

/// `null` is a real member of the cycle: it means "no `group:` token at
/// all", which the grammar keeps distinct from `group:none`.
const GROUP_CYCLE: (GroupKey | null)[] = [null, ...GROUP_KEYS];

function browserStorage() {
  try {
    return window.localStorage;
  } catch {
    // A privacy-mode browser with storage blocked: history degrades to
    // nothing rather than taking the page down.
    return { getItem: () => null, setItem: () => {} };
  }
}

export default function Search() {
  useListScrollRestoration();
  // Publish the scope so the palette, the `?` sheet and the which-key
  // overlay list THIS page's rows while it is mounted. The rows are all
  // `dispatch: "surface"`, so `CommandRoot` still fires none of them (it
  // finds no registered handler and leaves the key alone) — publishing is
  // what makes them DISCOVERABLE, which is the other half of "a binding
  // that is not in the registry does not exist".
  useCommandScope("search");
  const ramp = useRamp({ focusedPane: 1 });
  const [searchParams, setSearchParams] = useSearchParams();
  const [q, setQ] = useState(() => searchParams.get("q") ?? "");
  const [repo, setRepo] = useState<string | undefined>(() => searchParams.get("repo") ?? undefined);
  const [popoverHit, setPopoverHit] = useState<TranscriptHit | null>(null);
  const [refineText, setRefineText] = useState("");
  // V80-R1 — collapsible, persisted (`lib/prefs.ts`'s `searchPreviewOpen`):
  // no stored choice yet defaults to open ≥1280px, closed narrower, so a
  // laptop-width visit doesn't waste a third of the page on an empty
  // "nothing focused" pane by default.
  const [previewOn, setPreviewOn] = useState(() =>
    loadSearchPreviewOpen(typeof window !== "undefined" ? window.innerWidth : 1280),
  );
  const [historyOpen, setHistoryOpen] = useState(false);
  const [syntaxOpen, setSyntaxOpen] = useState(false);
  const storage = useMemo(browserStorage, []);
  const [history, setHistory] = useState<string[]>(() => loadHistory(storage));
  const [saved, setSaved] = useState<SavedSearch[]>(() => loadSaved(storage));
  const [stack, stackDispatch] = useReducer(drawerSetsReducer, initialDrawerSets);
  const inputRef = useRef<HTMLInputElement | null>(null);
  const refineRef = useRef<HTMLInputElement | null>(null);
  const navigate = useNavigate();

  const { sections, loading, error, response } = useOmniSearch(q, repo, LIMIT);
  const scopes = useScopes();

  // V80-M3 — "in review diff" chip: ONE fetch of the current review's
  // changed files (`lib/currentReview.ts`), never a per-row fetch. Only
  // possible while the search itself is scoped to a repo (`?repo=`) —
  // there is no single "current review" to mean anything for a fleet-wide
  // search, and an unscoped hit's OWN repo may not even be the one the
  // marker is for.
  const { current: currentReview, reviewId: currentReviewIdNum } = useValidatedCurrentReview(
    repo ?? "",
  );
  const reviewFilesQ = useReviewFiles(
    repo,
    currentReviewIdNum,
    "latest",
    !!repo && !!currentReview && currentReviewIdNum !== undefined,
  );
  const reviewFilePaths = useMemo(
    () => (reviewFilesQ.data ? new Set(reviewFilesQ.data.files.map((f) => f.path)) : null),
    [reviewFilesQ.data],
  );
  const [state, dispatch] = useReducer(paletteReducer, initialPaletteState());
  const trimmedQ = q.trim();

  const page = useMemo(() => buildPageView(sections, refineText), [sections, refineText]);
  const viewSections = useMemo(() => page.view.map((v) => v.section), [page]);
  const noResults = trimmedQ !== "" && !loading && !error && page.view.length > 0 && page.shown === 0;

  useEffect(() => {
    dispatch({ type: "SET_SECTIONS", sections: sectionsToRowCounts(viewSections) });
  }, [viewSections]);

  useEffect(() => {
    const t = setTimeout(() => {
      const next: Record<string, string> = {};
      if (q.trim()) next.q = q;
      if (repo) next.repo = repo;
      setSearchParams(next, { replace: true });
    }, URL_SYNC_MS);
    return () => clearTimeout(t);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [q, repo]);

  // History records a query the user actually SETTLED on (the URL sync has
  // fired and the response landed), never every keystroke.
  useEffect(() => {
    if (trimmedQ === "" || loading || error) return;
    const t = setTimeout(() => setHistory(pushHistory(storage, trimmedQ)), 1200);
    return () => clearTimeout(t);
  }, [trimmedQ, loading, error, storage]);

  // --- the focused hit, and the debounced preview target ------------------
  const focusedTarget = useMemo(
    () => resolveSearchTarget(viewSections, state.cursor, repo, q),
    [viewSections, state.cursor, repo, q],
  );
  const [previewAt, setPreviewAt] = useState<{ repo: string; path: string; line?: number } | null>(null);
  useEffect(() => {
    if (!previewOn || !focusedTarget || focusedTarget.kind !== "reader") {
      setPreviewAt(null);
      return;
    }
    const { repo: r, path, line } = focusedTarget;
    const t = setTimeout(() => setPreviewAt({ repo: r, path, line }), PREVIEW_DEBOUNCE_MS);
    return () => clearTimeout(t);
  }, [focusedTarget, previewOn]);

  function activate(target: ReturnType<typeof resolveSearchTarget>) {
    if (!target) return;
    switch (target.kind) {
      case "full-search":
        return;
      case "reader":
        navigate(readerUrl(target.repo, target.path, undefined, target.line));
        return;
      case "external":
        window.open(target.href, "_blank", "noreferrer");
        return;
      case "popover":
        setPopoverHit((cur) => (cur?.uuid === target.hit.uuid ? null : target.hit));
        return;
    }
  }

  /// Snapshot the CURRENT page into the result-set stack. Rows are what the
  /// page shows (grouped + refined), so a kept set is the thing you were
  /// looking at, not a different query re-run later — the same
  /// eviction-is-a-view-operation ring the Desk drawer uses.
  function keepCurrentSet() {
    const rows: DrawerRow[] = [];
    page.view.forEach((v, si) => {
      const count = sectionsToRowCounts([v.section])[0]?.rowCount ?? 0;
      for (let i = 0; i < count; i++) {
        const t = resolveSearchTarget(viewSections, { section: si, row: i }, repo, q);
        if (t && t.kind === "reader") rows.push({ repo: t.repo, path: t.path, line: t.line ?? 1 });
      }
    });
    if (rows.length === 0) {
      toast.warn("Nothing file-shaped on this page to keep");
      return;
    }
    stackDispatch({
      type: "keep",
      set: {
        id: drawerSetId("search", parseKbcq(q).normalized || q),
        title: parseKbcq(q).normalized || q,
        kind: "search",
        rows,
      },
    });
    toast.ok(`Kept ${rows.length} row(s) — Alt-[ / Alt-] walk the stack`);
  }

  const group = parseKbcq(q).group;

  // Every id `searchCommands.ts` declares, or this does not compile.
  const handlers: SearchHandlers = {
    "search.row.next": () => dispatch({ type: "MOVE_ROW", delta: 1 }),
    "search.row.prev": () => dispatch({ type: "MOVE_ROW", delta: -1 }),
    "search.group.next": () => dispatch({ type: "MOVE_SECTION", delta: 1 }),
    "search.group.prev": () => dispatch({ type: "MOVE_SECTION", delta: -1 }),
    "search.open": () => {
      if (trimmedQ === "") return;
      activate(focusedTarget);
    },
    "search.refine": () => refineRef.current?.focus(),
    // Two rungs, innermost first — see the registry row's note. Escape
    // never re-queries and never navigates; clearing the refinement KEEPS
    // the result set underneath it.
    "search.refine.clear": () => {
      if (syntaxOpen) {
        setSyntaxOpen(false);
        return;
      }
      if (popoverHit) {
        setPopoverHit(null);
        return;
      }
      setRefineText("");
      inputRef.current?.focus();
    },
    "search.facets": () => setQ((cur) => setFacets(cur, !parseKbcq(cur).facets)),
    "search.group.cycle": () =>
      setQ((cur) => {
        const at = GROUP_CYCLE.indexOf(parseKbcq(cur).group);
        return setGroup(cur, GROUP_CYCLE[(at + 1) % GROUP_CYCLE.length]);
      }),
    "search.preview": () =>
      setPreviewOn((v) => {
        const next = !v;
        saveSearchPreviewOpen(next);
        return next;
      }),
    "search.copy-cli": () => {
      const line = cliLineFor(parseKbcq(q).normalized || q, repo);
      copyToClipboard(line);
      toast.ok(`Copied: ${line}`);
    },
    "search.save": () => {
      const name = window.prompt("Save this search as:", parseKbcq(q).normalized || q);
      if (name === null) return;
      setSaved(saveSearch(storage, name, parseKbcq(q).normalized || q));
      setHistoryOpen(true);
    },
    "search.history": () => setHistoryOpen((v) => !v),
    "search.keep": keepCurrentSet,
    "search.stack.next": () => stackDispatch({ type: "stepSet", delta: 1 }),
    "search.stack.prev": () => stackDispatch({ type: "stepSet", delta: -1 }),
  };

  // The seven action pills as ONE ViewBar. Same handlers the keys run; the
  // labels are verbs and the keys come from the cmd/1 rows.
  const clusters: Cluster[] = [
    {
      label: "Results",
      controls: [
        {
          id: "search.facets",
          label: "Facets",
          kind: "toggle",
          active: parseKbcq(q).facets,
          key: keyFor("search.facets"),
          onSelect: handlers["search.facets"],
        },
        {
          id: "search.group",
          label: "Group by",
          kind: "select",
          key: keyFor("search.group.cycle"),
          value: group ?? "",
          options: [
            { value: "", label: "Default" },
            ...GROUP_KEYS.map((k) => ({ value: k, label: GROUP_LABELS[k] })),
          ],
          onSelect: (v) => setQ((cur) => setGroup(cur, v ? (v as GroupKey) : null)),
        },
        {
          id: "search.preview",
          label: "Preview",
          kind: "toggle",
          active: previewOn,
          key: keyFor("search.preview"),
          onSelect: handlers["search.preview"],
        },
      ],
    },
    {
      label: "Query",
      controls: [
        { id: "search.save", label: "Save", kind: "button", key: keyFor("search.save"), onSelect: handlers["search.save"] },
        {
          id: "search.history",
          label: "History",
          kind: "toggle",
          active: historyOpen,
          key: keyFor("search.history"),
          onSelect: handlers["search.history"],
        },
        {
          id: "search.copy-cli",
          label: "Copy CLI",
          kind: "button",
          key: keyFor("search.copy-cli"),
          onSelect: handlers["search.copy-cli"],
        },
      ],
    },
    {
      label: "Set",
      controls: [
        {
          id: "search.keep",
          label: "Keep as set",
          kind: "button",
          key: keyFor("search.keep"),
          onSelect: handlers["search.keep"],
        },
      ],
    },
  ];

  const counts = laneCounts(sections);
  const countLine: string | null =
    trimmedQ === "" || error
      ? null
      : loading
        ? "Searching…"
        : page.total === 0
          ? "No results"
          : `${page.total} ${page.total === 1 ? "result" : "results"} · lanes: ${counts
              .map((c) => `${c.label} ${c.count}`)
              .join(", ")}`;

  /// ONE keyboard door for the whole page: canonicalise the event to a
  /// kbc-cmd/1 token, ask the SAME resolver `CommandRoot` uses which
  /// `scope: "search"` row owns it, and run that id's handler. No key is
  /// hard-coded here — a binding change is a `registry.json` edit.
  function onKeyDown(e: React.KeyboardEvent) {
    const cmd = resolveCommand(tokenOf(e), "search");
    if (!cmd) return;
    const handler = (handlers as Record<string, (() => void) | undefined>)[cmd.id];
    if (!handler) return;
    e.preventDefault();
    handler();
  }

  const laneActions = () => {
    const sym = PREFIX_CHIPS.find((c) => c.hint === "symbols");
    const text = PREFIX_CHIPS.find((c) => c.hint === "text");
    const bare = stripLeadingPrefix(q.trim());
    const go = (chip: (typeof PREFIX_CHIPS)[number]) => () => {
      setQ((cur) => applyPrefixChip(cur, chip));
      inputRef.current?.focus();
    };
    return [
      ...(sym ? [{ key: "try-symbols", label: `Search symbols (@${bare})`, onClick: go(sym) }] : []),
      ...(text ? [{ key: "try-text", label: `Try the text lane: /${bare}`, onClick: go(text) }] : []),
    ];
  };

  const activeSet = activeDrawerSet(stack);
  const tabs = drawerTabOrder(stack);
  // V80-R1 — the preview column adds nothing before a search has even
  // started (there is no row to focus yet), so it stays OFF the grid
  // entirely rather than rendering `SearchPreview`'s own "nothing focused"
  // `EmptyState` right beside this page's OWN "Search everywhere"
  // `EmptyState` — one empty-state per view, never two competing for the
  // same explanation.
  const previewPaneVisible = previewOn && trimmedQ !== "";

  return (
    <div className="kbc-searchpage" id="main" onKeyDown={onKeyDown}>
      <header className="kbc-searchpage__head">
        <h1 className="kbc-searchpage__title">Search</h1>
        <input
          ref={inputRef}
          value={q}
          onChange={(e) => setQ(e.target.value)}
          placeholder="Search files, symbols, text… (@symbol  #file  /regex/  ?question  ~session)"
          className="kbc-searchpage__input"
          aria-label="search query"
          autoFocus
        />
        <span className="kbc-searchpage__syntax">
          <button
            type="button"
            className="kbc-searchpage__syntax-btn"
            aria-expanded={syntaxOpen}
            aria-controls="kbc-search-syntax"
            onClick={() => setSyntaxOpen((v) => !v)}
          >
            Syntax
          </button>
          {syntaxOpen && (
            <div id="kbc-search-syntax" className="kbc-searchpage__syntax-pop" role="group" aria-label="search prefixes">
              {PREFIX_CHIPS.map((chip) => (
                <button
                  key={chip.label}
                  type="button"
                  className="kbc-searchpage__syntax-row"
                  aria-label={`${chip.hint} prefix (${chip.label})`}
                  title={`${chip.hint} - inserts "${chip.label}"`}
                  onClick={() => {
                    setQ((cur) => applyPrefixChip(cur, chip));
                    setSyntaxOpen(false);
                    inputRef.current?.focus();
                  }}
                >
                  <code>{chip.label}</code>
                  <span>{chip.hint}</span>
                </button>
              ))}
            </div>
          )}
        </span>
        <input
          ref={refineRef}
          value={refineText}
          onChange={(e) => setRefineText(e.target.value)}
          placeholder="refine (Alt-r) — orderless, !excludes"
          className="kbc-searchpage__refine"
          aria-label="refine within results"
        />
        {repo && <span className="kbc-searchpage__scope">scoped to {repo}</span>}
        {loading && (
          <span className="kbc-searchpage__status" aria-live="polite">
            searching…
          </span>
        )}
      </header>

      {/* V80-R1 — "index generation N" moved here, right under the header,
          as a MetaLine (was a standalone `<p>` further down the page, past
          the toolbar/history/stack sections). */}
      <MetaLine
        className="kbc-searchpage__meta"
        items={[
          countLine && <span data-kbc-role="result-count">{countLine}</span>,
          response?.stale && (
            <span
              className="kbc-searchpage__generation"
              title="The daemon's index generation this answer was computed from. It rises each time the index changes; two answers with the same number saw the same files."
            >
              index generation {response.stale.generation}
            </span>
          ),
        ]}
      />

      <ViewBar clusters={clusters} maxVisible={5} />

      {/* The query that actually RAN, plus every token the parser could not
          honour — the same two lines `kb-code search` prints. */}
      {response?.normalized && response.normalized !== q.trim() && (
        <p className="kbc-searchpage__ran">ran: {response.normalized}</p>
      )}
      {(response?.diagnostics ?? []).map((d, i) => (
        <p key={i} className="kbc-searchpage__diag">
          {d.message}
          {d.suggestion ? ` (did you mean \`${d.suggestion}\`?)` : ""}
        </p>
      ))}
      {page.refined && (
        <p className="kbc-searchpage__refined">
          {page.shown} of {page.total} shown — refined within these results (Escape clears)
        </p>
      )}

      {historyOpen && (
        <section className="kbc-searchpage__history" aria-label="recent and saved searches">
          <h2 className="kbc-facets__label">Saved (this browser only)</h2>
          {saved.length === 0 && <p className="kbc-facets__empty">Nothing saved yet — Alt-s saves this query.</p>}
          {saved.map((s) => (
            <button key={s.name} type="button" className="kbc-facets__row" onClick={() => setQ(s.query)}>
              <span className="kbc-facets__value">{s.name}</span>
              <span className="kbc-facets__count kbc-facets__count--clause">{s.query}</span>
            </button>
          ))}
          <h2 className="kbc-facets__label">Recent</h2>
          {history.length === 0 && <p className="kbc-facets__empty">No history yet.</p>}
          {history.map((h) => (
            <button key={h} type="button" className="kbc-facets__row" onClick={() => setQ(h)}>
              <span className="kbc-facets__value">{h}</span>
            </button>
          ))}
        </section>
      )}

      {tabs.length > 0 && (
        <section className="kbc-searchpage__stack" aria-label="result-set stack">
          {tabs.map((t, i) => (
            <button
              key={t.id}
              type="button"
              className={`kbc-search__chip-btn${t.id === stack.activeId ? " is-active" : ""}${t.evicted ? " is-evicted" : ""}`}
              onClick={() => stackDispatch({ type: "activate", id: t.id })}
              title={t.evicted ? "closed — click to reopen" : `${t.rows.length} row(s)`}
            >
              {i + 1}. {t.title}
            </button>
          ))}
          {activeSet && (
            <span className="kbc-searchpage__status">
              {activeSet.rows.length} row(s) kept · Alt-[ older · Alt-] newer
            </span>
          )}
        </section>
      )}

      <div className={`kbc-searchpage__cols${previewPaneVisible ? "" : " kbc-searchpage__cols--no-preview"}`}>
        <FacetRail query={q} facets={response?.facets} scopes={scopes.data} onQuery={setQ} />
        <div className="kbc-searchpage__body" role="listbox" aria-label="search results">
          {error ? (
            <div className="kbc-searchpage__error" role="alert">
              {error}
            </div>
          ) : trimmedQ === "" ? (
            <EmptyState
              icon={<Icon.Search />}
              title="Search everywhere"
              hint="Type to search files, symbols, text, semantic, sessions, and transcripts. The Syntax button lists the prefixes that jump to one lane."
            />
          ) : noResults ? (
            <StateCard
              kind="empty"
              title={`No matches for “${q}”`}
              reason={
                page.refined
                  ? `The refinement "${refineText}" removed every row of this page. Escape clears it.`
                  : `Nothing in any lane of ${repo ? `repo ${repo}` : "any indexed repo"}.`
              }
              actions={
                page.refined
                  ? [
                      {
                        key: "clear-refinement",
                        label: "Clear the refinement",
                        onClick: () => {
                          setRefineText("");
                          inputRef.current?.focus();
                        },
                      },
                    ]
                  : [
                      ...laneActions(),
                      ...(repo
                        ? [{ key: "all-repos", label: "Search every repo", onClick: () => setRepo(undefined) }]
                        : []),
                    ]
              }
            />
          ) : (
            page.view.map((v, i) => (
              <SearchSection
                key={v.key}
                section={v.section}
                headerLabel={v.label}
                laneIndex={i}
                sections={viewSections}
                cursor={state.cursor}
                query={q}
                repo={repo}
                expandedTranscriptUuid={popoverHit?.uuid}
                onHover={(row) => dispatch({ type: "SET_CURSOR", cursor: { section: i, row } })}
                onPopover={(hit) => activate({ kind: "popover", hit })}
                onRamp={(rung, target) => {
                  ramp.activate(rung, target);
                }}
                reviewFilePaths={reviewFilePaths}
                reviewFileRepo={repo}
              />
            ))
          )}
        </div>
        {previewPaneVisible && (
          <SearchPreview repo={previewAt?.repo ?? null} path={previewAt?.path ?? null} line={previewAt?.line} />
        )}
      </div>
    </div>
  );
}

// Referenced so a future reader sees the contract without chasing the type:
// every id below has a handler above, enforced by `SearchHandlers` being a
// total `Record` and by `searchCommands.test.ts`'s two-way registry walk.
void SEARCH_COMMAND_IDS;
