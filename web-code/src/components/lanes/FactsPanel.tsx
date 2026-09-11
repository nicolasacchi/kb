import { Link } from "react-router-dom";
import { useLaneFacts, useLanes } from "../../hooks/useLanes";
import {
  buildFactsView,
  formatRunProvenance,
  trustClassName,
  trustClassOf,
  type DisplayFact,
} from "../../lib/lanes";
import { lanesUrl } from "../../lib/codeUrl";
import "../../styles/lanes.css";

export interface FactsPanelProps {
  repo: string;
  path: string;
  coverageBandOn: boolean;
  onToggleCoverageBand: () => void;
  activeLine: number | null;
  onGotoLine: (line: number, lineEnd?: number) => void;
}

function FactRow({
  row,
  active,
  onGotoLine,
}: {
  row: DisplayFact;
  active: boolean;
  onGotoLine: (line: number, lineEnd?: number) => void;
}) {
  const { fact } = row;
  const cls = trustClassOf(fact);
  const line = fact.line && fact.line > 0 ? fact.line : null;
  const lineEnd = fact.line_end && fact.line_end > (line ?? 0) ? fact.line_end : undefined;
  const provenance = formatRunProvenance(fact);
  const body = (
    <>
      <span className="kbc-facts__kind">{fact.kind}</span>
      <span className="kbc-facts__value">{row.valueText}</span>
      <span className={trustClassName(fact.class)} data-kbc-fact-class={cls} title={fact.reason}>
        {cls}
        {row.freshness ? <span className="kbc-facts__fresh"> {row.freshness}</span> : null}
      </span>
      <span className="kbc-facts__age" title={provenance}>
        {row.ageText}
      </span>
    </>
  );
  return (
    <li
      className="kbc-facts__row"
      data-kbc-fact-row
      data-kbc-fact-lane={fact.lane}
      data-kbc-fact-kind={fact.kind}
      data-kbc-fact-active={active ? "1" : undefined}
      title={provenance}
    >
      {line ? (
        <button
          type="button"
          className="kbc-facts__jump"
          onClick={() => onGotoLine(line, lineEnd)}
          data-kbc-fact-jump={line}
        >
          {body}
        </button>
      ) : (
        body
      )}
    </li>
  );
}

/// V76-R3a — the rail's Facts tab. Facts grouped by lane with the lane's
/// own status (enabled / disabled-with-reason / empty-with-reason). Trust
/// in LINE STYLE; age as text; run provenance on hover. The coverage-band
/// toggle is off by default and never invents a fifth gutter.
export default function FactsPanel({
  repo,
  path,
  coverageBandOn,
  onToggleCoverageBand,
  activeLine,
  onGotoLine,
}: FactsPanelProps) {
  const registry = useLanes(repo);
  const facts = useLaneFacts(repo, path);
  const view = buildFactsView({
    registry: registry.data,
    facts: facts.data,
    registryLoading: registry.isLoading,
    factsLoading: facts.isLoading,
    error: registry.error instanceof Error ? registry.error.message : facts.error instanceof Error ? facts.error.message : null,
  });

  if (!path) {
    return (
      <div className="kbc-inspector__hint" data-kbc-facts-state="empty">
        No file open.
      </div>
    );
  }

  return (
    <div className="kbc-facts" data-kbc-facts-panel data-kbc-facts-state={view.kind}>
      <div className="kbc-facts__head">
        <Link className="kbc-facts__chip" to={lanesUrl(repo)} data-kbc-facts-lanes-link>
          lanes
        </Link>
        <button
          type="button"
          className={"kbc-facts__toggle" + (coverageBandOn ? " is-on" : "")}
          aria-pressed={coverageBandOn}
          data-kbc-facts-coverage-toggle
          data-cmd="facts.coverage-band"
          onClick={onToggleCoverageBand}
          title="show coverage as a blame-gutter band (off by default)"
        >
          coverage band {coverageBandOn ? "on" : "off"}
        </button>
      </div>
      {view.kind === "loading" && (
        <p className="kbc-facts__hint" data-kbc-facts-loading>
          Loading…
        </p>
      )}
      {view.kind === "error" && (
        <p className="kbc-facts__error" data-kbc-facts-error>
          {view.error}
        </p>
      )}
      {view.kind === "partial" && (
        <p className="kbc-facts__partial" data-kbc-facts-partial>
          truncated — more stored facts exist than this response returned
        </p>
      )}
      {view.notes.map((n) => (
        <p key={n} className="kbc-facts__hint" data-kbc-facts-note>
          {n}
        </p>
      ))}
      {view.withheldDisabled > 0 && (
        <p className="kbc-facts__hint" data-kbc-facts-withheld>
          {view.withheldDisabled} stored facts withheld (lane disabled)
        </p>
      )}
      {view.kind === "empty" && view.groups.every((g) => g.status !== "enabled" || g.facts.length === 0) && (
        <p className="kbc-facts__hint" data-kbc-facts-empty>
          No facts for this file.
        </p>
      )}
      {view.groups.map((g) => (
        <section key={g.id} className="kbc-facts__lane" data-kbc-facts-lane={g.id}>
          <header className="kbc-facts__lane-head">
            <span className="kbc-facts__lane-title">{g.title}</span>
            <span className="kbc-facts__lane-status" data-kbc-lane-status={g.status}>
              {g.status === "enabled" ? `${g.facts.length}` : `${g.status}: ${g.statusReason}`}
            </span>
          </header>
          {g.facts.length > 0 && (
            <ul className="kbc-facts__list">
              {g.facts.map((row, i) => (
                <FactRow
                  key={`${row.fact.kind}:${row.fact.line ?? 0}:${row.fact.reason}:${i}`}
                  row={row}
                  active={activeLine !== null && row.fact.line === activeLine}
                  onGotoLine={onGotoLine}
                />
              ))}
            </ul>
          )}
        </section>
      ))}
    </div>
  );
}
