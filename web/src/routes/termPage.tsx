// BSHELL — /terminals/:id (HUBDOOR; /term/:id redirects here), the terminal's own page. A thin route over `TermShell`:
// it reads the id (`openTermParam`), the `["terms"]` row (`useTerms`) and the
// admission bit, then wraps the lazy `TerminalPanel` (the same seam the slot
// and the launched-terminal host use, so xterm stays out of the main chunk).

import { Suspense, lazy, useEffect, useRef, type ReactElement } from "react";
import { Link, useLocation, useNavigate, useParams } from "react-router-dom";
import { useQueryClient } from "@tanstack/react-query";
import { StateCard } from "@kb/ui/StateCard";
import { useIdentity } from "../hooks/useArtifactHost";
import { useIsMobile } from "../hooks/useIsMobile";
import { openTermParam } from "../components/TermList";
import { TerminalControls, cwdBase } from "../components/TermControls";
import { TermShell } from "../terminal/TermShell";
import { useTerms } from "../terminal/useTerms";
import { buildHubUrl } from "../terminal/hub/termHub";

const TerminalPanel = lazy(() => import("../terminal/TerminalPanel.lazy"));

/** A just-launched term whose row may not be listed yet (navigation state). */
type LaunchHint = { id: string; harness: string };

function launchHint(state: unknown): LaunchHint | undefined {
  const l = (state as { launched?: { id?: unknown; harness?: unknown } } | null)?.launched;
  return l && typeof l.id === "string" && typeof l.harness === "string"
    ? { id: l.id, harness: l.harness }
    : undefined;
}

export default function TermPageRoute(): ReactElement {
  const { id } = useParams();
  const termId = openTermParam(id ?? null);
  const identity = useIdentity();
  const { terms, isLoading, isError } = useTerms();
  const qc = useQueryClient();
  const navigate = useNavigate();
  const location = useLocation();
  // The panel owns the phone/desktop choice; the End/switcher placement keeps the
  // width predicate the hosts always used (a landscape phone keeps the dock tall).
  const narrow = useIsMobile();
  const row = termId ? terms.find((t) => t.id === termId) : undefined;
  const seen = useRef(false);
  if (row) seen.current = true;
  // The launch hint is consumed once: remembered here, then cleared from
  // history.state so a reload cannot resurrect a panel for a dead id.
  const hintRef = useRef<LaunchHint | undefined>(undefined);
  const stateHint = launchHint(location.state);
  if (stateHint) hintRef.current = stateHint;
  const hint = hintRef.current;
  const hinted = termId !== null && hint?.id === termId && !seen.current ? hint : undefined;
  const hasStateHint = stateHint !== undefined;
  useEffect(() => {
    if (hasStateHint) void navigate({ pathname: location.pathname, search: location.search, hash: location.hash }, { replace: true, state: null });
  }, [hasStateHint, navigate, location.pathname, location.search, location.hash]);
  const harness = row?.harness ?? hinted?.harness ?? "terminal";
  const base = row ? cwdBase(row.cwd) : "";

  // The tab title names the terminal while it is open; restored on unmount.
  useEffect(() => {
    if (!row && !hinted) return;
    const prev = document.title;
    document.title = base ? `${harness} · ${base} — terminal` : `${harness} — terminal`;
    return () => {
      document.title = prev;
    };
  }, [row, hinted, harness, base]);

  const retry = (): void => void qc.invalidateQueries({ queryKey: ["terms"] });

  if (identity === null) {
    return <StateCard kind="loading" title="Loading terminal" />;
  }
  if (identity.terminal_admitted !== true) {
    return (
      <StateCard
        kind="refused"
        title="Terminals are not enabled"
        reason="This daemon does not admit terminal access for this caller."
      />
    );
  }
  const notFound = (title: string): ReactElement => (
    <div data-testid="term-not-found">
      <StateCard kind="empty" title={title} reason="It may have ended." />
      <p>
        <Link to="/sessions">Back to sessions</Link>
      </p>
    </div>
  );
  if (termId === null) return notFound("Not a terminal id");
  if (!row && !hinted) {
    if (isLoading) return <StateCard kind="loading" title="Loading terminal" reason={termId} />;
    if (isError) {
      return (
        <div data-testid="term-list-error">
          <StateCard
            kind="degraded"
            title="Cannot read the terminal list"
            reason={`Terminal ${termId} cannot be opened yet.`}
            actions={[{ label: "Retry", onClick: retry, key: "retry" }]}
          />
        </div>
      );
    }
    return notFound("Terminal not found");
  }

  const sessionId = row?.session_id ?? undefined;
  const controls = (
    <TerminalControls
      target={{ termId, harness, cwd: row?.cwd, stale: row?.stale === true }}
      onSwitch={narrow ? (to) => navigate(buildHubUrl({ id: to })) : undefined}
      className={narrow ? "kb-term-phone__btn" : undefined}
      onEnded={retry}
    />
  );
  const close = (): void => {
    void navigate(sessionId ? `/sessions/${encodeURIComponent(sessionId)}` : "/sessions", { replace: true });
  };

  return (
    <TermShell key={termId} termId={termId} harness={harness} sid={sessionId} header={narrow ? undefined : controls}>
      <Suspense fallback={<StateCard kind="loading" title="Loading terminal" reason={harness} />}>
        <TerminalPanel
          termId={termId}
          sid={sessionId ?? ""}
          harness={harness}
          onClose={close}
          headerExtra={narrow ? controls : undefined}
        />
      </Suspense>
    </TermShell>
  );
}
