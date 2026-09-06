import { matchPath, useLocation, useSearchParams } from "react-router-dom";
import { useKbs } from "./useKbs";

// Single source of truth for "which kb is the user in?".
//
// The SPA encodes the active kb two incompatible ways: in the URL PATH on
// the reader (/a/:kb/*) and list detail (/lists/:kb/:id), and in the ?kb=
// QUERY param on every section view (gallery, search, memory, notes,
// sessions, lists). Reconcile both here so navigation can carry the active
// space forward, instead of each call site re-deriving it from the query
// string alone — which dropped the path kb and sent every top-button click
// on a reader page back to the first kb.

function pathKbOf(pathname: string): string | null {
  const detail = matchPath({ path: "/a/:kb/*" }, pathname);
  if (detail?.params.kb) return detail.params.kb;
  const listDetail = matchPath({ path: "/lists/:kb/:id" }, pathname);
  if (listDetail?.params.kb) return listDetail.params.kb;
  return null;
}

// Explicit selection: path kb > ?kb=, with NO first-kb fallback. Use this to
// BUILD links — a section with no kb selected stays kb-less (clean URLs),
// while leaving the reader carries the artifact's kb forward.
export function useExplicitKb(): string | null {
  const loc = useLocation();
  const [params] = useSearchParams();
  return pathKbOf(loc.pathname) || params.get("kb") || null;
}

// Resolved active kb: the explicit selection, else the first configured kb.
// Use this for DISPLAY (workspace pill, context-line scope) and as the
// default search scope — non-null once kbs have loaded.
export function useActiveKb(): string | null {
  const explicit = useExplicitKb();
  const { data: kbs } = useKbs();
  return explicit || kbs?.[0]?.name || null;
}
