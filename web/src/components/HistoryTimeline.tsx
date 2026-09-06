import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Link } from "react-router-dom";
import { fetchHistory, type HistoryEntry, type HistoryKind } from "../api/history";
import { isAbortError } from "../api/client";
import { artifactHref } from "../lib/artifactHref";
import { dayBucket, dayHeading, formatHistoryTime } from "../lib/time";
import { sse } from "../api/sse";
import { useIdentity } from "../hooks/useArtifactHost";
import EmptyState from "./EmptyState";
import ActivityCalendar from "./ActivityCalendar";
import UserChip from "./UserChip";
import { Icon } from "./icons";

// v0.6+ H5 — gallery's 4th view: chronological list of artifact opens,
// search queries, and authored comments. Reuses gallery chrome (kb
// switcher in TopBar, etc.); the body is grouped by day with sticky
// headers. Clicking an open-entry navigates to the artifact — the
// detail.tsx mount then fires its own POST /history/open which the
// server treats as a continuation of the same visit (30-min gap rule)
// and the runtime jumps to the saved scroll position.

type Filter = "all" | HistoryKind;

const FILTER_LABELS: Record<Filter, string> = {
  all: "All",
  open: "Opens",
  search: "Searches",
  comment: "Comments",
};

// v0.14 T3 — `dayBucket` + `dayHeading` lifted to lib/time.ts so the
// /sessions view shares the exact same grouping grammar.

// `hhmm` shim → shared `formatHistoryTime("hhmm-only")`.  Keeps the
// per-row label compact (the day stripe header carries the date),
// but defers to the single source of truth in lib/time.ts so the
// Recent tab popover + this view never drift again (M-spa).
function hhmm(unix: number): string {
  return formatHistoryTime(unix, "hhmm-only");
}

function scrollPercent(entry: HistoryEntry): number | null {
  if (entry.scroll_max <= 0) return null;
  // High-water mark (V0007), not the current scroll_y — a visit that
  // reached the bottom keeps showing 100% even if the reader later
  // scrolled back up.
  const pct = Math.min(100, Math.round((entry.scroll_y_max / entry.scroll_max) * 100));
  return Math.max(0, pct);
}

export default function HistoryTimeline({ kb }: { kb: string }) {
  const [entries, setEntries] = useState<HistoryEntry[]>([]);
  const [filter, setFilter] = useState<Filter>("all");
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  // AbortController so a kb/filter change cancels the prior in-flight
  // fetchHistory before its setEntries lands stale data.
  const loadCtl = useRef<AbortController | null>(null);

  const load = useCallback(() => {
    loadCtl.current?.abort();
    const ctl = new AbortController();
    loadCtl.current = ctl;
    setError(null);
    fetchHistory(kb, { limit: 200, kind: filter, signal: ctl.signal })
      .then((es) => {
        if (ctl.signal.aborted) return;
        setEntries(es);
        setLoading(false);
      })
      .catch((e) => {
        if (isAbortError(e)) return;
        setError(String(e));
        setLoading(false);
      });
  }, [kb, filter]);

  useEffect(() => {
    setLoading(true);
    load();
    return () => loadCtl.current?.abort();
  }, [load]);

  // Refetch on history.recorded SSE — server emits on every new insert
  // (not on bumps / scroll updates), so this fires at most once per
  // user-initiated event. Leading-edge with a small cooldown keeps the
  // list fresh without thrashing when a burst of comments lands.
  useEffect(() => {
    let lastTick = 0;
    const COOLDOWN_MS = 750;
    const offEvent = sse.subscribeEvent("history.recorded", (payload) => {
      if (payload.kb && payload.kb !== kb) return;
      const now = Date.now();
      if (now - lastTick < COOLDOWN_MS) return;
      lastTick = now;
      load();
    });
    // On an SSE gap/resync the worker's cursor jumped — history.recorded
    // events during the disconnect were missed, so this local store would
    // show stale entries. Reload from scratch, mirroring the query cache's
    // wholesale invalidation on resync (queryClient.ts, invariant #23).
    const offResync = sse.onResync(() => load());
    return () => {
      offEvent();
      offResync();
    };
  }, [kb, load]);

  // Group by local-day bucket; preserve newest-first within each group.
  const groups = useMemo(() => {
    const out: { bucket: string; rows: HistoryEntry[] }[] = [];
    let current: { bucket: string; rows: HistoryEntry[] } | null = null;
    for (const e of entries) {
      const b = dayBucket(e.started_at);
      if (!current || current.bucket !== b) {
        current = { bucket: b, rows: [] };
        out.push(current);
      }
      current.rows.push(e);
    }
    return out;
  }, [entries]);

  return (
    <div className="history" aria-label={`history for kb ${kb}`}>
      <ActivityCalendar kb={kb} />
      <div className="history__filters" role="tablist" aria-label="filter by action">
        {(Object.keys(FILTER_LABELS) as Filter[]).map((f) => (
          <button
            key={f}
            role="tab"
            aria-selected={filter === f}
            className={`history__filter ${filter === f ? "is-on" : ""}`}
            onClick={() => setFilter(f)}
          >
            {FILTER_LABELS[f]}
          </button>
        ))}
      </div>

      {error && (
        <div className="history__error" role="alert">
          {error}
        </div>
      )}
      {loading && <div className="history__loading">loading…</div>}
      {!loading && !error && entries.length === 0 && (
        <EmptyState
          icon={<Icon.History />}
          title="no history yet"
          hint="Open an artifact, run a search, or add a comment — every visit lands on this day-grouped timeline."
        />
      )}

      {groups.map((g) => (
        <section key={g.bucket} className="history__group">
          <h3 className="history__day">{dayHeading(g.bucket)}</h3>
          <ul className="history__list">
            {g.rows.map((e) => (
              <HistoryRow key={e.id} kb={kb} entry={e} />
            ))}
          </ul>
        </section>
      ))}
    </div>
  );
}

