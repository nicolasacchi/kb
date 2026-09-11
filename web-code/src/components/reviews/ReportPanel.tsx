// PRR-U2 §2 S2 (Report tab) + §6 (Import affordance, thin) + §8
// (report/count drift) — the Report tab: deck under the header, Section 01
// Summary (markdown-lite), Section 02 findings, Section 03 files (rows link
// into the Files tab), Section 04 CI checks, and (v1, this unit's placement
// call — see this unit's own report) Section 05 GitHub conversation.
import { useMemo } from "react";
import type {
  ClaimOut,
  FindingSeverity,
  ReviewDetailPr,
  ReviewFinding,
  ReviewReport,
  ReviewReportOut,
} from "../../api/types";
import { useReviewFiles, useReviewFindings, useReviewReport } from "../../hooks/useReviews";
import { parseMarkdownLite, type InlineRun, type MarkdownBlock } from "../../lib/markdownLite";
import { FenceBlock } from "../SafeMarkdown";
import { findingFacetText, findingFacets } from "../../lib/reviewDoc";
// V76-R2a — the live-counts derivation's home moved to `lib/reviewRoom.ts`
// (the hero needs it without a component↔lib cycle); re-exported here so
// existing importers (`ReportPanel.test.ts`) keep working.
import { liveFindingCounts, type LiveCounts } from "../../lib/reviewRoom";
export { liveFindingCounts, type LiveCounts };
import AgentVerdictCard from "./AgentVerdictCard";
import CiChecksCard from "./CiChecksCard";
import ClaimRegister from "./ClaimRegister";
import FindingCard, { findingAct, severityRank } from "./FindingCard";
import GithubConversationCard from "./GithubConversationCard";
// ── V76-R2a — the hero + section decorators (icon + colour token per
// section kind) replace the bare `kbc-eyebrow` text rows. ──
import ReportHero from "./ReportHero";
import { SectionDecor } from "./RoomChips";

