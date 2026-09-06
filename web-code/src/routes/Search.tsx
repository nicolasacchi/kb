import { useEffect, useMemo, useReducer, useRef, useState } from "react";
import { useRamp } from "../nav/ramp";
import { useNavigate, useSearchParams } from "react-router-dom";
import type { TranscriptHit } from "../api/types";
import EmptyState from "../components/EmptyState";
import { Icon } from "../components/icons";
import PrefixChips from "../components/search/PrefixChips";
import SearchSection from "../components/search/SearchSection";
import { useOmniSearch } from "../hooks/useOmniSearch";
import { readerUrl } from "../lib/breadcrumbs";
import { sectionsToRowCounts } from "../lib/omniSearch";
import { initialPaletteState, paletteReducer } from "../lib/paletteReducer";
import { applyPrefixChip, PREFIX_CHIPS } from "../lib/prefixChips";
import { laneRowCount, orderSections } from "../lib/searchLanes";
import { resolveSearchTarget } from "../lib/searchTargets";
import { useListScrollRestoration } from "../hooks/useScrollRestoration";

const LIMIT = 20;
// URL-sync debounce - shorter than the fetch debounce would spam
// `history.replaceState`; matches it here since both fire off the same
// keystroke.
const URL_SYNC_MS = 120;

/// `/search?q=&repo=` (W4.3) - the SAME fixed lane sections the Omnibox
/// overlay renders, as a full, shareable page: the omnibox's Enter-on-
/// header (and every "See all" chip) lands here with the query carried
/// over. `q`/`repo` round-trip through the URL (debounced
/// `history.replaceState`, `{ replace: true }` so typing doesn't spam
/// browser history) so the page is bookmarkable/shareable mid-search.
export default function Search() {
  // V70-A6 — root CLAUDE.md #31, ported: this list scrolls the WINDOW, so
  // one offset keyed on the full URL is the whole story. Back onto it lands
  // where the reader left, not where the browser guessed.
  useListScrollRestoration();
  // V70-A6 — the Ramp (§P7). One handler for every rung on every result
  // surface; the search page has no panes of its own, so `Enter`/`Shift-Enter`
  // both land in the reader and the tab/window rungs carry the trail.
  const ramp = useRamp({ focusedPane: 1 });
  const [searchParams, setSearchParams] = useSearchParams();
  const [q, setQ] = useState(() => searchParams.get("q") ?? "");
  // Repo scope is read once from the URL on load - this page has no repo
  // switcher of its own yet (the omnibox already scopes by route; a page
  // reached from Home stays unscoped). `repo:` in the query text itself
  // still narrows individual lanes via the grammar, same as anywhere else.
  const [repo] = useState<string | undefined>(() => searchParams.get("repo") ?? undefined);
  const [popoverHit, setPopoverHit] = useState<TranscriptHit | null>(null);
  const inputRef = useRef<HTMLInputElement | null>(null);
  const navigate = useNavigate();

  const { sections, loading, error } = useOmniSearch(q, repo, LIMIT);
  const [state, dispatch] = useReducer(paletteReducer, initialPaletteState());
  const ordered = useMemo(() => orderSections(sections), [sections]);
  const trimmedQ = q.trim();
  // F6 — the page-level "no matches anywhere" EmptyState: every lane came
  // back with zero rows (an `unavailable_reason`/`pending` section already
  // contributes 0 via `laneRowCount`, so this doesn't fire just because a
  // lane's disabled). Distinct from `trimmedQ === ""` below - that's "no
  // query yet," this is "searched, found nothing."
  const totalRows = useMemo(() => ordered.reduce((n, s) => n + laneRowCount(s), 0), [ordered]);
  const noResults = trimmedQ !== "" && !loading && !error && ordered.length > 0 && totalRows === 0;

  useEffect(() => {
    dispatch({ type: "SET_SECTIONS", sections: sectionsToRowCounts(sections) });
  }, [sections]);

  useEffect(() => {
    const t = setTimeout(() => {
      const next: Record<string, string> = {};
      if (q.trim()) next.q = q;
      if (repo) next.repo = repo;
      setSearchParams(next, { replace: true });
    }, URL_SYNC_MS);
    return () => clearTimeout(t);
    // `setSearchParams` is stable per react-router's own guarantee; `repo`
    // never changes after mount (see the state initializer above).
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [q]);

  function activate(target: ReturnType<typeof resolveSearchTarget>) {
    if (!target) return;
    switch (target.kind) {
      case "full-search":
        return; // already here
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

  function onKeyDown(e: React.KeyboardEvent) {
    if (e.key === "Escape") {
      if (popoverHit) {
        e.preventDefault();
        setPopoverHit(null);
      }
      return;
    }
    if (e.key === "Tab") {
      e.preventDefault();
      dispatch({ type: "MOVE_SECTION", delta: e.shiftKey ? -1 : 1 });
      return;
    }
    if (e.key === "ArrowDown") {
      e.preventDefault();
      dispatch({ type: "MOVE_ROW", delta: 1 });
      return;
    }
    if (e.key === "ArrowUp") {
      e.preventDefault();
      dispatch({ type: "MOVE_ROW", delta: -1 });
      return;
    }
    if (e.key === "Enter") {
      e.preventDefault();
      // No query yet - the lane sections aren't rendered (the "no query
      // yet" EmptyState is shown instead, see the render below), so there's
      // nothing visible for Enter to activate even though `sections` may
      // already hold the empty-query files-lane recents underneath.
      if (trimmedQ === "") return;
      activate(resolveSearchTarget(sections, state.cursor, repo, q));
      return;
    }
  }

  return (
    <div className="kbc-searchpage" id="main" onKeyDown={onKeyDown}>
      <header className="kbc-searchpage__head">
        <h1 className="kbc-searchpage__title">Search</h1>
        <input
          ref={inputRef}
          value={q}
          onChange={(e) => setQ(e.target.value)}
          placeholder="Search files, symbols, text, semantic, sessions, transcripts…"
          className="kbc-searchpage__input"
          aria-label="search query"
          autoFocus
        />
        {repo && <span className="kbc-searchpage__scope">scoped to {repo}</span>}
        {loading && (
          <span className="kbc-searchpage__status" aria-live="polite">
            searching…
          </span>
        )}
      </header>
      <PrefixChips
        onInsert={(chip) => {
          setQ((cur) => applyPrefixChip(cur, chip));
          inputRef.current?.focus();
        }}
      />
      <div className="kbc-searchpage__body" role="listbox" aria-label="search results">
        {error ? (
          <div className="kbc-searchpage__error" role="alert">
            {error}
          </div>
        ) : trimmedQ === "" ? (
          <EmptyState
            icon={<Icon.Search />}
            title="Search everywhere"
            hint="Type to search files, symbols, text, semantic, sessions, and transcripts — or click a prefix chip below to jump straight to one lane."
          />
        ) : noResults ? (
          <EmptyState
            icon={<Icon.Search />}
            title="No matches"
            hint={`Nothing in any lane for "${q}".`}
            action={{
              label: `Try the text lane: /${q}`,
              onClick: () => {
                const textChip = PREFIX_CHIPS.find((c) => c.hint === "text");
                if (textChip) setQ((cur) => applyPrefixChip(cur, textChip));
                inputRef.current?.focus();
              },
            }}
          />
        ) : (
          ordered.map((section, i) => (
            <SearchSection
              key={section.lane}
              section={section}
              laneIndex={i}
              sections={sections}
              cursor={state.cursor}
              query={q}
              repo={repo}
              expandedTranscriptUuid={popoverHit?.uuid}
              onHover={(row) => dispatch({ type: "SET_CURSOR", cursor: { section: i, row } })}
              onPopover={(hit) => activate({ kind: "popover", hit })}
              onRamp={(rung, target) => {
                ramp.activate(rung, target);
              }}
            />
          ))
        )}
      </div>
    </div>
  );
}
