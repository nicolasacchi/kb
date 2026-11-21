// T14 — the terminal seam, filled: the panel mounts lazily after an
// explicit click, in `<Suspense>` with A4's `StateCard` fallback.
//
// Contract (A4's file, seam body only):
//   * lane `available: false` (or unadmitted) → nothing.
//   * `available: true` with a term → "Terminal" header with the state
//     chip + "Open terminal"; clicking mounts `<TerminalPanel>`
//     (lazily, via `React.lazy`). The panel's own Close (`onClose`)
//     unmounts it (disconnect only; End is the separate, confirmed kill) and brings the button
//     back: the slot has no second Close.
//   * a child (subagent) session whose lane resolves to the ROOT's
//     term (`via: "root"`) shows a link to the session and no panel.
// The slot imports nothing from the terminal engine packages; the only
// engine edge is the dynamic `lazy(...)` import below, so the panel
// chunk is fetched only after the click.

import { Suspense, lazy, useState, type ReactElement } from "react";
import { Link } from "react-router-dom";
import { StateCard } from "@kb/ui/StateCard";
import type { TerminalLane } from "../api/sessions";
import { useIsMobile } from "../hooks/useIsMobile";
import { TerminalControls } from "./TermList";

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

const TerminalPanel = lazy(() => import("../terminal/TerminalPanel.lazy"));

export function TerminalSlot(props: TerminalSlotProps): ReactElement | null {
  const [open, setOpen] = useState(false);
  const isMobile = useIsMobile();
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
  const harness = lane.harness ?? props.harness;
  // FUTERMLIST — the End action, once: above the panel on a desktop, in the
  // sheet's title area on a phone. Close stays the panel's own plain detach.
  const controls = (
    <TerminalControls
      target={{ termId, harness }}
      className={isMobile ? "kb-term-phone__btn" : undefined}
    />
  );
  return (
    <div className="kb-term-slot" data-testid="kb-terminal-slot">
      {open ? null : (
        <div className="kb-term-slot__head">
          <span className="kb-term-slot__state" data-testid="kb-term-slot-state">
            {lane.state ?? ""}
          </span>
          <button
            type="button"
            className="kb-term__btn kb-term-slot__open"
            data-testid="kb-term-open"
            onClick={() => setOpen(true)}
          >
            Open terminal
          </button>
        </div>
      )}
      {open && !isMobile ? (
        <div className="kb-term-slot__ctl" data-testid="kb-term-slot-ctl">
          {controls}
        </div>
      ) : null}
      {open ? (
        <Suspense
          fallback={
            <StateCard kind="loading" title="Loading terminal" reason={harness} />
          }
        >
          <TerminalPanel
            termId={termId}
            sid={props.sid}
            harness={harness}
            onClose={() => setOpen(false)}
            headerExtra={isMobile ? controls : undefined}
          />
        </Suspense>
      ) : null}
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
