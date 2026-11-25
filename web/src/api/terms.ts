// T14 — the SPA client for the kb-term terminal routes (`/api/terms/*`).
//
// Route paths, the intent-header pair, the ticket response shape and the
// list envelope below are copied from the merged server contract:
//
//   * `crates/kb-server/src/router.rs`: `GET /terms`,
//     `POST /terms/{id}/ticket`, `GET /terms/{id}/ws`.
//   * `crates/kb-server/src/terminal/gate.rs`: `TERMINAL_INTENT_HEADER`
//     is the `"x-kb-intent"` literal (HTTP header names are
//     case-insensitive); the gate requires the value `terminal` on
//     every POST. The SPA constant spells the canonical case.
//   * `crates/kb-server/src/routes/terms.rs`: `TicketResponse`
//     (`{ticket, expires_in, term}`, 32 lowercase-hex chars, single-use
//     ~30 s TTL) and the list envelope `{terms: [...]}` whose items are
//     `SessionInfo` plus the T13-enriched `session_id`/`state`/
//     `blocked` fields.
//
// Terminal BYTES never enter TanStack Query; this module only mints
// tickets and lists rows. The per-panel WebSocket lives in
// `terminal/session.ts` and is deliberately NOT routed through
// `api/sse.ts` or the SharedWorker (invariant #24 carve-out).

import { currentDaemonBase } from "./base";
import type { LiveState } from "./generated/LiveState";

/**
 * The intent-header pair the terminal gate requires on every POST and
 * every GET (`crates/kb-server/src/terminal/gate.rs`:
 * `TERMINAL_INTENT_HEADER` and the `terminal_intent()` value check). The literals are pinned by
 * `terms.test.ts`; change them together with the server file.
 */
export const TERMINAL_INTENT_HEADER = "X-Kb-Intent";
export const TERMINAL_INTENT_VALUE = "terminal";

/** A non-2xx answer from the terminal routes. */
export class TermsApiError extends Error {
  readonly status: number;
  /** problem+json `type` (e.g. `urn:kb:errors:terminal-not-found`), or null. */
  readonly type: string | null;
  constructor(status: number, type: string | null, message: string) {
    super(message);
    this.name = "TermsApiError";
    this.status = status;
    this.type = type;
  }
}

/** The `POST /api/terms/{id}/ticket` reply (`terms.rs` `TicketResponse`). */
export interface TicketResult {
  ticket: string;
  expires_in: number;
  term: string;
}

/**
 * One `GET /api/terms` row. There is no ts-rs-generated type for the
 * list (`TermsList` is private server-side), so this is copied from the
 * `SessionInfo` struct (`crates/kb-server/src/terminal/wire.rs`) plus
 * the three fields T13 enriches each item with (`routes/terms.rs`
 * `enrich_terms`): `session_id` (bound session, if any), the derived
 * `state`, and `blocked`.
 */
export interface TermRow {
  id: string;
  harness: string;
  cwd: string;
  started_unix: number;
  resume_sid: string | null;
  pane_pid: number;
  cols: number;
  rows: number;
  bracket_paste_flag: boolean;
  pane_dead: boolean;
  exit_code: number | null;
  grid_owner: string | null;
  conf_version: number;
  anchor_pid: number | null;
  stale: boolean;
  session_id?: string | null;
  state?: LiveState | null;
  blocked?: boolean;
  /**
   * Derived folder-match guess for a row with no verified binding (null
   * when absent). Never verified, never persisted, never a gate input; a
   * later verified beat replaces it with `session_id`.
   */
  session_hint?: TermSessionHint | null;
}

/** A guessed session link: same harness and exactly the same cwd. */
export interface TermSessionHint {
  session_id: string;
  via: "cwd";
}

async function termsError(res: Response): Promise<TermsApiError> {
  let type: string | null = null;
  let message = `${res.status} ${res.statusText}`;
  try {
    const p = (await res.json()) as { type?: string; title?: string; detail?: string };
    if (typeof p.type === "string") type = p.type;
    if (typeof p.title === "string") message = `${p.title}: ${p.detail ?? ""}`;
  } catch {
    // Bodiless 404 (terminal disabled) or unparseable body — the status
    // line above is the message.
  }
  return new TermsApiError(res.status, type, message);
}

/**
 * Mint a single-use ticket for `termId`. Same-origin, so the browser
 * adds `Origin` automatically (never set it — the server demands exact
 * Origin equality). No `Authorization` / `X-Kb-Token` (the gate refuses
 * token callers when `refuse_token_callers` is on). Non-2xx throws
 * `TermsApiError`; 404 = terminal disabled or session gone, no retry.
 */
export const AUTH_EXPIRED_TYPE = "urn:kb:errors:auth-expired";

export async function mintTicket(termId: string, signal?: AbortSignal): Promise<TicketResult> {
  const res = await fetch(`${currentDaemonBase()}/api/terms/${encodeURIComponent(termId)}/ticket`, {
    method: "POST",
    headers: {
      Accept: "application/json",
      "Content-Type": "application/json",
      [TERMINAL_INTENT_HEADER]: TERMINAL_INTENT_VALUE,
    },
    body: "{}",
    signal,
    // Never follow an edge-auth (login) redirect: it would surface as an opaque
    // cross-origin TypeError, indistinguishable from being offline.
    redirect: "manual",
  });
  if (res.type === "opaqueredirect") {
    throw new TermsApiError(401, AUTH_EXPIRED_TYPE, "session expired (redirected to sign in)");
  }
  if (!res.ok) throw await termsError(res);
  return (await res.json()) as TicketResult;
}

