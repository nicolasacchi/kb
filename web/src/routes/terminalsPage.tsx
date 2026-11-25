// HUBDOOR — /terminals, the Terminals hub's list page. Pure client join of the
// `["terms"]` rows and the live-status rows (HUBCORE: joinHub / filterHub /
// orderHub); no server of its own. Rows open /terminals/:id. Live-status is a
// LEFT join: while it loads or fails the list still renders from the term rows.
// HUBHOST: launching is a first-class action of this page. "New session" opens
// the LaunchDialog; `?launch=1&resume=<sid>` OPENS it prefilled (a GET never
// launches, the operator confirms) and the two params are stripped at once.
// The dialog is mounted above every state of the page (loading, error, empty).
// The switcher column, actions and labels/pins UI belong to later units.

import { useCallback, useEffect, useMemo, useRef, useState, type ReactElement } from "react";
import { Link, useLocation, useNavigate } from "react-router-dom";
import { useQueryClient } from "@tanstack/react-query";
import { StateCard } from "@kb/ui/StateCard";
import { useIdentity } from "../hooks/useArtifactHost";
import { useLiveSessions } from "../hooks/useLiveSessions";
import { useNowTick } from "../hooks/useNowTick";
import { relativeAge } from "../lib/time";
import { isLaunchHarness } from "../lib/termLaunch";
import { useTerms } from "../terminal/useTerms";
import { LaunchDialog } from "../terminal/hub/LaunchDialog";
import { EndTerminalButton } from "../components/TermControls";
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

function HubItem({ row, now, onEnded }: { row: HubRow; now: number; onEnded: () => void }): ReactElement {
  const { term, live, tier } = row;
  const since = tier === "needs" ? row.needsSince : term.started_unix;
  const age = relativeAge(since, now);
  return (
    <li className="kb-hub__item" data-testid="hub-item" data-term={term.id}>
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
      {/* End (Remove for a dead/stale row) stays reachable from the hub now that
          /sessions no longer lists terminals; HUBACT owns the full action set. */}
      <EndTerminalButton
        className="kb-hub__end"
        onEnded={onEnded}
        target={{ termId: term.id, harness: term.harness, cwd: term.cwd, stale: term.stale || term.pane_dead }}
      />
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

  // Deep link, consumed ONCE: latch `launch`/`resume` from the URL we mounted
  // at, then strip them (replace, filters kept) so reload and Back never
  // reopen the dialog. Only the dialog's Launch click ever POSTs.
  const [deep] = useState(() => {
    const u = parseHubUrl(loc.pathname, loc.search);
    return u.launch ? { resume: u.resume } : null;
  });
  const [launchOpen, setLaunchOpen] = useState(deep !== null);
  const [prefill, setPrefill] = useState<{ resumeSid: string } | undefined>(
    deep?.resume ? { resumeSid: deep.resume } : undefined,
  );
  const newBtn = useRef<HTMLButtonElement | null>(null);
  useEffect(() => {
    if (deep !== null) navigate(buildHubUrl({ filter }), { replace: true });
    // Mount-only: the latch above already holds the values.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);
  const refresh = useCallback(() => void qc.invalidateQueries({ queryKey: ["terms"] }), [qc]);
  const openLaunch = useCallback(() => {
    setPrefill(undefined);
    setLaunchOpen(true);
  }, []);
  const closeLaunch = useCallback(() => {
    setLaunchOpen(false);
    setPrefill(undefined);
    queueMicrotask(() => newBtn.current?.focus());
  }, []);
  const onLaunched = useCallback(
    (id: string, harness: string) => {
      void qc.invalidateQueries({ queryKey: ["terms"] });
      navigate(buildHubUrl({ id }), { state: { launched: { id, harness } } });
    },
    [qc, navigate],
  );

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

  let state: ReactElement | null = null;
  if (isLoading) state = <StateCard kind="loading" title="Loading terminals" />;
  else if (isError && terms.length === 0) {
    state = (
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
  } else if (terms.length === 0) {
    state = (
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
        {state === null && (
          <span className={`kb-hub__count${needsCount > 0 ? " is-needs" : ""}`} data-testid="hub-needs-count">
            {needsCount} need you
          </span>
        )}
        <button
          ref={newBtn}
          type="button"
          className="kb-hub__new"
          data-testid="hub-new"
          onClick={openLaunch}
        >
          New session
        </button>
      </header>
      {state}
      {state === null && (
      <>
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
              <HubItem key={r.term.id} row={r} now={now} onEnded={refresh} />
            ))}
          </ul>
          {endedRows.length > 0 && (
            <details className="kb-hub__ended" data-testid="hub-ended">
              <summary>Ended ({endedRows.length})</summary>
              <ul className="kb-hub__list">
                {endedRows.map((r) => (
                  <HubItem key={r.term.id} row={r} now={now} onEnded={refresh} />
                ))}
              </ul>
            </details>
          )}
        </>
      )}
      </>
      )}
      <LaunchDialog
        open={launchOpen}
        admitted
        prefill={prefill}
        onClose={closeLaunch}
        onLaunched={onLaunched}
      />
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
