// T14/BSHELL — the terminal seam on the session page: a door, not a host.
//
// Contract:
//   * lane `available: false` (or unadmitted) → nothing.
//   * `available: true` with a term → the state word plus a link to the
//     terminal's own page (`/term/<id>`, `TermShell`); no panel mounts here.
//   * a child (subagent) session whose lane resolves to the ROOT's
//     term (`via: "root"`) shows a link to the session and no panel.
// The slot imports nothing from the terminal engine packages.

import type { ReactElement } from "react";
import { Link } from "react-router-dom";
import type { TerminalLane } from "../api/sessions";

export type TerminalSlotProps = {
  sid: string;
  // The root session that owns the terminal (card summary's
  // root_session_id); used by the `via: "root"` link on a child page.
  rootSid?: string;
  termId?: string;
  harness: string;
  state?: string;
  // The card's T13 terminal lane (available, optional reason/term_id/
  // harness/state/blocked/via, always-present admitted).
  lane: TerminalLane,
};

export function TerminalSlot(props: TerminalSlotProps): ReactElement | null {
  const { lane } = props;
  const termId = props.termId ?? lane.term_id;
  if (!lane.available || !lane.admitted) return null;
  if (lane.via === "root") {
    if (!props.rootSid) {
      return <span data-testid="session-term-root-link">Terminal in parent session</span>;
    }
    return (
      <Link
        to={`/sessions/${encodeURIComponent(props.rootSid)}`}
        data-testid="session-term-root-link"
      >
        Terminal in parent session
      </Link>
    );
  }
  if (!termId) return null;
  return (
    <div className="kb-term-slot" data-testid="kb-terminal-slot">
      <div className="kb-term-slot__head">
        <span className="kb-term-slot__state" data-testid="kb-term-slot-state">
          {lane.state ?? ""}
        </span>
        <Link
          className="kb-term__btn kb-term-slot__open"
          data-testid="kb-term-open"
          to={`/term/${encodeURIComponent(termId)}`}
        >
          Open terminal
        </Link>
      </div>
    </div>
  );
}

// The page renders the terminal wrapper <details> only when the slot
// can render something: the lane must be available AND admitted.
export function shouldRenderTerminal(
  lane: TerminalSlotProps["lane"],
): boolean {
  return lane.available && lane.admitted;
}
