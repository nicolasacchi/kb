// V76-R2a — the Report tab's hero. One glance: title, PR link, base/head
// chips, risk dial, the three live counts as chips (each one filtering the
// findings rail), the lede, "N of M files viewed", and the agent block —
// HIDDEN when neither the report nor the review names an agent/session
// (never an "AGENT · UNSET" box; §8's "the room never lies" applies to
// absences most of all).
//
// Every number comes off the wire through `lib/reviewRoom.ts`'s
// derivations (`heroCounts` IS `ReportPanel`'s `liveFindingCounts`); this
// component computes nothing itself.
import { Link } from "react-router-dom";
import type { ReviewDetailPr, ReviewFileRow, ReviewFinding, ReviewReport } from "../../api/types";
import type { FindingSeverity } from "../../api/types";
import { Icon } from "../icons";
import {
  filesViewedOf,
  heroAgentOf,
  heroBaseSourceOf,
  heroCounts,
  heroLedeOf,
  truncateMiddle,
} from "../../lib/reviewRoom";
import { severityToken } from "./AgentVerdictCard";
import { prExternalUrl } from "./PrChip";
import RiskDial from "./RiskDial";
import { SectionDecor } from "./RoomChips";

export interface ReportHeroProps {
  repo: string;
  review: ReviewDetailPr;
  report: ReviewReport;
  findings: ReviewFinding[];
  files: ReviewFileRow[];
  /// A count chip filters the findings rail to that severity (`"ok"` for
  /// verified) — the rail's own filter state, lifted to `ReviewDetail`.
  onFilterFindings: (severity: FindingSeverity) => void;
}

export default function ReportHero({
  review,
  report,
  findings,
  files,
  onFilterFindings,
}: ReportHeroProps) {
  const counts = heroCounts(findings);
  const viewed = filesViewedOf(files);
  const lede = heroLedeOf(report);
  const agent = heroAgentOf(report, review.session_id ?? null);
  const baseSource = heroBaseSourceOf(review);
  const prUrl = prExternalUrl(review.pr_repo_slug, review.pr_number);
  const title = review.title?.trim() || review.head_ref;

  return (
    <section className="kbc-room-hero" data-kbc-room-hero>
      <div className="kbc-room-hero__top">
        <div className="kbc-room-hero__id">
          <h2 className="kbc-room-hero__title" data-kbc-room-hero-title>
            {title}
          </h2>
          <div className="kbc-room-hero__refs">
            {review.pr_number != null &&
              (prUrl ? (
                <a
                  className="kbc-room-chip kbc-room-chip--link"
                  style={{ ["--kbc-room-chip-color" as string]: "var(--blue)" }}
                  href={prUrl}
                  target="_blank"
                  rel="noreferrer"
                  data-kbc-room-hero-pr={review.pr_number}
                >
                  <Icon.PullRequest /> PR #{review.pr_number} <Icon.External />
                </a>
              ) : (
                <span
                  className="kbc-room-chip"
                  style={{ ["--kbc-room-chip-color" as string]: "var(--blue)" }}
                  data-kbc-room-hero-pr={review.pr_number}
                >
                  <Icon.PullRequest /> PR #{review.pr_number}
                </span>
              ))}
            <span
              className="kbc-room-chip"
              style={{ ["--kbc-room-chip-color" as string]: "var(--ink-mute)" }}
              title={review.base_ref}
              data-kbc-room-hero-base
            >
              <Icon.Branch /> base {truncateMiddle(review.base_ref, 32)}
              {baseSource && <span className="kbc-room-hero__src"> · {baseSource}</span>}
            </span>
            <span aria-hidden="true" className="kbc-room-hero__arrow">
              →
            </span>
            <span
              className="kbc-room-chip"
              style={{ ["--kbc-room-chip-color" as string]: "var(--ink)" }}
              title={review.head_ref}
              data-kbc-room-hero-head
            >
              <Icon.Branch /> head {truncateMiddle(review.head_ref, 32)}
            </span>
          </div>
        </div>
        {report.risk_score != null && (
          <RiskDial score={report.risk_score} color={severityToken(report.verdict)} />
        )}
      </div>

      <div className="kbc-room-hero__counts" data-kbc-room-hero-counts>
        <button
          type="button"
          className="kbc-room-chip kbc-room-chip--btn"
          style={{ ["--kbc-room-chip-color" as string]: "var(--red)" }}
          onClick={() => onFilterFindings("blocker")}
          title="filter the findings rail to blockers"
          data-kbc-room-hero-count="blocker"
        >
          <Icon.Warn /> {counts.blockers} blocker{counts.blockers === 1 ? "" : "s"}
        </button>
        <button
          type="button"
          className="kbc-room-chip kbc-room-chip--btn"
          style={{ ["--kbc-room-chip-color" as string]: "var(--warn)" }}
          onClick={() => onFilterFindings("concern")}
          title="filter the findings rail to concerns"
          data-kbc-room-hero-count="concern"
        >
          <Icon.Warn /> {counts.concerns} concern{counts.concerns === 1 ? "" : "s"}
        </button>
        <button
          type="button"
          className="kbc-room-chip kbc-room-chip--btn"
          style={{ ["--kbc-room-chip-color" as string]: "var(--green)" }}
          onClick={() => onFilterFindings("ok")}
          title="filter the findings rail to verified-ok findings"
          data-kbc-room-hero-count="ok"
        >
          <Icon.Check /> {counts.verified} verified
        </button>
        <span
          className="kbc-room-chip"
          style={{ ["--kbc-room-chip-color" as string]: "var(--ink-mute)" }}
          data-kbc-room-hero-viewed={`${viewed.viewed}/${viewed.total}`}
        >
          <Icon.Eye /> {viewed.viewed} of {viewed.total} files viewed
        </span>
      </div>

      {lede && (
        <p className="kbc-room-hero__lede" data-kbc-room-hero-lede>
          {lede}
        </p>
      )}

      {/* The agent block: ABSENT when nothing names an agent/session — see
          this module's header. `heroAgentOf` returns `null` in that case. */}
      {agent && (
        <div className="kbc-room-hero__agent" data-kbc-room-hero-agent>
          <Icon.Spark />
          {agent.author && <span data-kbc-room-hero-agent-name>{agent.author}</span>}
          {agent.sessionId && (
            <Link
              to={`/session/${encodeURIComponent(agent.sessionId)}/diff`}
              className="kbc-room-hero__session"
              title={agent.sessionId}
              data-kbc-room-hero-session
            >
              session {truncateMiddle(agent.sessionId, 24)}
            </Link>
          )}
        </div>
      )}

      {report.verdict_headline && (
        <div className="kbc-room-hero__verdict">
          <SectionDecor kind="verdict" title={report.verdict_headline} />
        </div>
      )}
    </section>
  );
}
