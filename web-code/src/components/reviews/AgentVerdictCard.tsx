// PRR-U2 §2 S2 — the verdict dialectic strip's agent half. Severity wash
// background exactly like report-pr.html's `.verdict::before` gradient,
// translated to tokens (`color-mix(in srgb, var(--warn) 12%, transparent)`,
// mock's `.kbc-averdict::before`).
import type { FindingSeverity, ReviewReport } from "../../api/types";
import { Icon } from "../icons";
import ProseBlock from "../prose/ProseBlock";
import RiskDial from "./RiskDial";

/// Severity → the CSS custom property name (not the resolved color) so the
/// wash/dial can both key off ONE token per severity via `var(--sev-color)`
/// set once on the card root — see `styles/review-room.css`.
export function severityToken(severity: FindingSeverity | undefined): string {
  if (severity === "blocker") return "var(--red)";
  if (severity === "concern") return "var(--warn)";
  if (severity === "ok") return "var(--green)";
  return "var(--ink-dim)";
}

/// Display word for the severity badge — the server's own vocab, shown
/// verbatim-but-capitalized (never the mock's invented "Caution" wording —
/// plan arbitration: "verb names from the server").
export function severityWord(severity: FindingSeverity | undefined): string {
  if (severity === "blocker") return "Blocker";
  if (severity === "concern") return "Concern";
  if (severity === "ok") return "OK";
  return "Unset";
}

export interface AgentVerdictCardProps {
  report: ReviewReport;
  repo?: string;
  reviewId?: number;
}

/// Renders `null` when the report carries no verdict signal at all (no
/// `verdict` AND no `verdict_headline`/`risk_score`) — a report can exist
/// but be summary-only (§8's "report/count drift" honesty extends to "a
/// report authored before this field existed").
export default function AgentVerdictCard({ report, repo, reviewId }: AgentVerdictCardProps) {
  const hasSignal =
    report.verdict != null || report.verdict_headline != null || report.risk_score != null;
  if (!hasSignal) return null;

  const color = severityToken(report.verdict);
  return (
    <div
      className="kbc-averdict"
      style={{ ["--kbc-averdict-color" as string]: color }}
      data-kbc-agent-verdict={report.verdict ?? "unset"}
    >
      <div className="kbc-averdict__badge">
        <span className="who">
          <Icon.Spark /> agent
        </span>
        <span className="word" data-kbc-agent-verdict-word>
          {severityWord(report.verdict)}
        </span>
      </div>
      <div>
        {report.verdict_headline && (
          <div className="kbc-averdict__hl" data-kbc-agent-verdict-headline>
            {report.verdict_headline}
          </div>
        )}
        {report.verdict_body && (
          <div className="kbc-averdict__body" data-kbc-agent-verdict-body>
            {repo ? (
              <ProseBlock text={report.verdict_body} refs={report.verdict_body_refs} repo={repo} reviewId={reviewId} />
            ) : (
              report.verdict_body
            )}
          </div>
        )}
      </div>
      {report.risk_score != null && <RiskDial score={report.risk_score} color={color} />}
    </div>
  );
}
