// `GET /api/reviews/{id}/doc/lint`, rendered (V73-K2b).
//
// READ-ONLY, and deliberately so: composing a document is loopback-only
// (D22's local-canonical ruling, unchanged by this unit), so the panel shows
// the exact `kb-code review compose` line the agent would run rather than
// offering to run it — the same CLI-parity posture `lib/searchHistory.ts`
// records for a saved search.
//
// Every row is rendered verbatim, in the daemon's own order, with its own
// severity. "did you mean" candidates are the daemon's suggestions in the
// author's vocabulary — never a fix this panel applies.
import type { ReviewDocLintOut } from "../../api/types";
import { Icon } from "../icons";
import { lintCandidatesText, lintCensusText, lintSeverityClass } from "../../lib/reviewDoc";

export interface DocLintPanelProps {
  lint: ReviewDocLintOut | null | undefined;
  loading: boolean;
}

export default function DocLintPanel({ lint, loading }: DocLintPanelProps) {
  if (loading) return <div className="kbc-doclint kbc-doclint--loading">Linting…</div>;
  if (!lint) return null;
  return (
    <section className="kbc-doclint" data-kbc-doclint aria-label="Document lint">
      <header className="kbc-doclint__head">
        <Icon.ClipboardCheck />
        <span className="kbc-doclint__census" data-kbc-doclint-census>
          {lintCensusText(lint)}
        </span>
      </header>
      {lint.rows.length > 0 && (
        <ul className="kbc-doclint__rows">
          {lint.rows.map((r, i) => (
            <li
              key={i}
              className={`kbc-doclint__row ${lintSeverityClass(r.severity)}`}
              data-kbc-doclint-row={r.rule}
              data-kbc-doclint-severity={r.severity}
            >
              <span className="kbc-doclint__severity">{r.severity}</span>
              {r.line != null && <span className="kbc-doclint__line">line {r.line}</span>}
              <span className="kbc-doclint__msg">{r.message}</span>
              {r.ref && <code className="kbc-doclint__ref">[[{r.ref}]]</code>}
              {r.candidates && r.candidates.length > 0 && (
                <span className="kbc-doclint__cands" data-kbc-doclint-candidates>
                  {lintCandidatesText(r.candidates)}
                </span>
              )}
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}
