// Unit 2 — the desk radiator: `/ambient`. A full-screen, low-chrome,
// glanceable view built for a spare monitor — the SAME data family as the
// e-ink daycard (`GET /api/kb/{kb}/daycard`, `routes/daycard.rs`), rendered
// as React instead of the daemon's inline-SVG HTML twin (the daemon owns
// markup for the e-ink panel; the SPA owns its own DOM here, both reading
// the identical JSON).
//
// Pull-only, display-only: the ONLY interactive affordance is "click through
// to the artifact" (a plain `<Link>`), plus the since permalink itself so a
// glance can be handed to someone else. No filters, no controls, no
// dismiss/snooze, no dashboard chrome — this is a display, not a tool.
// No push subscription: the slow timer below is the only liveness.
//
// CT-E1 — "what changed since I last looked". `?since=` on this page (unix
// seconds or `YYYY-MM-DD`, the route's own grammar) is the permalink and
// wins. Absent that, a browser-local watermark keyed by identity user + kb
// is promoted into the URL and sent as `?since=`. Absent both, the request
// is the original `fetchDaycard(kb, {})` — no since param, same query key.
// The watermark is written here because no owned route stores "last looked".
//
// Since permalink asks the mounted daycard route for every kb (`?all=1`).
// The handler can see `state.kbs`, so no new route is required. Absent since
// is still `fetchDaycard(kb, {})` — day mode, active kb only. The watermark
// below is unchanged.
//
// Liveness: a SLOW plain timer (`REFRESH_MS`), explicitly sanctioned for
// this one ambient surface rather than the SSE-invalidation bridge every
// other TanStack Query hook rides (invariant #23's three prior documented
// exceptions — sessions, open note, stale anchors — plus this one). This is
// NOT a second `/api/events` connection: invariant #24 (one SSE connection
// per daemon per browser, owned by the SharedWorker) is untouched — this
// route never imports `api/sse`.
//
// `prefers-reduced-motion` is respected in `styles/ambient.css` (the only
// transition-bearing rule there is gated behind the media query); this
// component itself runs no animation loop.

import { useEffect, useRef, useState } from "react";
import { Link, useSearchParams } from "react-router-dom";
import { useQuery } from "@tanstack/react-query";
import { useActiveKb } from "../hooks/useActiveKb";
import { useIdentity } from "../hooks/useArtifactHost";
import { useDocumentTitle } from "../hooks/useDocumentTitle";
import { fetchDaycard, type DaycardDoc } from "../api/client";
import { currentDaemonBase } from "../api/base";
import type { DaycardSinceResponse } from "../api/generated/DaycardSinceResponse";
import type { DaycardResponse } from "../api/generated/DaycardResponse";
import type { DaycardCommentItem } from "../api/generated/DaycardCommentItem";
import type { DaycardSessionItem } from "../api/generated/DaycardSessionItem";
import { artifactHref } from "../lib/artifactHref";
import EmptyState from "../components/EmptyState";
import "../styles/ambient.css";

// A slow poll, not a dashboard refresh rate — the whole point of this
// surface is that nobody is staring at it waiting for a number to move.
const REFRESH_MS = 5 * 60 * 1000;
// The on-screen clock only needs minute resolution.
const CLOCK_TICK_MS = 60 * 1000;
// Identity is warmed at app start; this only covers a cold mount where the
// cache is still empty. After it, a missing user means "no watermark", and
// the no-since request proceeds unchanged.
const IDENTITY_WAIT_MS = 2000;
// Browser-local "last looked", per attribution user and kb. Not telemetry.
const WATERMARK_KEY = "kb:ambient:since";
// Same grammar as `daycard::parse_since_bound`: unix seconds or YYYY-MM-DD.
const SINCE_TOKEN = /^(?:-?\d+|\d{4}-\d{2}-\d{2})$/;

function readWatermark(user: string, kb: string): string | null {
  try {
    if (typeof localStorage === "undefined") return null;
    const raw = localStorage.getItem(WATERMARK_KEY);
    if (!raw) return null;
    const parsed = JSON.parse(raw) as unknown;
    if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) return null;
    const byUser = (parsed as Record<string, unknown>)[user];
    if (!byUser || typeof byUser !== "object" || Array.isArray(byUser)) return null;
    const v = (byUser as Record<string, unknown>)[kb];
    return typeof v === "string" && SINCE_TOKEN.test(v) ? v : null;
  } catch {
    return null;
  }
}

