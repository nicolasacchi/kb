// Unit 2 — the desk radiator: `/ambient`. A full-screen, low-chrome,
// glanceable view built for a spare monitor — the SAME data family as the
// e-ink daycard (`GET /api/kb/{kb}/daycard`, `routes/daycard.rs`), rendered
// as React instead of the daemon's inline-SVG HTML twin (the daemon owns
// markup for the e-ink panel; the SPA owns its own DOM here, both reading
// the identical JSON).
//
// Pull-only, display-only: the ONLY interactive affordance is "click through
// to the artifact" (a plain `<Link>`). No filters, no controls, no
// dismiss/snooze, no dashboard chrome — this is a display, not a tool.
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

import { useEffect, useState } from "react";
import { Link } from "react-router-dom";
import { useQuery } from "@tanstack/react-query";
import { useActiveKb } from "../hooks/useActiveKb";
import { useDocumentTitle } from "../hooks/useDocumentTitle";
import { fetchDaycard, type DaycardDoc } from "../api/client";
import { artifactHref } from "../lib/artifactHref";
import EmptyState from "../components/EmptyState";
import "../styles/ambient.css";

// A slow poll, not a dashboard refresh rate — the whole point of this
// surface is that nobody is staring at it waiting for a number to move.
const REFRESH_MS = 5 * 60 * 1000;
// The on-screen clock only needs minute resolution.
const CLOCK_TICK_MS = 60 * 1000;

export default function AmbientRoute() {
  useDocumentTitle("Ambient");
  const kb = useActiveKb();

  const { data, isLoading, isError } = useQuery({
    queryKey: ["daycard", kb] as const,
    queryFn: ({ signal }) => fetchDaycard(kb as string, {}, signal),
    enabled: !!kb,
    // Documented exception to invariant #23 (see module doc above): this
    // route wants a slow wall-clock refresh, not SSE-precise invalidation.
    staleTime: 0,
    refetchInterval: REFRESH_MS,
    refetchIntervalInBackground: true,
  });

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

  return (
    <div className="kb-ambient" data-testid="ambient-view">
      <header className="kb-ambient__head">
        <h1 className="kb-ambient__kb">{kb}</h1>
        <div className="kb-ambient__clock">{formatClock(now)}</div>
      </header>

      {isLoading && <p className="kb-ambient__status">Loading…</p>}
      {isError && (
        <p className="kb-ambient__status">Daemon unreachable — retrying.</p>
      )}

      {data && (
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
