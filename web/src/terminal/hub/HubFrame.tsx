// HUBSWITCH — the frame around one terminal's page: on desktop a collapsible
// switcher column beside the shell; on a phone just the children (the phone
// switcher lives in the panel's own chrome). HubFrame sits OUTSIDE
// `<TermShell key={termId}>`, so switching terminals keeps the column mounted
// (and its scroll position) while the shell remounts.

import { useCallback, useId, useState, type ReactElement, type ReactNode } from "react";
import { usePhonePanel } from "../../hooks/useIsMobile";
import { HubSwitcher } from "./HubSwitcher";
import { SWITCH_OPEN_KEY } from "./switchModel";
import "../../styles/hubSwitcher.css";

function readOpen(): boolean {
  try {
    return window.localStorage.getItem(SWITCH_OPEN_KEY) !== "0";
  } catch {
    return true;
  }
}

function writeOpen(open: boolean): void {
  try {
    window.localStorage.setItem(SWITCH_OPEN_KEY, open ? "1" : "0");
  } catch {
    // storage unavailable: the choice just does not persist
  }
}

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
  if (phone) return <>{children}</>;
  return (
    <div className="kb-hubframe" data-open={open ? "1" : "0"} data-testid="hub-frame">
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
      <div className="kb-hubframe__main">{children}</div>
    </div>
  );
}
