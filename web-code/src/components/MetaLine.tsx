import type { ReactNode } from "react";
import "../styles/page.css";

// V80-R0 — the ONE shared "·"-separated caption row (item 5 of the ramp/
// density unit's brief): chips/meta/table-cell-adjacent secondary text
// that just needs a few short facts on one line (a count, a timestamp, a
// path). `--fs-sm`/muted/tabular-figures, matching the numeric-chip intent
// named in item 1 of this unit's brief. Falsy items (`null`/`undefined`/
// `false` — the common "only show this when the count is nonzero" shape)
// are dropped BEFORE the separator is placed, so a caller never has to
// hand-compute which items survived to get the `·`s right.
export interface MetaLineProps {
  items: ReactNode[];
  className?: string;
}

export default function MetaLine({ items, className }: MetaLineProps) {
  const visible = items.filter((it) => it !== null && it !== undefined && it !== false);
  if (visible.length === 0) return null;
  return (
    <p className={className ? `kbc-metaline ${className}` : "kbc-metaline"}>
      {visible.map((it, i) => (
        <span className="kbc-metaline__item" key={i}>
          {i > 0 && (
            <span className="kbc-metaline__sep" aria-hidden="true">
              ·
            </span>
          )}
          {it}
        </span>
      ))}
    </p>
  );
}