function writeWatermark(user: string, kb: string, since: string): void {
  try {
    if (typeof localStorage === "undefined") return;
    const raw = localStorage.getItem(WATERMARK_KEY);
    const parsed = raw ? (JSON.parse(raw) as unknown) : {};
    const store =
      parsed && typeof parsed === "object" && !Array.isArray(parsed)
        ? (parsed as Record<string, unknown>)
        : {};
    const prev = store[user];
    const byKb: Record<string, string> = {};
    if (prev && typeof prev === "object" && !Array.isArray(prev)) {
      for (const [k, v] of Object.entries(prev as Record<string, unknown>)) {
        if (typeof v === "string") byKb[k] = v;
      }
    }
    byKb[kb] = since;
    store[user] = byKb;
    localStorage.setItem(WATERMARK_KEY, JSON.stringify(store));
  } catch {
    // Private mode / quota — the URL since still works for this visit.
  }
}

function ambientPermalink(kb: string, since: string): string {
  const q = new URLSearchParams();
  q.set("kb", kb);
  q.set("since", since);
  return `/ambient?${q.toString()}`;
}

function sessionHref(kb: string, sessionId: string): string {
  const q = new URLSearchParams();
  q.set("kb", kb);
  q.set("focus", sessionId);
  return `/sessions?${q.toString()}`;
}

function commentHref(kb: string, sourceRelative: string, commentId: string): string {
  const base = artifactHref(kb, sourceRelative, { panel: "comments" });
  const sep = base.includes("?") ? "&" : "?";
  return `${base}${sep}comment=${encodeURIComponent(commentId)}`;
}

function isFleet(data: DaycardSinceResponse | DaycardSinceFleet): data is DaycardSinceFleet {
  return "kbs" in data && Array.isArray(data.kbs);
}

function isSinceDoc(data: { since_unix?: number } | { activity: unknown }): data is DaycardSinceFleet {
  return "since_unix" in data && typeof data.since_unix === "number";
}

// Since-mode fetch. `fetchDaycard` only accepts `day`, and that module is
// not ours to extend. Absent since never reaches this function — the caller
// keeps the original `fetchDaycard(kb, {})` call. `Accept: application/json`
// matches `get()` so the route's JSON branch is selected (HTML is the
// default for a bare e-ink client). `all=1` is the fleet query on that same
// mounted route: every kb, not only the path segment.
type DaycardSinceFleet = {
  since_unix: number;
  to_unix: number;
  kbs: DaycardSinceResponse[];
  degraded?: Array<{ kb: string; lane: string; error_class: string }>;
};

function asFleet(data: DaycardSinceResponse | DaycardSinceFleet): DaycardSinceFleet {
  if (isFleet(data)) return data;
  return { since_unix: data.since_unix, to_unix: data.to_unix, kbs: [data] };
}

async function fetchDaycardSince(
  kb: string,
  since: string,
  signal?: AbortSignal,
): Promise<DaycardSinceFleet> {
  const q = new URLSearchParams();
  q.set("since", since);
  q.set("all", "1");
  const r = await fetch(
    `${currentDaemonBase()}/api/kb/${encodeURIComponent(kb)}/daycard?${q}`,
    { headers: { Accept: "application/json" }, signal },
  );
  if (!r.ok) throw new Error(`${r.status} ${r.statusText}`);
  return asFleet((await r.json()) as DaycardSinceResponse | DaycardSinceFleet);
}

