// FUTERMLIST — the Terminals list on /sessions: the one home for "which
// terminals exist", Open, End and Remove; plus the End button and the phone
// sheet's terminal switcher, which reuse the same action.
//
// * Rows come from the existing `["terms"]` query (`useTerms`); the SSE bridge
//   invalidates it, nothing here polls (#23). Terminal bytes never enter it.
// * Open mounts the SAME lazy panel through `LaunchedTerminal` (the one
//   mount a bare term id can use); the open id lives in the URL (`?term=`),
//   so a reload, a share and Back land on the open sheet.
// * End/Remove, the End button and the phone switcher live in `TermControls`.

import { useCallback, useEffect, useRef, useState, type ReactElement } from "react";
import { Link } from "react-router-dom";
import { useQueryClient } from "@tanstack/react-query";
import type { TermRow } from "../api/terms";
import { useTerms } from "../terminal/useTerms";
import { ownerLabel } from "../terminal/gridOwner";
import { EndTerminalButton, cwdBase, isLive } from "./TermControls";
import { LaunchedTerminal } from "./LaunchedTerminal";

/**
 * The `?term=` value as an open terminal id: a plain lowercase token (letters,
 * digits, dash; the server validates the exact grammar and answers an unknown id
 * with a mapped 4xx), else none. Never a path, never a space.
 */
export function openTermParam(v: string | null): string | null {
  return v !== null && /^[a-z0-9][a-z0-9-]{0,31}$/.test(v) ? v : null;
}

/** `p` with `term` set to `id` (or removed for null); every other param kept as is. */
export function withTermParam(p: URLSearchParams, id: string | null): URLSearchParams {
  const n = new URLSearchParams(p);
  if (id === null) n.delete("term");
  else n.set("term", id);
  return n;
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
  launched,
}: {
  /// The `?term=` id the page has open (null = none).
  openId: string | null;
  onOpen: (termId: string) => void;
  onCloseOpen: () => void;
  /// A just-launched term whose row may not be listed yet. Honoured only for
  /// its own id and only until that id has been seen listed once.
  launched?: { id: string; harness: string };
}): ReactElement | null {
  const qc = useQueryClient();
  const { terms, isLoading, isError } = useTerms();
  const refresh = useCallback(() => void qc.invalidateQueries({ queryKey: ["terms"] }), [qc]);
  const openRow = openId ? terms.find((t) => t.id === openId) : undefined;
  // Ids seen in the list: a launch hint stops counting once its row has been
  // listed (so an End that removes the row never remounts a ghost sheet).
  const seen = useRef<Set<string>>(new Set());
  if (openRow) seen.current.add(openRow.id);
  const hint = launched && launched.id === openId ? launched : undefined;
  const mountable =
    openId !== null && (openRow !== undefined || (hint !== undefined && !seen.current.has(openId)));
  // Only a SUCCESSFUL list that lacks the id means "not found": a failed read
  // says nothing about the terminal, so `?term=` stays and an error is shown.
  const unknownOpen = openId !== null && !isLoading && !isError && !mountable;
  const listError = openId !== null && isError && openRow === undefined;
  const [missing, setMissing] = useState<string | null>(null);
  useEffect(() => {
    if (unknownOpen && openId !== null) {
      setMissing(openId);
      onCloseOpen();
    } else if (openId !== null) setMissing(null);
  }, [unknownOpen, openId, onCloseOpen]);
  if (isLoading && openId === null) return null;
  if (terms.length === 0 && openId === null && missing === null) return null;
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
                {/* The open terminal's panel carries its own End: one End per terminal. */}
                {!(mountable && openId === r.id) && (
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
                )}
              </li>
            ))}
          </ul>
        </>
      )}
      {listError && (
        <p className="kb-ses__ins-errors" role="alert" data-testid="term-list-error">
          Cannot read the terminal list, so terminal {openId} cannot be opened yet.{" "}
          <button type="button" className="kb-ses__newterm-btn" data-testid="term-list-retry" onClick={refresh}>
            Retry
          </button>
        </p>
      )}
      {missing !== null && (
        <p className="kb-ses__terms-missing" role="status" data-testid="term-not-found">
          Terminal not found. It may have ended.
        </p>
      )}
      {mountable && openId !== null && (
        <LaunchedTerminal
          key={openId}
          termId={openId}
          harness={openRow?.harness ?? hint?.harness ?? "terminal"}
          cwd={openRow?.cwd}
          onClose={onCloseOpen}
          onSwitch={onOpen}
        />
      )}
    </section>
  );
}
