import { useQuery } from "@tanstack/react-query";
import { fetchPrComments, fetchPrs } from "../api/client";

/// `GET /api/prs?repo=` (Phase G4) — every open PR on `repo`'s GitHub
/// origin. `retry: false` — a non-GitHub-origin 400 (`ApiError`) is a
/// caller/repo-config fact, not a transient failure worth React Query's
/// default retry.
export function usePrs(repo: string | undefined) {
  return useQuery({
    queryKey: ["prs", repo],
    queryFn: () => fetchPrs(repo as string),
    enabled: repo !== undefined,
    retry: false,
  });
}

/// `GET /api/prs/{number}/comments?repo=` (Phase G4) — one PR's review +
/// issue comments (Compare's read-only side strip, `components/history/
/// PrCommentsStrip.tsx`).
export function usePrComments(repo: string | undefined, number: number | undefined) {
  return useQuery({
    queryKey: ["pr-comments", repo, number],
    queryFn: () => fetchPrComments(repo as string, number as number),
    enabled: repo !== undefined && number !== undefined,
    retry: false,
  });
}
