import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useLocation, useNavigate } from "react-router-dom";
import { useUrl } from "../hooks/useUrl";
import { useScrollRestoration } from "../hooks/useScrollRestoration";
import { useDocumentTitle } from "../hooks/useDocumentTitle";
import { useSearchResults } from "../hooks/useSearchResults";
import { useReadingProgress } from "../hooks/useReadingProgress";
import { useRovingCursor } from "../hooks/useRovingCursor";
import { type SearchHit, type SearchMode } from "../api/client";
import { recordSearch } from "../api/history";
import { artifactHref } from "../lib/artifactHref";
import { useKbs } from "../hooks/useKbs";
import SearchResultCard from "../components/search/SearchResultCard";
import SearchRail from "../components/search/SearchRail";
import SearchSortControl from "../components/search/SearchSortControl";
import SearchChipBar, { type Chip } from "../components/search/SearchChipBar";
import SearchSavedBar from "../components/search/SearchSavedBar";
import ZeroHitRecovery from "../components/search/ZeroHitRecovery";
import RecentMisses from "../components/search/RecentMisses";
import { formatReadWindow } from "../components/search/temporalRange";
import EmptyState from "../components/EmptyState";
import MobileDrawer from "../components/chrome/MobileDrawer";
import { useIsMobile } from "../hooks/useIsMobile";
import { Icon } from "../components/icons";
import {
  defaultSearchDir,
  SEARCH_SORTS,
  type SearchSort,
  type SortDir,
} from "../lib/searchSort";

const MODES: SearchMode[] = ["hybrid", "keyword", "semantic"];
const DEFAULT_LIMIT = 50;

function clampLimit(n: number): number {
  if (!Number.isFinite(n) || n <= 0) return DEFAULT_LIMIT;
  return Math.min(200, Math.max(20, Math.round(n)));
}

// Group federated (scope=all) hits by their originating corpus,
// most-hits-first so the most relevant corpus leads.
function groupByKb(hits: SearchHit[]): { kb: string; hits: SearchHit[] }[] {
  const m = new Map<string, SearchHit[]>();
  for (const h of hits) {
    const k = h.kb ?? "(unknown)";
    const arr = m.get(k);
    if (arr) arr.push(h);
    else m.set(k, [h]);
  }
  return [...m.entries()]
    .map(([kb, hs]) => ({ kb, hits: hs }))
    .sort((a, b) => b.hits.length - a.hits.length);
}

