import { PREFIX_CHIPS, type PrefixChip } from "../../lib/prefixChips";

export interface PrefixChipsProps {
  onInsert: (chip: PrefixChip) => void;
}

/// A subtle row of lane-selecting prefix chips (`@ # / ? ~ ~~`) under the
/// omnibox/search-page input - clicking one INSERTS its prefix
/// (`lib/prefixChips.ts`'s `applyPrefixChip`), it does not itself run a
/// search.
export default function PrefixChips({ onInsert }: PrefixChipsProps) {
  return (
    <div className="kbc-search__chips" role="group" aria-label="search prefixes">
      {PREFIX_CHIPS.map((chip) => (
        <button
          key={chip.label}
          type="button"
          className="kbc-search__chip-btn"
          aria-label={`${chip.hint} prefix (${chip.label})`}
          title={`${chip.hint} - inserts "${chip.label}"`}
          onClick={() => onInsert(chip)}
        >
          {chip.label}
        </button>
      ))}
    </div>
  );
}
