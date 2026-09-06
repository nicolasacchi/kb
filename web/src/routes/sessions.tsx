import { useEffect, useMemo, useRef } from "react";
import { Link, useLocation, useNavigate, useSearchParams } from "react-router-dom";
import { useScrollRestoration } from "../hooks/useScrollRestoration";
import { useQuery } from "@tanstack/react-query";
import {
  useSessionPresence,
  useSessions,
  useSessionDetail,
  useSessionFolders,
  useSessionProjects,
  useSessionRecollect,
  useSessionThreads,
} from "../hooks/useSessions";
import { useLiveSessions } from "../hooks/useLiveSessions";
import LiveSessionsNowBand from "../components/LiveSessionsNowBand";
import type { PresenceLiveSet } from "../lib/sessionPresence";
import { useDocumentTitle } from "../hooks/useDocumentTitle";
import { useIsMobile } from "../hooks/useIsMobile";
import { useBodyScrollLock } from "../hooks/useBodyScrollLock";
import { useNowTick } from "../hooks/useNowTick";
import { useReportQueryStats } from "../components/chrome/queryStats";
import EmptyState from "../components/EmptyState";
import { Icon } from "../components/icons";
import {
  fetchResearchRollup,
  fetchSession,
  fetchSessionFunnel,
  fetchSessionLedger,
  fetchSessionMemories,
  fetchSessionTouches,
  saveThreadAsList,
  type CommitOut,
  type DecisionOut,
  type ProjectOut,
  type RecollectSessionOut,
  type ResearchOut,
  type SessionCommentsResponse,
  type SessionFileOut,
  type SessionRow,
  type ThreadOut,
} from "../api/sessions";
import { useState } from "react";
import { sessionDisplayName } from "../lib/sessionDisplayName";
import { triggerDownload, sessionExportUrl } from "../lib/download";
import { dayBucket, dayHeading, humanizeDuration } from "../lib/time";
import {
  replayUrl,
  sessionsProjectsHomeUrl,
  sessionsProjectUrl,
  sessionsWorklogUrl,
} from "../lib/sessionsUrl";
import {
  commitBadge,
  decisionBadge,
  errorBadge,
  harnessGlyph,
  memoryBadge,
  outcomeLine,
  subagentBadge,
  substanceBadge,
} from "../lib/sessionChips";
import { sessionPresence, presenceSummary, formatPresenceSummary } from "../lib/sessionPresence";
import {
  coldSeedSessionsView,
  loadLastSessionsView,
  saveLastSessionsView,
  loadHideSessionsTrivial,
  saveHideSessionsTrivial,
} from "../api/prefs";
import { artifactHref } from "../lib/artifactHref";
import { groupIdsByKb } from "../lib/idsPivot";
import IdsGalleryChip from "../components/IdsGalleryChip";
import CitationText from "../components/CitationText";
import { useCodeUrlForKb } from "../hooks/useCodeUrlForKb";

// v0.14 S8 — hover-prefetch delay. Long enough that a fast cursor
// sweep across the list doesn't fire N parallel fetches; short enough
// that a deliberate hover-then-click feels instant.
const PREFETCH_DELAY_MS = 300;

