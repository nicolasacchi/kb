import { matchPath, useLocation, useSearchParams } from "react-router";
import { useRepos } from "./useRepos";

// F1 — single source of truth for "which repo is the user in?", mirroring
// kb's own `web/src/hooks/useActiveKb.ts` (root CLAUDE.md invariant #33).
//
// kb-code's own grammar: the reader owns its repo in the URL PATH
// (`/r/:repo/*`); every other repo-aware surface (today, just `/search`)
// carries it in the `?repo=` QUERY param. `repoOf` is a pure function of
// (pathname, search) — `matchPath` needs no Router context, so it's unit-
// tested directly (see `useActiveRepo.test.ts`).
export function repoOf(pathname: string, search: URLSearchParams): string | null {
  const reader = matchPath({ path: "/r/:repo/*" }, pathname);
  if (reader?.params.repo) return reader.params.repo;
  return search.get("repo") || null;
}

// Explicit selection: path repo > ?repo=, with NO first-repo fallback. Use
// this to BUILD links / decide "is this view scoped to one repo" — Home and
// an unscoped /search stay repo-less (the pill dims), while the reader
// carries its repo forward.
export function useExplicitRepo(): string | null {
  const loc = useLocation();
  const [params] = useSearchParams();
  return repoOf(loc.pathname, params);
}

// Resolved active repo: the explicit selection, else the first configured
// repo. Use this for DISPLAY (the repo pill) — non-null once repos have
// loaded.
export function useActiveRepo(): string | null {
  const explicit = useExplicitRepo();
  const { data } = useRepos();
  return explicit || data?.repos[0]?.name || null;
}
