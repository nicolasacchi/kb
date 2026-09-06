import type { ReviewFileRow, ReviewRiskFile } from "../../api/types";
import { formatAttentionScore } from "../../lib/attentionRamp";

/** V3.2-B3 — attention badge: score + term expander; null → "—" never 0. */
export default function RiskBadge({
  file,
  riskRow,
}: {
  file: ReviewFileRow;
  riskRow: ReviewRiskFile | undefined;
}) {
  const risk = riskRow?.risk ?? null;
  const missing = riskRow?.inputs_missing ?? [];

  if (risk == null) {
    return (
      <span
        className="kbc-review__risk kbc-review__risk--null"
        title="no computable signals for this file"
        data-kbc-review-risk={file.path}
        data-kbc-review-risk-null
        onClick={(e) => e.stopPropagation()}
      >
        —
      </span>
    );
  }

  const terms = risk.terms;
  const termRows: Array<[string, string]> = [];
  if (terms.relative_churn != null) {
    termRows.push(["relative_churn", terms.relative_churn.toFixed(3)]);
  } else {
    termRows.push(["relative_churn", "not available"]);
  }
  if (terms.ownership_minor != null) {
    termRows.push(["ownership_minor", terms.ownership_minor.toFixed(3)]);
  } else {
    termRows.push(["ownership_minor", "not available"]);
  }
  if (terms.hotspot_rank != null) {
    termRows.push(["hotspot_rank", terms.hotspot_rank.toFixed(3)]);
  } else {
    termRows.push(["hotspot_rank", "not available"]);
  }
  if (terms.agent_first_touch != null) {
    termRows.push(["agent_first_touch", terms.agent_first_touch ? "true" : "false"]);
  } else {
    termRows.push(["agent_first_touch", "not available"]);
  }
  if (terms.session_pain != null) {
    termRows.push(["session_pain", terms.session_pain.toFixed(3)]);
  } else {
    termRows.push(["session_pain", "not available"]);
  }

  return (
    <details
      className="kbc-review__risk"
      data-kbc-review-risk={file.path}
      data-kbc-review-risk-score={String(risk.score)}
      onClick={(e) => e.stopPropagation()}
    >
      <summary className="kbc-review__risk-sum" title="attention score — expand for terms">
        {formatAttentionScore(risk.score)}
      </summary>
      <dl className="kbc-review__risk-terms" data-kbc-review-risk-terms>
        {termRows.map(([k, v]) => (
          <div key={k}>
            <dt>{k}</dt>
            <dd>{v}</dd>
          </div>
        ))}
        {missing.length > 0 && (
          <div className="kbc-review__risk-missing" data-kbc-review-risk-missing>
            <dt>not computed</dt>
            <dd>{missing.join(", ")}</dd>
          </div>
        )}
      </dl>
    </details>
  );
}
