// HUBSWITCH — the frame around one terminal's page: on desktop a collapsible
// switcher column beside the shell; on a phone no column (the phone switcher
// lives in the panel's own chrome). HubFrame sits OUTSIDE
// `<TermShell key={termId}>`, so switching terminals keeps the column mounted
// (and its scroll position) while the shell remounts.

import { useCallback, useId, useState, type ReactElement, type ReactNode } from "react";
import { usePhonePanel } from "../../hooks/useIsMobile";
import { HubSwitcher } from "./HubSwitcher";
import { SWITCH_OPEN_KEY } from "./switchModel";
import { useSwitchKeys } from "./useSwitchKeys";
import { useSwitchRows } from "./useSwitchRows";
import "../../styles/hubSwitcher.css";

/**
 * The column opens by default only on a wide window: beside the shell's own
 * rail it would otherwise crowd the header and crop wide grids. A stored
 * choice ("1" open, "0" closed) always wins.
 */
export const SWITCH_AUTO_OPEN_MIN_WIDTH = 1600;

function readOpen(): boolean {
  try {
    const v = window.localStorage.getItem(SWITCH_OPEN_KEY);
    if (v === "1") return true;
    if (v === "0") return false;
  } catch {
    // fall through to the width default
  }
  return window.innerWidth >= SWITCH_AUTO_OPEN_MIN_WIDTH;
}

function writeOpen(open: boolean): void {
  try {
    window.localStorage.setItem(SWITCH_OPEN_KEY, open ? "1" : "0");
  } catch {
    // storage unavailable: the choice just does not persist
  }
}

/** Mounts the `terminals` key scope; a component so a phone frame can leave it out. */
function SwitchKeys({ termId }: { termId: string | null }): null {
  // Lives in the frame, not the column: collapsing the column must not turn the keys off.
  useSwitchKeys(useSwitchRows(), termId);
  return null;
}

/**
 * The wrapper and the children's position are the same on a phone and on
 * desktop, so rotating across the breakpoint never remounts the shell (and
 * its terminal socket); only the aside and the key scope come and go.
 */
export function HubFrame({ termId, children }: { termId: string | null; children: ReactNode }): ReactElement {
  const phone = usePhonePanel();
  const [open, setOpen] = useState<boolean>(readOpen);
  const panelId = useId();
  const toggle = useCallback(() => {
    setOpen((o) => {
      writeOpen(!o);
      return !o;
    });
  }, []);
  return (
    <div
      className="kb-hubframe"
      data-phone={phone ? "1" : "0"}
      data-open={open ? "1" : "0"}
      data-testid={phone ? undefined : "hub-frame"}
    >
      {phone ? null : (
        <aside className="kb-hubframe__side">
          <button
            type="button"
            className="kb-hubframe__toggle"
            data-testid="hub-switch-toggle"
            aria-expanded={open}
            aria-controls={panelId}
            onClick={toggle}
          >
            {open ? "Hide terminals" : "Terminals"}
          </button>
          <div id={panelId} className="kb-hubframe__panel" hidden={!open}>
            {open ? <HubSwitcher currentId={termId} /> : null}
          </div>
        </aside>
      )}
      <div className="kb-hubframe__main">{children}</div>
      {phone ? null : <SwitchKeys termId={termId} />}
    </div>
  );
}