// Track F — the full search page. A deeper, URL-driven companion to the
// Cmd+K popup: every axis (query, mode, scope, kb, filters, limit) lives in
// the URL so a search is shareable / bookmarkable / saveable. The popup
// stays the fast path; this page is for digging — cross-corpus search,
// filters (F5), and rich result cards. F4 renders the rich cards + corpus
// grouping.
export default function SearchRoute() {
  const { params, set, setMany } = useUrl();
  const loc = useLocation();
  // X1 — restore window scroll per search URL (query/mode/scope/filters), so
  // returning from a result lands back where you were in the result list.
  useScrollRestoration(loc.pathname + loc.search);

  const q = params.get("q") || "";
  const rawMode = params.get("mode");
  const mode: SearchMode = MODES.includes(rawMode as SearchMode)
    ? (rawMode as SearchMode)
    : "hybrid";
  const scope = params.get("scope") === "all" ? "all" : "one";
  const category = params.get("category") || "";
  const folder = params.get("folder") || "";
  const limit = clampLimit(Number(params.get("limit")) || DEFAULT_LIMIT);
  // FS3 — faceted filters. Multi-value axes are csv in the URL.
  const csv = (k: string): string[] => {
    const v = params.get(k);
    return v ? v.split(",").filter(Boolean) : [];
  };
  const read = csv("read");
  const tags = csv("tags");
  const excludeTags = csv("exclude_tags");
  const status = csv("status");
  const severity = csv("severity");
  const caps = csv("caps");
  const since = params.get("since") || "";
  const sinceField: "created" | "modified" =
    params.get("since_field") === "created" ? "created" : "modified";
  // FS7 — single-kb membership facets.
  const session = params.get("session") || "";
  const list = params.get("list") || "";
  // W1.search — "read during" temporal scrubber (unix seconds, a
  // history-opens window; distinct from `since`/`since_field`'s
  // mtime/created recency, and from #35's docs-list `from`/`to` mtime
  // axis — same-shaped names, different tables, kept as separate URL
  // params so the two never collide).
  const parseUnixParam = (raw: string | null): number | null => {
    if (raw == null || raw === "") return null;
    const n = Number(raw);
    return Number.isFinite(n) ? n : null;
  };
  const readFrom = parseUnixParam(params.get("read_from"));
  const readTo = parseUnixParam(params.get("read_to"));
  // FS4 — sort menu. Server-side; default `relevance` stays out of the URL.
  const rawSort = params.get("sort");
  const sort: SearchSort = SEARCH_SORTS.some((s) => s.key === rawSort)
    ? (rawSort as SearchSort)
    : "relevance";
  const dir: SortDir =
    params.get("dir") === "asc"
      ? "asc"
      : params.get("dir") === "desc"
        ? "desc"
        : defaultSearchDir(sort);

  // kb resolution: explicit ?kb wins; else the first configured corpus.
  // `scope=all` ignores kb on the wire (federated fan-out). K1 — the shared
  // ["kbs"] query (not a private fetch) so the kbs[0] default matches the
  // header pill's resolution and never shifts on a racing fetch order (#23).
  const { data: kbs = [] } = useKbs();
  const activeKb = params.get("kb") || kbs[0]?.name || undefined;
  const wireKb = scope === "all" ? undefined : activeKb;
  // R0-opt-in (architecture invariant #11) — looked up so the rail's
  // "include session transcripts" toggle can reflect the server's silent
  // default for the active kb, not just the literal `category` param.
  const activeKbMeta = kbs.find((k) => k.name === activeKb);

  useDocumentTitle(q ? `Search · ${q}` : "Search");

  const res = useSearchResults({
    q,
    mode,
    scope,
    kb: wireKb,
    category: category || undefined,
    folder: folder || undefined,
    read: read.length ? read : undefined,
    tags: tags.length ? tags : undefined,
    excludeTags: excludeTags.length ? excludeTags : undefined,
    status: status.length ? status : undefined,
    severity: severity.length ? severity : undefined,
    caps: caps.length ? caps : undefined,
    since: since || undefined,
    sinceField: since ? sinceField : undefined,
    session: session || undefined,
    list: list || undefined,
    readFrom: readFrom ?? undefined,
    readTo: readTo ?? undefined,
    sort: sort !== "relevance" ? sort : undefined,
    dir: sort !== "relevance" && dir !== defaultSearchDir(sort) ? dir : undefined,
    limit,
  });

  // Reading chips are single-kb only (per-kb history); federated trades the
  // chip for breadth. The hook is disabled (empty map) when kb is undefined.
  const progress = useReadingProgress(scope === "one" ? activeKb : undefined);

  const hitCount = res.hits.length;
  const scopeLabel = scope === "all" ? "all corpora" : (activeKb ?? "this kb");
  // Normalize the per-card score bar against the result set's strongest hit
  // (scores are mode-relative: RRF-fused, BM25, or vector-similarity).
  const scoreMax = useMemo(
    () =>
      res.hits.reduce(
        (m, h) => (h.score != null && h.score > m ? h.score : m),
        0,
      ),
    [res.hits],
  );
  const groups = useMemo(
    () => (scope === "all" ? groupByKb(res.hits) : null),
    [scope, res.hits],
  );

  // W2.6a — the roving cursor over the flat results list. `groups` (the
  // federated scope=all view) renders per-kb sections, so the cursor's flat
  // index needs a running per-group offset; ungrouped is just `res.hits`
  // directly. Both branches mirror exactly what's rendered below (a card
  // only counts here if it's actually on screen).
  const flatResults = useMemo(() => {
    if (groups) return groups.flatMap((g) => g.hits.map((hit) => ({ hit, kb: g.kb })));
    return activeKb ? res.hits.map((hit) => ({ hit, kb: activeKb })) : [];
  }, [groups, res.hits, activeKb]);
  const groupOffsets = useMemo(() => {
    if (!groups) return null;
    const offsets = new Map<string, number>();
    let acc = 0;
    for (const g of groups) {
      offsets.set(g.kb, acc);
      acc += g.hits.length;
    }
    return offsets;
  }, [groups]);

  const navigate = useNavigate();
  // Plain (non-virtualized) result list — no imperative scrollToIndex
  // handle to reach into, so this keeps its own small ref map and calls the
  // DOM directly, same spirit as VirtualGrid/VirtualList's handle but
  // scoped to what a flat, un-virtualized list actually needs.
  const cardEls = useRef<Map<number, HTMLElement>>(new Map());
  const registerCardEl = useCallback((index: number, el: HTMLElement | null) => {
    if (el) cardEls.current.set(index, el);
    else cardEls.current.delete(index);
  }, []);
  const scrollCursorIntoView = useCallback((index: number) => {
    cardEls.current.get(index)?.scrollIntoView({ block: "nearest" });
  }, []);
  const activateFocused = useCallback(
    (index: number) => {
      const r = flatResults[index];
      if (!r) return;
      void recordSearch(r.kb, q);
      navigate(artifactHref(r.kb, r.hit.source_relative));
    },
    [flatResults, navigate, q],
  );
  const cursor = useRovingCursor({
    rows: flatResults.length,
    onActivate: activateFocused,
    scrollToIndex: scrollCursorIntoView,
    enabled: flatResults.length > 0,
  });

  // FS5 — active-filter chips, derived from the same URL the rail reads.
  // Each chip's remove is the inverse of its rail setter; clear-all nulls
  // every filter axis but keeps q/mode/scope/kb/sort/limit.
  const removeFromCsv = (key: string, current: string[], value: string) => {
    const next = current.filter((v) => v !== value);
    set(key, next.length ? next.join(",") : null);
  };
  const chips: Chip[] = [];
  for (const r of read)
    chips.push({
      key: `read:${r}`,
      label: r.replace("_", " "),
      onRemove: () => removeFromCsv("read", read, r),
    });
  for (const t of tags)
    chips.push({
      key: `tag:${t}`,
      label: `tag: ${t}`,
      onRemove: () => removeFromCsv("tags", tags, t),
    });
  for (const t of excludeTags)
    chips.push({
      key: `xtag:${t}`,
      label: `−tag: ${t}`,
      onRemove: () => removeFromCsv("exclude_tags", excludeTags, t),
    });
  for (const s of status)
    chips.push({
      key: `status:${s}`,
      label: `status: ${s}`,
      onRemove: () => removeFromCsv("status", status, s),
    });
  for (const s of severity)
    chips.push({
      key: `sev:${s}`,
      label: `severity: ${s}`,
      onRemove: () => removeFromCsv("severity", severity, s),
    });
  for (const c of caps)
    chips.push({
      key: `cap:${c}`,
      label: c,
      onRemove: () => removeFromCsv("caps", caps, c),
    });
  if (category)
    chips.push({
      key: "category",
      label: `category: ${category}`,
      onRemove: () => set("category", null),
    });
  if (folder)
    chips.push({
      key: "folder",
      label: `folder: ${folder}`,
      onRemove: () => set("folder", null),
    });
  if (since)
    chips.push({
      key: "since",
      label: `${sinceField} ≤ ${since}`,
      onRemove: () => setMany({ since: null, since_field: null }),
    });
  if (session)
    chips.push({
      key: "session",
      label: "session",
      onRemove: () => set("session", null),
    });
  if (list)
    chips.push({
      key: "list",
      label: "list",
      onRemove: () => set("list", null),
    });
  // W1.search item 1.b/4 — the "read during" window is a removable chip
  // like every other facet (this is its only render location: the rail's
  // TemporalScrubber owns the inputs, not a second chip affordance).
  if (readFrom != null || readTo != null)
    chips.push({
      key: "read_during",
      label: formatReadWindow(readFrom, readTo),
      onRemove: () => setMany({ read_from: null, read_to: null }),
    });
  const clearAll = () =>
    setMany({
      read: null,
      tags: null,
      exclude_tags: null,
      status: null,
      severity: null,
      caps: null,
      category: null,
      folder: null,
      since: null,
      since_field: null,
      session: null,
      list: null,
      read_from: null,
      read_to: null,
    });

  // FS6 — browse mode. With no query, any active filter / non-relevance
  // sort runs an empty-query browse (the daemon lists + filters + sorts);
  // the empty state only shows when there's truly nothing to do. `chips`
  // already enumerates every active filter, so its length is `hasFilters`.
  const hasFilters = chips.length > 0;
  const hasSort = sort !== "relevance";
  const browse = !q.trim() && (hasFilters || hasSort);
  const showResults = q.trim().length > 0 || browse;
  const unit = browse ? "artifact" : "result";

  // FS10 — on mobile the rail moves into a drawer; the same prop-driven
  // SearchRail element renders either inline (desktop) or in the sheet.
  const isMobile = useIsMobile();
  const [filtersOpen, setFiltersOpen] = useState(false);
  const rail = (
    <SearchRail
      mode={mode}
        scope={scope}
        category={category}
        folder={folder}
        limit={limit}
        read={read}
        tags={tags}
        excludeTags={excludeTags}
        status={status}
        severity={severity}
        caps={caps}
        since={since}
        sinceField={sinceField}
        session={session}
        list={list}
        readFrom={readFrom}
        readTo={readTo}
        activeKb={activeKb}
        defaultSearchCategory={activeKbMeta?.default_search_category}
        onMode={(m) => set("mode", m === "hybrid" ? null : m)}
        // Switching scope clears the single-kb-only facets in one navigate
        // (folder/tags/session/list are per-corpus; category may not exist
        // cross-kb).
        onScope={(s) =>
          setMany({
            scope: s === "one" ? null : "all",
            category: null,
            folder: null,
            tags: null,
            exclude_tags: null,
            session: null,
            list: null,
          })
        }
        onLimit={(n) => set("limit", n === DEFAULT_LIMIT ? null : String(n))}
        set={set}
        setMany={setMany}
      />
  );

  return (
    <div className="kb-search">
      {!isMobile && rail}
      <div className="kb-search__main">
        <div className="kb-search__topbar">
          {isMobile && (
            <button
              type="button"
              className="kb-search__filters-btn"
              onClick={() => setFiltersOpen(true)}
              aria-label="open filters"
            >
              {chips.length > 0 ? `Filters · ${chips.length}` : "Filters"}
            </button>
          )}
          <SearchInput value={q} onChange={(next) => set("q", next || null)} />
          <SearchSavedBar
            kb={activeKb}
            onApplyQuery={(qq, m) =>
              setMany({
                q: qq || null,
                mode: m && m !== "hybrid" ? m : null,
              })
            }
          />
        </div>
        <SearchChipBar chips={chips} onClearAll={clearAll} />

        {!showResults ? (
          <>
            <EmptyState
              icon={<Icon.Search />}
              title="Search the knowledge base"
              hint="Type a query, or pick a filter to browse. The full page adds search modes, cross-corpus search, filters, sorting, and rich result cards — the Cmd+K popup stays for fast lookups."
            />
            {/* item 1.d — zero-hit memory: quiet retry chips for past
                queries that came back empty, only on this true landing
                state (not the zero-HITS state below, which gets the
                fuller ZeroHitRecovery treatment). */}
            <RecentMisses kb={activeKb} onRetry={(qq) => set("q", qq)} />
          </>
        ) : (
          <>
            <div className="kb-search__meta" role="status">
              <span className="kb-search__count">
                {res.loading && hitCount === 0
                  ? browse
                    ? "browsing…"
                    : "searching…"
                  : `${hitCount} ${unit}${hitCount === 1 ? "" : "s"}`}
              </span>
              {!browse && (
                <>
                  <span className="kb-search__dot">·</span>
                  <span>{mode}</span>
                </>
              )}
              <span className="kb-search__dot">·</span>
              <span>{scopeLabel}</span>
              {res.ms != null && (
                <>
                  <span className="kb-search__dot">·</span>
                  <span>{res.ms}ms</span>
                </>
              )}
              {res.cacheHit && <span className="kb-search__cache">cached</span>}
              <SearchSortControl
                sort={sort}
                dir={dir}
                onSort={(s, d) =>
                  setMany({
                    sort: s === "relevance" ? null : s,
                    dir:
                      s !== "relevance" && d !== defaultSearchDir(s) ? d : null,
                  })
                }
              />
            </div>

            {res.error ? (
              <EmptyState
                icon={<Icon.Search />}
                title="Search failed"
                hint={res.error}
              />
            ) : !res.loading && hitCount === 0 ? (
              <>
                <EmptyState
                  icon={<Icon.Search />}
                  title="No matches"
                  hint={
                    browse
                      ? "No artifacts match these filters — clear one in the rail, or try one of the ideas below."
                      : "Widen the query, switch search mode, or clear a filter — or try one of the ideas below."
                  }
                />
                {/* item 1 — never-empty search: bounded probe chips for
                    nearby escapes (mode/scope/filters) plus the museum
                    delta (top facet examples + a deterministic sample
                    deck), so a zero-hit page is never a dead end. */}
                <ZeroHitRecovery
                  q={q}
                  mode={mode}
                  scope={scope}
                  kb={wireKb}
                  activeKb={activeKb}
                  filters={{
                    category: category || undefined,
                    folder: folder || undefined,
                    tags: tags.length ? tags : undefined,
                    excludeTags: excludeTags.length ? excludeTags : undefined,
                    status: status.length ? status : undefined,
                    severity: severity.length ? severity : undefined,
                    caps: caps.length ? caps : undefined,
                    since: since || undefined,
                    sinceField: since ? sinceField : undefined,
                    session: session || undefined,
                    list: list || undefined,
                    readFrom: readFrom ?? undefined,
                    readTo: readTo ?? undefined,
                  }}
                  hasFilters={hasFilters}
                  set={set}
                  setMany={setMany}
                />
              </>
            ) : groups ? (
              groups.map((g) => {
                const base = groupOffsets?.get(g.kb) ?? 0;
                return (
                  <section key={g.kb} className="kb-search-group">
                    <header className="kb-search-group__h">
                      <span className="kb-search-group__name">{g.kb}</span>
                      <span className="kb-search-group__ct">{g.hits.length}</span>
                    </header>
                    <div className="kb-search__cards">
                      {g.hits.map((h, i) => {
                        const flatIndex = base + i;
                        const isFocused = cursor.focusedIndex === flatIndex;
                        return (
                          <div
                            key={`${g.kb}:${h.id}`}
                            ref={(el) => registerCardEl(flatIndex, el)}
                            className={isFocused ? "kb-search-card--focused" : undefined}
                            role="option"
                            aria-selected={isFocused}
                            data-kb-cursor={isFocused ? "true" : undefined}
                          >
                            <SearchResultCard
                              hit={h}
                              kb={g.kb}
                              query={q}
                              scoreMax={scoreMax}
                              showCorpus
                              mode={mode}
                              rank={i + 1}
                              total={g.hits.length}
                            />
                          </div>
                        );
                      })}
                    </div>
                  </section>
                );
              })
            ) : activeKb ? (
              <div className="kb-search__cards">
                {res.hits.map((h, i) => {
                  const isFocused = cursor.focusedIndex === i;
                  return (
                    <div
                      key={h.id}
                      ref={(el) => registerCardEl(i, el)}
                      className={isFocused ? "kb-search-card--focused" : undefined}
                      role="option"
                      aria-selected={isFocused}
                      data-kb-cursor={isFocused ? "true" : undefined}
                    >
                      <SearchResultCard
                        hit={h}
                        kb={activeKb}
                        query={q}
                        scoreMax={scoreMax}
                        progress={progress.get(h.id)}
                        mode={mode}
                        rank={i + 1}
                        total={res.hits.length}
                      />
                    </div>
                  );
                })}
              </div>
            ) : null}

            {/* lance search has no offset — "show more" re-queries at a
                higher limit (not an append), capped at the server's 200. */}
            {!res.error && hitCount >= limit && limit < 200 && (
              <button
                type="button"
                className="kb-search__more"
                onClick={() => set("limit", String(clampLimit(limit * 2)))}
              >
                Show more — up to {clampLimit(limit * 2)}
              </button>
            )}
          </>
        )}
      </div>
      {isMobile && (
        <MobileDrawer
          open={filtersOpen}
          onClose={() => setFiltersOpen(false)}
          side="left"
          title="Filters"
          ariaLabel="search filters"
        >
          {rail}
        </MobileDrawer>
      )}
    </div>
  );
}

