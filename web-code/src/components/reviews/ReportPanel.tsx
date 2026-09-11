// PRR-U2 §2 S2 (Report tab) + §6 (Import affordance, thin) + §8
// (report/count drift) — the Report tab: deck under the header, Section 01
// Summary (markdown-lite), Section 02 findings, Section 03 files (rows link
// into the Files tab), Section 04 CI checks, and (v1, this unit's placement
// call — see this unit's own report) Section 05 GitHub conversation.
import type {
  ClaimOut,
  ReviewDetailPr,
  ReviewFinding,
  ReviewReport,
  ReviewReportOut,
} from "../../api/types";
import { useReviewFiles, useReviewFindings, useReviewReport } from "../../hooks/useReviews";
import ProseBlock from "../prose/ProseBlock";
import { findingFacetText, findingFacets } from "../../lib/reviewDoc";
import AgentVerdictCard from "./AgentVerdictCard";
import CiChecksCard from "./CiChecksCard";
import ClaimRegister from "./ClaimRegister";
import FindingCard, { severityRank } from "./FindingCard";
import GithubConversationCard from "./GithubConversationCard";

/// `GET /api/reviews/{id}/report`'s ONLY structural signal for "no report
/// exists" is the literal `{report: null}` shape — every OTHER response is
/// the raw stored blob (which normally has no `report` key at all; see
/// `ReviewReportEmpty`'s own doc). Total: never throws on a malformed value.
export function hasReviewReport(out: ReviewReportOut | undefined): out is ReviewReport {
  if (!out) return false;
  if ("report" in out && (out as { report: unknown }).report === null) return false;
  return true;
}

export interface LiveCounts {
  blockers: number;
  concerns: number;
  verified: number;
}

/// Stat tiles are ALWAYS derived from live finding rows (§8) — this is that
/// derivation, pure and shared with the drift-caption check below.
export function liveFindingCounts(findings: ReviewFinding[]): LiveCounts {
  const counts: LiveCounts = { blockers: 0, concerns: 0, verified: 0 };
  for (const f of findings) {
    if (f.superseded) continue;
    if (f.severity === "blocker") counts.blockers += 1;
    else if (f.severity === "concern") counts.concerns += 1;
    else counts.verified += 1;
  }
  return counts;
}

/// §8's "report/count drift" caption — a quiet `ⓘ` note when the imported
/// report's OWN claimed counts disagree with what the live finding rows say
/// right now (a later disposition/carry-forward/manual finding can drift
/// the two apart). `null` when the report carries no `stats` claim at all
/// (nothing to compare) or when every claimed count that IS present still
/// matches. Never silently "fixes" the report's number — the drift is
/// surfaced, not resolved.
export function statDriftCaption(claimed: ReviewReport["stats"] | undefined, live: LiveCounts): string | null {
  if (!claimed) return null;
  const parts: string[] = [];
  if (claimed.blockers != null && claimed.blockers !== live.blockers) {
    parts.push(`${claimed.blockers} blocker${claimed.blockers === 1 ? "" : "s"} claimed; ${live.blockers} live`);
  }
  if (claimed.concerns != null && claimed.concerns !== live.concerns) {
    parts.push(`${claimed.concerns} concern${claimed.concerns === 1 ? "" : "s"} claimed; ${live.concerns} live`);
  }
  if (claimed.verified != null && claimed.verified !== live.verified) {
    parts.push(`${claimed.verified} verified claimed; ${live.verified} live`);
  }
  if (parts.length === 0) return null;
  return `report said ${parts.join(" · ")}`;
}

/// S6's thin import affordance — the copyable CLI two-step (findings import
/// + report set) named exactly per design-server.md §3's verb table. No
/// file-picker dialog (that's SHOULD-tier, `kb-code review report --set
/// --from-file` already covers the loopback path a real operator uses).
export function importCliCommands(reviewId: number): string[] {
  return [
    `kb-code review findings import ${reviewId} --stdin --json`,
    `kb-code review report ${reviewId} --set --from-file report.json --json`,
  ];
}

export interface ReportPanelProps {
  repo: string;
  review: ReviewDetailPr;
  ps: string;
  onOpenFilesTab: () => void;
  /// V73-K2c (kbc-claim/1, design D18) — this review's claims, fetched once
  /// by `ReviewDetail.tsx` and rendered beside the findings list below.
  claims: ClaimOut[];
}

