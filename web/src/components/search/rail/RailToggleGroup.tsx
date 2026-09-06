// FS3 — a segmented button group for the search rail, reusing the
// existing `.kb-search-rail__seg` styling. Drives the toggle-style facets
// (read-state, capabilities, date presets, created↔modified pivot). Each
// button is `aria-pressed`; clicking calls `onToggle(value)` and the
// parent recomputes the active set (multi) or swaps the lone value
// (single) before writing the URL.

export type ToggleOption = { value: string; label: string };

type Props = {
  options: ToggleOption[];
  active: ReadonlySet<string>;
  onToggle: (value: string) => void;
  ariaLabel: string;
};

export default function RailToggleGroup({
  options,
  active,
  onToggle,
  ariaLabel,
}: Props) {
  return (
    <div className="kb-search-rail__seg" role="group" aria-label={ariaLabel}>
      {options.map((o) => {
        const on = active.has(o.value);
        return (
          <button
            key={o.value}
            type="button"
            className={on ? "on" : ""}
            aria-pressed={on}
            onClick={() => onToggle(o.value)}
          >
            {o.label}
          </button>
        );
      })}
    </div>
  );
}