export default function AmbientRoute() {
  const [params, setParams] = useSearchParams();
  const kb = useActiveKb();
  const identity = useIdentity();
  const urlSinceRaw = params.get("since");
  const urlSince =
    urlSinceRaw && urlSinceRaw.trim() !== "" ? urlSinceRaw.trim() : null;
  const user = identity?.user?.trim() ? identity.user : null;

  // Don't issue the no-since fetch until we know whether a watermark applies.
  // A URL since does not wait — the permalink is already explicit.
  const [identitySettled, setIdentitySettled] = useState(() => identity != null);
  useEffect(() => {
    if (identity) {
      setIdentitySettled(true);
      return;
    }
    const id = window.setTimeout(() => setIdentitySettled(true), IDENTITY_WAIT_MS);
    return () => window.clearTimeout(id);
  }, [identity]);

  // Freeze the since chosen for this visit. A live localStorage read would
  // pick up the watermark this page just wrote and flip the glance on the
  // next clock tick. Re-resolve only when the URL, kb, user, or identity
  // settle-state changes.
  const visitKey = `${kb ?? ""}\0${user ?? ""}\0${urlSince ?? ""}\0${identitySettled ? "1" : "0"}`;
  const frozen = useRef<{ key: string; since: string | null } | null>(null);
  if (kb && (urlSince != null || identitySettled)) {
    if (!frozen.current || frozen.current.key !== visitKey) {
      const stored = !urlSince && user ? readWatermark(user, kb) : null;
      frozen.current = { key: visitKey, since: urlSince ?? stored };
    }
  }
  const since = frozen.current?.key === visitKey ? frozen.current.since : null;

  useDocumentTitle(since ? `Ambient · since ${since}` : "Ambient");
  // Promote a resolved since into the address bar so the glance is linkable.
  // Replace, don't push: a watermark promotion is not a navigation. Never
  // writes a since that wasn't already resolved, so a bare `/ambient` with
  // no watermark stays bare.
  useEffect(() => {
    if (!kb || !since) return;
    if (params.get("since") === since && params.get("kb") === kb) return;
    const next = new URLSearchParams(params);
    next.set("kb", kb);
    next.set("since", since);
    setParams(next, { replace: true });
  }, [kb, since, params, setParams]);

  const waitingForSince = !!kb && urlSince == null && !identitySettled;
  const { data, isLoading, isError } = useQuery<DaycardResponse | DaycardSinceFleet>({
    queryKey: since ? (["daycard", kb, "since", "all", since] as const) : (["daycard", kb] as const),
    queryFn: ({ signal }) =>
      since
        ? fetchDaycardSince(kb as string, since, signal)
        : fetchDaycard(kb as string, {}, signal),
    enabled: !!kb && (urlSince != null || identitySettled),
    // Documented exception to invariant #23 (see module doc above): this
    // route wants a slow wall-clock refresh, not SSE-precise invalidation.
    staleTime: 0,
    refetchInterval: REFRESH_MS,
    refetchIntervalInBackground: true,
  });

  // Stamp "last looked" once per visit, after a successful pull — not on
  // every poll, or a spare monitor left open would shrink the next window
  // to one refresh. The current URL since is left alone (permalink).
  const stamped = useRef<string | null>(null);
  useEffect(() => {
    if (!user || !kb || !data) return;
    const visit = `${user}\0${kb}\0${since ?? ""}`;
    if (stamped.current === visit) return;
    stamped.current = visit;
    const unix = isSinceDoc(data) ? data.to_unix : Math.floor(Date.now() / 1000);
    writeWatermark(user, kb, String(unix));
  }, [user, kb, data, since]);

  const [now, setNow] = useState(() => new Date());
  useEffect(() => {
    const id = window.setInterval(() => setNow(new Date()), CLOCK_TICK_MS);
    return () => window.clearInterval(id);
  }, []);

  if (!kb) {
    return (
      <div className="kb-ambient kb-ambient--empty">
        <EmptyState
          title="No corpus selected"
          hint="Pick a kb from the workspace pill first."
        />
      </div>
    );
  }

  const sinceData = data && isSinceDoc(data) ? data : null;
  const fleet = sinceData
    ? asFleet(sinceData)
    : null;
  const fleetTitle =
    fleet && fleet.kbs.length > 0 ? fleet.kbs.map((slice) => slice.kb).join(" · ") : kb;
  const unavailable =
    fleet?.degraded && fleet.degraded.length > 0
      ? fleet.degraded.map((d) => `${d.kb} (${d.error_class})`).join(", ")
      : "";

  return (
    <div className="kb-ambient" data-testid="ambient-view">
      <header className="kb-ambient__head">
        <h1 className="kb-ambient__kb">{fleetTitle}</h1>
        <div className="kb-ambient__clock">
          {since ? (
            <>
              <Link to={ambientPermalink(kb, since)} data-testid="ambient-since">
                since {since}
              </Link>
              {" · "}
            </>
          ) : null}
          {formatClock(now)}
        </div>
      </header>

      {(waitingForSince || isLoading) && <p className="kb-ambient__status">Loading…</p>}
      {isError && (
        <p className="kb-ambient__status">Daemon unreachable — retrying.</p>
      )}

      {fleet && (
        <p className="kb-ambient__status" data-testid="ambient-fleet">
          while you were away: {fleet.since_unix} → {fleet.to_unix}
          {unavailable ? ` · unavailable: ${unavailable}` : ""}
        </p>
      )}

      {fleet && (
        <div className="kb-ambient__grid">
          <section
            className="kb-ambient__panel"
            aria-labelledby="amb-sessions"
            data-testid="ambient-sessions"
          >
            <h2 id="amb-sessions">Sessions</h2>
            {fleet.kbs.every((slice) => slice.sessions.length === 0) ? (
              <p className="kb-ambient__empty">Nothing here.</p>
            ) : (
              fleet.kbs.map((slice) =>
                slice.sessions.length === 0 ? null : (
                  <div key={slice.kb}>
                    <p className="kb-ambient__sub">{slice.kb}</p>
                    <SessionList
                      kb={slice.kb}
                      items={slice.sessions}
                      truncated={slice.sessions_truncated}
                    />
                  </div>
                ),
              )
            )}
          </section>
          <section
            className="kb-ambient__panel"
            aria-labelledby="amb-memories"
            data-testid="ambient-memories"
          >
            <h2 id="amb-memories">Memories written</h2>
            {fleet.kbs.every((slice) => slice.memories.length === 0) ? (
              <p className="kb-ambient__empty">Nothing here.</p>
            ) : (
              fleet.kbs.map((slice) =>
                slice.memories.length === 0 ? null : (
                  <div key={slice.kb}>
                    <p className="kb-ambient__sub">{slice.kb}</p>
                    <SinceDocs
                      kb={slice.kb}
                      docs={slice.memories}
                      truncated={slice.memories_truncated}
                    />
                  </div>
                ),
              )
            )}
          </section>
          <section
            className="kb-ambient__panel"
            aria-labelledby="amb-artifacts"
            data-testid="ambient-artifacts"
          >
            <h2 id="amb-artifacts">Artifacts created / updated</h2>
            {fleet.kbs.every((slice) => slice.artifacts.length === 0) ? (
              <p className="kb-ambient__empty">Nothing here.</p>
            ) : (
              fleet.kbs.map((slice) =>
                slice.artifacts.length === 0 ? null : (
                  <div key={slice.kb}>
                    <p className="kb-ambient__sub">{slice.kb}</p>
                    <SinceDocs
                      kb={slice.kb}
                      docs={slice.artifacts}
                      truncated={slice.artifacts_truncated}
                    />
                  </div>
                ),
              )
            )}
          </section>
          <section
            className="kb-ambient__panel"
            aria-labelledby="amb-comments"
            data-testid="ambient-comments"
          >
            <h2 id="amb-comments">
              Comments raised (
              {fleet.kbs.reduce((n, slice) => n + slice.comments_still_open, 0)} still open)
            </h2>
            {fleet.kbs.every((slice) => slice.comments.length === 0) ? (
              <p className="kb-ambient__empty">Nothing here.</p>
            ) : (
              fleet.kbs.map((slice) =>
                slice.comments.length === 0 ? null : (
                  <div key={slice.kb}>
                    <p className="kb-ambient__sub">{slice.kb}</p>
                    <CommentList
                      kb={slice.kb}
                      items={slice.comments}
                      truncated={slice.comments_truncated}
                    />
                  </div>
                ),
              )
            )}
          </section>
        </div>
      )}

      {data && !isSinceDoc(data) && (
        <div className="kb-ambient__grid">
          <section
            className="kb-ambient__panel"
            aria-labelledby="amb-activity"
            data-testid="ambient-activity"
          >
            <h2 id="amb-activity">Today</h2>
            <dl className="kb-ambient__activity">
              <ActivityRow label="opens" value={data.activity.opens} />
              <ActivityRow label="searches" value={data.activity.searches} />
              <ActivityRow label="comments" value={data.activity.comments} />
            </dl>
          </section>

          <section
            className="kb-ambient__panel"
            aria-labelledby="amb-resurface"
            data-testid="ambient-resurface"
          >
            <h2 id="amb-resurface">Worth picking back up</h2>
            {data.resurface.length === 0 ? (
              <p className="kb-ambient__empty">Nothing waiting.</p>
            ) : (
              <ul className="kb-ambient__list">
                {data.resurface.map((item) => (
                  <li key={item.id}>
                    <Link to={artifactHref(kb, item.source_relative)}>
                      {item.title}
                    </Link>
                    {item.reasons[0] && (
                      <span className="kb-ambient__sub">{item.reasons[0]}</span>
                    )}
                  </li>
                ))}
              </ul>
            )}
          </section>

          <section
            className="kb-ambient__panel"
            aria-labelledby="amb-recent"
            data-testid="ambient-recent"
          >
            <h2 id="amb-recent">Recently touched</h2>
            <DocList kb={kb} docs={data.recent} />
          </section>

          <section
            className="kb-ambient__panel"
            aria-labelledby="amb-never"
            data-testid="ambient-never"
          >
            <h2 id="amb-never">Never opened</h2>
            <DocList kb={kb} docs={data.never_opened} />
          </section>
        </div>
      )}
    </div>
  );
}

