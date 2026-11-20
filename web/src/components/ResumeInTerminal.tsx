// FUWEBLAUNCH — "Resume in a kb terminal" on the session page's Resume
// section: relaunch THIS session's harness in its recorded cwd with
// `resume_sid = sid` (never a bare continue; the runner refuses it). Shown
// only for an admitted caller, a known cwd, a harness in the launchable set
// and a session with no live bound terminal. Nothing is sent until the click.

import { useEffect, useRef, useState } from "react";
import { launchTerm } from "../api/terms";
import { toast } from "../lib/toast";
import { isLaunchHarness, launchBody, validateCwd } from "../lib/termLaunch";
import { LaunchedTerminal } from "./LaunchedTerminal";

type Phase =
  | { kind: "idle" }
  | { kind: "launching" }
  | { kind: "error"; reason: string }
  | { kind: "open"; termId: string };

export function ResumeInTerminal({
  sessionId,
  harness,
  cwd,
  terminalAdmitted,
  hasLiveTerminal,
  pollMs,
  deadlineMs,
}: {
  sessionId: string;
  harness: string;
  cwd?: string | null;
  terminalAdmitted: boolean;
  /// `card.terminal.available`: a live bound terminal already exists.
  hasLiveTerminal: boolean;
  /// Binding-poll timings (tests shorten them; defaults in `termLaunch.ts`).
  pollMs?: number;
  deadlineMs?: number;
}) {
  const [phase, setPhase] = useState<Phase>({ kind: "idle" });
  const ctl = useRef<AbortController | null>(null);
  useEffect(() => () => ctl.current?.abort(), []);

  const eligible =
    terminalAdmitted &&
    !hasLiveTerminal &&
    isLaunchHarness(harness) &&
    !!cwd &&
    validateCwd(cwd) === null;
  // A launched panel stays mounted even if the card later flips to "live".
  if (!eligible && phase.kind !== "open") return null;

  const go = () => {
    if (!cwd) return;
    ctl.current?.abort();
    const c = new AbortController();
    ctl.current = c;
    setPhase({ kind: "launching" });
    launchTerm(launchBody(harness, cwd, sessionId), c.signal).then(
      (r) => {
        if (!c.signal.aborted) setPhase({ kind: "open", termId: r.id });
      },
      (e: unknown) => {
        if (c.signal.aborted) return;
        const reason = e instanceof Error ? e.message : String(e);
        toast.err(`terminal launch failed: ${reason}`);
        setPhase({ kind: "error", reason });
      },
    );
  };

  return (
    <div className="kb-ses__handoff" data-testid="session-resume-term">
      {phase.kind !== "open" && (
        <div className="kb-ses__handoff-actions">
          <button
            type="button"
            className="kb-ses__resume-copy"
            data-testid="session-resume-term-go"
            disabled={phase.kind === "launching"}
            onClick={go}
            title={`Runs the ${harness} resume for this session in a kb terminal on the daemon's host`}
          >
            Resume in a kb terminal
          </button>
          {phase.kind === "launching" && (
            <span className="kb-ses__handoff-meta" data-testid="session-resume-term-launching">
              launching terminal…
            </span>
          )}
        </div>
      )}
      {phase.kind === "error" && (
        <p className="kb-ses__ins-errors" role="alert" data-testid="session-resume-term-error">
          terminal launch failed: {phase.reason}
        </p>
      )}
      {phase.kind === "open" && (
        <LaunchedTerminal
          termId={phase.termId}
          harness={harness}
          sid={sessionId}
          pollMs={pollMs}
          deadlineMs={deadlineMs}
          onClose={() => setPhase({ kind: "idle" })}
        />
      )}
    </div>
  );
}
