// FUTERMLIST — the End action, its button, the per-host controls and the phone
// sheet's terminal switcher. Shared by the Terminals list, `TerminalSlot` and
// `LaunchedTerminal` (one home for the action, #30); kept apart from `TermList`
// so the hosts do not import the list that imports them.
//
// End = `useConfirm` (#32), then ONE `POST /api/terms/{id}/kill` through the
// terminal-tier helper. The client never marks a terminal ended: the exit frame
// and the `term.ended` SSE do. A failure is `toast.err` and leaves everything as
// it was. Close elsewhere stays a plain detach.

import { useCallback, useRef, useState, type ReactElement } from "react";
import type { TermRow } from "../api/terms";
import { killTerm } from "../api/terms";
import { useTerms } from "../terminal/useTerms";
import { toast } from "../lib/toast";
import { useConfirmOptional } from "./ConfirmProvider";

export type EndTarget = {
  termId: string;
  harness: string;
  cwd?: string | null;
  /// The holder is gone: the action is "Remove" (clears the leftover row).
  stale?: boolean;
};

/** The confirm-then-kill action. `null` outside a confirm host (nothing to end with). */
export function useEndTerm(onEnded?: () => void): {
  end: (t: EndTarget) => Promise<void>;
  pending: boolean;
  available: boolean;
} {
  const confirm = useConfirmOptional();
  const [pending, setPending] = useState(false);
  const busy = useRef(false);
  const end = useCallback(
    async (t: EndTarget): Promise<void> => {
      if (!confirm || busy.current) return;
      busy.current = true;
      setPending(true);
      try {
        const where = t.cwd ? ` in ${t.cwd}` : "";
        const ok = await confirm(
          t.stale
            ? {
                title: "Remove terminal",
                body: `No process holds this ${t.harness} terminal${where} any more. This clears the leftover entry.`,
                confirmLabel: "Remove",
              }
            : {
                title: "End terminal",
                body: `Ends the ${t.harness} session${where}: its process stops and the terminal cannot be reopened. Close only disconnects.`,
                confirmLabel: "End terminal",
              },
        );
        if (!ok) return;
        try {
          await killTerm(t.termId);
          onEnded?.();
        } catch (e) {
          toast.err(`could not end the terminal: ${e instanceof Error ? e.message : String(e)}`);
        }
      } finally {
        busy.current = false;
        setPending(false);
      }
    },
    [confirm, onEnded],
  );
  return { end, pending, available: confirm !== null };
}

/** The End button (also Remove for a stale row). Renders nothing without a confirm host. */
export function EndTerminalButton({
  target,
  className = "kb-term__btn",
  onEnded,
}: {
  target: EndTarget;
  className?: string;
  onEnded?: () => void;
}): ReactElement | null {
  const { end, pending, available } = useEndTerm(onEnded);
  if (!available) return null;
  return (
    <button
      type="button"
      className={`${className} kb-term__end`}
      data-testid="kb-term-end"
      disabled={pending}
      onClick={() => void end(target)}
    >
      {target.stale ? "Remove" : "End"}
    </button>
  );
}

/**
 * The controls a panel host adds around the panel: End (always), and on a phone
 * the terminal switcher. Hosts render it once, either above the panel (desktop)
 * or through the panel's `headerExtra` (phone sheet), never both.
 */
export function TerminalControls({
  target,
  onSwitch,
  className,
  onEnded,
}: {
  target: EndTarget;
  /// Phone only: switch to another live terminal.
  onSwitch?: (termId: string) => void;
  className?: string;
  /// After a successful End (a list host refreshes its rows; the SSE bridge does too).
  onEnded?: () => void;
}): ReactElement {
  return (
    <>
      {onSwitch ? <TermSwitcher currentId={target.termId} onSwitch={onSwitch} /> : null}
      <EndTerminalButton target={target} className={className} onEnded={onEnded} />
    </>
  );
}

export function cwdBase(cwd: string): string {
  const parts = cwd.split("/").filter((p) => p !== "");
  return parts.length ? (parts[parts.length - 1] as string) : cwd;
}

/** A terminal the operator can still reach (not a dead/stale leftover). */
export function isLive(r: Pick<TermRow, "pane_dead" | "stale">): boolean {
  return !r.pane_dead && !r.stale;
}

/** The phone sheet's compact switcher over the OTHER live terminals (one tap). */
export function TermSwitcher({
  currentId,
  onSwitch,
}: {
  currentId: string;
  onSwitch: (termId: string) => void;
}): ReactElement | null {
  const { terms } = useTerms();
  const others = terms.filter((t) => t.id !== currentId && isLive(t));
  if (others.length === 0) return null;
  return (
    <select
      className="kb-term-phone__btn kb-term__switch"
      data-testid="kb-term-switch"
      aria-label="Switch terminal"
      value=""
      onChange={(e) => {
        if (e.target.value) onSwitch(e.target.value);
      }}
    >
      <option value="">Switch ({others.length})</option>
      {others.map((t) => (
        <option key={t.id} value={t.id}>
          {t.harness} · {cwdBase(t.cwd)}
          {t.blocked ? " · waiting" : ""}
        </option>
      ))}
    </select>
  );
}

