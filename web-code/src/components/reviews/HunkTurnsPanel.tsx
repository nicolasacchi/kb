// `kbc-hunk-turns/1` (V73-K2c) — the review diff's on-demand "which agent
// turn wrote this hunk?" affordance. Mounting THIS component is the fetch
// trigger (`useHunkTurns`'s own doc: never auto-fetched for every hunk,
// budget: on demand) — `HunkStrip.tsx`'s "turns" button toggles whether
// this is mounted at all, so there is never a standing subscription for a
// hunk nobody asked about.
//
// LOOPBACK-ONLY: off loopback (or on ANY other failure) this renders the
// honest refusal text rather than a blank panel — `useHunkTurns` surfaces
// any non-2xx as a plain `ApiError`, and this component renders
// `e.message` verbatim, since the route has no dedicated bearer-visible
// refusal shape of its own (the loopback gate sits at the router, ahead of
// the handler).
import { sessionUrl } from "../../lib/searchLanes";
import { useHunkTurns } from "../../hooks/useReviews";
import TurnTierBadge, { type TurnTier } from "./TurnTierBadge";

export interface HunkTurnsPanelProps {
  repo: string;
  reviewId: number;
  hunkId: string;
  ps?: string;
}

export default function HunkTurnsPanel({ repo, reviewId, hunkId, ps }: HunkTurnsPanelProps) {
  const q = useHunkTurns(repo, reviewId, hunkId, ps, true);

  if (q.isLoading) {
    return (
      <div className="kbc-hunkturns" data-kbc-hunk-turns-panel={hunkId} data-kbc-hunk-turns-state="loading">
        Loading turns…
      </div>
    );
  }
  if (q.error) {
    const msg = (q.error as Error).message || "loopback only";
    return (
      <div className="kbc-hunkturns" data-kbc-hunk-turns-panel={hunkId} data-kbc-hunk-turns-state="refused">
        <p className="kbc-hunkturns__refusal" data-kbc-hunk-turns-refusal>
          loopback only — {msg}
        </p>
      </div>
    );
  }
  const data = q.data;
  if (!data) return null;

  return (
    <div className="kbc-hunkturns" data-kbc-hunk-turns-panel={hunkId} data-kbc-hunk-turns-state="ok">
      <p className="kbc-hunkturns__basis" data-kbc-hunk-turns-basis={data.commit_basis}>
        {data.commit_basis_caption}
      </p>
      {data.kb_lane === "degraded" && (
        <p className="kbc-hunkturns__degraded" data-kbc-hunk-turns-degraded>
          {data.kb_lane_reason ?? "the kb sibling join is degraded — every tier caps at likely"}
        </p>
      )}
      {data.turns.length === 0 ? (
        <p className="kbc-hunkturns__empty" data-kbc-hunk-turns-empty>
          no match{data.reason ? `: ${data.reason}` : ""}
        </p>
      ) : (
        <ul className="kbc-hunkturns__list" data-kbc-hunk-turns-list>
          {data.turns.map((t) => (
            <li key={t.turn_id} className="kbc-hunkturns__row" data-kbc-hunk-turns-row={t.turn_id}>
              <TurnTierBadge tier={t.tier as TurnTier} />
              <span className="kbc-hunkturns__tool">{t.tool}</span>
              <span className="kbc-hunkturns__path">{t.path}</span>
              {t.commit && <span className="kbc-hunkturns__commit">{t.commit.slice(0, 10)}</span>}
              <a
                className="kbc-hunkturns__link"
                href={`${sessionUrl(t.session_id)}#${t.turn_id}`}
                target="_blank"
                rel="noreferrer"
                data-kbc-hunk-turns-link={t.turn_id}
              >
                open turn
              </a>
              <span className="kbc-hunkturns__why" title={t.why}>
                {t.why}
              </span>
            </li>
          ))}
        </ul>
      )}
      {data.notes.length > 0 && (
        <ul className="kbc-hunkturns__notes" data-kbc-hunk-turns-notes>
          {data.notes.map((n, i) => (
            <li key={i}>{n}</li>
          ))}
        </ul>
      )}
    </div>
  );
}