// v0.14 S5 — /sessions view. Mirrors /memory's three-column shell but
// swaps the salience-ranked table for a chronological list; sessions
// are inherently time-ordered, not rank-ordered. The right rail shows
// detail for the selected session (memories produced + touched
// artifacts), lazy-loaded.
export default function SessionsRoute() {
  const [params, setParams] = useSearchParams();
  const isMobile = useIsMobile();
  const location = useLocation();
  // W3.D/S2 — scroll restoration (invariant #31), with the ephemeralParams
  // amendment: `?focus=` (S2's mobile-sheet trigger) is declared so a row
  // tap doesn't fragment the list's ONE scroll position into N per-focus
  // slots, and a back-nav to bare `/sessions` restores it. W7/LF-2 adds
  // `?follow=1` to the same set (KEY CONSTRAINTS #31): follow-mode toggles
  // are reader/list view state, not a distinct scrollable view.
  useScrollRestoration(location.pathname + location.search, ["focus", "follow"]);
  // A1 — `folder=` is LEGACY-inbound-only (R12/C-17): the SPA never emits it
  // (sessionsUrl.ts never writes it), but an old bookmark/link keeps working.
  // W3.A — `project=` is the blessed P1-derived-key scoping axis this
  // builder emits.
  const folder = params.get("folder") ?? "";
  const project = params.get("project") ?? "";
  const query = params.get("q") ?? "";
  // W3.A/S1 — substance triage. Per this wave's build order: hiding trivial
  // captures defaults OFF (every session shown, including un-backfilled
  // NULL-substance rows — never hidden by omission); the control below
  // toggles it ON, which narrows to `routine,substantive`.
  const urlSubstance = params.get("substance");
  const hideTrivial = urlSubstance === "routine,substantive" || (urlSubstance === null && loadHideSessionsTrivial());
  // W5/I — the harness facet: `?harness=` csv, single-select in the UI
  // (multi-select is a straightforward csv extension if ever needed).
  const harness = params.get("harness") ?? "";
  // P7/W3.C — view mode: chronological list (default), narrative threads, or
  // the projects home.
  const rawView = params.get("view");
  const view: "list" | "threads" | "projects" =
    rawView === "threads" ? "threads" : rawView === "projects" ? "projects" : "list";

  // D3 — a TRULY bare `/sessions` (no query string at all) promotes to the
  // remembered view (projects home, if the operator chose it before) — any
  // deep link (even just `?q=`) is left exactly where it points. One-shot
  // via a mount-only effect + ref guard, the `coldSeedKb`/`coldSeedShell`
  // shape.
  const seeded = useRef(false);
  useEffect(() => {
    if (seeded.current) return;
    seeded.current = true;
    const promoted = coldSeedSessionsView({
      search: window.location.search,
      lastSessionsView: loadLastSessionsView(),
    });
    if (promoted) setParams(new URLSearchParams({ view: promoted }), { replace: true });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const { rows, loading, loadingMore, error, loadMore, hasMore } = useSessions(
    folder || undefined,
    query || undefined,
    true,
    project || undefined,
    hideTrivial ? "routine,substantive" : undefined,
    harness || undefined,
  );
  // W7 (R15/LF-1) — Tier-1 presence, polled while this page is mounted (the
  // "/sessions page" half of LF-1's "ONLY from /sessions or an open session
  // page" rule). Unconfigured/non-loopback daemons just report
  // `enabled:false` — presenceSet stays empty, every row falls back to its
  // Tier-0 chip untouched.
  const { presenceSet } = useSessionPresence(true);
  // LSC-4 — the Now band's live-status read (invariant #23's fifth
  // documented exception, hooks/useLiveSessions.ts). Always on: the whole
  // point is that the operator doesn't have to switch view/filters to see
  // it, and it's the SAME cached query the Header's WaitingChip reads (one
  // shared poll, not two).
  const { rows: liveRows } = useLiveSessions();
  // The Header's "N waiting" chip deep-links to
  // `/sessions#kb-now-waiting`. A hash on its own only auto-scrolls on a
  // fresh full navigation; an in-app SPA nav (or an already-open /sessions
  // tab) needs an explicit scroll — cheap, so it's done here rather than
  // skipped as "too expensive" (design §7). Re-runs when `liveRows` lands
  // so a hash that arrived before the band had data still resolves once it
  // does.
  useEffect(() => {
    if (location.hash !== "#kb-now-waiting") return;
    document.getElementById("kb-now-waiting")?.scrollIntoView({ block: "nearest" });
  }, [location.hash, liveRows]);
  // Shared wall clock for presence age labels (one tick / 30s, not per-row).
  const now = useNowTick(30_000);
  const listPresenceSummary = useMemo(
    () => presenceSummary(rows, now, presenceSet),
    [rows, presenceSet, now],
  );
  const folders = useSessionFolders();
  const activeFolder = folder ? folders.find((f) => f.folder === folder) : null;
  const { threads, loading: threadsLoading } = useSessionThreads(
    view === "threads",
  );
  // Always enabled (not gated on `view === "projects"`): the project PILL
  // (shown in every view) needs the label for the currently-scoped project,
  // not just the projects-home view. Cheap — one aggregate request, same
  // class as `useSessionFolders()` above.
  const { projects, loading: projectsLoading } = useSessionProjects();
  // W5/I — the set of harnesses actually present anywhere in the corpus
  // (union of every project's harness_mix keys), used ONLY to decide
  // whether the harness filter select is worth showing at all.
  const harnessSet = useMemo(() => {
    const s = new Set<string>();
    for (const p of projects) {
      for (const h of Object.keys(p.harness_mix)) s.add(h);
    }
    return s;
  }, [projects]);

  const setView = (next: "list" | "threads" | "projects") => {
    const p = new URLSearchParams(params);
    if (next !== "list") p.set("view", next);
    else p.delete("view");
    setParams(p, { replace: true });
    // D3 — persist ONLY the projects/list toggle (threads isn't part of the
    // remembered-landing axis).
    if (next === "projects" || next === "list") saveLastSessionsView(next);
  };

  const setHideTrivial = (next: boolean) => {
    const p = new URLSearchParams(params);
    if (next) p.set("substance", "routine,substantive");
    else p.delete("substance");
    setParams(p, { replace: true });
    // F2 — persist the user's choice to the preference
    saveHideSessionsTrivial(next);
  };

  const selectProject = (next: string) => {
    const p = new URLSearchParams(params);
    if (next) p.set("project", next);
    else p.delete("project");
    p.delete("focus");
    if (view === "projects") p.set("view", "list");
    setParams(p, { replace: true });
  };

  // P6 polish — debounce the search box: keep the input snappy (local `draft`)
  // but only commit to the URL ?q (which refetches /api/sessions) ~250ms after
  // the user stops typing. `searching` flags the in-flight gap for a hint.
  const [draft, setDraft] = useState(query);
  const searchTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  useEffect(() => {
    // Sync the input when ?q changes from outside (back/forward, folder switch).
    setDraft(query);
  }, [query]);
  // Cancel any pending commit on unmount, so navigating away (e.g. save-thread
  // → /lists) can't be clobbered by a trailing setParams from this route.
  useEffect(
    () => () => {
      if (searchTimer.current) clearTimeout(searchTimer.current);
    },
    [],
  );
  const onSearchChange = (val: string) => {
    setDraft(val);
    if (searchTimer.current) clearTimeout(searchTimer.current);
    const next = val.trim();
    searchTimer.current = setTimeout(() => {
      // Commit off the LIVE url at fire time, not a render-time closure: a
      // late debounce must (a) not yank the user back if they've navigated off
      // /sessions, and (b) preserve any view/folder set since (e.g. threads).
      if (!window.location.pathname.startsWith("/sessions")) return;
      const p = new URLSearchParams(window.location.search);
      if ((p.get("q") ?? "") === next) return;
      if (next) p.set("q", next);
      else p.delete("q");
      p.delete("focus");
      setParams(p, { replace: true });
    }, 250);
  };
  const searching = draft.trim() !== query;
  // P5 — two-lane search, lane B ("deep search" over the R1 digests). Lane A
  // is the existing metadata `q` box above (SQL LIKE, unchanged); lane B
  // fires the SAME committed `query` (not the live `draft`, so it shares one
  // debounce) through `/sessions/recollect`, scoped to the current project,
  // only in list view (threads/projects have their own search story) and
  // only past the 3-char gate (enforced inside the hook too).
  const { hits: deepHits, loading: deepLoading } = useSessionRecollect(
    query,
    project || undefined,
    view === "list",
  );
  useDocumentTitle("Sessions");
  useReportQueryStats(
    !loading && !error
      ? { total: rows.length, ms: 0, warnings: [] }
      : undefined,
  );

  const focused = params.get("focus");
  const selected = rows.find((r) => r.session_id === focused) ?? null;

  const selectFolder = (next: string) => {
    const p = new URLSearchParams(params);
    if (next) p.set("folder", next);
    else p.delete("folder");
    p.delete("focus"); // a focused session may not exist in the new folder
    setParams(p, { replace: true });
  };

  // W5/I — the harness facet: only shown when the corpus actually mixes
  // harnesses (per-project `harness_mix`, W3's `/sessions/projects` data) —
  // stays quiet for the common all-claude case, mirroring the CLI's/
  // folders-rollup's own "only surface when mixed" rule.
  const selectHarness = (next: string) => {
    const p = new URLSearchParams(params);
    if (next) p.set("harness", next);
    else p.delete("harness");
    p.delete("focus");
    setParams(p, { replace: true });
  };
  const {
    detail,
    memories,
    recalls,
    readings,
    files,
    decisions,
    commits,
    research,
    comments,
    loading: detailLoading,
  } = useSessionDetail(selected?.session_id ?? null);

  const select = (sid: string | null) => {
    const next = new URLSearchParams(params);
    if (sid) next.set("focus", sid);
    else next.delete("focus");
    setParams(next, { replace: true });
  };

  // S2 — Esc dismisses the mobile bottom sheet (mirrors detail.tsx's
  // reader-tools-sheet Esc handler exactly): only mounts while the sheet is
  // actually open (mobile + a row focused), skips modifier chords and typing
  // targets so it never eats an input/textarea/composer keystroke.
  useEffect(() => {
    if (!isMobile || !focused) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      if (e.metaKey || e.ctrlKey || e.altKey) return;
      const t = e.target as HTMLElement | null;
      if (
        t &&
        (t.tagName === "INPUT" || t.tagName === "TEXTAREA" || t.isContentEditable)
      ) {
        return;
      }
      e.preventDefault();
      select(null);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [isMobile, focused]);

  // SH.B2 — lock body scroll while the mobile session-details sheet is up.
  // Gated on `selected` (not just `focused`) to match the sheet's own
  // render condition below (`isMobile && selected`) — a `?focus=` pointing
  // at a not-yet-loaded/unmatched row renders no sheet, so nothing to lock.
  useBodyScrollLock(isMobile && !!selected);

  // v0.14 S8 — warm the browser cache for the hovered session so the
  // detail flash on click is instant. Per-id timer guards against
  // accidental fire on a fast cursor sweep across the list.
  const prefetched = useRef<Set<string>>(new Set());
  const hoverTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const prefetch = (sid: string) => {
    if (prefetched.current.has(sid)) return;
    if (hoverTimer.current) clearTimeout(hoverTimer.current);
    hoverTimer.current = setTimeout(() => {
      prefetched.current.add(sid);
      // Fire-and-forget — these calls populate the browser HTTP cache
      // so the useSessionDetail fetch on real click hits warm.
      void fetchSession(sid).catch(() => {});
      void fetchSessionMemories(sid).catch(() => {});
      void fetchSessionTouches(sid).catch(() => {});
    }, PREFETCH_DELAY_MS);
  };
  const cancelPrefetch = () => {
    if (hoverTimer.current) {
      clearTimeout(hoverTimer.current);
      hoverTimer.current = null;
    }
  };

  // v0.14 T3 — day-bucket the rows so the list has the same
  // chronological "Today / Yesterday / Month dd, yyyy" stripes the
  // History timeline uses. Rows are already started_at-DESC sorted
  // by the daemon's cross-kb fan-out; preserving that order inside
  // each bucket keeps the visual rhythm consistent.
  const bucketed = useMemo(() => {
    const groups = new Map<string, SessionRow[]>();
    for (const r of rows) {
      const key = dayBucket(r.started_at);
      const arr = groups.get(key);
      if (arr) arr.push(r);
      else groups.set(key, [r]);
    }
    return Array.from(groups.entries())
      .sort((a, b) => (a[0] < b[0] ? 1 : a[0] > b[0] ? -1 : 0))
      .map(([bucket, rs]) => ({ bucket, rows: rs }));
  }, [rows]);

  const activeProject = project
    ? projects.find((p) => p.project === project)
    : null;

  return (
    <div className="kb-ses" data-testid="sessions-view">
      <main className="kb-ses__main">
        <div className="kb-ses__head">
          <h1>sessions</h1>
          {/* W3.C/P6 — the project pill: dims to "click to pin" outside a
              project scope (the #33 kb-select precedent), opens projects home
              when clicked. Independent of the view toggle below (a project
              can be pinned while browsing list OR threads). A real `Link`
              (built via the ONE sessionsUrl.ts builder, R12) so it's
              ctrl/cmd-clickable like every other nav affordance. */}
          <Link
            to={sessionsProjectsHomeUrl()}
            className={`kb-ses__project-pill${project ? " is-pinned" : ""}`}
            onClick={() => saveLastSessionsView("projects")}
            data-testid="sessions-project-pill"
            title="browse projects"
          >
            {activeProject?.label ?? (project || "all projects")}
          </Link>
          {project && (
            <button
              type="button"
              className="kb-ses__project-pill-clear"
              onClick={() => selectProject("")}
              title="clear project scope"
              aria-label="clear project scope"
              data-testid="sessions-project-clear"
            >
              <Icon.X />
            </button>
          )}
          <div className="kb-ses__head-meta">
            {folders.length > 0 && !project && (
              <label className="kb-ses__folder-facet">
                <span className="kb-ses__folder-facet-label">folder</span>
                <select
                  className="kb-ses__folder-select"
                  data-testid="sessions-folder-filter"
                  value={folder}
                  onChange={(e) => selectFolder(e.target.value)}
                >
                  <option value="">all folders</option>
                  {folders.map((f) => (
                    <option key={f.folder} value={f.folder}>
                      {f.label} ({f.count})
                    </option>
                  ))}
                </select>
              </label>
            )}
            {/* W5/I — harness facet: only worth a control when the corpus
                actually mixes harnesses (quiet in the common all-claude
                case, same rule as the CLI/folders-rollup per-harness
                suffix). */}
            {harnessSet.size > 1 && (
              <label className="kb-ses__harness-facet">
                <span className="kb-ses__harness-facet-label">harness</span>
                <select
                  className="kb-ses__harness-select"
                  data-testid="sessions-harness-filter"
                  value={harness}
                  onChange={(e) => selectHarness(e.target.value)}
                >
                  <option value="">all harnesses</option>
                  {[...harnessSet].sort().map((h) => (
                    <option key={h} value={h}>
                      {harnessGlyph(h)} {h}
                    </option>
                  ))}
                </select>
              </label>
            )}
            {/* P6 — sessions-scoped keyword search (lane A, metadata). */}
            <input
              type="search"
              className="kb-ses__search"
              data-testid="sessions-search"
              placeholder="search sessions…"
              value={draft}
              onChange={(e) => onSearchChange(e.target.value)}
            />
            {/* W3.A/S1 — husk triage: hidden (never applied) by default;
                toggling ON narrows to routine+substantive. NULL substance
                (un-backfilled rows) always counts as substantive server-side,
                so this control can never hide un-backfilled history. */}
            <label className="kb-ses__hide-trivial" title="hide trivial (empty) captures">
              <input
                type="checkbox"
                checked={hideTrivial}
                onChange={(e) => setHideTrivial(e.target.checked)}
                data-testid="sessions-hide-trivial"
              />
              hide trivial
            </label>
            {/* P7/W3.C — view toggle: chronological list ↔ narrative threads
                ↔ projects home. */}
            <div className="kb-ses__viewtoggle" role="group" aria-label="view">
              <button
                type="button"
                className={view === "list" ? "is-active" : ""}
                onClick={() => setView("list")}
                data-testid="sessions-view-list"
              >
                list
              </button>
              <button
                type="button"
                className={view === "threads" ? "is-active" : ""}
                onClick={() => setView("threads")}
                data-testid="sessions-view-threads"
              >
                threads
              </button>
              <button
                type="button"
                className={view === "projects" ? "is-active" : ""}
                onClick={() => setView("projects")}
                data-testid="sessions-view-projects"
              >
                projects
              </button>
            </div>
            <span>
              {searching
                ? "searching…"
                : view === "threads"
                  ? `${threads.length} ${threads.length === 1 ? "thread" : "threads"}`
                  : view === "projects"
                    ? `${projects.length} ${projects.length === 1 ? "project" : "projects"}`
                    : `${rows.length} ${rows.length === 1 ? "session" : "sessions"}`}
            </span>
          </div>
        </div>

        {/* A-w2 — the shipped-but-uncalled activity-funnel + research-rollup
            endpoints (R9), surfaced as one quiet header line. Scoped by the
            folder filter (legacy) AND/OR the project filter (W3.A). */}


        {/* LF-1/S2 — presence strip: "N live · M active" counts from visible
            rows. Derived from the rows + presenceSet, shown only in list view
            when there's visibility into the presence data. */}
        {view === "list" && rows.length > 0 && (
          <div className="kb-ses__presence-strip" data-testid="sessions-presence-strip">
            {formatPresenceSummary(listPresenceSummary)}
          </div>
        )}        {view !== "projects" && (
          <SessionsActivityStrip
            folder={folder}
            project={project}
            onSelectFolder={selectFolder}
          />
        )}

        {/* P6 — per-project timeline header: the selected folder's rollups. */}
        {/* W3.C/P6 — the project-scoped header upgrades to the project card's
            own rollup (commit/error/harness signals the plain folder facet
            never had); `activeFolder` stays the fallback for a legacy
            `folder=` link with no resolved project. */}
        {activeProject ? (
          <div className="kb-ses__project" data-testid="sessions-project-stats">
            <strong className="kb-ses__project-name" title={activeProject.project}>
              {activeProject.label}
            </strong>
            <span className="kb-ses__project-stats">
              {activeProject.count} sessions ·{" "}
              {projectSpan(activeProject.earliest, activeProject.latest)} ·{" "}
              {activeProject.edited_total} files edited ·{" "}
              {formatTokens(activeProject.token_total)} tok
              {commitBadge(activeProject.commit_total) && (
                <> · {commitBadge(activeProject.commit_total)}</>
              )}
              {errorBadge(activeProject.error_sessions) && (
                <> · {errorBadge(activeProject.error_sessions)}</>
              )}
            </span>
          </div>
        ) : (
          activeFolder && (
            <div className="kb-ses__project" data-testid="sessions-project-stats">
              <strong className="kb-ses__project-name" title={activeFolder.folder}>
                {activeFolder.label}
              </strong>
              <span className="kb-ses__project-stats">
                {activeFolder.count} sessions ·{" "}
                {projectSpan(activeFolder.earliest, activeFolder.latest)} ·{" "}
                {activeFolder.edited_total} files edited ·{" "}
                {formatTokens(activeFolder.token_total)} tok
              </span>
            </div>
          )
        )}

        {error && (
          <div className="kb-ses__empty kb-ses__empty--err">
            failed to load: {error}
          </div>
        )}

        {/* W3.C/P6 — the projects home: a card per registry entry /
            auto-project, entered via the view toggle or the project pill. */}
        {view === "projects" && (
          <ProjectsHomeView
            projects={projects}
            loading={projectsLoading}
            rows={rows}
            presenceSet={presenceSet}
            now={now}
          />
        )}

        {/* P7 — narrative threads view. */}
        {view === "threads" && (
          <ThreadsView
            threads={threads}
            loading={threadsLoading}
            focused={focused}
            onSelect={select}
            onPrefetch={prefetch}
            onCancelPrefetch={cancelPrefetch}
            presenceSet={presenceSet}
            now={now}
          />
        )}

        {view === "list" && (
          <>
            {/* LSC-4 — the Now band: three live lanes above the historical
                day-buckets below. Independent of `rows`/`loading` (a live
                signal, not a page over history) — it renders nothing on its
                own when the fleet is quiet, per design §7. */}
            <LiveSessionsNowBand rows={liveRows} now={now} />
            {loading && <div className="kb-ses__empty">loading…</div>}
            {!loading && !error && rows.length === 0 && (
              <EmptyState
                icon={<Icon.Terminal />}
                title="no sessions yet"
                hint="Sessions appear once a transcript lands in a memory-scope corpus (the kb-capture Stop hook)."
              />
            )}
            {!loading && rows.length > 0 && (
              <>
                <div className="kb-ses__buckets" aria-label="captured sessions">
                  {bucketed.map((group) => (
                    <section className="kb-ses__bucket" key={group.bucket}>
                      <h3 className="kb-ses__bucket-head">
                        {dayHeading(group.bucket)}
                        <span className="kb-ses__bucket-count">
                          {group.rows.length}
                        </span>
                      </h3>
                      <ol className="kb-ses__list">
                        {group.rows.map((r) => (
                          <SessionListRow
                            key={`${r.kb}:${r.artifact_id}`}
                            row={r}
                            isSelected={focused === r.session_id}
                            onSelect={() => select(r.session_id)}
                            onPrefetch={() => prefetch(r.session_id)}
                            onCancelPrefetch={cancelPrefetch}
                            presenceSet={presenceSet}
                            now={now}
                          />
                        ))}
                      </ol>
                    </section>
                  ))}
                </div>
                {hasMore && (
                  <div className="kb-ses__more">
                    <button
                      type="button"
                      className="kb-ses__more-btn"
                      onClick={loadMore}
                      disabled={loadingMore}
                      data-testid="sessions-load-more"
                    >
                      {loadingMore ? "loading…" : "load older sessions"}
                    </button>
                  </div>
                )}
              </>
            )}
            {/* P5 — two-lane search, lane B: "in content — ranked". Only past
                the 3-char gate; labelled distinctly from lane A above so a
                hit's provenance (metadata match vs. digest match) is never
                ambiguous. */}
            {query.trim().length >= 3 && (
              <DeepSearchResults
                hits={deepHits}
                loading={deepLoading}
                focused={focused}
                onSelect={select}
              />
            )}
          </>
        )}
      </main>

      {/* S2 — desktop DOM byte-identical to pre-W3 (this <aside> renders
          unconditionally, same as before); on mobile it is REPLACED by the
          bottom sheet below (a fixed-position sibling, not this column). */}
      {!isMobile && (
        <aside className="kb-ses__rail" aria-label="session details">
          {!selected && (
            <div className="kb-ses__rail-empty">
              select a session to see produced memories + touched artifacts.
            </div>
          )}
          {selected && (
            <SessionInspector
              row={selected}
              detail={detail}
              memories={memories}
              recalls={recalls}
              readings={readings}
              files={files}
              decisions={decisions}
              commits={commits}
              research={research}
              comments={comments}
              loading={detailLoading}
            />
          )}
        </aside>
      )}

      {/* S2 — mobile bottom sheet, the v0.23 asSheet+scrim precedent
          (PreviewInspector/detail.tsx): sheet visibility IS `?focus=`
          present, so a shared `/sessions?focus=X` link opens straight into
          it (URL truth, #23 ethos — no second "is the sheet open" state). */}
      {isMobile && selected && (
        <>
          <div
            className="kb-pinsp-scrim is-open"
            onClick={() => select(null)}
            aria-hidden
          />
          <SessionInspector
            row={selected}
            detail={detail}
            memories={memories}
            recalls={recalls}
            readings={readings}
            files={files}
            decisions={decisions}
            commits={commits}
            research={research}
            comments={comments}
            loading={detailLoading}
            asSheet
            onMobileClose={() => select(null)}
          />
        </>
      )}
    </div>
  );
}

// W3.C/P6 — the projects home: card per registry entry / auto-project.
// Empty-registry cold start still reads useful (auto-projects, labelled by
// basename). Click → scopes the whole page to that project + switches to
// list view (`selectProject`, above).
function ProjectsHomeView({
  projects,
  loading,
  rows,
  presenceSet,
  now,
}: {
  projects: ProjectOut[];
  loading: boolean;
  rows: SessionRow[];
  presenceSet: PresenceLiveSet;
  now: number;
}) {
  if (loading) return <div className="kb-ses__empty">loading…</div>;
  if (projects.length === 0) {
    return (
      <EmptyState
        icon={<Icon.Terminal />}
        title="no projects yet"
        hint="Projects appear once a captured session resolves a working directory. Declare `[projects.*]` in kb.toml to name + merge them."
      />
    );
  }
  return (
    <div className="kb-ses__projects" aria-label="projects home" data-testid="sessions-projects-home">
      {projects.map((p) => {
        const harnessEntries = Object.entries(p.harness_mix).sort((a, b) => b[1] - a[1]);
        return (
          <div key={p.project} className="kb-ses__project-card">
            {/* W6 (moonshots M4) — the ledger toggle below is a SIBLING of
                this Link, never nested inside it: an <a> containing another
                interactive element is invalid HTML (browsers split the
                anchor at the nested control), so the click-through-to-
                project affordance and the ledger affordance live at the same
                level inside one bordered wrapper. */}
            <Link
              className="kb-ses__project-card-link"
              to={sessionsProjectUrl(p.project)}
              data-testid="session-project-card"
            >
              <span className="kb-ses__project-card-head">
                <strong>{p.label}</strong>
                <span className={`kb-ses__project-card-src kb-ses__project-card-src--${p.source}`}>
                  {p.source === "registry" ? "declared" : "auto"}
                </span>
              </span>
              <span className="kb-ses__project-card-stats">
                {p.count} sessions · {projectSpan(p.earliest, p.latest)}
                {/* LF-1/S2 — project card presence strip: reuse the same presenceSummary
                    logic for live/active counts from project-scoped sessions. */}
                {(() => {
                  const projectRows = rows.filter((r) => r.project_key === p.project);
                  const summary = presenceSummary(projectRows, now, presenceSet);
                  return summary.live > 0 || summary.active > 0
                    ? ` · ${formatPresenceSummary(summary)}`
                    : "";
                })()}
              </span>
              <span className="kb-ses__project-card-stats">
                {commitBadge(p.commit_total) && <span>{commitBadge(p.commit_total)}</span>}
                {errorBadge(p.error_sessions) && (
                  <span title={`${p.error_sessions} sessions with errors`}>
                    {errorBadge(p.error_sessions)}
                  </span>
                )}
                {p.edited_total} files edited · {formatTokens(p.token_total)} tok
              </span>
              {harnessEntries.length > 0 && (
                <span className="kb-ses__project-card-harness">
                  {harnessEntries.map(([h, n]) => (
                    <span key={h} title={`${h}: ${n}`}>
                      {harnessGlyph(h)} {n}
                    </span>
                  ))}
                </span>
              )}
            </Link>
            <ProjectLedgerSection project={p.project} />
          </div>
        );
      })}
    </div>
  );
}

// W6 (moonshots M4) — a per-project "Ledger" disclosure on the projects
// home card: collapsed by default (no fetch until expanded — N project
// cards must not fire N eager queries on page load), then a compact
// day-grouped list (last 7 days, quiet days omitted) reusing sessionChips'
// vocabulary and linking rows via sessionsUrl (`sessionsWorklogUrl`). Lives
// as a SIBLING of the card's `<Link>` (see the comment above), never nested
// inside it.
const LEDGER_DAYS = 7;

function ProjectLedgerSection({ project }: { project: string }) {
  const [expanded, setExpanded] = useState(false);
  const query = useQuery({
    queryKey: ["sessions", "ledger", project, LEDGER_DAYS] as const,
    queryFn: ({ signal }) => fetchSessionLedger(project, LEDGER_DAYS, signal),
    staleTime: Infinity,
    enabled: expanded,
  });
  const activeDays = (query.data?.days_out ?? []).filter(
    (d) => d.sessions.length > 0 || d.commits.length > 0 || d.decisions_count > 0,
  );
  return (
    <div className="kb-ses__project-ledger">
      <button
        type="button"
        className="kb-ses__project-ledger-toggle"
        aria-expanded={expanded}
        data-testid="project-ledger-toggle"
        onClick={() => setExpanded((v) => !v)}
      >
        {expanded ? "▾" : "▸"} ledger · last {LEDGER_DAYS}d
      </button>
      {expanded && (
        <div className="kb-ses__project-ledger-body" data-testid="project-ledger-body">
          {query.isLoading && <span className="kb-ses__empty">loading…</span>}
          {!query.isLoading && activeDays.length === 0 && (
            <span className="kb-ses__empty">no captures this week</span>
          )}
          {activeDays.length > 0 && (
            <ul className="kb-ses__project-ledger-days">
              {activeDays.map((d) => (
                <li key={d.date} className="kb-ses__project-ledger-day">
                  <span className="kb-ses__project-ledger-date">{d.date}</span>
                  <span className="kb-ses__project-ledger-sessions">
                    {d.sessions.map((s) => (
                      <Link
                        key={s.sid}
                        to={sessionsWorklogUrl(s.sid, project)}
                        className="kb-ses__project-ledger-session"
                        title={s.outcome ?? s.display_name}
                      >
                        {s.display_name}
                      </Link>
                    ))}
                    {commitBadge(d.commits.length) && (
                      <span title={`${d.commits.length} commits`}>
                        {commitBadge(d.commits.length)}
                      </span>
                    )}
                    {decisionBadge(d.decisions_count) && (
                      <span title={`${d.decisions_count} decisions`}>
                        {decisionBadge(d.decisions_count)}
                      </span>
                    )}
                  </span>
                </li>
              ))}
            </ul>
          )}
        </div>
      )}
    </div>
  );
}

// P5 — lane B of the two-lane search: "in content — ranked". Each hit shows
// why it matched (the digest excerpt) + the same substance/staleness signals
// `kb recollect` surfaces on the CLI, never buried.
function DeepSearchResults({
  hits,
  loading,
  focused,
  onSelect,
}: {
  hits: RecollectSessionOut[];
  loading: boolean;
  focused: string | null;
  onSelect: (sid: string) => void;
}) {
  if (!loading && hits.length === 0) return null;
  return (
    <section className="kb-ses__deep" aria-label="in content — ranked" data-testid="sessions-deep-search">
      <h3 className="kb-ses__deep-head">in content — ranked</h3>
      {loading && <div className="kb-ses__empty">searching…</div>}
      {!loading && (
        <ol className="kb-ses__list">
          {hits.map((h) => (
            <li
              key={`${h.kb}:${h.session_id}`}
              className={`kb-ses__row${focused === h.session_id ? " is-selected" : ""}`}
              data-testid="session-deep-hit"
            >
              <button
                type="button"
                className="kb-ses__row-btn"
                onClick={() => onSelect(h.session_id)}
              >
                <span className="kb-ses__row-line1">
                  <span className="kb-ses__row-name">{h.display_name}</span>
                  {h.stale && <span className="kb-ses__row-stale">stale</span>}
                </span>
                {h.summary && (
                  <span className="kb-ses__row-preview">{h.summary}</span>
                )}
                <span className="kb-ses__row-stats">
                  {h.folder && <span className="kb-ses__row-folder">{h.folder}</span>}
                  <span title="match score">score {h.score.toFixed(2)}</span>
                  <span>{h.age_days}d ago</span>
                  {commitBadge(h.commit_count) && <span>{commitBadge(h.commit_count)}</span>}
                  {errorBadge(h.error_count) && <span>{errorBadge(h.error_count)}</span>}
                </span>
              </button>
            </li>
          ))}
        </ol>
      )}
    </section>
  );
}

// A-w2 (wave-1 wire-don't-build) — the activity funnel + research-rollup
// endpoints (R9) shipped server-side with no SPA caller. One quiet header
// line: the funnel's real stages in their own order (searched → opened →
// edited → committed → commented, per FunnelStageOut) plus a distinct
// research-topic count. Both queries ride the SAME `folder` filter the
// sessions list already exposes (the dropdown above), so switching folders
// re-scopes the strip for free — no new filter axis. None of the per-stage
// numbers has a matching filter in the sessions list to click into (there's
// no `?stage=`), so they render as plain text; the one number that DOES
// have an existing filter to anchor into is the top contributing folder
// behind the research count (shown only in the unscoped view), which reuses
// the existing folder-select filter via `onSelectFolder`. Calm: no cards, no
// charts, hidden entirely when there's nothing to show (mirrors
// ResurfaceStrip's anti-feature contract).
function SessionsActivityStrip({
  folder,
  project,
  onSelectFolder,
}: {
  folder: string;
  project: string;
  onSelectFolder: (folder: string) => void;
}) {
  const funnelQuery = useQuery({
    queryKey: ["sessions", "funnel", folder || undefined, project || undefined] as const,
    queryFn: ({ signal }) =>
      fetchSessionFunnel(folder || undefined, signal, project || undefined),
    staleTime: Infinity,
  });
  // limit=50 (the server's max top-N, sessions.rs::ROLLUP_MAX_TOP_N) rather
  // than its default 5 — the strip only needs a COUNT + the top folder, so
  // asking for the fullest available per-folder slice keeps that count a
  // closer approximation of "distinct topics" than the UI-display default.
  const rollupQuery = useQuery({
    queryKey: [
      "sessions",
      "research-rollup",
      folder || undefined,
      50,
      project || undefined,
    ] as const,
    queryFn: ({ signal }) =>
      fetchResearchRollup(folder || undefined, 50, signal, project || undefined),
    staleTime: Infinity,
  });

  const stages = funnelQuery.data?.stages ?? [];
  const rollupFolders = rollupQuery.data ?? [];
  const researchTopics = rollupFolders.reduce((n, f) => n + f.queries.length, 0);
  const topFolder =
    !folder && !project && rollupFolders.length > 0
      ? [...rollupFolders].sort(
          (a, b) =>
            b.queries.reduce((n, q) => n + q.count, 0) -
            a.queries.reduce((n, q) => n + q.count, 0),
        )[0]
      : null;

  if (stages.length === 0 && researchTopics === 0) return null;

  return (
    <div className="kb-ses__funnel" data-testid="sessions-funnel">
      {stages.map((s, i) => (
        <span key={s.stage} className="kb-ses__funnel-stage">
          {i > 0 && <span className="kb-ses__funnel-sep">·</span>}
          <b>{s.sessions.toLocaleString()}</b> {s.stage}
        </span>
      ))}
      {researchTopics > 0 && (
        <span className="kb-ses__funnel-stage">
          {stages.length > 0 && <span className="kb-ses__funnel-sep">·</span>}
          <b>{researchTopics.toLocaleString()}</b>{" "}
          {researchTopics === 1 ? "research topic" : "research topics"}
          {topFolder && (
            <>
              {" "}
              (top:{" "}
              <button
                type="button"
                className="kb-ses__funnel-link"
                onClick={() => onSelectFolder(topFolder.folder)}
                title={`filter sessions to ${topFolder.folder}`}
              >
                {topFolder.label}
              </button>
              )
            </>
          )}
        </span>
      )}
    </div>
  );
}

// P8 — "save as list": materialise a thread into an editable kb-list/1 list,
// then jump to it. Disabled while saving; shows a transient confirmation.
//
// CT-E5 — this button now asks for the NARRATIVE list: each session expands
// into capture → files touched → memories produced → memories recalled, and
// the sessions run OLDEST-first (the thread payload is newest-first for the
// feed; a story runs forwards). Still ONE button, one home for the action
// (#30) — the flat shape stays reachable on the route/CLI for callers that
// pinned it.
function SaveThreadButton({ thread }: { thread: ThreadOut }) {
  const navigate = useNavigate();
  const [state, setState] = useState<"idle" | "saving">("idle");
  const onSave = async (e: React.MouseEvent) => {
    e.stopPropagation();
    if (state === "saving" || thread.sessions.length === 0) return;
    setState("saving");
    const kb = thread.sessions[0].kb;
    const ids = thread.sessions.map((s) => s.artifact_id).reverse();
    const res = await saveThreadAsList(
      kb,
      thread.title || thread.label,
      ids,
      true,
    );
    setState("idle");
    if (res)
      navigate(
        `/lists/${encodeURIComponent(res.kb)}/${encodeURIComponent(res.list_id)}`,
      );
  };
  return (
    <button
      type="button"
      className="kb-ses__thread-save"
      onClick={onSave}
      disabled={state === "saving"}
      data-testid="thread-save-as-list"
      title="Save this thread's story as an editable reading list — capture, files touched, memories produced, memories recalled"
    >
      {state === "saving" ? "saving…" : "save as list"}
    </button>
  );
}

// P7 — the narrative-threads view: each thread is a folder + time-contiguous
// run of sessions, newest thread first, sessions newest-first within.
function ThreadsView({
  threads,
  loading,
  focused,
  onSelect,
  onPrefetch,
  onCancelPrefetch,
  presenceSet,
  now,
}: {
  threads: ThreadOut[];
  loading: boolean;
  focused: string | null;
  onSelect: (sid: string) => void;
  onPrefetch: (sid: string) => void;
  onCancelPrefetch: () => void;
  presenceSet: PresenceLiveSet;
  now: number;
}) {
  if (loading) return <div className="kb-ses__empty">loading…</div>;
  if (threads.length === 0)
    return (
      <EmptyState
        icon={<Icon.Terminal />}
        title="no threads yet"
        hint="Threads cluster sessions in the same folder that ran close together in time."
      />
    );
  return (
    <div className="kb-ses__threads" aria-label="narrative threads">
      {threads.map((t) => (
        <section
          className="kb-ses__thread"
          key={`${t.folder}:${t.started_at}`}
          data-testid="session-thread"
        >
          <h3 className="kb-ses__thread-head">
            <span className="kb-ses__thread-title" title={t.folder}>
              {t.title}
            </span>
            <span className="kb-ses__thread-meta">
              {t.label} · {t.count} session{t.count === 1 ? "" : "s"} ·{" "}
              {projectSpan(t.started_at, t.ended_at)}
              <SaveThreadButton thread={t} />
            </span>
          </h3>
          <ol className="kb-ses__list">
            {t.sessions.map((r) => (
              <SessionListRow
                key={`${r.kb}:${r.artifact_id}`}
                row={r}
                isSelected={focused === r.session_id}
                onSelect={() => onSelect(r.session_id)}
                onPrefetch={() => onPrefetch(r.session_id)}
                onCancelPrefetch={onCancelPrefetch}
                presenceSet={presenceSet}
                now={now}
              />
            ))}
          </ol>
        </section>
      ))}
    </div>
  );
}

function SessionListRow({
  row,
  isSelected,
  onSelect,
  onPrefetch,
  onCancelPrefetch,
  presenceSet,
  now,
}: {
  row: SessionRow;
  isSelected: boolean;
  onSelect: () => void;
  onPrefetch: () => void;
  onCancelPrefetch: () => void;
  presenceSet: PresenceLiveSet;
  now: number;
}) {
  // W3.C/S1 — the outcome-first row: line 2 is the session's CLOSURE
  // (`outcome`), `»`-prefixed so it reads distinctly from the pre-backfill
  // fallback (bare `first_user_prompt`, unprefixed — visible degradation,
  // never silent). The chip cluster (commits/errors/memories/subagents) +
  // the harness glyph + the `∅` husk badge are ONE vocabulary
  // (`sessionChips.ts`), shared with the gallery card (S7) and the reader
  // rail (S4).
  const preview = outcomeLine(row.outcome, row.first_user_prompt);
  const husk = substanceBadge(row.substance);
  // W5/R9b/LF-1 — derived staleness/presence dot: Tier 0 is a static,
  // honestly-labelled chip; W7 upgrades to Tier 1 (writer-activity
  // EVIDENCE, `presenceSet` from `GET /api/sessions/presence`) when the
  // row's `session_id` is in the live set — ONLY Tier 1 gets the pulsing
  // dot (LF-1: "the pulse is reserved for evidence of a writer").
  const presence = sessionPresence(row, now, presenceSet);
  return (
    <li
      className={`kb-ses__row${isSelected ? " is-selected" : ""}${husk ? " kb-ses__row--husk" : ""}`}
      data-testid="session-row"
      data-session-id={row.session_id}
      onMouseEnter={onPrefetch}
      onMouseLeave={onCancelPrefetch}
    >
      <button
        type="button"
        className="kb-ses__row-btn"
        onClick={onSelect}
        aria-pressed={isSelected}
      >
        {/* A2/A3 — two lines: bold readable name, dimmed outcome/prompt. */}
        <span className="kb-ses__row-line1">
          <span className="kb-ses__row-time">
            {formatStarted(row.started_at)}
          </span>
          <span
            className="kb-ses__row-harness"
            title={row.harness}
            aria-hidden
          >
            {harnessGlyph(row.harness)}
          </span>
          <span className="kb-ses__row-name" title={row.first_user_prompt}>
            {sessionDisplayName(row)}
          </span>
          <span className="kb-ses__row-chips">
            {commitBadge(row.commit_count) && (
              <span title={`${row.commit_count} commits`}>
                {commitBadge(row.commit_count)}
              </span>
            )}
            {errorBadge(row.error_count) && (
              <span title={`${row.error_count} errors`}>
                {errorBadge(row.error_count)}
              </span>
            )}
            {memoryBadge(row.memory_count) && (
              <span title={`${row.memory_count} memories`}>
                {memoryBadge(row.memory_count)}
              </span>
            )}
            {subagentBadge(row.subagent_count) && (
              <span title="subagent work">{subagentBadge(row.subagent_count)}</span>
            )}
            {husk && (
              <span title="trivial capture — no real content" className="kb-ses__row-husk-badge">
                {husk}
              </span>
            )}
            {presence.status !== "idle" && (
              <>
                <span
                  className={`kb-ses__row-presence${presence.status === "live" ? " kb-ses__row-presence--live" : ""}`}
                  title={presence.copy}
                  data-testid="session-presence-chip"
                  data-presence-status={presence.status}
                >
                  ●
                </span>
                {/* LF-2 — the live badge is clickable: a sibling link to the session
                    artifact with ?follow=1. The link is a sibling overlay (not nested
                    inside the button), so it can be independent. Keyboard accessible via
                    Tab+Enter from the row button, or direct Ctrl-click. */}
                {presence.status === "live" && (
                  <Link
                    to={artifactHref(row.kb, row.source_relative, { follow: true })}
                    className="kb-ses__row-presence-link"
                    data-testid="session-live-badge-link"
                    title="go to session with follow-mode active"
                  />
                )}
              </>
            )}
          </span>
        </span>
        {preview && (
          <span className="kb-ses__row-preview">
            {preview.isOutcome && <span className="kb-ses__row-outcome-mark">» </span>}
            {preview.text}
          </span>
        )}
        <span className="kb-ses__row-stats">
          {row.folder && (
            <span className="kb-ses__row-folder" title={row.cwd ?? row.folder}>
              {row.folder}
            </span>
          )}
          <span title="messages in transcript">{row.message_count} msg</span>
          {row.files_edited_count > 0 && (
            <span title="files edited">{row.files_edited_count} edited</span>
          )}
          {row.memory_count > 0 && (
            <span className="kb-ses__row-mem" title="memories produced">
              {row.memory_count} mem
            </span>
          )}
          {row.duration_ms > 0 && (
            <span title="duration">{humanizeDuration(row.duration_ms)}</span>
          )}
          <span className="kb-ses__row-kb">{row.kb}</span>
        </span>
      </button>
    </li>
  );
}

// A4/A6 — the file-activity manifest. In-corpus files link to the artifact;
// out-of-corpus files (no kb mount) show the plain path with a copy button.
// Noise (build dirs, lockfiles) is folded behind a "show N more".
function FilesTouchedSection({
  files,
  loading,
  sessionEndedAt,
}: {
  files: SessionFileOut[];
  loading: boolean;
  // CT-F6 — the session's own end instant (`SessionRow.ended_at`, already
  // on the wire). Each in-corpus touch gets an "as it stood" link to that
  // artifact AT this moment, so a session's file list reads as the corpus
  // looked WHEN the session closed rather than as it looks today. 0 for a
  // degenerate/crashed capture — no link is offered rather than a link to
  // the epoch.
  sessionEndedAt: number;
}) {
  const edited = files.filter((f) => f.action !== "read");
  const read = files.filter((f) => f.action === "read");
  // CT-B3 — every in-corpus touch pivots into the gallery via the shipped
  // `?ids=` atom (invariant #35); grouped by kb since a chip only links
  // into ONE corpus (one per kb when a session spans more than one).
  const pivotGroups = groupIdsByKb(
    files
      .filter((f) => f.in_corpus && f.kb && f.target_artifact_id)
      .map((f) => ({ kb: f.kb as string, id: f.target_artifact_id as string })),
  );
  return (
    <section className="kb-ses__ins-section">
      <h3>Files touched</h3>
      {loading && <p className="kb-ses__ins-loading">loading…</p>}
      {!loading && files.length === 0 && (
        <p className="kb-ses__ins-empty">
          none — this session didn't read or edit any files
        </p>
      )}
      {!loading && pivotGroups.length > 0 && (
        <div className="kb-ses__pivot-row" data-testid="session-files-gallery-pivot">
          {pivotGroups.map((g) => (
            <IdsGalleryChip
              key={g.kb}
              kb={g.kb}
              ids={g.ids}
              label={
                pivotGroups.length > 1
                  ? `view ${g.ids.length} in ${g.kb}`
                  : `view ${g.ids.length} artifact${g.ids.length === 1 ? "" : "s"} in gallery`
              }
              testId={`session-files-gallery-chip-${g.kb}`}
            />
          ))}
        </div>
      )}
      {!loading && edited.length > 0 && (
        <>
          <p className="kb-ses__ins-grouplabel">edited / created ({edited.length})</p>
          <ul className="kb-ses__ins-list kb-ses__files">
            {edited.map((f, i) => fileItem(f, i, sessionEndedAt))}
          </ul>
        </>
      )}
      {!loading && read.length > 0 && (
        <>
          <p className="kb-ses__ins-grouplabel">read ({read.length})</p>
          <ul className="kb-ses__ins-list kb-ses__files">
            {read.map((f, i) => fileItem(f, i, sessionEndedAt))}
          </ul>
        </>
      )}
    </section>
  );
}

function fileItem(f: SessionFileOut, i: number, sessionEndedAt: number) {
  const inCorpus =
    f.in_corpus && f.kb && f.source_relative
      ? {
          kb: f.kb,
          // CT-F6 — through the ONE artifact URL builder (invariant #35's
          // one-builder rule), replacing the hand-rolled `/a/<kb>/<rel>`
          // string this used to assemble. Byte-identical output for the
          // plain case (the builder's goldens pin it), and the `?at=`
          // variant below can only exist if it goes through here.
          to: artifactHref(f.kb, f.source_relative),
          // The same artifact AT the instant the session closed: the
          // versions panel opens on the version that stood then (or says,
          // honestly, that nothing is that old).
          asOfEnd:
            sessionEndedAt > 0
              ? artifactHref(f.kb, f.source_relative, {
                  panel: "versions",
                  at: sessionEndedAt,
                })
              : null,
          label: f.title || f.basename,
        }
      : null;
  return (
    <li key={`${f.action}:${f.path}:${i}`} className="kb-ses__file">
      <span
        className={`kb-ses__file-act kb-ses__file-act--${f.action}`}
        title={f.action}
      >
        {f.action[0].toUpperCase()}
      </span>
      {inCorpus ? (
        <Link to={inCorpus.to} title={f.path}>
          {inCorpus.label}
        </Link>
      ) : (
        <code className="kb-ses__file-path" title={f.path}>
          {f.basename}
        </code>
      )}
      {/* V0026/W0.5 — this touch was recovered from a subagent's own sidecar
          transcript, not the main thread. */}
      {f.via_subagent && (
        <span
          className="kb-ses__ins-list-meta"
          title="touched by a delegated subagent, not the main thread"
        >
          subagent
        </span>
      )}
      {/* CT-F6 — LAST in the row (and `margin-left:auto`) so it's the
          rightmost affordance and never displaces the existing chrome. */}
      {inCorpus?.asOfEnd && (
        <Link
          to={inCorpus.asOfEnd}
          className="kb-ses__file-asof"
          data-testid="session-file-asof"
          title="open this artifact as it stood when the session ended — resolves to the nearest version at or before that moment"
        >
          as it stood
        </Link>
      )}
    </li>
  );
}

// S9 — the decisions log: every steering moment (AskUserQuestion answer or
// plan approval) in transcript order. Hidden when the session made none.
function DecisionsSection({
  decisions,
  loading,
  kb,
}: {
  decisions: DecisionOut[];
  loading: boolean;
  // CT-B5 — the session's OWN kb, to linkify a sha/path citation a decision's
  // prompt/answer prose might mention (e.g. "fixed in a1b2c3d") into kb-code.
  kb: string;
}) {
  // Called unconditionally before the early return below (Rules of Hooks).
  const codeUrl = useCodeUrlForKb(kb);
  if (loading || decisions.length === 0) return null;
  return (
    <section className="kb-ses__ins-section">
      <h3>Decisions · {decisions.length}</h3>
      <dl className="kb-ses__decisions">
        {decisions.map((d, i) => (
          <div key={i} className={`kb-ses__decision kb-ses__decision--${d.kind}`}>
            <dt>
              {d.kind === "plan" ? (
                "✓ plan approved"
              ) : (
                <CitationText text={d.prompt} codeUrl={codeUrl} />
              )}
            </dt>
            {d.answer && (
              <dd>
                <CitationText text={d.answer} codeUrl={codeUrl} />
              </dd>
            )}
          </div>
        ))}
      </dl>
    </section>
  );
}

// P5 — the commits this session produced. Hidden when none.
function CommitsSection({
  commits,
  loading,
}: {
  commits: CommitOut[];
  loading: boolean;
}) {
  if (loading || commits.length === 0) return null;
  return (
    <section className="kb-ses__ins-section">
      <h3>Commits · {commits.length}</h3>
      <ul className="kb-ses__ins-list kb-ses__commits">
        {commits.map((c, i) => (
          <li key={i} className="kb-ses__commit">
            {c.sha && <code className="kb-ses__commit-sha">{c.sha}</code>}
            <span className="kb-ses__commit-kind">{c.kind}</span>
            {c.subject && (
              <span className="kb-ses__commit-subject">{c.subject}</span>
            )}
          </li>
        ))}
      </ul>
    </section>
  );
}

const RESEARCH_LABELS: Record<string, string> = {
  kb_search: "kb",
  web: "web",
  skill: "skill",
  subagent: "subagent",
  plan_span: "plan",
  artifact_open: "open",
};

// R4 — what the session researched / explored (kb & web searches, subagents,
// skills, plan spans). Hidden when empty.
function ResearchSection({
  research,
  loading,
}: {
  research: ResearchOut[];
  loading: boolean;
}) {
  if (loading || research.length === 0) return null;
  return (
    <section className="kb-ses__ins-section" data-testid="session-research">
      <h3>Research · {research.length}</h3>
      <ul className="kb-ses__ins-list kb-ses__research">
        {research.map((r, i) => (
          <li
            key={i}
            className={`kb-ses__research-row kb-ses__research-row--${r.kind}`}
          >
            <span className="kb-ses__research-kind">
              {RESEARCH_LABELS[r.kind] ?? r.kind}
            </span>
            <span className="kb-ses__research-q" title={r.query}>
              {r.query}
            </span>
          </li>
        ))}
      </ul>
    </section>
  );
}

// R5 — open review comments on the in-corpus artifacts this session touched.
// Hidden when there are none.
function SessionCommentsSection({
  comments,
  loading,
}: {
  comments: SessionCommentsResponse;
  loading: boolean;
}) {
  if (loading || comments.total === 0) return null;
  return (
    <section className="kb-ses__ins-section" data-testid="session-comments">
      <h3>Open comments · {comments.total}</h3>
      {comments.artifacts.map((a) => (
        <div key={`${a.kb}:${a.artifact_id}`} className="kb-ses__cmt-group">
          <span className="kb-ses__cmt-artifact" title={a.title}>
            {a.title} <span className="kb-ses__cmt-act">{a.action}</span>
          </span>
          <ul className="kb-ses__ins-list">
            {a.comments.map((c) => (
              <li key={c.comment_id} className="kb-ses__cmt">
                <span className="kb-ses__cmt-author">{c.author}</span>
                <span className="kb-ses__cmt-body">{c.body}</span>
                {c.file_label && (
                  <span className="kb-ses__cmt-file">{c.file_label}</span>
                )}
              </li>
            ))}
          </ul>
        </div>
      ))}
    </section>
  );
}

// S10 — a deterministic, LLM-free "resume context" hand-off: the goal, branch,
// edited files, decisions, and what shipped (commits), formatted as a copyable
// block so a fresh session can pick up where this one left off.
function ResumeSection({
  row,
  files,
  decisions,
  commits,
}: {
  row: SessionRow;
  files: SessionFileOut[];
  decisions: DecisionOut[];
  commits: CommitOut[];
}) {
  const [copied, setCopied] = useState(false);
  const edited = files.filter((f) => f.action !== "read");
  const text = buildResume(row, edited, decisions, commits);
  const copy = () => {
    void navigator.clipboard?.writeText(text).then(
      () => {
        setCopied(true);
        setTimeout(() => setCopied(false), 1500);
      },
      () => {},
    );
  };
  return (
    <section className="kb-ses__ins-section">
      <h3>
        Resume context
        <button
          type="button"
          className="kb-ses__resume-copy"
          onClick={copy}
          data-testid="session-resume-copy"
        >
          {copied ? "copied" : "copy"}
        </button>
      </h3>
      <pre className="kb-ses__resume">{text}</pre>
    </section>
  );
}

function buildResume(
  row: SessionRow,
  edited: SessionFileOut[],
  decisions: DecisionOut[],
  commits: CommitOut[],
): string {
  const lines: string[] = [];
  lines.push(`Resuming: ${sessionDisplayName(row)}`);
  if (row.first_user_prompt) lines.push(`Goal: ${row.first_user_prompt}`);
  const where = [row.cwd, row.git_branch && `branch ${row.git_branch}`]
    .filter(Boolean)
    .join(" · ");
  if (where) lines.push(where);
  if (edited.length > 0) {
    lines.push("", `Edited (${edited.length}):`);
    for (const f of edited.slice(0, 20)) lines.push(`  - ${f.path}`);
    if (edited.length > 20) lines.push(`  … ${edited.length - 20} more`);
  }
  if (decisions.length > 0) {
    lines.push("", "Decisions:");
    for (const d of decisions)
      lines.push(
        d.kind === "plan"
          ? "  - plan approved"
          : `  - ${d.prompt} → ${d.answer ?? "?"}`,
      );
  }
  if (commits.length > 0) {
    lines.push("", `Committed (${commits.length}):`);
    for (const c of commits)
      lines.push(`  - ${c.sha ?? "-------"} ${c.subject ?? c.kind}`);
  }
  return lines.join("\n");
}

// The literal terminal command to resume this Claude Code session. Distinct
// from the LLM-free "resume context" summary above — this hands the exact
// session id to `claude -r` so you continue the real conversation. The id is
// the transcript's authoritative `sessionId` (recovered full, not the truncated
// capture-meta).
function ResumeCommand({ sessionId }: { sessionId: string }) {
  const [copied, setCopied] = useState(false);
  const cmd = `claude -r ${sessionId}`;
  const copy = () => {
    void navigator.clipboard?.writeText(cmd).then(
      () => {
        setCopied(true);
        setTimeout(() => setCopied(false), 1500);
      },
      () => {},
    );
  };
  return (
    <div className="kb-ses__resume-cmd" title="resume this session in your terminal">
      <span className="kb-ses__resume-cmd-prompt">$</span>
      <code className="kb-ses__resume-cmd-text">{cmd}</code>
      <button
        type="button"
        className="kb-ses__resume-copy"
        onClick={copy}
        data-testid="session-resume-cmd-copy"
      >
        {copied ? "copied" : "copy"}
      </button>
      <button
        type="button"
        className="kb-ses__resume-copy"
        onClick={() => triggerDownload(sessionExportUrl(sessionId))}
        data-testid="session-download-bundle"
        title="Download a portable .kbsession bundle — rehydrate it on another machine with `kb sessions rehydrate` to resume this session"
      >
        bundle
      </button>
    </div>
  );
}

function SessionInspector(props: {
  row: SessionRow;
  detail: ReturnType<typeof useSessionDetail>["detail"];
  memories: ReturnType<typeof useSessionDetail>["memories"];
  recalls: ReturnType<typeof useSessionDetail>["recalls"];
  readings: ReturnType<typeof useSessionDetail>["readings"];
  files: ReturnType<typeof useSessionDetail>["files"];
  decisions: ReturnType<typeof useSessionDetail>["decisions"];
  commits: ReturnType<typeof useSessionDetail>["commits"];
  research: ReturnType<typeof useSessionDetail>["research"];
  comments: ReturnType<typeof useSessionDetail>["comments"];
  loading: boolean;
  // S2 — mobile bottom-sheet mode (the v0.23 asSheet precedent). `false`/
  // absent (desktop) keeps the exact pre-W3 DOM (plain `<div>`, no dialog
  // semantics) — e2e at 1280×720 sees a byte-identical tree.
  asSheet?: boolean;
  onMobileClose?: () => void;
}) {
  const {
    row,
    detail,
    memories,
    recalls,
    readings,
    files,
    decisions,
    commits,
    research,
    comments,
    loading,
    asSheet = false,
    onMobileClose,
  } = props;
  const transcriptHref = `/a/${encodeURIComponent(row.kb)}/${row.source_relative
    .split("/")
    .map(encodeURIComponent)
    .join("/")}`;
  // CT-B3 — the memories THIS session's kb-recall hook pulled in, pivoted
  // into the gallery the same way as the files-touched chip above (one
  // group per kb).
  const recallPivotGroups = groupIdsByKb(recalls.map((r) => ({ kb: r.kb, id: r.id })));

  const Root = asSheet ? "aside" : "div";
  return (
    <Root
      className={`kb-ses__inspector${asSheet ? " kb-ses__inspector--sheet" : ""}`}
      {...(asSheet
        ? { role: "dialog" as const, "aria-modal": true, id: "kb-sessions-sheet" }
        : {})}
    >
      {asSheet && onMobileClose && (
        // Reuses the v0.23 reader-tools sheet-head classes verbatim (the
        // "ONE mobile dialog grammar app-wide" ethos, S2) — the mobile.css
        // `.kb-pinsp__sheet-head`/`.kb-pinsp__grab` rules are unscoped (not
        // parented under `.detail--*`), so they style this for free.
        <header className="kb-pinsp__sheet-head">
          <span className="kb-pinsp__grab" aria-hidden />
          <span className="kb-pinsp__lab">session details</span>
          <button
            type="button"
            className="kb-pinsp__sheet-x"
            onClick={onMobileClose}
            title="close"
            aria-label="close session details"
            data-testid="session-sheet-close"
          >
            <Icon.X />
          </button>
        </header>
      )}
      <header className="kb-ses__ins-head">
        <h2>{sessionDisplayName(row)}</h2>
        <p className="kb-ses__ins-sub">
          {formatStarted(row.started_at)} · {row.message_count} messages
          {row.files_edited_count > 0 &&
            ` · ${row.files_edited_count} edited`}
          {row.memory_count > 0 && ` · ${row.memory_count} memories`}
          {row.tool_calls > 0 && ` · ${row.tool_calls} tools`}
          {row.token_total > 0 && ` · ${formatTokens(row.token_total)} tok`}
          {row.error_count > 0 && (
            <span className="kb-ses__ins-errors">
              {" · "}
              {row.error_count} error{row.error_count === 1 ? "" : "s"}
            </span>
          )}
          {row.model && ` · ${row.model}`}
        </p>
        {/* CT-A7 — the detail-panel peer of the CLI's `subagents N agent(s)
            · X tok · Y tools · Z edited` line (kb-cli's commands/sessions.rs
            `sessions show`); the row list only ever surfaced the bare ⚑
            badge (sessionChips.ts), never the aggregate numbers already on
            the wire. */}
        {row.subagent_count > 0 && (
          <p className="kb-ses__ins-subagents">
            {`subagents: ${row.subagent_count} agent${row.subagent_count === 1 ? "" : "s"} · ${formatTokens(row.subagent_tokens)} tok · ${row.subagent_tool_calls} tools · ${row.subagent_files_edited} edited`}
            {row.subagent_launched_unstatted > 0 && (
              <span title="some delegated subagents produced no sidecar stats — they ran but reported no numbers">
                {" · +unstatted"}
              </span>
            )}
          </p>
        )}
        <p className="kb-ses__ins-sid">
          <code>{row.session_id}</code> · <span>{row.kb}</span>
          {row.folder && (
            <>
              {" · "}
              <span title={row.cwd ?? row.folder}>{row.folder}</span>
            </>
          )}
        </p>
      </header>
      <div className="kb-ses__ins-actions">
        <Link to={transcriptHref}>open transcript →</Link>
        {/* W3.R-c — the session-replay reader (`/replay/:kb/:sid`). The
            transcript link above is the raw capture; this is the same session
            under a playhead, with the artifact that was in play beside it. */}
        <Link
          to={replayUrl(row.kb, row.session_id)}
          className="kb-ses__ins-actions-replay"
          data-testid="session-replay-link"
        >
          replay this session →
        </Link>
        {row.memory_count > 0 && (
          <Link
            to={`/memory?session=${encodeURIComponent(row.session_id)}`}
            className="kb-ses__ins-actions-mem"
          >
            view in /memory →
          </Link>
        )}
      </div>

      {/* The exact `claude -r <id>` command to resume this conversation. */}
      <ResumeCommand sessionId={row.session_id} />

      {/* W3.C/S1/S2 — the outcome block: the session's CLOSURE (the S3
          SessionContextCard's peer). Deviation from S2's literal
          mobile-only-reorder spec: rendered in the SAME position on both
          desktop and mobile (right after the actions/resume block, before
          the stacked sections) rather than only-mobile-first — the closure
          reads as valuable enough to lead with everywhere, and a single
          render path is one fewer thing to drift; see the W3 build report. */}
      <OutcomeSection outcome={row.outcome} />

      <section className="kb-ses__ins-section">
        <h3>Memories produced</h3>
        {loading && <p className="kb-ses__ins-loading">loading…</p>}
        {!loading && memories.length === 0 && (
          <p className="kb-ses__ins-empty">
            none — this session didn't produce any agent-curated memories
          </p>
        )}
        {!loading && memories.length > 0 && (
          <ul className="kb-ses__ins-list">
            {memories.map((m) => (
              <li key={`${m.kb}:${m.id}`}>
                <Link
                  to={`/a/${encodeURIComponent(m.kb)}/${m.source_relative
                    .split("/")
                    .map(encodeURIComponent)
                    .join("/")}`}
                >
                  {m.title || m.id}
                </Link>
                <span className="kb-ses__ins-list-meta">{m.kb}</span>
              </li>
            ))}
          </ul>
        )}
      </section>

      {/* MI-W4.2c — the PULL side of "Memories produced" above: what this
          session's kb-recall hook actually injected. The write side and
          the pull side of one conversation, finally together. */}
      <section className="kb-ses__ins-section">
        <h3>Memories recalled</h3>
        {loading && <p className="kb-ses__ins-loading">loading…</p>}
        {!loading && recalls.length === 0 && (
          <p className="kb-ses__ins-empty">
            none — the kb-recall hook never injected anything into this
            session
          </p>
        )}
        {!loading && recallPivotGroups.length > 0 && (
          <div className="kb-ses__pivot-row" data-testid="session-recalls-gallery-pivot">
            {recallPivotGroups.map((g) => (
              <IdsGalleryChip
                key={g.kb}
                kb={g.kb}
                ids={g.ids}
                label={
                  recallPivotGroups.length > 1
                    ? `view ${g.ids.length} in ${g.kb}`
                    : `view ${g.ids.length} artifact${g.ids.length === 1 ? "" : "s"} in gallery`
                }
                testId={`session-recalls-gallery-chip-${g.kb}`}
              />
            ))}
          </div>
        )}
        {!loading && recalls.length > 0 && (
          <ul className="kb-ses__ins-list" data-testid="session-recalls-list">
            {recalls.map((r, i) => (
              <li key={`${r.kb}:${r.id}:${i}`}>
                {r.source_relative ? (
                  <Link
                    to={`/a/${encodeURIComponent(r.kb)}/${r.source_relative
                      .split("/")
                      .map(encodeURIComponent)
                      .join("/")}`}
                  >
                    {r.title || r.id}
                  </Link>
                ) : (
                  <span title="this memory no longer resolves">{r.title || r.id}</span>
                )}
                <span className="kb-ses__ins-list-meta">{r.kb}</span>
              </li>
            ))}
          </ul>
        )}
      </section>

      {/* S9 — the decisions log: every steering moment (AskUserQuestion
          answer / plan approval) in order. */}
      <DecisionsSection decisions={decisions} loading={loading} kb={row.kb} />

      {/* P5 — the commits this session produced ("what shipped"). */}
      <CommitsSection commits={commits} loading={loading} />
      <ResearchSection research={research} loading={loading} />
      <SessionCommentsSection comments={comments} loading={loading} />

      {/* A4/A6 — the file-activity manifest: edited/created files first
          (linked when in a corpus), then reads, then out-of-corpus paths. */}
      <FilesTouchedSection
        files={files}
        loading={loading}
        sessionEndedAt={row.ended_at}
      />

      {/* S10 — a deterministic "resume context" hand-off (LLM-free). */}
      <ResumeSection
        row={row}
        files={files}
        decisions={decisions}
        commits={commits}
      />

      <section className="kb-ses__ins-section">
        <h3>Read during session</h3>
        {loading && <p className="kb-ses__ins-loading">loading…</p>}
        {!loading && readings.length === 0 && (
          <p className="kb-ses__ins-empty">
            none — nothing was opened in the reader during this window
          </p>
        )}
        {!loading && readings.length > 0 && (
          <ul className="kb-ses__ins-list">
            {readings.map((r) => (
              <li key={`${r.kb}:${r.artifact_id}`}>
                {r.source_relative ? (
                  <Link
                    to={`/a/${encodeURIComponent(r.kb)}/${r.source_relative
                      .split("/")
                      .map(encodeURIComponent)
                      .join("/")}`}
                  >
                    {r.title || r.artifact_id}
                  </Link>
                ) : (
                  <code>{r.artifact_id}</code>
                )}
                <span className="kb-ses__ins-list-meta">
                  {r.read_pct != null ? `${r.read_pct}%` : "—"}
                </span>
              </li>
            ))}
          </ul>
        )}
      </section>

      {detail?.memory_ids && detail.memory_ids.length > memories.length && (
        <p className="kb-ses__ins-note">
          (showing top {memories.length} of {detail.memory_ids.length})
        </p>
      )}
    </Root>
  );
}

// W3.C/S1 — the outcome block: the session's CLOSURE, full 240-char server
// preview (the `SessionOut.outcome` wire field — the peer of
// `first_user_prompt`). Hidden when absent (a pre-backfill row, or a husk
// with no closing prose) — the row-level `»`-prefixed preview already
// degrades visibly in that case.
function OutcomeSection({ outcome }: { outcome: string | null | undefined }) {
  if (!outcome || !outcome.trim()) return null;
  return (
    <section className="kb-ses__ins-section kb-ses__ins-outcome" data-testid="session-outcome">
      <h3>Outcome</h3>
      <p className="kb-ses__ins-outcome-text">{outcome}</p>
    </section>
  );
}

function formatStarted(unix: number): string {
  if (!unix) return "—";
  const d = new Date(unix * 1000);
  return d.toLocaleString(undefined, {
    month: "short",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  });
}

// P6 — "Jun 12 – Jun 25" project span; a single day collapses to one date.
function projectSpan(earliest: number, latest: number): string {
  const fmt = (u: number) =>
    new Date(u * 1000).toLocaleDateString(undefined, {
      month: "short",
      day: "numeric",
    });
  if (!earliest || earliest === latest) return fmt(latest);
  return `${fmt(earliest)} – ${fmt(latest)}`;
}

function formatTokens(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
  if (n >= 1_000) return `${Math.round(n / 1_000)}k`;
  return String(n);
}
