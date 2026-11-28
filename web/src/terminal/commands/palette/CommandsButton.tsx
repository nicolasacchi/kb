// CMDPALETTE: the header Commands button, owner of the palette's open state
// and of the Alt+K shortcut. Passed through TerminalPanel's `headerActions`.
//
// Alt+K is a window keydown listener in the CAPTURE phase, registered only
// while this button is mounted (the terminal page). It matches
// `event.code === "KeyK"` with Alt alone, and stops the event so the pane never
// sees it. No bare key and no Ctrl chord is bound. Alt+K reaches a pty as
// ESC-k (Meta-k): the one accepted loss, named in the dialog footer.

import { useCallback, useEffect, useRef, useState, type ReactElement } from "react";
import { usePhonePanel } from "../../../hooks/useIsMobile";
import { CommandPalette } from "./CommandPalette";
import { focusTerminalPane } from "./sendEntry";

export function isPaletteChord(e: KeyboardEvent): boolean {
  return e.code === "KeyK" && e.altKey && !e.ctrlKey && !e.metaKey && !e.shiftKey && !e.isComposing;
}

export function CommandsButton({ termId, harness }: { termId: string; harness: string }): ReactElement {
  const [open, setOpen] = useState(false);
  const phone = usePhonePanel();
  const btn = useRef<HTMLButtonElement | null>(null);
  const restore = useRef<"button" | "pane" | null>(null);

  useEffect(() => {
    const onKey = (e: KeyboardEvent): void => {
      if (!isPaletteChord(e)) return;
      e.preventDefault();
      e.stopPropagation();
      setOpen(true);
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, []);

  // Focus moves only AFTER the modal dialog is gone (a modal dialog makes the rest inert).
  useEffect(() => {
    if (open || restore.current === null) return;
    const to = restore.current;
    restore.current = null;
    if (to === "pane") focusTerminalPane();
    else btn.current?.focus();
  }, [open]);

  const dismiss = useCallback(() => {
    restore.current = "button";
    setOpen(false);
  }, []);
  const sent = useCallback(() => {
    restore.current = phone ? "button" : "pane";
    setOpen(false);
  }, [phone]);

  // The phone header has no height to spare (the landscape dock keeps half the
  // sheet for the pane); the phone reaches the same list through the rail's
  // Commands tab, so no header button there.
  if (phone) return <></>;

  return (
    <>
      <button
        ref={btn}
        type="button"
        className="kb-cmdp__open"
        data-testid="kb-term-commands-btn"
        aria-haspopup="dialog"
        aria-keyshortcuts="Alt+K"
        onClick={() => setOpen(true)}
      >
        Commands
      </button>
      {open && <CommandPalette termId={termId} harness={harness} onDismiss={dismiss} onSent={sent} />}
    </>
  );
}