// Local controlled input that buffers keystrokes and flushes to the URL on
// a short debounce, so the address bar (and the query key) don't thrash per
// keystroke — mirrors useDocs' "debounce the key" pattern. Adopts external
// URL changes (back button, popup escalation, saved-query restore) while
// ignoring the echo of its own debounced write.
function SearchInput({
  value,
  onChange,
}: {
  value: string;
  onChange: (next: string) => void;
}) {
  const [draft, setDraft] = useState(value);
  const inputRef = useRef<HTMLInputElement>(null);
  const onChangeRef = useRef(onChange);
  const lastWrittenRef = useRef(value);
  onChangeRef.current = onChange;

  useEffect(() => {
    if (value !== lastWrittenRef.current) {
      lastWrittenRef.current = value;
      setDraft(value);
    }
  }, [value]);

  useEffect(() => {
    inputRef.current?.focus();
  }, []);

  useEffect(() => {
    if (draft === value) return;
    const t = setTimeout(() => {
      lastWrittenRef.current = draft;
      onChangeRef.current(draft);
    }, 110);
    return () => clearTimeout(t);
  }, [draft, value]);

  return (
    <div className="kb-search__box">
      <Icon.Search />
      <input
        ref={inputRef}
        type="search"
        className="kb-search__input"
        placeholder="Search the knowledge base…"
        value={draft}
        onChange={(e) => setDraft(e.target.value)}
        aria-label="search query"
        autoFocus
      />
    </div>
  );
}
