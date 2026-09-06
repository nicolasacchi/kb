// MI-W4.7 — an UpSet-style set-intersection view over the memory
// population's cross-kb scope: one horizontal bar per distinct
// kb-combination actually observed (global / kb-a only / kb-a+kb-b / …),
// sized by memory count, with a dot-matrix beneath each bar showing which
// kbs are members of that row's set. A Venn breaks down past 3-4 sets;
// UpSet reads at any set count.
//
// Pure client-side aggregation over whatever `hits` the /memory page
// already has loaded (`lib/scopeOverlap.ts`) — no new route. Clicking a
// row pivots the list via the SAME `?kb=` `forKb` lens `routes/memory.tsx`
// already exposes (`onPivotKb`).

import { useMemo } from "react";
import {
  aggregateScopeOverlap,
  scopeOverlapColumns,
  scopeOverlapPivotKb,
  type ScopeOverlapInput,
} from "../lib/scopeOverlap";

export default function ScopeOverlapUpset({
  hits,
  onPivotKb,
}: {
  hits: ScopeOverlapInput[];
  onPivotKb: (kb: string) => void;
}) {
  const rows = useMemo(() => aggregateScopeOverlap(hits), [hits]);
  const columns = useMemo(() => scopeOverlapColumns(rows), [rows]);
  const maxCount = Math.max(1, ...rows.map((r) => r.count));

  return (
    <section className="kb-scopeup" data-testid="scope-overlap" aria-label="cross-kb scope overlap">
      <h2 className="kb-scopeup__title">scope overlap</h2>
      {rows.length === 0 ? (
        <p className="kb-scopeup__empty" data-testid="scope-overlap-empty">
          no memories to summarise yet.
        </p>
      ) : (
        <div className="kb-scopeup__rows">
          {rows.map((row) => {
            const pivotKb = scopeOverlapPivotKb(row);
            const label = row.isGlobal ? "★ global" : row.members.join(" + ");
            const noun = row.count === 1 ? "memory" : "memories";
            const title = row.isGlobal
              ? `${row.count} global ${noun} — visible to every kb`
              : `${row.count} ${noun} visible to ${row.members.join(" + ")}`;
            return (
              <button
                type="button"
                key={row.key}
                className={`kb-scopeup__row${row.isGlobal ? " kb-scopeup__row--global" : ""}`}
                data-testid="scope-overlap-row"
                disabled={!pivotKb}
                title={pivotKb ? `${title} — click to view` : title}
                onClick={() => pivotKb && onPivotKb(pivotKb)}
              >
                <span className="kb-scopeup__label">{label}</span>
                <span className="kb-scopeup__bar-track">
                  <span
                    className="kb-scopeup__bar"
                    style={{ width: `${(row.count / maxCount) * 100}%` }}
                  />
                </span>
                <span className="kb-scopeup__count">{row.count}</span>
                <span className="kb-scopeup__dots" aria-hidden>
                  {row.isGlobal ? (
                    <span className="kb-scopeup__dot kb-scopeup__dot--all" title="every kb" />
                  ) : (
                    columns.map((c) => (
                      <span
                        key={c}
                        className={`kb-scopeup__dot${row.members.includes(c) ? " is-on" : ""}`}
                      />
                    ))
                  )}
                </span>
              </button>
            );
          })}
        </div>
      )}
    </section>
  );
}
