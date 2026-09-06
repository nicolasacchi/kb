// PRR-U2 §10 — v1 of the GitHub conversation card, on the RAW read routes
// only: reviewer states (`GET /api/prs/{n}/reviews`) + the general/inline
// comment list (`GET /api/prs/{n}/comments`, existing `usePrComments`).
// GitHub-origin styling (distinct from the finding/thread cards — this is
// content this daemon never authors), a `fetched_at` caption, and a manual
// refresh button — finite `staleTime` is the freshness mechanism (the
// `["prs"]` documented exception, see `api/queryClient.ts`). Reply
// affordance = an external link to GitHub (this SPA never posts to GitHub,
// design doc §3.2's "publishing is 100% agent-layer" law).
import type { PrCommentOut, ReviewerStateOut } from "../../api/types";
import { usePrComments } from "../../hooks/usePrs";
import { usePrReviews } from "../../hooks/useReviews";
import { formatFetchedCaption } from "./CiChecksCard";
import { prExternalUrl } from "./PrChip";
import { Icon } from "../icons";

export type ConversationState =
  | { kind: "loading" }
  | { kind: "unavailable"; reason: string }
  | { kind: "empty" }
  | { kind: "ready"; reviewers: ReviewerStateOut[]; comments: PrCommentOut[] };

/// Pure combinator over the two independent queries' results — a SINGLE
/// discriminated state the component switches on, rather than sprinkling
/// `isLoading`/`isError`/`unavailable_reason` checks through JSX. Either
/// query still loading ⇒ `"loading"`; either carries an `unavailable_reason`
/// ⇒ `"unavailable"` (named, not swallowed — the reviews reason wins if
/// both are present, since reviewer state is the more load-bearing half);
/// both landed with zero content ⇒ `"empty"`; otherwise `"ready"`.
export function combineConversationState(
  reviews: { isLoading: boolean; data: { reviewers: ReviewerStateOut[]; unavailable_reason?: string } | undefined },
  comments: { isLoading: boolean; data: { comments: PrCommentOut[]; unavailable_reason?: string } | undefined },
): ConversationState {
  if (reviews.isLoading || comments.isLoading) return { kind: "loading" };
  const reason = reviews.data?.unavailable_reason ?? comments.data?.unavailable_reason;
  if (reason) return { kind: "unavailable", reason };
  const reviewers = reviews.data?.reviewers ?? [];
  const comments_ = comments.data?.comments ?? [];
  if (reviewers.length === 0 && comments_.length === 0) return { kind: "empty" };
  return { kind: "ready", reviewers, comments: comments_ };
}

function reviewerGlyph(state: string): string {
  if (state === "APPROVED") return "✓";
  if (state === "CHANGES_REQUESTED") return "✗";
  if (state === "DISMISSED") return "⊘";
  return "…"; // COMMENTED | PENDING
}

export interface GithubConversationCardProps {
  repo: string;
  reviewId: number;
  prNumber: number | undefined;
  prRepoSlug: string | undefined;
}

export default function GithubConversationCard({
  repo,
  reviewId,
  prNumber,
  prRepoSlug,
}: GithubConversationCardProps) {
  const reviewsQ = usePrReviews(repo, reviewId, prNumber);
  const commentsQ = usePrComments(repo, prNumber);

  if (prNumber == null) return null;

  const state = combineConversationState(reviewsQ, commentsQ);
  const externalUrl = prExternalUrl(prRepoSlug, prNumber);
  const fetchedAt = reviewsQ.dataUpdatedAt ? Math.floor(reviewsQ.dataUpdatedAt / 1000) : undefined;

  return (
    <section className="kbc-review__card kbc-gh-conversation" data-kbc-gh-conversation={reviewId}>
      <div className="kbc-card__title">
        GitHub conversation
        <button
          type="button"
          className="kbc-btn kbc-btn--ghost"
          onClick={() => {
            void reviewsQ.refetch();
            void commentsQ.refetch();
          }}
          data-kbc-gh-conversation-refresh
          aria-label="refresh GitHub conversation"
        >
          <Icon.Refresh />
        </button>
      </div>

      {state.kind === "loading" && <div className="kbc-skeleton" style={{ height: 48 }} />}

      {state.kind === "unavailable" && (
        <p className="kbc-review__card-empty" data-kbc-gh-conversation-unavailable>
          GitHub unavailable ({state.reason})
        </p>
      )}

      {state.kind === "empty" && (
        <p className="kbc-review__card-empty" data-kbc-gh-conversation-empty>
          No GitHub reviews or comments yet.
        </p>
      )}

      {state.kind === "ready" && (
        <>
          {state.reviewers.length > 0 && (
            <ul className="kbc-gh-reviewers" data-kbc-gh-reviewers>
              {state.reviewers.map((r) => (
                <li key={r.reviewer} data-kbc-gh-reviewer={r.reviewer}>
                  <span aria-hidden="true">{reviewerGlyph(r.state)}</span> {r.reviewer} · {r.state}
                </li>
              ))}
            </ul>
          )}
          {state.comments.length > 0 && (
            <ul className="kbc-gh-comments" data-kbc-gh-comments>
              {state.comments.slice(0, 5).map((c, i) => (
                <li key={i} data-kbc-gh-comment>
                  <strong>{c.author}</strong>
                  {c.path ? ` on ${c.path}${c.line != null ? `:${c.line}` : ""}` : ""}: {c.body}
                </li>
              ))}
            </ul>
          )}
          {externalUrl && (
            <a className="kbc-gh-reply" href={externalUrl} target="_blank" rel="noreferrer" data-kbc-gh-conversation-reply>
              Reply on GitHub <Icon.External />
            </a>
          )}
        </>
      )}

      {formatFetchedCaption(fetchedAt) && (
        <div className="kbc-ci-cap" data-kbc-gh-conversation-caption>
          {formatFetchedCaption(fetchedAt)}
        </div>
      )}
    </section>
  );
}
