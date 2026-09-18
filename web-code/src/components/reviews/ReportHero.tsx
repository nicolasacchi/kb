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
import { Link } from "react-router";
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
import ProseBlock from "../prose/ProseBlock";
// V80-R3 — type-only: the hero highlights the rail's CURRENT severity
// filter as the active segment of its own segmented control, so the two
// can never silently disagree about what "filtered" means (the same
// lifted-state rule the count chips' `onFilterFindings` callback already
// follows). No runtime dependency — this is an `import type`.
import type { FindingSeverityFilter } from "./ReviewThreadsCard";

export interface ReportHeroProps {
  repo: string;
  review: ReviewDetailPr;
  report: ReviewReport;
  findings: ReviewFinding[];
  files: ReviewFileRow[];
  /// A count chip filters the findings rail to that severity (`"ok"` for
  /// verified) — the rail's own filter state, lifted to `ReviewDetail`.
  onFilterFindings: (severity: FindingSeverity) => void;
  /// V80-R3 — the rail's CURRENT filter, so the severity segment it drives
  /// can show which one is active. Optional: absent (e.g. the standalone
  /// `ProseBlock.test.ts` renders) simply shows no segment pressed, same as
  /// today's behaviour.
  activeSeverity?: FindingSeverityFilter;
  /// V80-M4 — "human threads counted in the derived totals": every OPEN
  /// review thread whose opener is human (`lib/reviewRoom.ts`'s
  /// `humanOpenThreads`, the SAME derivation the rail's counts and the
  /// guided tour share). Defaults to `0` — a standalone render (no
  /// comments fetched yet) reads as "none open," never "unknown."
  humanOpenCount?: number;
}

/// The identity line's "ps" chip: the ACTIVE patchset when the review names
/// one, else the latest captured — `null` when there are none yet (a
/// brand-new review with no snapshot), never a fabricated "ps0".
function heroPsOf(review: Pick<ReviewDetailPr, "patchsets">): number | null {
  if (review.patchsets.length === 0) return null;
  return review.patchsets[review.patchsets.length - 1].ps_number;
}

