import { Link } from "react-router-dom";
import { useQuery } from "@tanstack/react-query";
import { fetchChanges } from "../api/changes";
import {
  ambientSinceHref,
  composeSinceStrip,
  sinceHeading,
} from "../lib/sinceYouLeft";

// OL IDEA5 — "Since you left": ONE strip at the top of the Front Page naming
// what landed across PROJECTS (sessions that ran, the commits they recorded,
// slate posts) since the visit watermark. The corpus axis (artifacts,
// memories, comments) is the footer link to /ambient — not rebuilt here.
//
// Recorded refusals: no dismiss control, no badge, no read state. The strip
// goes away when the watermark rotates (once per browser session). It renders
// nothing on a first visit, a failed fetch or an empty window — silence is
// the honest answer. Fleet-wide in both Front Page scopes; `kb` only scopes
// the footer link. Server state rides ["changes", since] (the SSE bridge
// invalidates the ["changes"] prefix, #23).

type Props = {
  /// The visit watermark (unix seconds) the edition line already uses; null on
  /// a first visit.
  since: number | null;
  nowMs: number;
  /// The Front Page scope; only shapes the footer link.
  kb: string | null;
};

export function SinceYouLeft({ since, nowMs, kb }: Props) {
  const { data } = useQuery({
    queryKey: ["changes", since] as const,
    queryFn: ({ signal }) => fetchChanges({ since: since as number }, signal),
    enabled: since != null,
    staleTime: Infinity,
    // A failure is an error state (not cached data): one retry, then the
    // strip stays silent and the next mount refetches.
    retry: 1,
  });
  if (since == null || !data || data.projects.length === 0) return null;

  const strip = composeSinceStrip(data);
  return (
    <section className="kb-fp__since" data-testid="since-you-left" aria-label="since you left">
      <h3 className="kb-fp__section-h">
        {sinceHeading(data.since_unix, nowMs, data.window_clamped)}
        <span className="kb-fp__since-scope"> · across projects</span>
      </h3>
      <ul className="kb-fp__since-list">
        {strip.rows.map((row) => (
          <li key={row.key} className="kb-fp__since-row">
            <div className="kb-fp__since-head">
              <Link to={row.href} className="kb-fp__since-label">
                {row.label}
              </Link>
              {row.counts && <span className="kb-fp__since-counts mono">{row.counts}</span>}
            </div>
            {row.details.map((d, i) =>
              d.href ? (
                <Link
                  key={i}
                  to={d.href}
                  className={`kb-fp__since-detail kb-fp__since-detail--${d.kind}`}
                >
                  {d.text}
                </Link>
              ) : (
                <span key={i} className={`kb-fp__since-detail kb-fp__since-detail--${d.kind}`}>
                  {d.text}
                </span>
              ),
            )}
          </li>
        ))}
      </ul>
      {strip.more > 0 && <div className="kb-fp__since-more mono">+{strip.more} more projects</div>}
      <Link to={ambientSinceHref(since, kb)} className="kb-fp__since-foot">
        Corpus activity since then
      </Link>
    </section>
  );
}

export default SinceYouLeft;
