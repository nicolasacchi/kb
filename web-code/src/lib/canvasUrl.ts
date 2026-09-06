// V3.4-C2 — `/r/{repo}/~canvas[?id=][&review=]` URL builder/parser.
// URL owns which canvas is open (`?id=`); no local storage.

import { codeBasePath } from "./codeUrl";

export interface CanvasUrlOpts {
  /** Open canvas id (server row id). */
  id?: number | null;
  /** Optional review association — passed on create only; kept in URL for bookmarking. */
  review?: number | null;
}

/// `canvasUrl(repo, { id?, review? })` → `/r/{repo}/~canvas[?id=][&review=]`.
export function canvasUrl(repo: string, opts: CanvasUrlOpts = {}): string {
  const base = `${codeBasePath(repo, "")}/~canvas`;
  const qs = new URLSearchParams();
  if (opts.id != null && Number.isFinite(opts.id)) qs.set("id", String(opts.id));
  if (opts.review != null && Number.isFinite(opts.review)) qs.set("review", String(opts.review));
  const s = qs.toString();
  return s ? `${base}?${s}` : base;
}

/// Parse `?id=` / `?review=` from a URLSearchParams (or Record).
export function parseCanvasSearch(sp: URLSearchParams | { get(name: string): string | null }): {
  id: number | null;
  review: number | null;
} {
  const idRaw = sp.get("id");
  const reviewRaw = sp.get("review");
  const id = idRaw != null && idRaw !== "" && Number.isFinite(Number(idRaw)) ? Number(idRaw) : null;
  const review =
    reviewRaw != null && reviewRaw !== "" && Number.isFinite(Number(reviewRaw))
      ? Number(reviewRaw)
      : null;
  return { id, review };
}
