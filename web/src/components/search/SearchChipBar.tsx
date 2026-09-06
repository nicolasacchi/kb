// FS5 — the active-filter chip bar. Presentational: the route derives one
// chip per active filter param (single source of truth — the same URL the
// rail reads), so a chip and its rail control can never drift. Each chip
// removes its own filter (the inverse of the rail setter); "clear all"
// nulls every filter axis at once (keeping q/mode/scope/kb/sort/limit).

import { Icon } from "../icons";

export type Chip = { key: string; label: string; onRemove: () => void };

type Props = { chips: Chip[]; onClearAll: () => void };

export default function SearchChipBar({ chips, onClearAll }: Props) {
  if (chips.length === 0) return null;
  return (
    <div
      className="kb-search-chips"
      role="group"
      aria-label="active filters"
      aria-live="polite"
    >
      {chips.map((c) => (
        <span key={c.key} className="kb-search-chips__chip">
          <span className="kb-search-chips__label">{c.label}</span>
          <button
            type="button"
            className="kb-search-chips__x"
            aria-label={`remove ${c.label}`}
            onClick={c.onRemove}
          >
            <Icon.X />
          </button>
        </span>
      ))}
      {chips.length > 1 && (
        <button
          type="button"
          className="kb-search-chips__clear"
          onClick={onClearAll}
        >
          clear all
        </button>
      )}
    </div>
  );
}