export default function ReportHero({
  repo,
  review,
  report,
  findings,
  files,
  onFilterFindings,
  activeSeverity,
  humanOpenCount = 0,
}: ReportHeroProps) {
  const counts = heroCounts(findings);
  const viewed = filesViewedOf(files);
  const lede = heroLedeOf(report);
  const agent = heroAgentOf(report, review.session_id ?? null);
  const baseSource = heroBaseSourceOf(review);
  const prUrl = prExternalUrl(review.pr_repo_slug, review.pr_number);
  const title = review.title?.trim() || review.head_ref;
  const ps = heroPsOf(review);

  return (
    <section className="kbc-room-hero" data-kbc-room-hero>
      <div className="kbc-room-hero__top">
        <div className="kbc-room-hero__id">
          <h2 className="kbc-room-hero__title" data-kbc-room-hero-title>
            {title}
          </h2>
          <div className="kbc-room-hero__refs">
            {/* V80-R3 — the identity line: state · ps · base→head · agent,
                all as chips on ONE line (was scattered: state/ps lived only
                in `ReviewHeader.tsx` above the fold, and the agent block was
                a separate bordered row further down this same hero). */}
            <span
              className="kbc-room-chip"
              style={{
                ["--kbc-room-chip-color" as string]:
                  review.state === "open" ? "var(--accent-soft)" : "var(--ink-mute)",
              }}
              data-kbc-room-hero-state={review.state}
            >
              {review.state}
            </span>
            {ps != null && (
              <span
                className="kbc-room-chip"
                style={{ ["--kbc-room-chip-color" as string]: "var(--ink-mute)" }}
                data-kbc-room-hero-ps={ps}
              >
                ps{ps}
              </span>
            )}
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
            {/* The agent chip: ABSENT when nothing names an agent/session —
                see this module's header. `heroAgentOf` returns `null` in
                that case (never an "UNSET" box). Same children/attrs as
                before this unit — only the wrapper moved from a separate
                bordered block into this identity line. */}
            {agent && (
              <span
                className="kbc-room-chip kbc-room-hero__agent-chip"
                style={{ ["--kbc-room-chip-color" as string]: "var(--accent-soft)" }}
                data-kbc-room-hero-agent
              >
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
              </span>
            )}
          </div>
        </div>
        {report.risk_score != null && (
          <RiskDial score={report.risk_score} color={severityToken(report.verdict)} />
        )}
      </div>

      <div className="kbc-room-hero__counts kbc-stats" data-kbc-room-hero-counts data-kbc-report-stats>
        {/* V80-R3 — the three severity counts are now segments of ONE
            control (was three free-floating chips): a joined group, and
            (when the rail's filter is threaded through) the currently
            active segment is pressed — still the SAME `onFilterFindings`
            call and the SAME `data-kbc-room-hero-count` hooks. */}
        <div className="kbc-room-hero__severity-seg" role="group" aria-label="finding severity">
          <button
            type="button"
            className="kbc-room-chip kbc-room-chip--btn"
            style={{ ["--kbc-room-chip-color" as string]: "var(--red)" }}
            onClick={() => onFilterFindings("blocker")}
            title="filter the findings rail to blockers"
            aria-pressed={activeSeverity === "blocker"}
            data-kbc-room-hero-count="blocker"
          >
            <Icon.Warn />
            <span className="cell b">
              <span className="v">{counts.blockers}</span>{" "}
              <span className="l">blocker{counts.blockers === 1 ? "" : "s"}</span>
            </span>
          </button>
          <button
            type="button"
            className="kbc-room-chip kbc-room-chip--btn"
            style={{ ["--kbc-room-chip-color" as string]: "var(--warn)" }}
            onClick={() => onFilterFindings("concern")}
            title="filter the findings rail to concerns"
            aria-pressed={activeSeverity === "concern"}
            data-kbc-room-hero-count="concern"
          >
            <Icon.Warn />
            <span className="cell c">
              <span className="v">{counts.concerns}</span>{" "}
              <span className="l">concern{counts.concerns === 1 ? "" : "s"}</span>
            </span>
          </button>
          <button
            type="button"
            className="kbc-room-chip kbc-room-chip--btn"
            style={{ ["--kbc-room-chip-color" as string]: "var(--green)" }}
            onClick={() => onFilterFindings("ok")}
            title="filter the findings rail to verified-ok findings"
            aria-pressed={activeSeverity === "ok"}
            data-kbc-room-hero-count="ok"
          >
            <Icon.Check />
            <span className="cell o">
              <span className="v">{counts.verified}</span> <span className="l">verified</span>
            </span>
          </button>
        </div>
        <span
          className="kbc-room-chip"
          style={{ ["--kbc-room-chip-color" as string]: "var(--ink-mute)" }}
          data-kbc-room-hero-viewed={`${viewed.viewed}/${viewed.total}`}
        >
          <Icon.Eye /> {viewed.viewed} of {viewed.total} files viewed
        </span>
        {/* V80-M4 — "the Room reads human threads first-class": a peer count
            beside the agent's own findings/viewed chips, never hidden even
            at zero (the room never lies by omission). */}
        <span
          className="kbc-room-chip"
          style={{ ["--kbc-room-chip-color" as string]: "var(--blue)" }}
          data-kbc-room-hero-human-open={humanOpenCount}
        >
          <Icon.Comment /> {humanOpenCount} open question{humanOpenCount === 1 ? "" : "s"} from you
        </span>
      </div>

      {lede && (
        <p
          className={`kbc-room-hero__lede${report.deck?.trim() ? " kbc-report__deck" : ""}`}
          data-kbc-room-hero-lede
          data-kbc-report-deck={report.deck?.trim() ? "" : undefined}
        >
          <ProseBlock
            text={lede}
            refs={report.deck?.trim() ? report.deck_refs : report.summary_refs}
            repo={repo}
            reviewId={review.id}
            inline
          />
        </p>
      )}

      {report.verdict_headline && (
        <div className="kbc-room-hero__verdict">
          <SectionDecor kind="verdict" title={report.verdict_headline} />
        </div>
      )}
    </section>
  );
}
