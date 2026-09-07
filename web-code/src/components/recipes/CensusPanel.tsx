import { useState } from "react";
import type { KbcStepCensus } from "../../api/types";
import { censusExplain } from "../../lib/recipeAddr";
import { Icon } from "../icons";

export interface CensusPanelProps {
  census: KbcStepCensus;
  /// Wired to `recipe.census-open` (Alt-c) when this is the FOCUSED step —
  /// an external toggle so the keyboard command and the mouse disclosure
  /// triangle drive the same state.
  expanded?: boolean;
  onToggleExpanded?: () => void;
}

/// kbc-recipe/1's "why is this empty" panel — an empty step ALWAYS renders
/// this one-line reason (never a blank table); `inputs`/`filters_applied`/
/// `notes` are additional detail behind a disclosure, not hidden entirely
/// but not forced on screen either. `censusExplain` is the client-side
/// mirror of the server's own `StepCensus::explain()` — kept in lock-step
/// deliberately (see that function's doc) rather than inventing new copy.
export default function CensusPanel({ census, expanded, onToggleExpanded }: CensusPanelProps) {
  const [localOpen, setLocalOpen] = useState(false);
  if (!census.empty_reason) return null;
  const open = expanded ?? localOpen;
  const toggle = onToggleExpanded ?? (() => setLocalOpen((v) => !v));
  const hasDetail =
    (census.inputs && Object.keys(census.inputs).length > 0) ||
    (census.filters_applied && census.filters_applied.length > 0) ||
    (census.notes && census.notes.length > 0);

  return (
    <div className="kbc-recipe-census" data-kbc-recipe-census data-kbc-recipe-census-reason={census.empty_reason}>
      <p className="kbc-recipe-census__line" data-kbc-recipe-census-reason-text>
        <Icon.Warn className="kbc-recipe-census__icon" />
        {censusExplain(census)}
      </p>
      {hasDetail && (
        <>
          <button
            type="button"
            className="kbc-recipe-census__toggle"
            aria-expanded={open}
            onClick={toggle}
            data-kbc-recipe-census-toggle
          >
            {open ? "hide detail ▾" : "show detail ▸"}
          </button>
          {open && (
            <dl className="kbc-recipe-census__detail" data-kbc-recipe-census-detail>
              {census.inputs && Object.keys(census.inputs).length > 0 && (
                <div>
                  <dt>inputs</dt>
                  <dd>
                    {Object.entries(census.inputs)
                      .map(([k, v]) => `${k}: ${v}`)
                      .join(", ")}
                  </dd>
                </div>
              )}
              {census.filters_applied && census.filters_applied.length > 0 && (
                <div>
                  <dt>filters applied</dt>
                  <dd>{census.filters_applied.join("; ")}</dd>
                </div>
              )}
              {census.notes && census.notes.length > 0 && (
                <div>
                  <dt>notes</dt>
                  <dd>{census.notes.join("; ")}</dd>
                </div>
              )}
            </dl>
          )}
        </>
      )}
    </div>
  );
}
