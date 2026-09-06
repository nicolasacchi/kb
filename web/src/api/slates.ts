// kb-slate/1 client — one file per feature (the `inbox.ts` convention),
// over the same `currentDaemonBase()` primitive every other fetcher reads
// at call time.
//
// Unlike `inbox.ts` these fetchers do NOT swallow errors into an empty
// payload: a slate is a coordination surface, so "the daemon said no" has
// to reach the caller (the 409 friction toasts and the useConfirm prompt
// are the whole point). Every non-2xx becomes a `SlateApiError` carrying
// the RFC 7807 problem body's `code` and `holder` extension members.
//
// `client.ts`'s own `get`/`mutate` are module-private, so the two helpers
// below are the same three lines with the slate-shaped thrower.

import { currentDaemonBase } from "./base";
import type {
  SlateAppendResponse,
  SlateBoardResponse,
  SlateDeltaResponse,
  SlateDigestResponse,
  SlateHistoryRow,
  SlateHolder,
  SlateMode,
  SlatePost,
  SlatePostBody,
  SlateProblemCode,
  SlateSummary,
} from "./slateTypes";

export type * from "./slateTypes";

/// The slate's problem+json body: RFC 7807 core plus the two extension
/// members `routes/slates.rs` sets.
export type SlateProblem = {
  type: string;
  title: string;
  status: number;
  detail?: string;
  code?: string;
  holder?: SlateHolder;
};

/// Typed error for any non-2xx slate response. `code` prefers the problem's
/// own extension member and falls back to the `detail` prefix
/// (`"<code>: <text>"`) — the exact ladder the CLI reads.
export class SlateApiError extends Error {
  readonly status: number;
  readonly problem?: SlateProblem;
  constructor(message: string, status: number, problem?: SlateProblem) {
    super(message);
    this.name = "SlateApiError";
    this.status = status;
    this.problem = problem;
  }
  get code(): string | null {
    const p = this.problem;
    if (!p) return null;
    if (p.code) return p.code;
    const m = /^([a-z][a-z-]*):\s/.exec(p.detail ?? "");
    return m ? m[1] : null;
  }
  get holder(): SlateHolder | null {
    return this.problem?.holder ?? null;
  }
  /// The human sentence to toast: the problem title, else the status line.
  get title(): string {
    return this.problem?.title ?? this.message;
  }
  is(code: SlateProblemCode): boolean {
    return this.code === code;
  }
}

async function throwSlateError(r: Response): Promise<never> {
  const ct = r.headers.get("content-type") ?? "";
  let problem: SlateProblem | undefined;
  let detail: string | undefined;
  if (ct.includes("application/problem+json")) {
    try {
      problem = (await r.json()) as SlateProblem;
      detail = `${problem.title}: ${problem.detail ?? ""}`;
    } catch {
      // Torn or empty problem body — fall through to the status line.
    }
  }
  throw new SlateApiError(
    detail ?? `${r.status} ${r.statusText}`,
    r.status,
    problem,
  );
}

async function slateGet<T>(path: string, signal?: AbortSignal): Promise<T> {
  const r = await fetch(`${currentDaemonBase()}${path}`, {
    headers: { Accept: "application/json" },
    signal,
  });
  if (!r.ok) await throwSlateError(r);
  return (await r.json()) as T;
}

async function slateMutate<T>(
  path: string,
  method: "POST" | "DELETE",
  body?: unknown,
): Promise<T> {
  const headers: Record<string, string> = { Accept: "application/json" };
  if (body !== undefined) headers["Content-Type"] = "application/json";
  const r = await fetch(`${currentDaemonBase()}${path}`, {
    method,
    headers,
    body: body !== undefined ? JSON.stringify(body) : undefined,
  });
  if (!r.ok) await throwSlateError(r);
  return (await r.json()) as T;
}

const slugPath = (slug: string) =>
  `/api/slates/${encodeURIComponent(slug)}`;

// ── reads ────────────────────────────────────────────────────────────────

/// `GET /api/slates` — every slate, for the `/slates` list and the nav chip.
export const fetchSlates = (signal?: AbortSignal): Promise<SlateSummary[]> =>
  slateGet<SlateSummary[]>("/api/slates", signal);

