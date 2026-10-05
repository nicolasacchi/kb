import { artifactHref } from "./artifactHref";

// N13 — the daemon's by-path lookup follows the moves log, so `/a/<kb>/<old>`
// can resolve to a doc whose live `source_relative` is different. When that
// happens the address bar (copy-link, history, bookmarks) should show the live
// path. Pure: returns the canonical `pathname + search + hash` to REPLACE the
// current location with, or null when the URL already names the live path.
//
// The path is built with `artifactHref` (never by hand); `search` and `hash`
// are carried over verbatim so `?p=`, `?sec=`, `?pane2=` etc. survive.
export function canonicalArtifactLocation(
  kb: string,
  requestedRel: string,
  docRel: string | null | undefined,
  search: string,
  hash: string,
): string | null {
  if (!docRel) return null;
  // Same normalisation the daemon applies to the requested path.
  const req = requestedRel.replace(/^\/+/, "").replace(/\\/g, "/");
  if (req === docRel) return null;
  return `${artifactHref(kb, docRel)}${search}${hash}`;
}
