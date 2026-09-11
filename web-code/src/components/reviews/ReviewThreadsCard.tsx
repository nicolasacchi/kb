import { useMemo, useState } from "react";
import { Link } from "react-router-dom";
import type { FindingDispositionState, FindingSeverity, ReviewComment, ReviewFinding } from "../../api/types";
import { useReviewComments } from "../../hooks/useReviewComments";
import { useGithubThreads, useReviewFindings } from "../../hooks/useReviews";
import { findingsByAnnotationId } from "../../lib/diffFindings";
import { matchesQuestionFilter, questionStateForThread, type QuestionFilterKey } from "../../lib/questionState";
import { commentSide, indexThreads } from "../../lib/reviewComments";
import { FindingRow } from "./FindingCard";
import GithubDiffCard from "./GithubDiffCard";
import { reviewDiffHref } from "./ReviewHeader";
import { rungForMouse, type RampRung, type RampTarget } from "../../nav/ramp";

export type ThreadFilter = "all" | "open" | "resolved" | "orphaned" | "github" | QuestionFilterKey;

// ── PRR-U2 (§2 S2's side panel: "ReviewThreadsCard gains severity/
// disposition filter chips over findings, FindingRow list") ──
//
// PRR-U4 fills in the ❓ awaiting-agent / awaiting-you filters the U2 note
// above deferred (design doc §4's question/answer loop) — see `matches`'
// two new branches and `lib/questionState.ts`'s `matchesQuestionFilter`.

export type FindingSeverityFilter = "all" | FindingSeverity;
export type FindingDispositionFilter = "all" | "open" | FindingDispositionState;

/// Pure predicate — severity filter is exact-match-or-all; disposition
/// filter's `"open"` means "no disposition set yet" (undecided), matching
/// the server's own `disposition: NULL` meaning (`review_findings` §1.3).
export function matchesFindingFilters(
  finding: ReviewFinding,
  severity: FindingSeverityFilter,
  disposition: FindingDispositionFilter,
): boolean {
  if (finding.superseded) return false;
  if (severity !== "all" && finding.severity !== severity) return false;
  if (disposition === "all") return true;
  if (disposition === "open") return finding.disposition == null;
  return finding.disposition?.state === disposition;
}

export function threadHref(
  repo: string,
  reviewId: number,
  thread: ReviewComment,
  ps?: string,
): string {
  const base = reviewDiffHref(repo, reviewId, thread.path);
  const params = new URLSearchParams();
  params.set("thread", thread.id);
  if (ps && ps !== "latest") params.set("ps", ps);
  if (!thread.resolution.orphaned && thread.resolution.line != null) {
    params.set("line", String(thread.resolution.line));
    params.set("side", commentSide(thread));
  }
  return `${base}?${params.toString()}`;
}

export interface ReviewThreadsCardProps {
  repo: string;
  reviewId: number;
  ps: string;
  onOpenFile: (path: string) => void;
  /// V70-A6 — the Ramp (§P7). A thread row is a pointer AT CODE, so its
  /// "open elsewhere" gestures go through the ONE shared handler like every
  /// other result row. Absent ⇒ pre-A6 behaviour (a plain click only).
  onRamp?: (rung: RampRung, target: RampTarget) => void;
  /// PRR-F (design-addendum-2.md §A) — undefined for a non-PR-bound review;
  /// the "GitHub (N)" filter chip renders `(0)` and its row list stays
  /// permanently empty (the hook itself never fires — `useGithubThreads`'s
  /// own `enabled` gate).
  prNumber?: number;
  /// V76-R2a — optionally CONTROLLED severity filter: the Report hero's
  /// count chips filter this rail, so the state lives in `ReviewDetail.tsx`
  /// when threaded. Absent ⇒ the internal `useState` below, byte-identical
  /// to before.
  sevFilter?: FindingSeverityFilter;
  onSevFilter?: (f: FindingSeverityFilter) => void;
}

const FILTERS: { key: ThreadFilter; label: string }[] = [
  { key: "all", label: "All" },
  { key: "open", label: "Open" },
  { key: "resolved", label: "Resolved" },
  { key: "orphaned", label: "Orphaned" },
  { key: "awaiting-agent", label: "❓ awaiting agent" },
  { key: "awaiting-you", label: "❓ awaiting you" },
  { key: "github", label: "GitHub" },
];

