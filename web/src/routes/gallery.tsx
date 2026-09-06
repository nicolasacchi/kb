import { lazy, Suspense, useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useNavigate, useLocation, Link } from "react-router-dom";
import { useQueryClient } from "@tanstack/react-query";
import {
  fetchAtlasEdges,
  fetchDocByPath,
  fetchKbs,
  isAbortError,
  type AtlasEdge,
  type DocSummary,
  type KbSummary,
} from "../api/client";
import NotesPanel from "../components/NotesPanel";
import { sse } from "../api/sse";
import PinnedIndexCard from "../components/PinnedIndexCard";
import { useReportQueryStats } from "../components/chrome/queryStats";
// Lazy: the atlas canvas (+ AtlasInspector) is heavy and only renders in the
// atlas view, so keep it out of the gallery/initial chunk until ?view=atlas.
const AtlasView = lazy(() => import("../components/AtlasView"));
// W3.M-d — the map-home shell (`?shell=map`), lazy for the same reason: it
// wraps the same atlas canvas plus a projection panel.
const MapShell = lazy(() => import("../components/MapShell"));
import HistoryTimeline from "../components/HistoryTimeline";
import ReflectionCanvas from "../components/ReflectionCanvas";
import Broadsheet from "../components/Broadsheet";
import GallerySection from "../components/GallerySection";
import GalleryLobby from "../components/GalleryLobby";
import { ResurfaceStrip } from "../components/ResurfaceStrip";
import { EchoStrip } from "../components/EchoStrip";
import EmptyState from "../components/EmptyState";
import Loading from "../components/Loading";
import VirtualGrid, { type VirtualGridHandle } from "../components/VirtualGrid";
import VirtualList, { type VirtualListHandle } from "../components/VirtualList";
import FolderBreadcrumb from "../components/FolderBreadcrumb";
import RenameFolderModal from "../components/RenameFolderModal";
import { Icon } from "../components/icons";
import { folderDownloadUrl } from "../lib/download";
import { folderIndexNote } from "../lib/folderIndexNote";
import { galleryUrl, type GallerySort } from "../lib/galleryUrl";
import { useDocs } from "../hooks/useDocs";
import { useAtlasPoints } from "../hooks/useAtlasPoints";
import { useRovingCursor } from "../hooks/useRovingCursor";
import { useSessions, useSessionFolders } from "../hooks/useSessions";
import type { SessionRow } from "../api/sessions";
import { useUrl } from "../hooks/useUrl";
import { useIsMobile } from "../hooks/useIsMobile";
import { useScrollRestoration } from "../hooks/useScrollRestoration";
import { setLastGalleryUrl } from "../lib/lastGalleryUrl";
import { useReadingProgress } from "../hooks/useReadingProgress";
import { useDocumentTitle } from "../hooks/useDocumentTitle";
import { artifactHref } from "../lib/artifactHref";
import { isIndexPage, tagsFor } from "../lib/derive";
import { composeDraftExclusion, DRAFT_TAG, isDraftView } from "../lib/draft";
import {
  defaultDir,
  groupByFolder,
  type GroupKey,
  type SortDir,
  type SortKey,
} from "../lib/sort";

// W2.5 — "press" is the broadsheet home: a fifth mode (`?view=press`)
// packing the CURRENT filter set into a deterministic front page (see
// components/Broadsheet.tsx); its nav tab lives in lib/navItems.ts.
// W2.6a — hover/focus prefetch delay, mirrored from Card.tsx's own
// PREFETCH_DELAY_MS (the roving cursor warms the same TanStack key
// independently rather than reaching into Card's private timer).
const CURSOR_PREFETCH_DELAY_MS = 300;

// W3.C-b — "canvas" is the multi-facet reflection canvas: four synchronized
// day tracks (created · read · sessions · comments) sharing one brush, whose
// brushed set pivots back into this gallery through the ALREADY-SHIPPED
// `?ids=` / `?from=`+`?to=` atoms (invariant #35 — the canvas adds none).
// Like `history` it owns its whole body and needs no docs page, so it is
// suppressed from the grid/list chrome the same way (see `chromelessView`).
type View = "grid" | "list" | "atlas" | "history" | "press" | "canvas";

const SINCE_SECS: Record<string, number | null> = {
  "7d": 7 * 86400,
  "30d": 30 * 86400,
  all: null,
};

