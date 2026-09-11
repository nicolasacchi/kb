import type { LaneFactOut } from "../../api/types";
import {
  factsForLine,
  formatAgeSecs,
  formatFactValue,
  formatRunProvenance,
  freshnessCaption,
  trustClassName,
  trustClassOf,
} from "../../lib/lanes";
import { useLanes } from "../../hooks/useLanes";
import "../../styles/lanes.css";

export interface FactsPeekProps {
  repo: string;
  facts: readonly LaneFactOut[] | undefined;
  line: number | null | undefined;
}

/// V76-R3a — Facts block inside PeekPanel's `cardExtra` (the V72-I2 slot).
/// Trust in LINE STYLE; run provenance as the row title. Renders nothing
/// when the hovered line carries no facts (same "absent extra is a no-op"
/// contract RailsAtomCard keeps).
export default function FactsPeek({ repo, facts, line }: FactsPeekProps) {
  const registry = useLanes(repo);
  if (!line || line < 1) return null;
  const rows = factsForLine(facts, line);
  if (rows.length === 0) return null;
  const retention = new Map((registry.data?.lanes ?? []).map((l) => [l.id, l.retention_days]));
  return (
    <div className="kbc-facts-peek" data-kbc-facts-peek data-kbc-facts-peek-count={rows.length}>
      <div className="kbc-facts-peek__head">Facts</div>
      {rows.map((fact, i) => {
        const cls = trustClassOf(fact);
        const fresh = freshnessCaption(fact.age_secs, retention.get(fact.lane) ?? 30);
        return (
          <div
            key={`${fact.lane}:${fact.kind}:${i}`}
            className="kbc-facts-peek__row"
            data-kbc-facts-peek-row
            title={formatRunProvenance(fact)}
          >
            <span className="kbc-facts__kind">{fact.lane}</span>
            <span className="kbc-facts__value">{formatFactValue(fact)}</span>
            <span className={trustClassName(fact.class)} data-kbc-fact-class={cls} title={fact.reason}>
              {cls}
              {fresh ? <span className="kbc-facts__fresh"> {fresh}</span> : null}
            </span>
            <span className="kbc-facts__age">{formatAgeSecs(fact.age_secs)}</span>
          </div>
        );
      })}
    </div>
  );
}