function HistoryRow({ kb, entry }: { kb: string; entry: HistoryEntry }) {
  const pct = scrollPercent(entry);
  const identity = useIdentity();
  const me = identity?.user;
  // v0.34 W — teammate chip when the row's user differs from me.
  const otherUser = (
    <UserChip user={entry.user} me={me} className="history__user" />
  );

  if (entry.kind === "open" && entry.artifact_id) {
    return (
      <li className="history__row history__row--open">
        <time className="history__time">{hhmm(entry.updated_at)}</time>
        <span className="history__kind" aria-label="opened">
          <OpenGlyph />
        </span>
        {entry.source_relative ? (
          <Link
            to={artifactHref(kb, entry.source_relative)}
            className="history__title"
          >
            {entry.title ?? entry.artifact_id}
          </Link>
        ) : (
          // Artifact has left lance — no resolvable path permalink.
          <span className="history__title history__title--gone">
            {entry.title ?? entry.artifact_id}
          </span>
        )}
        <span className="history__trail">
          {otherUser}
          {pct !== null && (
            <span
              className="history__scroll"
              title={`${pct}% read`}
              aria-label={`scrolled ${pct}%`}
            >
              <span className="history__scroll-bar">
                <span
                  className="history__scroll-fill"
                  style={{ width: `${pct}%` }}
                />
              </span>
              <span className="history__scroll-pct">{pct}%</span>
            </span>
          )}
        </span>
      </li>
    );
  }

  if (entry.kind === "search") {
    return (
      <li className="history__row history__row--search">
        <time className="history__time">{hhmm(entry.started_at)}</time>
        <span className="history__kind" aria-label="searched">
          <SearchGlyph />
        </span>
        <span className="history__query">{entry.query}</span>
        <span className="history__trail">{otherUser}</span>
      </li>
    );
  }

  if (entry.kind === "comment" && entry.artifact_id) {
    return (
      <li className="history__row history__row--comment">
        <time className="history__time">{hhmm(entry.started_at)}</time>
        <span className="history__kind" aria-label="commented on">
          <CommentGlyph />
        </span>
        {entry.source_relative ? (
          <Link
            to={artifactHref(kb, entry.source_relative)}
            className="history__title"
          >
            {entry.title ?? entry.artifact_id}
          </Link>
        ) : (
          <span className="history__title history__title--gone">
            {entry.title ?? entry.artifact_id}
          </span>
        )}
        <span className="history__trail">
          {otherUser}
          {entry.comment_id && (
            <span className="history__comment-id" title="comment id">
              #{entry.comment_id}
            </span>
          )}
        </span>
      </li>
    );
  }

  return null;
}

// Inline 14px glyphs matching the existing icon set's stroke weight.
function OpenGlyph() {
  return (
    <svg viewBox="0 0 16 16" width={14} height={14} fill="none" stroke="currentColor" strokeWidth={1.4}>
      <path d="M3.5 1.5h6L13 5v9.5H3.5z" />
      <path d="M9.5 1.5V5h3.5" />
    </svg>
  );
}
function SearchGlyph() {
  return (
    <svg viewBox="0 0 16 16" width={14} height={14} fill="none" stroke="currentColor" strokeWidth={1.4}>
      <circle cx="7" cy="7" r="4.5" />
      <path d="m10.5 10.5 3 3" />
    </svg>
  );
}
function CommentGlyph() {
  return (
    <svg viewBox="0 0 16 16" width={14} height={14} fill="none" stroke="currentColor" strokeWidth={1.4}>
      <path d="M2 3.5a1 1 0 0 1 1-1h10a1 1 0 0 1 1 1V10a1 1 0 0 1-1 1H6.5L4 13.5V11H3a1 1 0 0 1-1-1z" strokeLinejoin="round" />
    </svg>
  );
}
