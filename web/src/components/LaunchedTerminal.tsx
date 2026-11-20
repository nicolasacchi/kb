// FUWEBLAUNCH — the inline terminal of a just-launched kb-term session, shared
// by "New terminal" (/sessions) and "Resume in a kb terminal" (session page).
//
// The panel mounts at once with the returned term id: a harness in a
// never-trusted directory shows its trust prompt BEFORE any hook runs, so the
// session may stay UNBOUND until the operator answers it in the terminal.
// Binding is read from the existing `["terms"]` query (`useTerms`); while the
// row is unbound this component invalidates that query every `pollMs` until a
// session id appears, the pane dies or `deadlineMs` passes (no SSE path, no
// new fetcher; the hook itself stays non-polling). The link to the bound
// session is shown, never followed on its own.

import { Suspense, lazy, useEffect, useRef, useState } from "react";
import { Link } from "react-router-dom";
import { useQueryClient } from "@tanstack/react-query";
import { StateCard } from "@kb/ui/StateCard";
import { useTerms } from "../terminal/useTerms";
import { BOUND_DEADLINE_MS, BOUND_POLL_MS } from "../lib/termLaunch";

// Same lazy seam as `TerminalSlot`/`HandoffPanel`: xterm loads after a launch.
const TerminalPanel = lazy(() => import("../terminal/TerminalPanel.lazy"));

export function LaunchedTerminal({
  termId,
  harness,
  sid,
  onClose,
  pollMs = BOUND_POLL_MS,
  deadlineMs = BOUND_DEADLINE_MS,
}: {
  termId: string;
  harness: string;
  /// The session this launch resumes, when known (panel context only).
  sid?: string;
  onClose: () => void;
  pollMs?: number;
  deadlineMs?: number;
}) {
  const qc = useQueryClient();
  const { terms } = useTerms();
  const row = terms.find((t) => t.id === termId);
  const boundSid = row?.session_id ?? null;
  const dead = row?.pane_dead === true;
  const [timedOut, setTimedOut] = useState(false);
  const stop = boundSid !== null || dead;
  const startedAt = useRef(Date.now());

  useEffect(() => {
    if (stop) return;
    startedAt.current = Date.now();
    const iv = setInterval(() => {
      if (Date.now() - startedAt.current >= deadlineMs) {
        clearInterval(iv);
        setTimedOut(true);
        return;
      }
      void qc.invalidateQueries({ queryKey: ["terms"] });
    }, pollMs);
    return () => clearInterval(iv);
  }, [stop, qc, pollMs, deadlineMs]);

  return (
    <div className="kb-ses__launched" data-testid="launched-terminal" data-term={termId}>
      {dead && (
        <p className="kb-ses__ins-errors" role="alert" data-testid="launched-terminal-dead">
          the terminal ended
          {row?.exit_code != null ? ` (exit code ${row.exit_code})` : ""}
        </p>
      )}
      {boundSid ? (
        <p className="kb-ses__handoff-meta" data-testid="launched-terminal-bound">
          <Link to={`/sessions/${encodeURIComponent(boundSid)}#sec-terminal`}>
            open this session's page
          </Link>
        </p>
      ) : (
        !dead && (
          <p className="kb-ses__handoff-meta" data-testid="launched-terminal-unbound">
            {timedOut
              ? "No session has appeared yet. It shows up in the list once the harness starts; answer any prompt in the terminal below."
              : "The session appears in the list once the harness starts. Answer any prompt in the terminal below."}
          </p>
        )
      )}
      <Suspense fallback={<StateCard kind="loading" title="Loading terminal" reason={harness} />}>
        <TerminalPanel
          termId={termId}
          sid={boundSid ?? sid ?? ""}
          harness={harness}
          mode="watch"
          onClose={onClose}
        />
      </Suspense>
    </div>
  );
}
