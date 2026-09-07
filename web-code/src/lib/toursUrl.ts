// `kbc-tour/1` — the tour surface's URL state (V74-L3b).
//
// A deliberate MIRROR of `lib/boardsUrl.ts`, not a generalisation of it. The
// two surfaces share a step model on the SERVER (a tour is a board whose
// nodes are its steps — server invariant 26), and they share this module's
// RULES, but a shared parser would have to be parameterised by a param name
// that is the same string in both, which buys nothing and makes each
// surface's grammar unreadable on its own. `parseStep`'s contract below is
// `boardsUrl.parseStep`'s, and `toursUrl.test.ts` pins that they agree.
//
// The Location Contract's rule, unchanged: every param is APPENDED LAST, is
// omitted at its default, and has a TOTAL parser that degrades junk to the
// default rather than throwing. There is no parallel store — a reload
// reproduces the view, including which step is on screen.

import { tourUrl, toursPageUrl } from "./codeUrl";

/// `?step=` is 1-BASED on the wire, because that is what a human reads off
/// the step counter ("3 of 9"). Internally the index is 0-based; the two
/// conversions live here and nowhere else.
export const TOUR_STEP_PARAM = "step";
export const TOUR_CTX_PARAM = "ctx";
export const TOURS_STATUS_PARAM = "status";

/// The 0-based step index a `?step=` carries, or `null` when there is none.
/// TOTAL: a non-numeric, zero, negative or out-of-range value reads as "no
/// step", never as step 0.
export function parseTourStep(raw: string | null, stepCount: number): number | null {
  if (raw === null || raw.trim() === "") return null;
  const n = Number(raw);
  if (!Number.isInteger(n) || n < 1 || n > stepCount) return null;
  return n - 1;
}

/// A boolean flag param — the daemon's own truths (`tours::routes::flag`), so
/// the SPA and the route agree about what `?ctx=yes` means.
export function parseTourFlag(raw: string | null): boolean {
  return raw === "1" || raw === "true" || raw === "yes";
}

/// One of the daemon's statuses, or `null` for "every status". A junk value
/// reads as `null`: the list then shows everything, the honest superset.
export function parseToursStatus(raw: string | null, statuses: readonly string[]): string | null {
  if (!raw) return null;
  return statuses.includes(raw) ? raw : null;
}

export interface TourHrefOpts {
  /// 0-based; emitted as the 1-based `?step=`.
  step?: number | null;
  ctx?: boolean;
}

/// One tour's URL with its view knobs, params in a FIXED order and each
/// omitted at its default, so two calls with the same view produce the same
/// string.
export function tourHref(repo: string, slug: string, opts: TourHrefOpts = {}): string {
  const qs = new URLSearchParams();
  if (opts.step !== null && opts.step !== undefined && opts.step >= 0) {
    qs.set(TOUR_STEP_PARAM, String(opts.step + 1));
  }
  if (opts.ctx) qs.set(TOUR_CTX_PARAM, "1");
  const suffix = qs.toString();
  const base = tourUrl(repo, slug);
  return suffix ? `${base}?${suffix}` : base;
}

/// The list's URL, optionally filtered by status.
export function toursHref(repo: string, status?: string | null): string {
  const base = toursPageUrl(repo);
  return status ? `${base}?${TOURS_STATUS_PARAM}=${encodeURIComponent(status)}` : base;
}
