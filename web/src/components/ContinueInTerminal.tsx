// HUBHOST — "Continue in terminal": a door from a session (its page, a list
// row) to the Terminals hub's launcher, `/terminals?launch=1&resume=<sid>`.
// It is a LINK, never a launch: the hub opens the dialog prefilled from the
// session's own row and the operator confirms there. Shown only when the
// terminal tier is admitted, the session is resumable (`resumeRefusal`: the one
// rule) and no live terminal is already bound to it.

import type { MouseEvent } from "react";
import { Link } from "react-router-dom";
import { useIdentity } from "../hooks/useArtifactHost";
import { resumeRefusal, type ResumeSource } from "../terminal/hub/launchResume";
import { buildHubUrl } from "../terminal/hub/termHub";

/** True when the door should render. `cwd` may be absent (the row has none). */
export function useContinueEligible(row: ResumeSource, hasLiveTerminal = false): boolean {
  const identity = useIdentity();
  return identity?.terminal_admitted === true && !hasLiveTerminal && resumeRefusal(row) === null;
}

export function ContinueInTerminal({
  sessionId,
  harness,
  cwd,
  hasLiveTerminal = false,
  className,
  onClick,
}: {
  sessionId: string;
  harness: string;
  cwd?: string | null;
  /// A live bound terminal already exists for this session.
  hasLiveTerminal?: boolean;
  className?: string;
  onClick?: (e: MouseEvent<HTMLAnchorElement>) => void;
}) {
  const ok = useContinueEligible({ session_id: sessionId, harness, cwd: cwd ?? undefined }, hasLiveTerminal);
  if (!ok) return null;
  return (
    <Link
      to={buildHubUrl({ launch: true, resume: sessionId })}
      className={className}
      data-testid="continue-in-terminal"
      title={`Opens the launcher to resume this ${harness} session in a kb terminal; nothing starts until you confirm`}
      onClick={onClick}
    >
      Continue in terminal
    </Link>
  );
}
