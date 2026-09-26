import { useQuery } from "@tanstack/react-query";
import { fetchReviewCredentials, fetchReviewStoreCard } from "../api/client";

/// `GET /api/repos/{name}/store` (RS-U11) — the store card: registration,
/// state, members, disk facts, doctor findings. Shared by the review
/// header's `forge-unverified` chip (`store.forge_verified`/`forge_kind`)
/// and the Home dashboard's "Review store" section — same `["review-
/// store", repo]` key either way, so opening several reviews in one repo
/// (or a review after Home already fetched it) shares ONE cached read
/// rather than re-issuing the store-dir walk per consumer.
export function useReviewStoreCard(repo: string | undefined) {
  return useQuery({
    queryKey: ["review-store", repo],
    queryFn: () => fetchReviewStoreCard(repo as string),
    enabled: repo !== undefined,
  });
}

/// `GET /api/repos/{name}/credentials` (RS-U11) — the fetch credential as
/// last resolved. Never secret bytes.
export function useReviewCredentials(repo: string | undefined) {
  return useQuery({
    queryKey: ["review-credentials", repo],
    queryFn: () => fetchReviewCredentials(repo as string),
    enabled: repo !== undefined,
  });
}