/**
 * The `POST /api/terms` reply: the runner's launch confirm (`terms.rs`
 * `launch` relays the runner `data`; `id` is guaranteed, dims are the
 * runner's).
 */
export interface LaunchResult {
  id: string;
  cols?: number;
  rows?: number;
}

/** The `POST /api/terms` body (`terms.rs` `LaunchBody`). */
export interface LaunchBody {
  harness: string;
  cwd: string;
  /**
   * Source session id of a cross-harness handoff: the pane runs
   * `kb sessions handoff <id> --to <harness> --transplant --launch`.
   * Mutually exclusive with `resume_sid`.
   */
  handoff_from?: string;
  resume_sid?: string;
  cols?: number;
  rows?: number;
}

/**
 * Launch a terminal. Operator-intent only (call from a click handler):
 * same gate as `mintTicket` — same-origin, intent header, no token.
 * Non-2xx throws `TermsApiError` (400 = rejected body, 404 = terminal
 * disabled, 5xx = runner failure).
 */
export async function launchTerm(body: LaunchBody, signal?: AbortSignal): Promise<LaunchResult> {
  const res = await fetch(`${currentDaemonBase()}/api/terms`, {
    method: "POST",
    headers: {
      Accept: "application/json",
      "Content-Type": "application/json",
      [TERMINAL_INTENT_HEADER]: TERMINAL_INTENT_VALUE,
    },
    body: JSON.stringify(body),
    signal,
  });
  if (!res.ok) throw await termsError(res);
  return (await res.json()) as LaunchResult;
}

/**
 * List all terminals (`{terms: [...]}` envelope unwrapped). A same-origin
 * GET carries no `Origin`, so the GET gate (`terminal/gate.rs`
 * `gate_read`) admits it on the intent header alone: without it the
 * list is refused 403 and no row ever reaches the panel.
 */
export async function listTerms(signal?: AbortSignal): Promise<TermRow[]> {
  const res = await fetch(`${currentDaemonBase()}/api/terms`, {
    headers: { Accept: "application/json", [TERMINAL_INTENT_HEADER]: TERMINAL_INTENT_VALUE },
    signal,
  });
  if (!res.ok) throw await termsError(res);
  const body = (await res.json()) as { terms?: TermRow[] };
  return body.terms ?? [];
}

/**
 * End a terminal: `POST /api/terms/{id}/kill`. Operator-intent only (call from
 * a confirmed click): same gate as `mintTicket` (same-origin Origin, intent
 * header, no token), no body. A 200 is success even when the holder was
 * already gone (`exit_code: null`). Non-2xx throws `TermsApiError`. The caller
 * never marks the terminal ended itself: the exit frame and the `term.ended`
 * SSE do.
 */
export async function killTerm(termId: string, signal?: AbortSignal): Promise<void> {
  const res = await fetch(`${currentDaemonBase()}/api/terms/${encodeURIComponent(termId)}/kill`, {
    method: "POST",
    headers: { Accept: "application/json", [TERMINAL_INTENT_HEADER]: TERMINAL_INTENT_VALUE },
    signal,
  });
  if (!res.ok) throw await termsError(res);
}

/**
 * The WebSocket URL for a panel. `ws:`/`wss:` is derived from
 * `currentDaemonBase()` (or `location` for a same-origin daemon);
 * `from` is the resume offset, included only when `from > 0`; `epoch`
 * (the tap stream epoch `from` belongs to) and `mseq` (newest marker
 * sequence applied, only when > 0) ride along with it and are dropped
 * without it.
 * Same-origin daemons only (the server demands exact Origin equality).
 */
export function wsUrl(
  termId: string,
  ticket: string,
  from?: number,
  epoch?: string,
  mseq?: number,
): string {
  const base = currentDaemonBase();
  let scheme: string;
  let host: string;
  if (base !== "") {
    const u = new URL(base);
    scheme = u.protocol === "https:" ? "wss" : "ws";
    host = u.host;
  } else if (typeof location !== "undefined") {
    scheme = location.protocol === "https:" ? "wss" : "ws";
    host = location.host;
  } else {
    // Node/vitest only: there is no page origin to derive the scheme or
    // host from. The host is inert (no real socket is opened here).
    scheme = "ws";
    host = "localhost";
  }
  const q = new URLSearchParams();
  q.set("ticket", ticket);
  if (from !== undefined && from > 0) {
    q.set("from", String(from));
    // The resume coordinates travel together: an epoch (and the marker
    // sequence applied under it) means nothing without an offset.
    if (epoch !== undefined && epoch !== "") q.set("epoch", epoch);
    if (mseq !== undefined && Number.isSafeInteger(mseq) && mseq > 0) q.set("mseq", String(mseq));
  }
  return `${scheme}://${host}/api/terms/${encodeURIComponent(termId)}/ws?${q.toString()}`;
}
