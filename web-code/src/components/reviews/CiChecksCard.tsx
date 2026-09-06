// PRR-U2 §2 S2 (Report tab Section 04) + §8 (states) — CI checks, with a
// "fetched HH:MM" caption (the snapshot travels with the review, never
// re-fetched silently) and a named-absence degrade when GitHub was
// unavailable at fetch time.
import type { CheckRunOut } from "../../api/types";
import { useReviewChecks } from "../../hooks/useReviews";
import { Icon } from "../icons";

export interface ChecksSummary {
  total: number;
  pass: number;
  fail: number;
  warn: number;
  pending: number;
  /// The worst status present, by GitHub-check severity (`fail` > `pending`
  /// > `warn` > `pass`) — `null` for an empty check list (no signal, not a
  /// green one).
  worst: "fail" | "pending" | "warn" | "pass" | null;
}

const WORST_ORDER: Array<ChecksSummary["worst"]> = ["fail", "pending", "warn", "pass"];

/// Pure aggregate over a check-run list — the CI summary chip (`✓ 17/17`)
/// and the worst-status color both derive from this, never re-counted ad
/// hoc at each render site.
export function summarizeChecks(checks: CheckRunOut[]): ChecksSummary {
  const summary: ChecksSummary = { total: checks.length, pass: 0, fail: 0, warn: 0, pending: 0, worst: null };
  for (const c of checks) {
    if (c.status === "pass") summary.pass += 1;
    else if (c.status === "fail") summary.fail += 1;
    else if (c.status === "warn") summary.warn += 1;
    else summary.pending += 1;
  }
  for (const w of WORST_ORDER) {
    const key = w as "pass" | "fail" | "warn" | "pending";
    if (summary[key] > 0) {
      summary.worst = w;
      break;
    }
  }
  return summary;
}

/// Named-absence text for the checks section when nothing was fetched —
/// distinguishes "GitHub unavailable at import" (a reason string is
/// present) from "genuinely zero checks configured" (fetched fine, empty
/// list) — never conflates the two into one generic "no checks" line.
export function checksAbsenceReason(unavailableReason: string | undefined, total: number): string | null {
  if (unavailableReason) {
    return `checks not fetched — GitHub unavailable (${unavailableReason})`;
  }
  if (total === 0) {
    return "no CI checks reported for this PR's head commit";
  }
  return null;
}

export function formatFetchedCaption(fetchedAtUnixSeconds: number | undefined): string | null {
  if (fetchedAtUnixSeconds == null) return null;
  const d = new Date(fetchedAtUnixSeconds * 1000);
  const hh = String(d.getHours()).padStart(2, "0");
  const mm = String(d.getMinutes()).padStart(2, "0");
  return `fetched ${hh}:${mm}`;
}

export interface CiChecksCardProps {
  repo: string;
  reviewId: number;
  prNumber: number | undefined;
}

export default function CiChecksCard({ repo, reviewId, prNumber }: CiChecksCardProps) {
  const q = useReviewChecks(repo, reviewId, prNumber);

  if (prNumber == null) return null;

  if (q.isLoading) {
    return (
      <div className="kbc-eyebrow-section" data-kbc-ci-checks-loading>
        <div className="kbc-skeleton" style={{ height: 60 }} />
      </div>
    );
  }

  const checks = q.data?.checks ?? [];
  const reason = checksAbsenceReason(q.data?.unavailable_reason, checks.length);
  const summary = summarizeChecks(checks);
  const dataFetchedAt = q.dataUpdatedAt ? Math.floor(q.dataUpdatedAt / 1000) : undefined;

  return (
    <div data-kbc-ci-checks={reviewId}>
      {reason ? (
        <p className="kbc-review__card-empty" data-kbc-ci-checks-absent>
          {reason}
        </p>
      ) : (
        <>
          <div className="kbc-ci-rows">
            {checks.map((c) => (
              <div className="kbc-ci-row" key={c.name} data-kbc-ci-row={c.name}>
                <span className={`st st--${c.status}`} data-kbc-ci-status={c.status}>
                  {c.status === "pass" ? "✓" : c.status === "fail" ? "✗" : c.status === "warn" ? "!" : "…"}
                </span>
                <span className="nm">{c.name}</span>
                {c.note && <span className="note">{c.note}</span>}
                {c.duration != null && (
                  <span className="dur">
                    {Math.floor(c.duration / 60)}m {c.duration % 60}s
                  </span>
                )}
              </div>
            ))}
          </div>
          {q.data?.truncated && (
            <p className="kbc-review__card-empty" data-kbc-ci-checks-truncated>
              list truncated
            </p>
          )}
        </>
      )}
      <div className="kbc-ci-cap" data-kbc-ci-checks-caption>
        {summary.total > 0 && (
          <span data-kbc-ci-summary={summary.worst ?? "none"}>
            <Icon.Check /> {summary.pass}/{summary.total}
          </span>
        )}
        {formatFetchedCaption(dataFetchedAt) && <span> · {formatFetchedCaption(dataFetchedAt)}</span>}
        <span> · snapshot travels with the review — never re-fetched silently</span>
      </div>
    </div>
  );
}