/// `GET /api/reviews/{id}/report`'s ONLY structural signal for "no report
/// exists" is the literal `{report: null}` shape — every OTHER response is
/// the raw stored blob (which normally has no `report` key at all; see
/// `ReviewReportEmpty`'s own doc). Total: never throws on a malformed value.
export function hasReviewReport(out: ReviewReportOut | undefined): out is ReviewReport {
  if (!out) return false;
  if ("report" in out && (out as { report: unknown }).report === null) return false;
  return true;
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

function InlineRuns({ runs }: { runs: InlineRun[] }) {
  return (
    <>
      {runs.map((r, i) =>
        r.kind === "bold" ? <b key={i}>{r.text}</b> : r.kind === "code" ? <code key={i}>{r.text}</code> : <span key={i}>{r.text}</span>,
      )}
    </>
  );
}

function MarkdownLite({ text }: { text: string }) {
  const blocks: MarkdownBlock[] = useMemo(
    () => parseMarkdownLite(text, { fences: true }),
    [text],
  );
  return (
    <div className="kbc-report__summary">
      {blocks.map((b, i) => {
        if (b.kind === "paragraph") {
          return (
            <p key={i}>
              <InlineRuns runs={b.runs} />
            </p>
          );
        }
        if (b.kind === "list") {
          return (
            <ul key={i}>
              {b.items.map((item, j) => (
                <li key={j}>
                  <InlineRuns runs={item} />
                </li>
              ))}
            </ul>
          );
        }
        // V76-C1 opts fences ON so a report summary that cites a snippet
        // paints through highlight/1. Headings stay rendered as prose.
        if (b.kind === "heading") {
          return (
            <p key={i}>
              <InlineRuns runs={b.runs} />
            </p>
          );
        }
        return <FenceBlock key={i} text={b.text} lang={b.lang} />;
      })}
    </div>
  );
}

export interface ReportPanelProps {
  repo: string;
  review: ReviewDetailPr;
  ps: string;
  onOpenFilesTab: () => void;
  /// V73-K2c (kbc-claim/1, design D18) — this review's claims, fetched once
  /// by `ReviewDetail.tsx` and rendered beside the findings list below.
  claims: ClaimOut[];
  /// V76-R2a — a hero count chip filters the findings rail to that
  /// severity. The rail's filter state lives in `ReviewDetail.tsx` (lifted,
  /// so this panel and the rail can never disagree about what "filtered"
  /// means); absent in tests that render the panel standalone.
  onFilterFindings?: (severity: FindingSeverity) => void;
}

/// findings v2's `act` splits the list one more way: a `praise` finding is
/// not a finding at all in the severity sense — it gets its own green
/// section (below), and Section 02's count/counts stay honest about what
/// they exclude.
export function isPraiseFinding(f: ReviewFinding): boolean {
  return findingAct(f) === "praise";
}

export default function ReportPanel({ repo, review, ps, onOpenFilesTab, claims, onFilterFindings }: ReportPanelProps) {
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
  // V76-R2a — praise findings leave the findings list for their own green
  // section. Both lists are CUTS of the SAME `sorted` array; the counts
  // below name what each cut holds, so the split never hides a row.
  const praise = sorted.filter(isPraiseFinding);
  const actionable = sorted.filter((f) => !isPraiseFinding(f));
  const live = liveFindingCounts(findings);
  const drift = statDriftCaption(report.stats, live);
  const files = filesQ.data?.files ?? [];

  return (
    <div data-kbc-report-panel={review.id}>
      {/* V76-R2a — the hero: title, PR/base/head chips, risk dial, the
          three live counts as rail-filtering chips, lede, files-viewed.
          The agent block inside is HIDDEN when unset. */}
      <ReportHero
        repo={repo}
        review={review}
        report={report}
        findings={findings}
        files={files}
        onFilterFindings={(sev) => onFilterFindings?.(sev)}
      />

      <div className="kbc-verdicts" data-kbc-room-section="verdict">
        <AgentVerdictCard report={report} />
      </div>

      {drift && (
        <p className="kbc-report__drift-caption" data-kbc-report-drift title={drift}>
          ⓘ {drift}
        </p>
      )}

      {report.summary && (
        <>
          <SectionDecor kind="summary" />
          <MarkdownLite text={report.summary} />
        </>
      )}

      <SectionDecor
        kind="findings"
        title={`Findings · ${actionable.length}`}
        count={undefined}
      />
      {/* findings v2 (V73-K2b) — the act/blocking facets. DERIVED from the
          rows this section already renders and from nothing else: a second
          VIEW of one list, never a second count (which is exactly how
          kb-code once shipped three disagreeing "usages" numbers). */}
      {actionable.length > 0 && (
        <p className="kbc-report__facets" data-kbc-report-facets>
          {findingFacetText(findingFacets(actionable))}
        </p>
      )}
      {actionable.length === 0 ? (
        <p className="kbc-review__card-empty" data-kbc-report-findings-empty>
          No findings on this patchset.
        </p>
      ) : (
        actionable.map((f) => <FindingCard key={f.slug} repo={repo} reviewId={review.id} finding={f} ps={ps} />)
      )}

      {/* V76-R2a — praise is its own green section, AFTER the actionable
          list: it re-orders nothing and hides nothing (the split is named
          in both sections' counts). */}
      {praise.length > 0 && (
        <>
          <SectionDecor kind="praise" title={`Praise · ${praise.length}`} />
          <div className="kbc-room-praise" data-kbc-room-praise>
            {praise.map((f) => (
              <FindingCard key={f.slug} repo={repo} reviewId={review.id} finding={f} ps={ps} />
            ))}
          </div>
        </>
      )}

      {/* V73-K2c (kbc-claim/1, design D18) — the claim register, BESIDE the
          findings list rather than folded into it: a claim is agent PROSE
          the daemon cannot re-derive, a finding is a disposition-bearing
          judgement — two different registers on the same page. */}
      <ClaimRegister repo={repo} reviewId={review.id} claims={claims} scopeLabel="this review" />

      {/* Files/CI/GitHub keep the plain eyebrow — the decorator vocabulary
          (lib/reviewRoom.ts's `RoomSectionKind`) covers the Room's PROSE
          sections; these three are wire listings, not prose. */}
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