function matches(
  filter: ThreadFilter,
  c: ReviewComment,
  findingsById: ReadonlyMap<string, ReviewFinding>,
): boolean {
  if (filter === "all") return true;
  if (filter === "orphaned") return c.resolution.orphaned;
  if (filter === "resolved") return c.resolved;
  if (filter === "open") return !c.resolved;
  // PRR-F — "github" is its own rendering branch (GithubDiffCard, not a
  // ReviewComment at all) — never matched here.
  if (filter === "github") return false;
  // PRR-U4 — design doc §4's mirrored question filters, delegated to the
  // shared pure predicate (`lib/questionState.ts`) every renderer uses.
  return matchesQuestionFilter(filter, c, findingsById.get(c.id) ?? null);
}

export default function ReviewThreadsCard({
  repo,
  reviewId,
  ps,
  onOpenFile,
  onRamp,
  prNumber,
  sevFilter: sevFilterProp,
  onSevFilter,
}: ReviewThreadsCardProps) {
  const q = useReviewComments(repo, reviewId, ps, true);
  const [filter, setFilter] = useState<ThreadFilter>("all");
  const indexed = useMemo(() => (q.data ? indexThreads(q.data) : null), [q.data]);

  // PRR-F — GitHub threads, own filter lane (see the module doc above and
  // `GithubDiffCard`'s own doc for why these never join the local `groups`
  // pipeline: a completely different, read-only wire shape).
  const githubQ = useGithubThreads(repo, reviewId, prNumber);
  const githubThreads = githubQ.data?.threads ?? [];

  const findingsQ = useReviewFindings(repo, reviewId, { ps });
  const [sevFilterLocal, setSevFilterLocal] = useState<FindingSeverityFilter>("all");
  // V76-R2a — controlled-when-threaded (the hero's count chips own it),
  // internal otherwise. One filter either way.
  const sevFilter = sevFilterProp ?? sevFilterLocal;
  const setSevFilter = onSevFilter ?? setSevFilterLocal;
  const [dispoFilter, setDispoFilter] = useState<FindingDispositionFilter>("all");
  const findings = useMemo(
    () => (findingsQ.data?.findings ?? []).filter((f) => matchesFindingFilters(f, sevFilter, dispoFilter)),
    [findingsQ.data, sevFilter, dispoFilter],
  );
  const findingTotal = (findingsQ.data?.findings ?? []).filter((f) => !f.superseded).length;

  // PRR-U4 — findings joined by ANNOTATION id (not slug), the same lookup
  // shape `DiffCommentsApi.findingsById` uses, so the ❓ chip/filter here
  // classifies a finding-backed thread identically to the diff view.
  const findingsByAnnId = useMemo(
    () => findingsByAnnotationId(findingsQ.data?.findings ?? []),
    [findingsQ.data],
  );

  const groups = useMemo(() => {
    if (!q.data) return [];
    return q.data.groups
      .map((g) => ({
        path: g.path,
        comments: g.comments.filter((c) => matches(filter, c, findingsByAnnId)),
      }))
      .filter((g) => g.comments.length > 0);
  }, [q.data, filter, findingsByAnnId]);

  return (
    <section className="kbc-review__card" data-kbc-review-annotations data-kbc-review-threads>
      {findingTotal > 0 && (
        <div className="kbc-review__findings" data-kbc-review-findings>
          <h2 className="kbc-review__card-title">
            Findings <span className="n">{findingTotal}</span>
          </h2>
          <div className="kbc-filters" role="group" aria-label="finding severity filter">
            {(["all", "blocker", "concern", "ok"] as FindingSeverityFilter[]).map((s) => (
              <button
                key={s}
                type="button"
                className={sevFilter === s ? "is-on" : ""}
                aria-pressed={sevFilter === s}
                onClick={() => setSevFilter(s)}
                data-kbc-finding-filter-severity={s}
              >
                {s === "all" ? "All" : s}
              </button>
            ))}
          </div>
          <div className="kbc-filters" role="group" aria-label="finding disposition filter">
            {(["all", "open", "agree", "dispute", "waive", "fix-later"] as FindingDispositionFilter[]).map(
              (d) => (
                <button
                  key={d}
                  type="button"
                  className={dispoFilter === d ? "is-on" : ""}
                  aria-pressed={dispoFilter === d}
                  onClick={() => setDispoFilter(d)}
                  data-kbc-finding-filter-disposition={d}
                >
                  {d === "all" ? "All" : d}
                </button>
              ),
            )}
          </div>
          {findings.length === 0 ? (
            <p className="kbc-review__card-empty">No findings match this filter.</p>
          ) : (
            findings.map((f) => <FindingRow key={f.slug} repo={repo} reviewId={reviewId} finding={f} ps={ps} />)
          )}
        </div>
      )}
      <h2 className="kbc-review__card-title">Threads</h2>
      <div className="kbc-rthreads__filters" role="group" aria-label="thread filter">
        {FILTERS.map((f) => (
          <button
            key={f.key}
            type="button"
            className={"kbc-rthreads__filter" + (filter === f.key ? " is-active" : "")}
            aria-pressed={filter === f.key}
            onClick={() => setFilter(f.key)}
            data-kbc-review-threads-filter={f.key}
          >
            {f.label}
            {f.key === "open" && indexed ? ` ${indexed.rollup.open}` : ""}
            {f.key === "github" ? ` (${githubThreads.length})` : ""}
          </button>
        ))}
      </div>
      {filter === "github" ? (
        githubQ.isLoading ? (
          <p className="kbc-review__card-empty">Loading…</p>
        ) : githubThreads.length === 0 ? (
          <p className="kbc-review__card-empty" data-kbc-review-threads-github-empty>
            {prNumber == null ? "This review isn't bound to a PR." : "No GitHub threads."}
          </p>
        ) : (
          githubThreads.map((t) => (
            <div key={t.id ?? `${t.path}-${t.created_at}`} className="kbc-review__ann-group">
              <button
                type="button"
                className="kbc-review__ann-path"
                onMouseDown={(e) => {
                  const rung = rungForMouse(e);
                  if (!rung || rung === "here" || !t.path || !onRamp) return;
                  e.preventDefault();
                  onRamp(rung, {
                    repo,
                    path: t.path,
                    line: t.resolved?.line,
                    via: "review",
                    subject: `github thread on ${t.path}`,
                  });
                }}
                onClick={() => t.path && onOpenFile(t.path)}
                disabled={!t.path}
                data-kbc-review-threads-github-open={t.id ?? undefined}
              >
                {t.path ?? "general"}
                {t.resolved ? `:L${t.resolved.line}` : ""}
              </button>
              <GithubDiffCard thread={t} showPositionFoot />
            </div>
          ))
        )
      ) : q.isLoading ? (
        <p className="kbc-review__card-empty">Loading…</p>
      ) : groups.length === 0 ? (
        <p className="kbc-review__card-empty">
          {filter === "all" ? "No threads on this change set." : `No ${filter} threads.`}
        </p>
      ) : (
        groups.map((g) => (
          <div key={g.path} className="kbc-review__ann-group">
            {g.path === "" ? (
              // PRR-U4 — the review-level (path-less "general question")
              // group, per design doc §4 + `review_comments.rs`'s
              // `build_comment_groups`: `path: ""` sorts first and has no
              // file to open, so it renders as a plain section label
              // instead of the file-jump button every other group gets.
              <div className="kbc-review__ann-path kbc-review__ann-path--general" data-kbc-review-ann-path="">
                General
              </div>
            ) : (
              <button
                type="button"
                className="kbc-review__ann-path"
                onMouseDown={(e) => {
                  const rung = rungForMouse(e);
                  if (!rung || rung === "here" || !onRamp) return;
                  e.preventDefault();
                  onRamp(rung, { repo, path: g.path, via: "review" });
                }}
                onClick={() => onOpenFile(g.path)}
                data-kbc-review-ann-path={g.path}
              >
                {g.path}
              </button>
            )}
            {g.comments.map((c) => {
              const qstate = questionStateForThread(c, findingsByAnnId.get(c.id) ?? null);
              return (
                <Link
                  key={c.id}
                  to={threadHref(repo, reviewId, c, ps)}
                  className={
                    "kbc-rthreads__row" +
                    (c.resolved ? " is-resolved" : "") +
                    (c.resolution.orphaned ? " is-orphaned" : "")
                  }
                  data-kbc-review-threads-row={c.id}
                  data-kbc-review-ann={c.id}
                >
                  <strong>{c.intent}</strong>
                  {qstate && (
                    <span
                      className={`kbc-rthreads__qstate kbc-rthreads__qstate--${qstate}`}
                      data-kbc-question-state={qstate}
                    >
                      {qstate === "awaiting-agent" ? "❓ awaiting agent" : "❓ awaiting you"}
                    </span>
                  )}
                  {c.resolution.orphaned && c.resolution.original
                    ? ` · was ps${c.resolution.original.ps}:L${c.resolution.original.line}`
                    : c.resolution.line != null
                      ? ` · L${c.resolution.line}`
                      : ""}{" "}
                  · {c.author}: {c.body}
                </Link>
              );
            })}
          </div>
        ))
      )}
    </section>
  );
}
