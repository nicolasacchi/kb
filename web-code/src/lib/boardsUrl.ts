// `kbc-canvas/1` — the board surface's URL state (V74-L2).
//
// The Location Contract's rule, unchanged: every param is APPENDED LAST, is
// omitted at its default, and has a TOTAL parser that degrades junk to the
// default rather than throwing (`routes/reviewDiff`'s `parseDiffPs` family is
// the precedent). There is no parallel store — a reload reproduces the view,
// including which walkthrough step is on screen.
//
// The page path itself goes through `lib/codeUrl.ts` (root CLAUDE.md #35);
// this module owns only the query grammar above it.

import { boardUrl, boardsPageUrl } from "./codeUrl";

/// `?step=` is 1-BASED on the wire, because that is what a human reads off the
/// step counter ("3 of 9") and what `kb-code canvas present --url` would print.
/// Internally the index is 0-based; the two conversions live here and nowhere
/// else.
export const BOARD_STEP_PARAM = "step";
export const BOARD_LIVE_PARAM = "live";
export const BOARD_CTX_PARAM = "ctx";
export const BOARDS_STATUS_PARAM = "status";

/// The 0-based step index a `?step=` carries, or `null` when there is none.
/// TOTAL: a non-numeric, zero, negative or out-of-range value reads as "no
/// step", never as step 0 — guessing which step an operator meant is the quiet
/// repair this codebase does not do.
export function parseStep(raw: string | null, stepCount: number): number | null {
  if (raw === null || raw.trim() === "") return null;
  const n = Number(raw);
  if (!Number.isInteger(n) || n < 1 || n > stepCount) return null;
  return n - 1;
}

/// A boolean flag param. Only the daemon's own truths (`boards::routes::flag`)
/// count, so the SPA and the route agree about what `?live=yes` means.
export function parseBoardFlag(raw: string | null): boolean {
  return raw === "1" || raw === "true" || raw === "yes";
}

/// One of `BOARD_STATUSES`, or `null` for "every status". A junk value reads as
/// `null`: the list then shows everything, which is the honest superset.
export function parseBoardsStatus(raw: string | null, statuses: readonly string[]): string | null {
  if (!raw) return null;
  return statuses.includes(raw) ? raw : null;
}

export interface BoardHrefOpts {
  /// 0-based; emitted as the 1-based `?step=`.
  step?: number | null;
  live?: boolean;
  ctx?: boolean;
}

/// The board's URL with its view knobs. Params in a FIXED order, each omitted
/// at its default, so two calls with the same view produce the same string.
export function boardHref(repo: string, slug: string, opts: BoardHrefOpts = {}): string {
  const qs = new URLSearchParams();
  if (opts.step !== null && opts.step !== undefined && opts.step >= 0) {
    qs.set(BOARD_STEP_PARAM, String(opts.step + 1));
  }
  if (opts.live) qs.set(BOARD_LIVE_PARAM, "1");
  if (opts.ctx) qs.set(BOARD_CTX_PARAM, "1");
  const suffix = qs.toString();
  const base = boardUrl(repo, slug);
  return suffix ? `${base}?${suffix}` : base;
}

/// The list's URL, optionally filtered by status.
export function boardsHref(repo: string, status?: string | null): string {
  const base = boardsPageUrl(repo);
  return status ? `${base}?${BOARDS_STATUS_PARAM}=${encodeURIComponent(status)}` : base;
}
