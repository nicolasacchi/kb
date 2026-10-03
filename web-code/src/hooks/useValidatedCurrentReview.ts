import { useEffect } from "react";
import { useNavigate } from "react-router";
import { ApiError } from "../api/client";
import { clearCurrentReview, peekCurrentReview, useCurrentReview } from "../lib/currentReview";
import { isReviewIdString, judgeCurrentReview } from "../lib/currentReviewValidity";
import { mergeCurrentSearch } from "../lib/codeUrl";
import { toast } from "../lib/toast";
import { useReview } from "./useReviews";

/// A9-1 — the current-review marker is set from any `?review=` URL or
/// restored from sessionStorage and is never checked against the daemon.
/// This hook probes `GET /api/reviews/{id}` once per marker; a 404, a review
/// of another repo, or a non-numeric id clears the marker, strips
/// `?review=` from the URL and says so. A network/5xx failure keeps it.
/// THE one hook every surface uses to read the current review (reader,
/// search, omnibox, top-bar chip, annotations rail) — a bare
/// `useCurrentReview` would keep trusting a stale marker (A9.f5).
/// Returns the marker (`null` once cleared) plus its numeric id (undefined
/// unless the id is a plain positive integer).
export function useValidatedCurrentReview(repo: string) {
  const navigate = useNavigate();
  const current = useCurrentReview(repo);
  const wellFormed = current !== null && isReviewIdString(current.id);
  const numericId = wellFormed ? Number(current.id) : undefined;
  const probe = useReview(repo || undefined, numericId);

  const malformed = current !== null && !wellFormed;
  const verdict =
    numericId === undefined
      ? "pending"
      : judgeCurrentReview(repo, {
          isPending: probe.isPending,
          errorStatus: probe.error instanceof ApiError ? probe.error.status : undefined,
          reviewRepo: probe.data?.repo,
        });
  const gone = malformed || verdict === "gone";
  const id = current?.id;

  useEffect(() => {
    if (!gone || !repo) return;
    // Several surfaces (reader, search, omnibox, top bar, annotations rail)
    // mount this hook at once. The first effect to run clears the marker;
    // the rest see it gone and stay quiet — one clear, one strip, one toast.
    if (peekCurrentReview(repo) === null) return;
    clearCurrentReview(repo);
    navigate({ search: mergeCurrentSearch((p) => p.delete("review")) }, { replace: true });
    toast.err(`Review #${id ?? "?"} doesn't exist in this repo — stopped working it`);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [gone, repo, id]);

  return { current: gone ? null : current, reviewId: gone ? undefined : numericId };
}