/// `GET /api/slates/{slug}?view=board[&topic=]` — the board projection:
/// every SHOWN post with body, marks_by and has_sketch, never budget-
/// truncated (the board scrolls). Dropped and superseded posts are not
/// here; they live in `/history`.
export const fetchSlateBoard = (
  slug: string,
  opts: { topic?: string } = {},
  signal?: AbortSignal,
): Promise<SlateBoardResponse> => {
  const p = new URLSearchParams({ view: "board" });
  if (opts.topic) p.set("topic", opts.topic);
  return slateGet<SlateBoardResponse>(`${slugPath(slug)}?${p}`, signal);
};

/// `GET /api/slates/{slug}/history` — dropped and superseded posts only,
/// newest first. The drawer's source.
export const fetchSlateHistory = (
  slug: string,
  opts: { since?: number; limit?: number } = {},
  signal?: AbortSignal,
): Promise<SlateHistoryRow[]> => {
  const p = new URLSearchParams();
  if (opts.since !== undefined) p.set("since", String(opts.since));
  if (opts.limit !== undefined) p.set("limit", String(opts.limit));
  const qs = p.toString();
  return slateGet<SlateHistoryRow[]>(
    `${slugPath(slug)}/history${qs ? `?${qs}` : ""}`,
    signal,
  );
};

/// `GET /api/slates/{slug}` — the DIGEST (the CLI's own render, byte for
/// byte, in `text`). The board does not read this; it is here so a future
/// "what the agents see" panel has one call, and so the module mirrors §9.
export const fetchSlateDigest = (
  slug: string,
  opts: {
    mode?: SlateMode;
    budget?: number;
    topic?: string;
    all?: boolean;
    session?: string;
    since?: number;
  } = {},
  signal?: AbortSignal,
): Promise<SlateDigestResponse> => {
  const p = new URLSearchParams();
  if (opts.mode) p.set("mode", opts.mode);
  if (opts.budget !== undefined) p.set("budget", String(opts.budget));
  if (opts.topic) p.set("topic", opts.topic);
  if (opts.all) p.set("all", "1");
  if (opts.session) p.set("session", opts.session);
  if (opts.since !== undefined) p.set("since", String(opts.since));
  const qs = p.toString();
  return slateGet<SlateDigestResponse>(
    `${slugPath(slug)}${qs ? `?${qs}` : ""}`,
    signal,
  );
};

/// `GET /api/slates/{slug}/delta?since=` — the per-prompt delta (hooks and
/// `kb slate delta`). Mirrored for completeness; the board uses SSE +
/// `["slate", slug]` invalidation instead of a cursor.
export const fetchSlateDelta = (
  slug: string,
  opts: { since?: number; session?: string; budget?: number; limit?: number } = {},
  signal?: AbortSignal,
): Promise<SlateDeltaResponse> => {
  const p = new URLSearchParams();
  if (opts.since !== undefined) p.set("since", String(opts.since));
  if (opts.session) p.set("session", opts.session);
  if (opts.budget !== undefined) p.set("budget", String(opts.budget));
  if (opts.limit !== undefined) p.set("limit", String(opts.limit));
  const qs = p.toString();
  return slateGet<SlateDeltaResponse>(
    `${slugPath(slug)}/delta${qs ? `?${qs}` : ""}`,
    signal,
  );
};

/// `GET /api/slates/{slug}/posts?since=` — the RAW ledger records.
export const fetchSlatePosts = (
  slug: string,
  opts: { since?: number; limit?: number } = {},
  signal?: AbortSignal,
): Promise<SlatePost[]> => {
  const p = new URLSearchParams();
  if (opts.since !== undefined) p.set("since", String(opts.since));
  if (opts.limit !== undefined) p.set("limit", String(opts.limit));
  const qs = p.toString();
  return slateGet<SlatePost[]>(
    `${slugPath(slug)}/posts${qs ? `?${qs}` : ""}`,
    signal,
  );
};

// ── the one mutation ─────────────────────────────────────────────────────

/// `POST /api/slates/{slug}/posts` — the ONLY write. Every board action
/// (mark, pin, drop, edit, done, answer, take) is an append of one of the
/// twelve kinds; there is no in-place rewrite and no second route.
export const appendSlatePost = (
  slug: string,
  body: SlatePostBody,
): Promise<SlateAppendResponse> =>
  slateMutate<SlateAppendResponse>(`${slugPath(slug)}/posts`, "POST", body);
