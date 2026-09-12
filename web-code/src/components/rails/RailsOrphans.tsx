import { Link } from "react-router";
import type { RailsOrphansOut } from "../../api/types";
import TrustBadge from "../TrustBadge";
import { codeUrl } from "../../lib/codeUrl";
import { honestyLine, laneCaption } from "../../lib/railsCards";

// V72-I2 — the orphan report, rendered as a TRIAGE QUEUE.
//
// The report's own `caption` and every lane's own `why` are rendered
// VERBATIM. That is not decoration: `orphans.rs`'s whole design is "a row can
// be wrong because the lens does not read a template language this app uses,
// because a render is dynamic, or because a constant is reached by plain
// Ruby the lens does not model" — summarising that away turns a queue to
// read into a verdict to act on, which is exactly what the report refuses to
// be. Same rule `kbc-tree/1`'s honesty strip states for its own notes.
export interface RailsOrphansProps {
  repo: string;
  report: RailsOrphansOut | undefined;
  error?: Error | null;
  loading?: boolean;
}

export default function RailsOrphans({ repo, report, error, loading }: RailsOrphansProps) {
  const honesty = honestyLine(report?.honesty);
  return (
    <section className="kbc-rails__section kbc-rails__orphans" data-kbc-rails-orphans>
      <h2 className="kbc-rails__section-title">Orphan report</h2>

      {loading && <p className="kbc-rails__muted">Loading…</p>}
      {error && (
        <p className="kbc-rails__error" data-kbc-rails-error>
          {error.message}
        </p>
      )}

      {report && (
        <p className="kbc-rails__caption" data-kbc-rails-orphan-caption>
          {report.caption}
        </p>
      )}
      {honesty && (
        <p className={`kbc-rails__honesty is-${honesty.state}`} role="status">
          {honesty.text}
        </p>
      )}

      {(report?.lanes ?? []).map((lane) => (
        <div className="kbc-rails__lane" key={lane.id} data-kbc-rails-lane={lane.id}>
          <h3 className="kbc-rails__lane-title">
            {lane.title}
            <span className="kbc-rails__lane-n" data-kbc-rails-lane-total={lane.total}>
              {lane.total}
            </span>
          </h3>
          <p className="kbc-rails__lane-why" data-kbc-rails-lane-why>
            {lane.why}
          </p>
          {laneCaption(lane) !== "" && (
            <p className="kbc-rails__lane-caption" data-kbc-rails-lane-caption>
              {laneCaption(lane)}
            </p>
          )}
          <ul className="kbc-rails__lane-rows">
            {lane.rows.map((row) => (
              <li key={`${row.path}:${row.line ?? 0}:${row.name}`}>
                <Link
                  className="kbc-rails__lane-row"
                  to={codeUrl({ repo, path: row.path, line: row.line })}
                  data-kbc-rails-orphan-row={row.name}
                >
                  <span className="kbc-rails__lane-name">{row.name}</span>
                  <span className="kbc-rails__lane-path">
                    {row.line !== undefined ? `${row.path}:${row.line}` : row.path}
                  </span>
                </Link>
                <TrustBadge cls={row.trust} />
              </li>
            ))}
          </ul>
        </div>
      ))}

      {(report?.notes ?? []).map((note) => (
        <p className="kbc-rails__note" key={note} data-kbc-rails-note>
          {note}
        </p>
      ))}
    </section>
  );
}