// Gallery — dynamic h1 + filtered grid/list/atlas. The view toggle
// and kb-selector live in the TopBar (F2); the LeftRail owns the
// tags/capabilities/date filter UI (F2 / F4). All filter state lives
// in URL params (?view, ?kb, ?tags, ?caps, ?since) so reload + share
// links work.
export default function Gallery() {
  const { params, set } = useUrl();
  const navigate = useNavigate();
  const loc = useLocation();
  // X1 — restore the window scroll offset when returning to this exact filter
  // state (return from a detail view), keyed on the full URL so each view/
  // filter combination keeps its own position. Window-scrolled grid/list.
  useScrollRestoration(loc.pathname + loc.search);
  // N-track — contextual "Notes for <scope>" rail, toggled (default off so
  // the grid keeps full width + virtual lists don't recompute unprompted).
  const [notesOpen, setNotesOpen] = useState(false);
  // F4 — rename the active ?folder= filter via RenameFolderModal.
  const [renameFolderOpen, setRenameFolderOpen] = useState(false);
  const view = (params.get("view") as View) || "grid";
  // W3.M-d — the map-home shell (`components/MapShell.tsx`): a full-bleed
  // atlas with the gallery as the LIST PROJECTION of the map's selection.
  //
  // A URL flag, NOT a `View`: it is orthogonal to the view tabs (the tab
  // strip keeps its meaning, no tab is renamed or reordered), and reaching
  // it requires typing `?shell=map` or flipping Settings → Preferences →
  // home. That deliberate obscurity is the EVIDENCE GATE: map-home only
  // becomes the default when the local atlas census earns it (criterion
  // recorded in `api/prefs.ts`), and a prominent entry point would
  // manufacture the very evidence the census measures.
  //
  // Desktop only: on ≤860px a full-viewport map reads as a workbench and
  // fights both the mobile `.atlas` overrides and the v0.23 one-button
  // reader contract, so the flag degrades to the ordinary gallery.
  const isMobile = useIsMobile();
  const shellMap = params.get("shell") === "map" && !isMobile;
  // Views that own their whole body and are NOT a projection of the docs
  // row-set: the count/notes controls, the empty states and every result
  // strip are suppressed for them (W3.C-b added `canvas` beside `history` —
  // the grid/list virtualizer + sort/group chrome were already gated on
  // `view === "grid" || view === "list"` elsewhere, which excludes both).
  const chromelessView = view === "history" || view === "canvas";
  const kbFilter = params.get("kb") || "";
  // useSearchParams() returns a NEW URLSearchParams object every render,
  // so memoizing on `[params]` would recompute the Set every render.
  // Derive the raw string first and key the memo on it — the Set
  // identity then stays stable while the tag/cap string is unchanged,
  // so downstream useMemos (filtered, sections) don't needlessly recompute.
  const tagsStr = params.get("tags") || "";
  const capsStr = params.get("caps") || "";
  const activeTags = useMemo(
    () => new Set(tagsStr.split(",").filter(Boolean)),
    [tagsStr],
  );
  const activeCaps = useMemo(
    () => new Set(capsStr.split(",").filter(Boolean)),
    [capsStr],
  );
  const since = params.get("since") || "all";
  const activeFolder = params.get("folder");
  // v0.33 Y1 — exact-folder only; only meaningful with ?folder=.
  const folderExact = params.get("folder_exact") === "1";
  const indexOnly = params.get("index") === "1";
  const q = params.get("q") || "";
  const sort: SortKey = ((): SortKey => {
    const v = params.get("sort");
    return v === "indexed" ||
      v === "created" ||
      v === "title" ||
      v === "words"
      ? v
      : "recent";
  })();
  const dir: SortDir = ((): SortDir => {
    const v = params.get("dir");
    return v === "asc" || v === "desc" ? v : defaultDir(sort);
  })();
  const group: GroupKey = params.get("group") === "folder" ? "folder" : "none";
  // v0.22 — positive category include + absolute mtime window (the reader's
  // clickable-category and "modified around this time" deep-links land here).
  const category = params.get("category") || "";
  const parseUnixParam = (key: string): number | null => {
    const v = params.get(key);
    if (!v) return null;
    const n = Number.parseInt(v, 10);
    return Number.isFinite(n) ? n : null;
  };
  const fromUnix = parseUnixParam("from");
  const toUnix = parseUnixParam("to");
  // W1.gallery — the antilibrary read-state filter (all|never-opened|
  // in_progress|read; see Sidebar's "Reading" chip group). "" = no filter.
  const readFilter = params.get("read") || "";
  // W2.3a — csv id-set filter (the atlas lasso / working-set gallery pivot;
  // see kb_core::docs_query's `ids` gate, invariant #35). "" = no filter.
  // The UI that populates this param (lasso selection, "open as gallery
  // filter") is W2.3b's — this phase only wires the URL→hook plumbing +
  // the client-side defense-in-depth mirror below.
  const idsFilter = params.get("ids") || "";
  const activeIds = useMemo(
    () => new Set(idsFilter.split(",").filter(Boolean)),
    [idsFilter],
  );
  // W2.8 — capture-to-draft (zero-daemon version; lib/draft.ts). The draft
  // VIEW is active when the URL itself is already about drafts (the
  // Sidebar's "Drafts (N)" chip, or a hand-typed `?q=tag:draft`); everywhere
  // else the gallery composes in the default `NOT tag:draft` exclusion so
  // captured-as-draft artifacts stay off the grid until filed (tags edited).
  const draftView = isDraftView(params);

  const [kbs, setKbs] = useState<KbSummary[]>([]);
  const [docsKb, setDocsKb] = useState<string | null>(null);
  const [edges, setEdges] = useState<AtlasEdge[]>([]);
  const [error, setError] = useState<string | null>(null);
  // S8 (S-milestone) — paginated useDocs is the only path. Earlier
  // S-phases ran this behind localStorage.kbScaleMode so a rollback
  // was one localStorage flip away; with S8 the flag is gone and
  // scale-mode is the default. The server-side filter pipeline (S2)
  // handles every gallery axis (folder/tags/caps/since/index/sort);
  // the client-side filter useMemo below is left as a defense-in-depth
  // for the cases where the URL state hasn't fully synced with the
  // hook's last fetch.
  const activeKb = kbFilter || kbs[0]?.name || null;
  // X1 — remember this filtered gallery URL (+ its kb) so Detail's "back to
  // recent" returns here instead of the bare grid, restoring filters + scroll.
  //
  // W3.M-d, invariant #31 — DECISION: the map shell does NOT record itself.
  // `lastGalleryUrl` is the reader's "back to recent" target and the key
  // `useScrollRestoration` restores against; recording `/?shell=map` would
  // (a) send the reader's back button to a map instead of a list, and (b)
  // miss the scroll key entirely (the shell is a full-bleed canvas that
  // never window-scrolls, so there is no offset to restore). The last real
  // LIST the operator was on stays the return path — including the list they
  // pivot INTO from the map, which is recorded normally because that URL is
  // an ordinary `?ids=` gallery.
  useEffect(() => {
    if (shellMap) return;
    setLastGalleryUrl(loc.pathname + loc.search, activeKb);
  }, [loc.pathname, loc.search, activeKb, shellMap]);
  const scaleHook = useDocs(
    activeKb,
    {
      folder: activeFolder ?? undefined,
      // Y1 — only send when true (and folder is set); keeps the query key
      // byte-identical to pre-Y1 when the flag is off.
      ...(folderExact && activeFolder ? { folderExact: true } : {}),
      tags: activeTags.size > 0 ? [...activeTags] : undefined,
      caps: activeCaps.size > 0 ? [...activeCaps] : undefined,
      since: since !== "all" ? since : undefined,
      category: category || undefined,
      from: fromUnix ?? undefined,
      to: toUnix ?? undefined,
      indexOnly,
      sort,
      dir,
      group,
      // W3.M-d — the shell draws the same map, so it wants the same
      // atlas-shaped projection as `?view=atlas`.
      projection: view === "atlas" || shellMap ? "atlas" : "default",
      // W2.8 — the default draft exclusion rides the same `?q=` DSL the
      // server already overlays on top of the flat params (invariant #35);
      // skipped entirely in the draft view itself (drilling into
      // `?tags=draft` must still show them).
      q: draftView ? q || undefined : composeDraftExclusion(q),
      // W1.A wire — csv URL param → the string[] the fetch layer serializes;
      // the server's rollup filter is authoritative (#35).
      ...(readFilter ? { read: readFilter.split(",").filter(Boolean) } : {}),
      // W2.3a — same csv-param wiring for the id-set filter.
      ...(idsFilter ? { ids: idsFilter.split(",").filter(Boolean) } : {}),
    },
    200,
  );
  const docs = scaleHook.rows;
  const loading = scaleHook.isLoading;

  // W3.M-c — the atlas view's honest full-corpus draw. The `/atlas/points`
  // route has no filter parameters (M-a: a deliberate whole-corpus scan), so
  // it's only handed to AtlasView when NO gallery filter narrower than
  // "everything" is active — otherwise a tag/folder/date filter would look
  // like it silently stopped applying the moment you switched to the Atlas
  // tab. With a filter active, AtlasView keeps its pre-existing behaviour
  // (draw whatever page `useDocs` loaded); the status line still reports
  // that honestly via `docsTotal` below.
  const hasActiveGalleryFilter =
    !!activeFolder ||
    activeTags.size > 0 ||
    activeCaps.size > 0 ||
    since !== "all" ||
    indexOnly ||
    !!q ||
    !!category ||
    fromUnix != null ||
    toUnix != null ||
    !!readFilter ||
    !!idsFilter ||
    draftView;
  const atlasPointsQuery = useAtlasPoints(
    (view === "atlas" || shellMap) && docsKb ? docsKb : undefined,
  );
  const atlasFullPoints = hasActiveGalleryFilter
    ? undefined
    : atlasPointsQuery.data;

  // Sessions-gallery join (design: SPA-only, invariant #27). When a session
  // transcript (kb-category=memory-session) is in view, fetch the session rows
  // and key them by artifact_id (=== DocSummary.id) so each card can render the
  // readable session record instead of the timestamped filename. Gated on
  // `hasSessionDocs` so non-session corpora never fire /api/sessions.
  const hasSessionDocs = useMemo(
    () => docs.some((d) => d.kb_category === "memory-session"),
    [docs],
  );
  const sessionList = useSessions(undefined, undefined, hasSessionDocs);
  const sessionFolders = useSessionFolders(hasSessionDocs);
  // Eager-fill the join map so cards below the first session page still match.
  // Bounded (≤20 pages = 1000 sessions) to stay cheap on huge corpora; cards
  // past that fall back to the generic render.
  const sessPagesRef = useRef(0);
  useEffect(() => {
    sessPagesRef.current = 0;
  }, [hasSessionDocs]);
  useEffect(() => {
    if (
      hasSessionDocs &&
      sessionList.hasMore &&
      !sessionList.loadingMore &&
      sessPagesRef.current < 20
    ) {
      sessPagesRef.current += 1;
      sessionList.loadMore();
    }
  }, [
    hasSessionDocs,
    sessionList.hasMore,
    sessionList.loadingMore,
    sessionList.rows,
    sessionList,
  ]);
  const sessionByArtifact = useMemo(() => {
    if (!hasSessionDocs) return undefined;
    const m = new Map<string, SessionRow>();
    for (const r of sessionList.rows) m.set(r.artifact_id, r);
    return m;
  }, [hasSessionDocs, sessionList.rows]);
  // Corpus-wide rollup for the first-class sessions landing strip.
  const sessionRollup = useMemo(() => {
    if (!hasSessionDocs || sessionFolders.length === 0) return null;
    return {
      sessions: sessionFolders.reduce((a, f) => a + f.count, 0),
      folders: sessionFolders.length,
      edited: sessionFolders.reduce((a, f) => a + f.edited_total, 0),
    };
  }, [hasSessionDocs, sessionFolders]);
  // Q2 — feed the ribbon's match counter + ms timing + parser warnings.
  useReportQueryStats(
    scaleHook.ms !== undefined
      ? {
          total: scaleHook.total,
          ms: scaleHook.ms,
          warnings: scaleHook.queryWarnings,
        }
      : undefined,
  );
  // Bumped by AtlasView after a successful recompute so the docs fetch
  // re-runs and picks up the freshly-written atlas_x/y/cluster fields.
  const [recomputeTick, setRecomputeTick] = useState(0);
  // G5 — per-artifact reading-progress map (built from history). Empty
  // until the first fetch resolves; cards without a progress entry just
  // omit the chip (no '0%' noise).
  const progress = useReadingProgress(docsKb ?? undefined);

  const titlePart = useMemo(() => {
    if (activeFolder)
      return activeFolder.split("/").filter(Boolean).pop() ?? null;
    if (activeTags.size) return `tagged ${[...activeTags].join(", ")}`;
    if (category) return category;
    if (fromUnix != null || toUnix != null) return "modified around";
    if (view === "atlas") return "Atlas";
    if (view === "history") return "History";
    if (view === "canvas") return "Reflection";
    if (view === "press") return "Front Page";
    return null;
  }, [activeFolder, activeTags, category, fromUnix, toUnix, view]);
  useDocumentTitle(titlePart);

  useEffect(() => {
    const ctl = new AbortController();
    fetchKbs(ctl.signal)
      .then(setKbs)
      .catch((e) => {
        if (isAbortError(e)) return;
        setError(String(e));
      });
    return () => ctl.abort();
  }, []);

  // Track the kb name useDocs is loaded against so downstream views
  // (history, atlas-edges fetch) key off the right name.
  useEffect(() => {
    if (activeKb) setDocsKb(activeKb);
  }, [activeKb]);

  // Live filesystem updates — when an artifact is (re)indexed or
  // removed in the displayed kb, bump `recomputeTick` to re-run the
  // docs (and atlas-edges) fetch. Subscribed on [kbs, kbFilter] only —
  // not view/recomputeTick — so toggling the view doesn't tear the
  // subscription down.
  //
  // v0.7.1 P2 — leading-edge + cooldown, not a plain trailing debounce.
  // The indexer's initial walk dribbles `artifact.indexed` events out
  // over seconds (it serialises through the single-writer storage
  // actor); a trailing debounce either never settled or fired a full
  // refetch per event. This refetches immediately on the first event,
  // then at most once per COOLDOWN_MS while events keep arriving, and
  // once more after they stop.
  const fsRefetchTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const fsRefetchPending = useRef(false);
  useEffect(() => {
    if (kbs.length === 0) return;
    const target = kbFilter || kbs[0].name;
    const COOLDOWN_MS = 1500;
    const schedule = (payload: Record<string, unknown>) => {
      if (payload.kb !== target) return;
      if (fsRefetchTimer.current) {
        // In cooldown — remember a refetch is wanted when it ends.
        fsRefetchPending.current = true;
        return;
      }
      // Leading edge: refetch now, then open the cooldown window.
      // `recomputeTick` triggers the atlas-edges re-fetch — the docs row
      // set itself now refetches via the SSE bridge's ["docs", kb]
      // invalidation (TQ1), so this subscription only feeds the edges.
      setRecomputeTick((t) => t + 1);
      const tick = () => {
        if (fsRefetchPending.current) {
          fsRefetchPending.current = false;
          setRecomputeTick((t) => t + 1);
          fsRefetchTimer.current = setTimeout(tick, COOLDOWN_MS);
        } else {
          fsRefetchTimer.current = null;
        }
      };
      fsRefetchTimer.current = setTimeout(tick, COOLDOWN_MS);
    };
    const offIndexed = sse.subscribeEvent("artifact.indexed", schedule);
    const offRemoved = sse.subscribeEvent("artifact.removed", schedule);
    // Gap resync: synthesize an event for OUR kb so it passes
    // schedule's cross-kb filter and rides the same cooldown.
    const offResync = sse.onResync(() => schedule({ kb: target }));
    return () => {
      offIndexed();
      offRemoved();
      offResync();
      if (fsRefetchTimer.current) clearTimeout(fsRefetchTimer.current);
      fsRefetchTimer.current = null;
      fsRefetchPending.current = false;
    };
  }, [kbs, kbFilter]);

  // Atlas edges — only the atlas view needs them; fetched in parallel
  // with docs and re-fetched after a recompute. Failures are non-fatal:
  // the atlas still renders dots + labels without the edge layer.
  useEffect(() => {
    if (view !== "atlas" || kbs.length === 0) {
      setEdges([]);
      return;
    }
    const target = kbFilter || kbs[0].name;
    fetchAtlasEdges(target)
      .then(setEdges)
      .catch(() => setEdges([]));
  }, [kbs, kbFilter, view, recomputeTick]);

  // Defense-in-depth client-side filter — the server filters every
  // axis identically (S2), but during a URL→hook→fetch transition the
  // `docs` buffer briefly carries stale rows for the previous query.
  // Re-applying the predicates client-side prevents that flicker.
  // Sorting stays server-side: re-sorting page boundaries would corrupt
  // pagination.
  //
  // `read` is the one axis that's normally server-authoritative and NOT
  // mirrored here (the per-request read_state join isn't something the
  // client can safely re-derive) — but in THIS worktree the server-side
  // `read=` wiring isn't reachable from W1.gallery's owned files (see the
  // WIRE CAVEAT above), so this predicate is filtering client-side as a
  // working stand-in. Once api/client.ts + hooks/useDocs.ts pick up `read`,
  // this branch becomes redundant (harmless double-filtering) and should be
  // dropped.
  const filtered = useMemo(() => {
    return docs.filter((d) => {
      if (indexOnly && !isIndexPage(d)) return false;
      if (activeFolder) {
        const f = d.folder ?? "";
        if (folderExact) {
          // Y1 — exact match only (mirrors server `folder_exact=1`).
          if (f !== activeFolder) return false;
        } else if (f !== activeFolder && !f.startsWith(activeFolder + "/")) {
          return false;
        }
      }
      if (activeTags.size > 0) {
        const docTags = tagsFor(d);
        if (!docTags.some((t) => activeTags.has(t))) return false;
      }
      if (activeCaps.size > 0) {
        const has = capabilityFlags(d);
        for (const c of activeCaps) if (!has.has(c)) return false;
      }
      const sinceSecs = SINCE_SECS[since];
      if (sinceSecs != null) {
        const t = d.indexed_at_unix ?? d.mtime_unix;
        if (t == null) return false;
        const now = Date.now() / 1000;
        if (now - t > sinceSecs) return false;
      }
      // v0.22 — category exact match + absolute mtime window (mirrors the
      // server `category`/`from`/`to` gates).
      if (category && d.kb_category !== category) return false;
      if (fromUnix != null || toUnix != null) {
        const m = d.mtime_unix;
        if (m == null) return false;
        if (fromUnix != null && m < fromUnix) return false;
        if (toUnix != null && m > toUnix) return false;
      }
      // W2.8 — mirror the server's default `NOT tag:draft` exclusion
      // (composeDraftExclusion above). Tags-based, so — unlike `read` above
      // — this IS safe to re-derive client-side from row metadata; skipped
      // entirely in the draft view.
      if (!draftView && tagsFor(d).includes(DRAFT_TAG)) return false;
      // `read` is deliberately NOT mirrored client-side: read-state is
      // per-request server data (the rollup join), not row metadata — the
      // server filter is authoritative (#35), and rows the daemon already
      // filtered out never reach this memo.
      // W2.3a — unlike `read`, an id-set filter IS row metadata (`d.id`),
      // so mirroring it here is correct and cheap (same reasoning as the
      // draft-tag exclusion above) — it guards the render window between
      // a URL change and the next fetch settling. The server gate stays
      // authoritative (#35).
      if (activeIds.size > 0 && !activeIds.has(d.id)) return false;
      return true;
    });
  }, [
    docs,
    indexOnly,
    activeFolder,
    folderExact,
    activeTags,
    activeCaps,
    since,
    category,
    fromUnix,
    toUnix,
    readFilter,
    activeIds,
    draftView,
  ]);

  // v0.33 Y4 — convention folder note (`<folder>/index.md`). Still appears
  // as a normal card in the grid; this strip is a second surface, not a hide.
  const folderNote = useMemo(
    () => (activeFolder ? folderIndexNote(docs, activeFolder) : null),
    [docs, activeFolder],
  );

  // W3.M-c — whether the atlas branch has anything to draw: the loaded
  // (possibly paged) `filtered` rows, OR — when no gallery filter is active
  // — the full-corpus point set once it's arrived (so the map can render
  // before the paged `useDocs` fetch settles, not just after).
  const atlasHasRenderableData =
    filtered.length > 0 || (atlasFullPoints?.points.length ?? 0) > 0;

  const sections = useMemo(() => {
    if (group !== "folder" || view === "atlas") return null;
    return groupByFolder(filtered, sort, dir);
  }, [filtered, group, view, sort, dir]);

  // W2.6a — the roving cursor over the flat (non-grouped) grid/list. The
  // folder-grouped `sections` render path (`GallerySection`, one
  // `docs.map(Card)` per folder bucket) is honestly SKIPPED here: turning
  // its per-section local index into one flat cursor index would need a
  // running-offset prop threaded through `GallerySection.tsx`, which isn't
  // an owned file this phase — see the phase's reported concerns.
  const queryClient = useQueryClient();
  const gridRef = useRef<VirtualGridHandle>(null);
  const listRef = useRef<VirtualListHandle>(null);
  const [gridCols, setGridCols] = useState(1);
  const cursorEnabled =
    !sections && !!docsKb && (view === "grid" || view === "list") && filtered.length > 0;

  const activateFocused = useCallback(
    (index: number) => {
      const d = filtered[index];
      if (d && docsKb) navigate(artifactHref(docsKb, d.source_relative));
    },
    [filtered, docsKb, navigate],
  );
  const scrollCursorToIndex = useCallback(
    (index: number) => {
      if (view === "grid") gridRef.current?.scrollToIndex(index);
      else listRef.current?.scrollToIndex(index);
    },
    [view],
  );
  const cursor = useRovingCursor({
    rows: filtered.length,
    cols: view === "grid" ? gridCols : 1,
    onActivate: activateFocused,
    scrollToIndex: scrollCursorToIndex,
    enabled: cursorEnabled,
  });

  // Feed the focused row into the same hover-prefetch path Card.tsx's own
  // mouseenter/focus timer warms (`["doc", kb, source_relative]` via
  // `fetchDocByPath`) — Card.tsx's timer is private to that component, so
  // this re-derives the identical prefetch rather than reaching into it.
  const cursorPrefetchTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  useEffect(() => {
    if (!cursorEnabled || cursor.focusedIndex < 0 || !docsKb) return;
    const d = filtered[cursor.focusedIndex];
    if (!d) return;
    cursorPrefetchTimer.current = setTimeout(() => {
      cursorPrefetchTimer.current = null;
      void queryClient
        .prefetchQuery({
          queryKey: ["doc", docsKb, d.source_relative] as const,
          queryFn: ({ signal }) => fetchDocByPath(docsKb, d.source_relative, signal),
        })
        .catch(() => {});
    }, CURSOR_PREFETCH_DELAY_MS);
    return () => {
      if (cursorPrefetchTimer.current) {
        clearTimeout(cursorPrefetchTimer.current);
        cursorPrefetchTimer.current = null;
      }
    };
  }, [cursorEnabled, cursor.focusedIndex, filtered, docsKb, queryClient]);

  // W3.M-d — the map-home shell replaces the whole gallery body (header,
  // strips, grid) with a full-bleed map + its list projection. Placed AFTER
  // every hook above so the hook order is identical in both branches; the
  // atlas is lazy in both, sharing one Suspense fallback.
  if (shellMap && docsKb) {
    return (
      <Suspense
        fallback={
          <div className="empty" aria-busy="true">
            Loading map…
          </div>
        }
      >
        <MapShell
          kb={docsKb}
          docs={filtered}
          edges={edges}
          points={atlasFullPoints}
          docsTotal={scaleHook.total}
          onRecomputeDone={() => setRecomputeTick((t) => t + 1)}
        />
      </Suspense>
    );
  }

  return (
    <div className={notesOpen ? "gallery-shell gallery-shell--notes" : "gallery-shell"}>
    <div className="gallery">
      <header className="gallery__header">
        <h1 className="gallery-h1">
          {activeFolder ? (
            <>
              in{" "}
              <FolderBreadcrumb
                path={activeFolder}
                onNavigate={(f) => set("folder", f)}
              />
            </>
          ) : activeTags.size > 0 ? (
            <>
              tagged{" "}
              <span className="gallery-h1-accent">
                {[...activeTags].join(" + ")}
              </span>
            </>
          ) : (
            <>recent</>
          )}
        </h1>
        {/* v0.33 Y4 — compact folder-note strip (title + summary → artifact). */}
        {activeFolder && folderNote && activeKb && (
          <Link
            className="gallery-folder-note"
            to={artifactHref(activeKb, folderNote.source_relative)}
            data-kb-act="folder-note"
            title={folderNote.summary ?? folderNote.title}
          >
            <Icon.Note />
            <span className="gallery-folder-note__title">
              {folderNote.title || "folder note"}
            </span>
            {folderNote.summary ? (
              <span className="gallery-folder-note__excerpt">
                {folderNote.summary}
              </span>
            ) : null}
          </Link>
        )}
        {!chromelessView && (
          <div className="gallery__controls">
            <div className="gallery-count">
              {`${filtered.length} of ${scaleHook.total}`}
            </div>
            {activeFolder && activeKb && (
              <>
                <button
                  type="button"
                  className={`gallery-folder-exact ${folderExact ? "is-on" : ""}`}
                  onClick={() =>
                    set("folder_exact", folderExact ? null : "1")
                  }
                  aria-pressed={folderExact}
                  title={
                    folderExact
                      ? "showing this folder only — click to include subfolders"
                      : "include subfolders — click for this folder only"
                  }
                  data-kb-act="folder-exact"
                >
                  this folder only
                </button>
                <button
                  type="button"
                  className="gallery-folder-rename"
                  onClick={() => setRenameFolderOpen(true)}
                  title={`rename folder ${activeFolder}`}
                  aria-label="rename folder"
                  data-kb-act="rename-folder"
                >
                  rename
                </button>
                <a
                  className="gallery-download"
                  href={folderDownloadUrl(activeKb, activeFolder)}
                  download
                  title={`download all files in ${activeFolder} (.zip)`}
                  aria-label="download folder as zip"
                >
                  <Icon.Download />
                </a>
              </>
            )}
            {/* Sort + group lifted into the Band-2 context line (C1). */}
            <button
              type="button"
              className={`gallery-notes-toggle ${notesOpen ? "is-on" : ""}`}
              onClick={() => setNotesOpen((o) => !o)}
              aria-pressed={notesOpen}
              title="notes for the current folder / kb"
            >
              <Icon.Note /> Notes
            </button>
          </div>
        )}
      </header>

      {/* Corpus lobby — the generous, EMPTY-QUERY home for this kb (census
          line, category bar, highlights, example searches). Stricter gate
          than the strips below: any active query/filter/read facet means
          intent, and the lobby's whole point is the moment before intent —
          it also keeps its highlight cards out of filtered/sorted card
          assertions (the grid alone answers a filter). W2.8: this gate
          implies `!draftView` (activeTags is empty, `q` is falsy), so
          `docs`/`scaleHook.total` below already went through
          composeDraftExclusion server-side — drafts drop out with no
          extra handling needed here. */}
      {!activeFolder &&
        activeTags.size === 0 &&
        activeCaps.size === 0 &&
        !q &&
        !category &&
        !readFilter &&
        since === "all" &&
        fromUnix == null &&
        toUnix == null &&
        sort === "recent" &&
        (view === "grid" || view === "list") &&
        docsKb && (
          <GalleryLobby
            kb={docsKb}
            total={scaleHook.total}
            rows={docs}
            progress={progress}
          />
        )}

      {/* First-class sessions landing — a rollup strip + a jump to the full
          /sessions worklog, shown when the corpus holds captured transcripts
          and the gallery isn't narrowed to a folder/tag. */}
      {sessionRollup &&
        !activeFolder &&
        activeTags.size === 0 &&
        (view === "grid" || view === "list") && (
          <Link
            to={`/sessions?kb=${encodeURIComponent(activeKb ?? "")}`}
            className="gallery-sessions-strip"
            title="open the full sessions worklog (folders, threads, decisions)"
          >
            <span className="gallery-sessions-strip__lead">◆ Sessions</span>
            <span className="gallery-sessions-strip__stats">
              {sessionRollup.sessions.toLocaleString()} session
              {sessionRollup.sessions === 1 ? "" : "s"}
              {" · "}
              {sessionRollup.folders} folder
              {sessionRollup.folders === 1 ? "" : "s"}
              {sessionRollup.edited > 0 &&
                ` · ${sessionRollup.edited.toLocaleString()} files edited`}
            </span>
            <span className="gallery-sessions-strip__go">
              open worklog →
            </span>
          </Link>
        )}

      {/* Resurface — pull-only "pick up where you left off" strip (top 2
          queue items: open comments + unfinished reads). Same gating as the
          sessions strip; renders nothing when the queue is empty. */}
      {!activeFolder &&
        activeTags.size === 0 &&
        (view === "grid" || view === "list") && (
          <ResurfaceStrip kb={activeKb ?? null} />
        )}

      {/* On-this-day + session echoes (W2.2) — a calm second strip, same
          gate as resurface above; renders nothing when there's nothing to
          show. */}
      {!activeFolder &&
        activeTags.size === 0 &&
        (view === "grid" || view === "list") && (
          <EchoStrip kb={activeKb ?? null} />
        )}

      {error && (
        <div className="gallery__error" role="alert">
          {error}
        </div>
      )}
      {loading && <Loading icon={<Icon.Grid />} variant="inline" />}

      {/* v0.7.x — featured README/index block. Only shown on grid/list
          views (atlas + history have their own primary content); also
          hidden when filters reduce the result set to zero, since
          IndexHero filters off the same `filtered` list the grid uses
          (a folder/tag filter that excludes the index just hides it). */}
      {(view === "grid" || view === "list") && docsKb && filtered.length > 0 && (
        <PinnedIndexCard docs={filtered} kb={docsKb} />
      )}

      {/* Antilibrary framing (calm-computing contract: a reserve, never a
          backlog — no percentage, no progress bar, just the plain count).
          `filtered.length` is the best available count in this worktree —
          see the WIRE CAVEAT above on why `scaleHook.total` (the spec'd
          server-authoritative envelope total) isn't wired yet here. */}
      {(view === "grid" || view === "list") &&
        readFilter === "never-opened" &&
        !loading && (
          <div className="gallery-antilibrary-note" role="status">
            research reserve — {filtered.length.toLocaleString()} never-opened
            artifact{filtered.length === 1 ? "" : "s"}
          </div>
        )}

      {/* W2.8 — the draft view's one-line header (see lib/draft.ts). Calm
          framing — a staging shelf, not a backlog nudge; no count target,
          just the plain count (`scaleHook.total` is authoritative here —
          `tags` is a server-filtered axis, unlike `read` above). */}
      {(view === "grid" || view === "list") && draftView && !loading && (
        <div className="gallery-draft-note" role="status">
          drafts — captured, not yet filed · {scaleHook.total.toLocaleString()}{" "}
          draft{scaleHook.total === 1 ? "" : "s"}
        </div>
      )}

      {!chromelessView &&
        !loading &&
        !error &&
        filtered.length === 0 &&
        docs.length === 0 && (
          <EmptyState
            icon={<Icon.Grid />}
            title="no artifacts yet"
            hint="Drop a self-contained HTML or Markdown file into this kb's folder, add one from the CLI, or set up another knowledge base in Settings."
            action={{ label: "Add a knowledge base", to: "/settings#config" }}
            cli="kb add <path>"
          />
        )}

      {!chromelessView &&
        !loading &&
        !error &&
        filtered.length === 0 &&
        docs.length > 0 && (
          <EmptyState
            icon={<Icon.Search />}
            title="no matches"
            hint="Nothing matches the active filters — clear a tag, folder, or date in the rail to widen the search."
          />
        )}

      {view === "grid" && docsKb && filtered.length > 0 && (
        sections ? (
          // S7 — sections rendering. Server returns rows sorted by
          // (folder ASC, primary sort) when `group=folder`, so
          // groupByFolder() runs idempotently and we can render
          // section-per-section. An explicit load-more button drives
          // pagination (no scroll sentinel inside grouped sections).
          <div className="gallery-sections" aria-label={`${filtered.length} artifacts in ${docsKb}, grouped by folder`}>
            {sections.map((s) => (
              <GallerySection key={s.folder} folder={s.folder} docs={s.docs} kb={docsKb} view="grid" progress={progress} sessions={sessionByArtifact} />
            ))}
            {scaleHook.hasMore && (
              <button
                type="button"
                className="gallery-load-more"
                onClick={scaleHook.loadMore}
                disabled={scaleHook.isLoading}
              >
                {scaleHook.isLoading ? "loading…" : "load more"}
              </button>
            )}
          </div>
        ) : (
          <VirtualGrid
            ref={gridRef}
            docs={filtered}
            kb={docsKb}
            progress={progress}
            sessions={sessionByArtifact}
            onEnd={scaleHook.loadMore}
            hasMore={scaleHook.hasMore}
            focusedIndex={cursor.focusedIndex}
            onColsChange={setGridCols}
          />
        )
      )}
      {view === "list" && docsKb && filtered.length > 0 && (
        sections ? (
          <div className="gallery-sections gallery-sections--list" aria-label={`${filtered.length} artifacts in ${docsKb}, grouped by folder`}>
            {sections.map((s) => (
              <GallerySection key={s.folder} folder={s.folder} docs={s.docs} kb={docsKb} view="list" progress={progress} />
            ))}
            {scaleHook.hasMore && (
              <button
                type="button"
                className="gallery-load-more"
                onClick={scaleHook.loadMore}
                disabled={scaleHook.isLoading}
              >
                {scaleHook.isLoading ? "loading…" : "load more"}
              </button>
            )}
          </div>
        ) : (
          <VirtualList
            ref={listRef}
            docs={filtered}
            kb={docsKb}
            onEnd={scaleHook.loadMore}
            hasMore={scaleHook.hasMore}
            focusedIndex={cursor.focusedIndex}
          />
        )
      )}
      {/* W2.5 — the broadsheet home. Renders unconditionally on docsKb (no
          filtered.length gate) so the masthead's census line is honest even
          at zero rows — "the broadsheet IS a projection of the current
          filter set". The generic EmptyState above already covers the
          zero-artifact case for every non-history view. */}
      {view === "press" && docsKb && (
        <Broadsheet
          rows={filtered}
          kb={docsKb}
          docCount={kbs.find((k) => k.name === docsKb)?.doc_count ?? null}
        />
      )}
      {view === "atlas" && docsKb && atlasHasRenderableData && (
        <Suspense
          fallback={
            <div className="empty" aria-busy="true">
              Loading atlas…
            </div>
          }
        >
          <AtlasView
            docs={filtered}
            kb={docsKb}
            edges={edges}
            points={atlasFullPoints}
            docsTotal={scaleHook.total}
            onRecomputeDone={() => setRecomputeTick((t) => t + 1)}
          />
        </Suspense>
      )}
      {view === "history" && (() => {
        // History view doesn't depend on docs being loaded — derive the
        // kb directly from the filter / first configured kb so the view
        // renders immediately on a cold load.
        const kbForHistory = docsKb ?? kbFilter ?? kbs[0]?.name ?? null;
        return kbForHistory ? (
          <>
            {/* W3.C-b — the canvas is the density read of the SAME days this
                list enumerates; `set("view", …)` keeps every other param, so
                the two are one toggle rather than two destinations. */}
            <button
              type="button"
              className="rcanvas__crosslink"
              onClick={() => set("view", "canvas")}
            >
              the same days as four density tracks →
            </button>
            <HistoryTimeline kb={kbForHistory} />
          </>
        ) : null;
      })()}
      {view === "canvas" && (() => {
        // W3.C-b — same cold-load kb derivation as the history view above:
        // the canvas reads /timeline directly and never waits on a docs page
        // (its own brush→gallery pivot is what needs docs, and that happens
        // AFTER a navigation).
        const kbForCanvas = docsKb ?? kbFilter ?? kbs[0]?.name ?? null;
        return kbForCanvas ? (
          <>
            <ReflectionCanvas kb={kbForCanvas} />
            <button
              type="button"
              className="rcanvas__crosslink"
              onClick={() => set("view", "history")}
            >
              the same days as a history list →
            </button>
          </>
        ) : null;
      })()}
    </div>
    {notesOpen && (
      <aside className="gallery-notes-rail">
        <NotesPanel
          kb={activeKb}
          folder={activeFolder}
          onOpenNote={(k, i) =>
            navigate(
              `/notes?focus=${encodeURIComponent(k)}:${encodeURIComponent(i)}`,
            )
          }
        />
      </aside>
    )}
    {renameFolderOpen && activeFolder && activeKb && (
      <RenameFolderModal
        kb={activeKb}
        from={activeFolder}
        onClose={() => setRenameFolderOpen(false)}
        onRenamed={(to) => {
          // #35 — always galleryUrl; preserve other axes already in the URL.
          navigate(
            galleryUrl(activeKb, {
              folder: to,
              // Preserve exact-folder when renaming so the filter intent sticks.
              ...(folderExact ? { folderExact: true } : {}),
              tags: activeTags.size > 0 ? [...activeTags] : undefined,
              category: category || undefined,
              from: fromUnix,
              to: toUnix,
              sort: sort as GallerySort,
              dir,
            }),
          );
        }}
      />
    )}
    </div>
  );
}

function capabilityFlags(d: DocSummary): Set<string> {
  const set = new Set<string>();
  if (d.svg_count && d.svg_count > 0) set.add("svg");
  if (
    d.has_canvas ||
    d.has_form ||
    d.has_animation ||
    d.has_details ||
    d.has_drag
  ) {
    set.add("interactive");
  }
  if (d.code_block_count && d.code_block_count > 0) set.add("code");
  if (d.longread) set.add("longread");
  return set;
}