function ActivityRow({ label, value }: { label: string; value: number }) {
  return (
    <div className="kb-ambient__activity-row">
      <dt className="kb-ambient__activity-label">{label}</dt>
      <dd className="kb-ambient__activity-value">{value}</dd>
    </div>
  );
}

function DocList({ kb, docs }: { kb: string; docs: DaycardDoc[] }) {
  if (docs.length === 0) {
    return <p className="kb-ambient__empty">Nothing here.</p>;
  }
  return (
    <ul className="kb-ambient__list">
      {docs.map((d) => (
        <li key={d.id}>
          <Link to={artifactHref(kb, d.source_relative)}>{d.title}</Link>
        </li>
      ))}
    </ul>
  );
}

function SinceDocs({
  kb,
  docs,
  truncated,
}: {
  kb: string;
  docs: DaycardDoc[];
  truncated: boolean;
}) {
  return (
    <>
      <DocList kb={kb} docs={docs} />
      {truncated ? <p className="kb-ambient__sub">More than shown.</p> : null}
    </>
  );
}

function SessionList({
  kb,
  items,
  truncated,
}: {
  kb: string;
  items: DaycardSessionItem[];
  truncated: boolean;
}) {
  if (items.length === 0) {
    return <p className="kb-ambient__empty">Nothing here.</p>;
  }
  return (
    <>
      <ul className="kb-ambient__list">
        {items.map((item) => (
          <li key={item.session_id}>
            <Link to={sessionHref(kb, item.session_id)}>{item.title}</Link>
          </li>
        ))}
      </ul>
      {truncated ? <p className="kb-ambient__sub">More than shown.</p> : null}
    </>
  );
}

function CommentList({
  kb,
  items,
  truncated,
}: {
  kb: string;
  items: DaycardCommentItem[];
  truncated: boolean;
}) {
  if (items.length === 0) {
    return <p className="kb-ambient__empty">Nothing here.</p>;
  }
  return (
    <>
      <ul className="kb-ambient__list">
        {items.map((item) => (
          <li key={item.comment_id}>
            {item.source_relative ? (
              <Link to={commentHref(kb, item.source_relative, item.comment_id)}>
                {item.title}
              </Link>
            ) : (
              item.title
            )}
            {item.open ? " (open)" : " (resolved)"}
          </li>
        ))}
      </ul>
      {truncated ? <p className="kb-ambient__sub">More than shown.</p> : null}
    </>
  );
}

function formatClock(d: Date): string {
  return d.toLocaleString(undefined, {
    weekday: "short",
    year: "numeric",
    month: "short",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  });
}
