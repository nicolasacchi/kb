// T14 — the terminal seam, filled: the panel mounts lazily after an
// explicit click, in `<Suspense>` with A4's `StateCard` fallback.
//
// Contract (A4's file, seam body only):
//   * lane `available: false` (or unadmitted) → nothing.
//   * `available: true` with a term → "Terminal" header with the state
//     chip + "Open terminal"; clicking mounts `<TerminalPanel>`
//     (lazily, via `React.lazy`) and "Close terminal" unmounts it
//     (disconnect only, never kill).
//   * a child (subagent) session whose lane resolves to the ROOT's
//     term (`via: "root"`) shows a link to the session and no panel.
// The slot imports nothing from the terminal engine packages; the only
// engine edge is the dynamic `lazy(...)` import below, so the panel
// chunk is fetched only after the click.

import { Suspense, lazy, useState, type ReactElement } from "react";
import { Link } from "react-router-dom";
import { StateCard } from "@kb/ui/StateCard";
import type { TerminalLane } from "../api/sessions";

export type TerminalSlotProps = {
  sid: string;
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
  const { lane } = props;
  const termId = props.termId ?? lane.term_id;
  if (!lane.available || !lane.admitted) return null;
  if (lane.via === "root") {
    return (
      <Link
        to={`/sessions/${encodeURIComponent(props.sid)}`}
        data-testid="session-term-root-link"
      >
        Terminal in parent session
      </Link>
    );
  }
  if (!termId) return null;
  const harness = lane.harness ?? props.harness;
  return (
    <div data-testid="kb-terminal-slot">
      <span data-testid="kb-term-slot-state">{lane.state ?? ""}</span>
      <button
        type="button"
        data-testid="kb-term-open"
        onClick={() => setOpen((v) => !v)}
      >
        {open ? "Close terminal" : "Open terminal"}
      </button>
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
            mode="watch"
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
  void lane;
  return false;
}