export default function ReportPanel({ repo, review, ps, onOpenFilesTab, claims }: ReportPanelProps) {
  const reportQ = useReviewReport(repo, review.id);
  const findingsQ = useReviewFindings(repo, review.id, { ps });
  const filesQ = useReviewFiles(repo, review.id, ps);

  if (reportQ.isLoading) {
    return (
      <div data-kbc-report-loading>
        <div className="kbc-skeleton" style={{ height: 96, marginBottom: 12 }} />
        <div className="kbc-skeleton" style={{ height: 200 }} />
      </div>
    );
  }

  if (!hasReviewReport(reportQ.data)) {
    const commands = importCliCommands(review.id);
    return (
      <div className="kbc-report-empty" data-kbc-report-empty>
        <p>
          No agent review imported yet
          {review.pr_number != null ? " for this PR-bound review" : ""}. From a Claude session, run:
        </p>
        <pre className="kbc-report-empty__commands" data-kbc-report-empty-commands>
          {commands.join("\n")}
        </pre>
      </div>
    );
  }

  const report = reportQ.data;
  const findings = (findingsQ.data?.findings ?? []).filter((f) => !f.superseded);
  const sorted = [...findings].sort((a, b) => severityRank(a.severity) - severityRank(b.severity));
  const live = liveFindingCounts(findings);
  const drift = statDriftCaption(report.stats, live);
  const files = filesQ.data?.files ?? [];

  return (
    <div data-kbc-report-panel={review.id}>
      {report.deck && (
        <div className="kbc-report__deck" data-kbc-report-deck>
          <ProseBlock text={report.deck} repo={repo} reviewId={review.id} inline />
        </div>
      )}

      <div className="kbc-verdicts">
        <AgentVerdictCard report={report} repo={repo} reviewId={review.id} />
      </div>

      <div className="kbc-stats" data-kbc-report-stats>
        <div className="cell b">
          <div className="l">Blockers</div>
          <div className="v">{live.blockers}</div>
        </div>
        <div className="cell c">
          <div className="l">Concerns</div>
          <div className="v">{live.concerns}</div>
        </div>
        <div className="cell o">
          <div className="l">Verified</div>
          <div className="v">{live.verified}</div>
        </div>
      </div>
      {drift && (
        <p className="kbc-report__drift-caption" data-kbc-report-drift title={drift}>
          ⓘ {drift}
        </p>
      )}

      {report.summary && (
        <>
          <div className="kbc-eyebrow">Section 01 · Summary</div>
          <ProseBlock
            className="kbc-report__summary"
            text={report.summary}
            refs={report.summary_refs}
            repo={repo}
            reviewId={review.id}
          />
        </>
      )}

      <div className="kbc-eyebrow">
        Section 02 · {sorted.length} finding{sorted.length === 1 ? "" : "s"}
      </div>
      {/* findings v2 (V73-K2b) — the act/blocking facets. DERIVED from the
          rows this section already renders and from nothing else: a second
          VIEW of one list, never a second count (which is exactly how
          kb-code once shipped three disagreeing "usages" numbers). */}
      {sorted.length > 0 && (
        <p className="kbc-report__facets" data-kbc-report-facets>
          {findingFacetText(findingFacets(sorted))}
        </p>
      )}
      {sorted.length === 0 ? (
        <p className="kbc-review__card-empty" data-kbc-report-findings-empty>
          No findings on this patchset.
        </p>
      ) : (
        sorted.map((f) => <FindingCard key={f.slug} repo={repo} reviewId={review.id} finding={f} ps={ps} />)
      )}

      {/* V73-K2c (kbc-claim/1, design D18) — the claim register, BESIDE the
          findings list rather than folded into it: a claim is agent PROSE
          the daemon cannot re-derive, a finding is a disposition-bearing
          judgement — two different registers on the same page. */}
      <ClaimRegister repo={repo} reviewId={review.id} claims={claims} scopeLabel="this review" />

      <div className="kbc-eyebrow">Section 03 · Files</div>
      {files.length === 0 ? (
        <p className="kbc-review__card-empty">No file changes on this patchset.</p>
      ) : (
        <div className="kbc-report__files" data-kbc-report-files>
          {files.map((f) => (
            <button
              type="button"
              key={f.path}
              className="kbc-filerow"
              onClick={onOpenFilesTab}
              data-kbc-report-file-row={f.path}
            >
              <span
                className={
                  "dot " +
                  (f.open_annotations > 0 ? "dot--concern" : f.viewed ? "dot--ok" : "dot--none")
                }
                aria-hidden="true"
              />
              <span className="path">{f.path}</span>
              <span className="delta">
                <span className="add">+{f.additions}</span> <span className="del">−{f.deletions}</span>
              </span>
            </button>
          ))}
        </div>
      )}

      <div className="kbc-eyebrow">Section 04 · CI checks</div>
      <CiChecksCard repo={repo} reviewId={review.id} prNumber={review.pr_number} />

      {review.pr_number != null && (
        <>
          <div className="kbc-eyebrow">Section 05 · GitHub conversation</div>
          <GithubConversationCard
            repo={repo}
            reviewId={review.id}
            prNumber={review.pr_number}
            prRepoSlug={review.pr_repo_slug}
          />
        </>
      )}
    </div>
  );
}
