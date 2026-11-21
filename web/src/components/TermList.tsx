// FUTERMLIST — the Terminals list on /sessions: the one home for "which
// terminals exist", Open, End and Remove; plus the End button and the phone
// sheet's terminal switcher, which reuse the same action.
//
// * Rows come from the existing `["terms"]` query (`useTerms`); the SSE bridge
//   invalidates it, nothing here polls (#23). Terminal bytes never enter it.
// * Open mounts the SAME lazy panel through `LaunchedTerminal` (the one
//   mount a bare term id can use); the open id lives in the URL (`?term=`),
//   so a reload, a share and Back land on the open sheet.
// * End = `useConfirm` (#32), then ONE `POST /api/terms/{id}/kill` through the
//   terminal-tier helper. The client never marks a terminal ended: the exit
//   frame and the `term.ended` SSE do. A failure is `toast.err` and leaves
//   everything as it was. Close elsewhere stays a plain detach.

import { useCallback, useRef, useState, type ReactElement } from "react";
import { Link } from "react-router-dom";
import { useQueryClient } from "@tanstack/react-query";
import { killTerm, type TermRow } from "../api/terms";
import { useTerms } from "../terminal/useTerms";
import { ownerLabel } from "../terminal/gridOwner";
import { toast } from "../lib/toast";
import { useConfirmOptional } from "./ConfirmProvider";
import { LaunchedTerminal } from "./LaunchedTerminal";

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
}: {
  target: EndTarget;
  /// Phone only: switch to another live terminal.
  onSwitch?: (termId: string) => void;
  className?: string;
}): ReactElement {
  return (
    <>
      {onSwitch ? <TermSwitcher currentId={target.termId} onSwitch={onSwitch} /> : null}
      <EndTerminalButton target={target} className={className} />
    </>
  );
}

/** The `?term=` value as an open terminal id: the one kb id grammar, else none. */
export function openTermParam(v: string | null): string | null {
  return v !== null && /^[a-z0-9]{1,32}$/.test(v) ? v : null;
}

/** `p` with `term` set to `id` (or removed for null); every other param kept as is. */
export function withTermParam(p: URLSearchParams, id: string | null): URLSearchParams {
  const n = new URLSearchParams(p);
  if (id === null) n.delete("term");
  else n.set("term", id);
  return n;
}

function cwdBase(cwd: string): string {
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

function rowState(r: TermRow): string {
  if (r.stale) return "stale";
  if (r.pane_dead) return "ended";
  return r.state ?? "running";
}

export function TermList({
  openId,
  onOpen,
  onCloseOpen,
  hintHarness,
}: {
  /// The `?term=` id the page has open (null = none).
  openId: string | null;
  onOpen: (termId: string) => void;
  onCloseOpen: () => void;
  /// The harness of a just-launched term whose row is not listed yet.
  hintHarness?: string;
}): ReactElement | null {
  const qc = useQueryClient();
  const { terms, isLoading } = useTerms();
  const refresh = useCallback(() => void qc.invalidateQueries({ queryKey: ["terms"] }), [qc]);
  const openRow = openId ? terms.find((t) => t.id === openId) : undefined;
  if (isLoading && openId === null) return null;
  if (terms.length === 0 && openId === null) return null;
  return (
    <section className="kb-ses__terms" data-testid="term-list" aria-label="Terminals">
      {terms.length > 0 && (
        <>
          <h3 className="kb-ses__terms-h">Terminals</h3>
          <ul className="kb-ses__terms-ul">
            {terms.map((r) => (
              <li key={r.id} className="kb-ses__terms-row" data-testid="term-row" data-term={r.id}>
                <span className="kb-ses__terms-harness">{r.harness}</span>
                <span className="kb-ses__terms-cwd" title={r.cwd}>
                  {cwdBase(r.cwd)}
                </span>
                <span className="kb-ses__terms-state" data-testid="term-row-state">
                  {rowState(r)}
                  {r.blocked ? " · waiting" : ""}
                </span>
                <span className="kb-ses__terms-owner" data-testid="term-row-owner">
                  {r.cols}x{r.rows} · {ownerLabel(r.grid_owner ?? null)}
                </span>
                {r.session_id ? (
                  <Link
                    className="kb-ses__terms-link"
                    data-testid="term-row-session"
                    to={`/sessions/${encodeURIComponent(r.session_id)}#sec-terminal`}
                  >
                    session
                  </Link>
                ) : (
                  <span className="kb-ses__terms-unbound" data-testid="term-row-unbound">
                    unbound
                  </span>
                )}
                {isLive(r) && (
                  <button
                    type="button"
                    className="kb-ses__newterm-btn"
                    data-testid="term-row-open"
                    disabled={openId === r.id}
                    onClick={() => onOpen(r.id)}
                  >
                    Open
                  </button>
                )}
                <EndTerminalButton
                  className="kb-ses__newterm-btn"
                  onEnded={refresh}
                  target={{
                    termId: r.id,
                    harness: r.harness,
                    cwd: r.cwd,
                    stale: r.stale || r.pane_dead,
                  }}
                />
              </li>
            ))}
          </ul>
        </>
      )}
      {openId !== null && (
        <LaunchedTerminal
          key={openId}
          termId={openId}
          harness={openRow?.harness ?? hintHarness ?? "terminal"}
          cwd={openRow?.cwd}
          onClose={onCloseOpen}
          onSwitch={onOpen}
        />
      )}
    </section>
  );
}
