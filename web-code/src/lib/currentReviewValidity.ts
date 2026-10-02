// A9-1 — is a "current review" marker still pointing at a real review of
// THIS repo? The marker is browser-only chrome state (`currentReview.ts`)
// and is set from any `?review=<digits>` URL or restored from
// sessionStorage, so it can outlive the review (deleted) or belong to
// another repo (`GET /api/reviews/{id}` is repo-blind). Pure so the unit
// suite pins every branch; `hooks/useValidatedCurrentReview.ts` is the
// only caller.

/// Ids are daemon-minted positive integers. Anything else (`7.5`, `1e3`,
/// `0x10`, ` 7`, `-1`) must never reach a `Number()` conversion.
export function isReviewIdString(v: string): boolean {
  return /^[0-9]+$/.test(v) && Number.isSafeInteger(Number(v)) && Number(v) > 0;
}

export type CurrentReviewVerdict = "pending" | "ok" | "gone";

/// `probe` is the settled state of `GET /api/reviews/{id}`: a 404 means the
/// review is gone; a review belonging to another repo is just as unusable;
/// any other failure (network, 5xx) is NOT a verdict — keep the marker.
export function judgeCurrentReview(
  repo: string,
  probe: { isPending: boolean; errorStatus?: number; reviewRepo?: string },
): CurrentReviewVerdict {
  if (probe.errorStatus === 404) return "gone";
  if (probe.reviewRepo !== undefined) return probe.reviewRepo === repo ? "ok" : "gone";
  return probe.isPending ? "pending" : "ok";
}
