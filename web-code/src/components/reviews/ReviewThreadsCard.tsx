import { useMemo, useState } from "react";
import { Link } from "react-router";
import type { FindingDispositionState, FindingSeverity, ReviewComment, ReviewFinding } from "../../api/types";
import { useReviewComments } from "../../hooks/useReviewComments";
import { useGithubThreads, useReviewFindings } from "../../hooks/useReviews";
import { appendReviewParam, codeUrl } from "../../lib/codeUrl";
import { findingsByAnnotationId } from "../../lib/diffFindings";
import { matchesQuestionFilter, questionStateForThread, type QuestionFilterKey } from "../../lib/questionState";
import { commentSide, indexThreads } from "../../lib/reviewComments";
import { FindingRow } from "./FindingCard";
import ProseBlock from "../prose/ProseBlock";
import GithubDiffCard from "./GithubDiffCard";
import PromoteToFinding from "./PromoteToFinding";
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

/// V80-M4 — "open in reader": the SAME thread, at the patchset tip, in the
/// plain code reader rather than the review diff — a human's comment must
/// be reachable whether or not its file is in the diff at all (M1 handles
/// the diff side; this is the reader side). `null` for the review-level
/// "General" group (`path === ""`, no file to open) or when the tip sha is
/// not known yet (the review's files/patchsets query still loading) —
/// absence, never a broken link. Lands at the thread's own carry-forward
/// line via `codeUrl`'s `ref=<tipSha>` (never the orphaned original line —
/// an orphan's line is honestly unresolved at the CURRENT tip, so it opens
/// the file rather than guessing a line), and carries `?review=<id>` (M3)
/// so the reader's "current review" chip/rail picks the review up too.
export function threadReaderHref(
  repo: string,
  reviewId: number,
  thread: ReviewComment,
  tipSha: string | undefined,
): string | null {
  if (!tipSha || thread.path === "") return null;
  const line = !thread.resolution.orphaned && thread.resolution.line != null ? thread.resolution.line : undefined;
  return appendReviewParam(codeUrl({ repo, path: thread.path, ref: tipSha, line }), String(reviewId));
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
  /// V80-M4 — the active patchset's tip sha, for each thread's "open in
  /// reader" link (`threadReaderHref`). Absent ⇒ that link is simply not
  /// rendered (the review's own files/patchsets query hasn't resolved
  /// yet), never a broken href.
  tipSha?: string;
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

// V80-R3 — presentational split of the SAME `FILTERS` list (one shared
// `filter`/`setFilter` state, unchanged) into a mutually-exclusive STATE
// segment and the two "awaiting" + GitHub toggles beside it — the "Why"'s
// overcrowded single seven-chip row, now two scannable groups. Every
// button below keeps its `data-kbc-review-threads-filter` hook regardless
// of which group renders it.
const STATE_KEYS: ThreadFilter[] = ["all", "open", "resolved", "orphaned"];
const STATE_FILTERS = FILTERS.filter((f) => (STATE_KEYS as string[]).includes(f.key));
const AWAITING_FILTERS = FILTERS.filter((f) => !(STATE_KEYS as string[]).includes(f.key));

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
  tipSha,
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
        inDiff: g.in_diff,
        comments: g.comments.filter((c) => matches(filter, c, findingsByAnnId)),
      }))
      .filter((g) => g.comments.length > 0);
  }, [q.data, filter, findingsByAnnId]);

  // V80-M4 — "the Room reads human threads first-class": every group (a
  // path, or the path-less General group) sorts into exactly one of three
  // sections, off M0's own per-read `in_diff` caption — never a second
  // fetch, never a re-derived flag. `path === ""` (General) wins over
  // `in_diff` (which M0's own doc pins `false` for that group anyway, since
  // there is no file to be "in" the diff at all).
  const sections = useMemo(() => {
    const inDiff: typeof groups = [];
    const outsideDiff: typeof groups = [];
    const general: typeof groups = [];
    for (const g of groups) {
      if (g.path === "") general.push(g);
      else if (g.inDiff) inDiff.push(g);
      else outsideDiff.push(g);
    }
    return { inDiff, outsideDiff, general };
  }, [groups]);

  const sectionCounts = {
    inDiff: sections.inDiff.reduce((n, g) => n + g.comments.length, 0),
    outsideDiff: sections.outsideDiff.reduce((n, g) => n + g.comments.length, 0),
    general: sections.general.reduce((n, g) => n + g.comments.length, 0),
  };

  // One path/general group, shared by all three sections below — the
  // group-level chrome (file-jump button vs. the "General" label) and the
  // per-thread row are otherwise byte-identical regardless of which
  // section a group landed in.
  function renderGroup(g: (typeof groups)[number]) {
    return (
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
          const linkedFinding = findingsByAnnId.get(c.id) ?? null;
          // V80-M5 — "success re-renders the thread as a finding card": a
          // thread whose annotation now backs a HUMAN-authored
          // (`origin: "manual"`) finding — whether promoted just now or
          // authored earlier via the diff composer's own Finding mode —
          // renders as the SAME compact `FindingRow` the top "Findings"
          // roll-up uses, rather than a plain thread row a human would
          // then have to cross-reference. Agent-imported findings keep
          // their pre-M5 plain-row rendering here (unchanged) — only this
          // unit's own manual-authorship class gets the swap.
          if (linkedFinding && linkedFinding.origin === "manual") {
            return <FindingRow key={c.id} repo={repo} reviewId={reviewId} finding={linkedFinding} ps={ps} />;
          }
          const qstate = questionStateForThread(c, linkedFinding);
          // V80-M4 — "open in reader": a SIBLING link, never nested inside
          // the "open in diff" row's own `<Link>` (HTML forbids nested
          // anchors) — `data-kbc-review-threads-row` keeps naming the
          // EXISTING "open in diff" link, unchanged, so the pre-M4 e2e
          // click-to-diff behaviour is byte-identical.
          const readerHref = threadReaderHref(repo, reviewId, c, tipSha);
          return (
            <div key={c.id} className="kbc-rthreads__thread">
              <Link
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
                · {c.author}:{" "}
                {/* V80-R3 — the thread's own body reads a step above its
                    author/time meta (which stays at the row's `--fs-sm`
                    floor) — the "Why"'s complaint that the rail is
                    uniformly the smallest text on the site. */}
                <span className="kbc-rthreads__text">
                  <ProseBlock
                    text={c.body}
                    refs={c.body_refs}
                    repo={repo}
                    reviewId={reviewId}
                    inline
                    nolink
                  />
                </span>
              </Link>
              {readerHref && (
                <Link
                  to={readerHref}
                  className="kbc-rthreads__reader-link"
                  data-kbc-review-threads-open-reader={c.id}
                >
                  open in reader
                </Link>
              )}
              <PromoteToFinding repo={repo} reviewId={reviewId} thread={c} />
            </div>
          );
        })}
      </div>
    );
  }

  return (
    <section className="kbc-review__card" data-kbc-review-annotations data-kbc-review-threads>
      {findingTotal > 0 && (
        <div className="kbc-review__findings" data-kbc-review-findings>
          <h2 className="kbc-review__card-title">
            Findings <span className="n">{findingTotal}</span>
          </h2>
          {/* V80-R3 — two LABELLED segmented controls on one line (was two
              unlabelled chip rows stacked with no indication of what either
              was filtering) — wraps to its own second group when the rail
              is too narrow for both, never overlapping or clipping. */}
          <div className="kbc-finding-filters">
            <div className="kbc-finding-filters__group">
              <span className="kbc-finding-filters__label" id="kbc-sevfilter-label">
                Severity
              </span>
              <div
                className="kbc-filters"
                role="group"
                aria-label="finding severity filter"
                aria-labelledby="kbc-sevfilter-label"
              >
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
            </div>
            <div className="kbc-finding-filters__group">
              <span className="kbc-finding-filters__label" id="kbc-dispofilter-label">
                Disposition
              </span>
              <div
                className="kbc-filters"
                role="group"
                aria-label="finding disposition filter"
                aria-labelledby="kbc-dispofilter-label"
              >
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
            </div>
          </div>
          {findings.length === 0 ? (
            <p className="kbc-review__card-empty">No findings match this filter.</p>
          ) : (
            findings.map((f) => <FindingRow key={f.slug} repo={repo} reviewId={reviewId} finding={f} ps={ps} />)
          )}
        </div>
      )}
      <h2 className="kbc-review__card-title">Threads</h2>
      {/* V80-M4 — the section count line ALWAYS names all three counts,
          even at zero — a filter names what it hides (this codebase's own
          honesty rule), and a reader must be able to tell "0 outside the
          diff" apart from "the count is simply not shown here." Counts
          COMMENTS (post-filter), not groups, matching what actually
          renders below. */}
      <p className="kbc-rthreads__section-counts" data-kbc-review-threads-section-counts>
        {sectionCounts.inDiff} in the diff · {sectionCounts.outsideDiff} outside · {sectionCounts.general} general
      </p>
      {/* V80-R3 — ONE segmented state control (All/Open/Resolved/Orphaned,
          mutually exclusive) plus the "awaiting" + GitHub toggles as a
          second, separate group — the "Why"'s third overcrowded chip row,
          split for scanability. Still ONE `filter`/`setFilter` state; every
          button keeps its `data-kbc-review-threads-filter` hook. */}
      <div className="kbc-rthreads__filters" role="group" aria-label="thread state">
        {STATE_FILTERS.map((f) => (
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
          </button>
        ))}
      </div>
      <div className="kbc-rthreads__awaiting" role="group" aria-label="awaiting / GitHub">
        {AWAITING_FILTERS.map((f) => (
          <button
            key={f.key}
            type="button"
            className={"kbc-rthreads__filter" + (filter === f.key ? " is-active" : "")}
            aria-pressed={filter === f.key}
            onClick={() => setFilter(f.key)}
            data-kbc-review-threads-filter={f.key}
          >
            {f.label}
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
        <>
          {/* V80-M4 — sectioned In the diff / Outside the diff / General
              (off M0's own `in_diff` caption); a section renders only when
              non-empty (the count line above already named all three). */}
          {sections.inDiff.length > 0 && (
            <div className="kbc-rthreads__section" data-kbc-review-threads-section="in-diff">
              <h3 className="kbc-rthreads__section-title">In the diff</h3>
              {sections.inDiff.map((g) => renderGroup(g))}
            </div>
          )}
          {sections.outsideDiff.length > 0 && (
            <div className="kbc-rthreads__section" data-kbc-review-threads-section="outside-diff">
              <h3 className="kbc-rthreads__section-title">Outside the diff</h3>
              {sections.outsideDiff.map((g) => renderGroup(g))}
            </div>
          )}
          {sections.general.length > 0 && (
            <div className="kbc-rthreads__section" data-kbc-review-threads-section="general">
              {/* No second "General" heading here — each general GROUP
                  already renders its own "General" label below (the
                  path==="" branch inside `renderGroup`); there is at most
                  one such group per review. */}
              {sections.general.map((g) => renderGroup(g))}
            </div>
          )}
        </>
      )}
    </section>
  );
}
