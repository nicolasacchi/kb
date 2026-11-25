// FUTERMLIST — the Terminals list on /sessions: the one home for "which
// terminals exist", Open, End and Remove; plus the End button and the phone
// sheet's terminal switcher, which reuse the same action.
//
// * Rows come from the existing `["terms"]` query (`useTerms`); the SSE bridge
//   invalidates it, nothing here polls (#23). Terminal bytes never enter it.
// * Open is a link to the terminal's own page (`/terminals/<id>`, `TermShell`);
//   the legacy `?term=` deep link redirects there (sessions.tsx).
// * End/Remove, the End button and the phone switcher live in `TermControls`.

import { useCallback, type ReactElement } from "react";
import { Link } from "react-router-dom";
import { useQueryClient } from "@tanstack/react-query";
import type { TermRow } from "../api/terms";
import { useTerms } from "../terminal/useTerms";
import { ownerLabel } from "../terminal/gridOwner";
import { EndTerminalButton, cwdBase, isLive } from "./TermControls";
import { buildHubUrl } from "../terminal/hub/termHub";

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

export function TermList(): ReactElement | null {
  const qc = useQueryClient();
  const { terms, isLoading } = useTerms();
  const refresh = useCallback(() => void qc.invalidateQueries({ queryKey: ["terms"] }), [qc]);
  if (isLoading || terms.length === 0) return null;
  return (
    <section className="kb-ses__terms" data-testid="term-list" aria-label="Terminals">
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
              <Link
                className="kb-ses__newterm-btn"
                data-testid="term-row-open"
                to={buildHubUrl({ id: r.id })}
              >
                Open
              </Link>
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
    </section>
  );
}
