import {
  dateInputToFromUnix,
  dateInputToToUnix,
  presetRange,
  unixToDateInput,
  type TemporalPreset,
} from "./temporalRange";

const PRESETS: { key: TemporalPreset; label: string }[] = [
  { key: "today", label: "today" },
  { key: "7d", label: "7d" },
  { key: "30d", label: "30d" },
];

export type TemporalScrubberProps = {
  from: number | null;
  to: number | null;
  onChange: (from: number | null, to: number | null) => void;
};

// W1.search — the search rail's "read during" section: a from/to date
// pair (native <input type="date">, local-midnight bounds — see
// temporalRange.ts) plus one-click presets, writing `read_from`/
// `read_to` (unix seconds) through the caller's `onChange`. Fully
// controlled: the route owns the URL params and passes them down,
// mirroring every other rail leaf (SearchRail is the only thing that
// calls `set`/`setMany` here).
export default function TemporalScrubber({
  from,
  to,
  onChange,
}: TemporalScrubberProps) {
  const active = from != null || to != null;
  return (
    <div className="kb-search-rail__temporal">
      <div className="kb-search-rail__temporal-dates">
        <input
          type="date"
          className="kb-search-rail__input"
          value={unixToDateInput(from)}
          onChange={(e) => onChange(dateInputToFromUnix(e.target.value), to)}
          aria-label="read from date"
        />
        <span className="kb-search-rail__temporal-sep">–</span>
        <input
          type="date"
          className="kb-search-rail__input"
          value={unixToDateInput(to)}
          onChange={(e) => onChange(from, dateInputToToUnix(e.target.value))}
          aria-label="read to date"
        />
      </div>
      <div className="kb-search-rail__seg">
        {PRESETS.map((p) => (
          <button
            key={p.key}
            type="button"
            onClick={() => {
              const r = presetRange(p.key);
              onChange(r.from, r.to);
            }}
          >
            {p.label}
          </button>
        ))}
        <button
          type="button"
          disabled={!active}
          onClick={() => onChange(null, null)}
        >
          clear
        </button>
      </div>
    </div>
  );
}
