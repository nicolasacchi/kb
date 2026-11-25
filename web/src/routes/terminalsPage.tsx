// HUBDOOR — /terminals, the Terminals hub's list page. Pure client join of the
// `["terms"]` rows and the live-status rows (HUBCORE: joinHub / filterHub /
// orderHub); no server of its own. Rows open /terminals/:id. Live-status is a
// LEFT join: while it loads or fails the list still renders from the term rows.
// Launching, the switcher column, actions and labels/pins UI belong to later
// units; this page only reads.

import { useEffect, useMemo, type ReactElement } from "react";
import { Link, useLocation, useNavigate } from "react-router-dom";
import { useQueryClient } from "@tanstack/react-query";
import { StateCard } from "@kb/ui/StateCard";
import { useIdentity } from "../hooks/useArtifactHost";
import { useLiveSessions } from "../hooks/useLiveSessions";
import { useNowTick } from "../hooks/useNowTick";
import { relativeAge } from "../lib/time";
import { isLaunchHarness } from "../lib/termLaunch";
import { useTerms } from "../terminal/useTerms";
import { loadPrefs } from "../terminal/hub/termPrefs";
import {
  buildHubUrl,
  filterHub,
  hubFilterOptions,
  joinHub,
  orderHub,
  parseHubUrl,
  type HubRow,
  type HubTier,
} from "../terminal/hub/termHub";
import "../styles/terminalsHub.css";

const TIER_LABEL: Record<HubTier, string> = {
  needs: "needs you",
  pinned: "pinned",
  working: "working",
  idle: "idle",
  ended: "ended",
};

function HubItem({ row, now }: { row: HubRow; now: number }): ReactElement {
  const { term, live, tier } = row;
  const since = tier === "needs" ? row.needsSince : term.started_unix;
  const age = relativeAge(since, now);
  return (
    <li>
      <Link
        className="kb-hub__row"
        to={buildHubUrl({ id: term.id })}
        data-testid="hub-row"
        data-tier={tier}
        data-term={term.id}
      >
        <span className="kb-hub__tier" data-tier={tier}>
          {TIER_LABEL[tier]}
        </span>
        <span className="kb-hub__main">
          <span className="kb-hub__name">{row.title}</span>
          <span className="kb-hub__meta">
            {row.project} · {term.harness}
            {live?.last_line ? ` · ${live.last_line}` : ""}
          </span>
        </span>
        <span className="kb-hub__age">{age}</span>
      </Link>
    </li>
  );
}

function HubList(): ReactElement {
  const { terms, isLoading, isError } = useTerms();
  const live = useLiveSessions();
  const qc = useQueryClient();
  const navigate = useNavigate();
  const loc = useLocation();
  const now = useNowTick();
  const prefs = useMemo(() => loadPrefs(), []);
  const { filter } = useMemo(() => parseHubUrl(loc.pathname, loc.search), [loc.pathname, loc.search]);

  useEffect(() => {
    const prev = document.title;
    document.title = "Terminals";
    return () => {
      document.title = prev;
    };
  }, []);

  const all = useMemo(() => joinHub(terms, live.rows, prefs), [terms, live.rows, prefs]);
  const { rows, endedStart } = useMemo(() => orderHub(filterHub(all, filter)), [all, filter]);
  const needsCount = useMemo(() => all.filter((r) => r.tier === "needs").length, [all]);
  const options = useMemo(() => hubFilterOptions(all), [all]);

  if (isLoading) return <StateCard kind="loading" title="Loading terminals" />;
  if (isError && terms.length === 0) {
    return (
      <div data-testid="hub-list-error">
        <StateCard
          kind="degraded"
          title="Cannot read the terminal list"
          reason="The daemon did not answer the terminal list."
          actions={[
            { label: "Retry", key: "retry", onClick: () => void qc.invalidateQueries({ queryKey: ["terms"] }) },
          ]}
        />
      </div>
    );
  }
  if (terms.length === 0) {
    return (
      <div data-testid="hub-empty">
        <StateCard kind="empty" title="No terminals yet" reason="Running terminals will be listed here." />
      </div>
    );
  }

  const filtered = filter.project !== null || filter.harness !== null || filter.needs;
  const liveRows = rows.slice(0, endedStart);
  const endedRows = rows.slice(endedStart);
  const chip = (label: string, to: string, on: boolean, extra = ""): ReactElement => (
    <Link
      key={label + to}
      to={to}
      className={`kb-hub__chip${extra}${on ? " is-on" : ""}`}
      aria-current={on ? "true" : undefined}
      data-testid="hub-chip"
    >
      {label}
    </Link>
  );

  return (
    <section className="kb-hub" data-testid="hub-page" aria-label="Terminals">
      <header className="kb-hub__head">
        <h1 className="kb-hub__title">Terminals</h1>
        <span className={`kb-hub__count${needsCount > 0 ? " is-needs" : ""}`} data-testid="hub-needs-count">
          {needsCount} need you
        </span>
      </header>
      <nav className="kb-hub__filters" aria-label="Filter terminals">
        {chip("Needs you", buildHubUrl({ filter: { ...filter, needs: !filter.needs } }), filter.needs, " kb-hub__chip--needs")}
        {options.projects.map((p) =>
          chip(p, buildHubUrl({ filter: { ...filter, project: filter.project === p ? null : p } }), filter.project === p),
        )}
        {options.harnesses.filter(isLaunchHarness).map((h) =>
          chip(h, buildHubUrl({ filter: { ...filter, harness: filter.harness === h ? null : h } }), filter.harness === h),
        )}
      </nav>
      {rows.length === 0 ? (
        <div data-testid="hub-no-match">
          <StateCard
            kind="empty"
            title="No terminals match"
            reason={filtered ? "No terminal matches these filters." : undefined}
            actions={[{ label: "Clear filters", key: "clear", onClick: () => navigate(buildHubUrl({})) }]}
          />
        </div>
      ) : (
        <>
          <ul className="kb-hub__list" data-testid="hub-live">
            {liveRows.map((r) => (
              <HubItem key={r.term.id} row={r} now={now} />
            ))}
          </ul>
          {endedRows.length > 0 && (
            <details className="kb-hub__ended" data-testid="hub-ended">
              <summary>Ended ({endedRows.length})</summary>
              <ul className="kb-hub__list">
                {endedRows.map((r) => (
                  <HubItem key={r.term.id} row={r} now={now} />
                ))}
              </ul>
            </details>
          )}
        </>
      )}
    </section>
  );
}

export default function TerminalsPageRoute(): ReactElement {
  const identity = useIdentity();
  if (identity === null) return <StateCard kind="loading" title="Loading terminals" />;
  if (identity.terminal_admitted !== true) {
    return (
      <StateCard
        kind="refused"
        title="Terminals are not enabled"
        reason="This daemon does not admit terminal access for this caller."
      />
    );
  }
  return <HubList />;
}
