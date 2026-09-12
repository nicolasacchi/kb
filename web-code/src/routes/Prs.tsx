import { useMemo, useState } from "react";
import { Link, useNavigate, useParams } from "react-router";
import type { ReviewSummaryPr } from "../api/types";
import { ApiError, postPrFetch } from "../api/client";
import EmptyState from "../components/EmptyState";
import { Icon } from "../components/icons";
import { useCreateReviewPr, useReviews } from "../hooks/useReviews";
import { usePrs } from "../hooks/usePrs";
import { compareUrl, reviewUrl } from "../lib/codeUrl";
import { prRef } from "../lib/prRef";
import { relativeTime } from "../lib/format";
import { buildCreateReviewPrInput, reviewByPrNumber } from "../lib/reviewInbox";
import { toast } from "../lib/toast";
import "../styles/history.css";
import "../styles/review-inbox.css";
import { useListScrollRestoration } from "../hooks/useScrollRestoration";

/// `/r/:repo/~prs` (Phase G4's SPA half) — the GitHub PR read overlay: every
/// open PR on `repo`'s GitHub origin (`GET /api/prs`). A non-GitHub-origin/
/// no-origin repo is a hard 400 (`github.rs`'s module doc) — rendered as an
/// honest EmptyState rather than a generic error, since it's a repo-config
/// fact, not a failure. Every OTHER GitHub-side failure (rate-limited,
/// unreachable, a bad response) degrades to the response's own
/// `unavailable_reason` banner, `prs: []`, still a 200 — rendered inline
/// above an (empty) list rather than replacing the whole page.
///
/// PRR-U1 (design-ui.md §2, `/~prs` row): each row's PRIMARY CTA is now
/// "Open room" (a review already binds this PR number → `reviewUrl`) or
/// "Start review" (`POST /api/reviews/pr`, loopback-only, then navigate
/// straight into the Room). "Compare" survives as a SECONDARY action —
/// the pre-existing `postPrFetch` → three-dot Compare flow, unchanged
/// mechanics, just demoted from the row's one and only button.
export default function Prs() {
  // V70-A6 — root CLAUDE.md #31, ported: this list scrolls the WINDOW, so
  // one offset keyed on the full URL is the whole story. Back onto it lands
  // where the reader left, not where the browser guessed.
  useListScrollRestoration();
  const { repo = "" } = useParams<{ repo: string }>();
  const navigate = useNavigate();
  const prs = usePrs(repo);
  const [comparingNumber, setComparingNumber] = useState<number | null>(null);
  const [startingNumber, setStartingNumber] = useState<number | null>(null);
  const [startLoopback, setStartLoopback] = useState(false);

  const allReviewsQ = useReviews(repo, null);
  // `ReviewSummary` doesn't declare R4's additive `pr_number`/… fields —
  // the wire carries them regardless (`pr_binding_and_report_fields`'s
  // splice); intersect at the call site, same convention `ReviewDetailPr`
  // already established.
  const allReviews = (allReviewsQ.data?.reviews ?? []) as ReviewSummaryPr[];
  const boundReviews = useMemo(() => reviewByPrNumber(allReviews), [allReviews]);

  const createReviewPr = useCreateReviewPr(repo);

  async function handleCompare(number: number, baseRef: string) {
    setComparingNumber(number);
    try {
      await postPrFetch(repo, number);
      navigate(compareUrl(repo, { from: baseRef, to: prRef(number), threeDot: true }));
    } catch (e) {
      // A bare 404 with no structured `{"error": ...}` body is this route's
      // specific loopback-only signal (see `api/client.ts`'s `postPrFetch`
      // doc) — every OTHER failure (an actual `FetchFailed` 500, a network
      // error) gets its own real message instead.
      if (e instanceof ApiError && e.status === 404) {
        toast.err("PR fetch is loopback-only — open kb-code from the machine running kb-code-server to fetch it.");
      } else {
        const message = e instanceof Error ? e.message : String(e);
        toast.err(`PR fetch failed: ${message}`);
      }
    } finally {
      setComparingNumber(null);
    }
  }

  async function handleStartReview(number: number, baseRef: string) {
    if (startLoopback) return;
    setStartingNumber(number);
    try {
      const out = await createReviewPr.mutateAsync(buildCreateReviewPrInput(repo, { number, base_ref: baseRef }));
      navigate(reviewUrl(repo, out.id));
    } catch (e) {
      if (e instanceof ApiError && e.status === 404) {
        setStartLoopback(true);
        toast.err("Starting a review is loopback-only — open kb-code from the machine running kb-code-server.");
      } else {
        const message = e instanceof Error ? e.message : String(e);
        toast.err(`couldn't start review: ${message}`);
      }
    } finally {
      setStartingNumber(null);
    }
  }

  if (prs.isLoading) return <div className="kbc-reader__hint">Loading pull requests…</div>;

  if (prs.error) {
    if (prs.error instanceof ApiError && prs.error.status === 400) {
      return (
        <EmptyState
          icon={<Icon.Branch />}
          title="Not a GitHub origin"
          hint={`${repo}'s origin isn't a github.com remote (or has none configured) — the PR overlay only works against a GitHub-hosted repo.`}
          variant="view"
        />
      );
    }
    return <div className="kbc-reader__hint kbc-reader__hint--error">{(prs.error as Error).message}</div>;
  }

  const data = prs.data;
  if (!data) return null;

  return (
    <div className="kbc-prs">
      <h1 className="kbc-branches__title">Pull requests</h1>
      {data.unavailable_reason && (
        <div className="kbc-prs__unavailable" role="status" data-kbc-prs-unavailable>
          {data.unavailable_reason}
        </div>
      )}
      {data.truncated && (
        <div className="kbc-branches__truncated" data-kbc-prs-truncated>
          Showing a bounded subset of pull requests.
        </div>
      )}
      {!data.unavailable_reason && data.prs.length === 0 ? (
        <EmptyState icon={<Icon.Branch />} title="No open pull requests" />
      ) : (
        <ul className="kbc-prs__list" data-kbc-prs-list>
          {data.prs.map((pr) => {
            const bound = boundReviews.get(pr.number);
            return (
              <li key={pr.number} className="kbc-prs__row" data-kbc-prs-row={pr.number}>
                <span className="kbc-prs__number">#{pr.number}</span>
                <span className="kbc-prs__title">{pr.title}</span>
                {pr.draft && (
                  <span className="kbc-prs__draft-badge" data-kbc-prs-draft>
                    draft
                  </span>
                )}
                <span className="kbc-prs__author">{pr.author}</span>
                <span className="kbc-prs__refs">
                  {pr.head_ref} → {pr.base_ref}
                </span>
                <span className="kbc-prs__updated">{relativeTime(Date.parse(pr.updated_at) / 1000)}</span>
                <span className="kbc-prs__cta-group">
                  {bound ? (
                    <Link
                      to={reviewUrl(repo, bound.id)}
                      className="kbc-prs__room-link"
                      data-kbc-prs-room={pr.number}
                    >
                      Open room
                    </Link>
                  ) : (
                    <button
                      type="button"
                      className="kbc-prs__fetch-btn"
                      disabled={startingNumber === pr.number || startLoopback}
                      title={
                        startLoopback
                          ? "loopback-only — open kb-code from the machine running kb-code-server"
                          : undefined
                      }
                      onClick={() => void handleStartReview(pr.number, pr.base_ref)}
                      data-kbc-prs-start={pr.number}
                    >
                      {startingNumber === pr.number ? "Starting…" : "Start review"}
                    </button>
                  )}
                  <button
                    type="button"
                    className="kbc-prs__compare-btn"
                    disabled={comparingNumber === pr.number}
                    onClick={() => void handleCompare(pr.number, pr.base_ref)}
                    data-kbc-prs-compare={pr.number}
                  >
                    {comparingNumber === pr.number ? "Fetching…" : "Compare"}
                  </button>
                </span>
              </li>
            );
          })}
        </ul>
      )}
    </div>
  );
}
