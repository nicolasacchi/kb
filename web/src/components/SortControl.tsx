import { defaultDir, type SortKey, type SortDir } from "../lib/sort";

// SortControl — gallery header dropdown for the sort key + an
// optional direction toggle for word-count (time and title sorts use
// implicit directions). URL state lives on the route; this is a
// controlled component.

type Props = {
  sort: SortKey;
  dir: SortDir;
  onSort: (sort: SortKey, dir: SortDir) => void;
};

const OPTIONS: { key: SortKey; label: string }[] = [
  { key: "recent", label: "recent (modified)" },
  { key: "indexed", label: "recent (indexed)" },
  { key: "created", label: "recent (created)" },
  { key: "title", label: "title A→Z" },
  { key: "words", label: "words" },
];

export default function SortControl({ sort, dir, onSort }: Props) {
  return (
    <div className="sort-control" role="group" aria-label="sort">
      <label className="sort-control__label">sort</label>
      <select
        className="sort-control__select"
        value={sort}
        onChange={(e) => {
          // Changing the key resets direction to that key's default
          // (title→asc, others→desc) so the dropdown matches a deep
          // link to the same sort. The words-only ↓/↑ button below
          // owns explicit direction flips.
          const key = e.target.value as SortKey;
          onSort(key, defaultDir(key));
        }}
      >
        {OPTIONS.map((o) => (
          <option key={o.key} value={o.key}>
            {o.label}
          </option>
        ))}
      </select>
      {sort === "words" && (
        <button
          className="sort-control__dir"
          aria-label={dir === "desc" ? "descending" : "ascending"}
          onClick={() => onSort(sort, dir === "desc" ? "asc" : "desc")}
        >
          {dir === "desc" ? "↓" : "↑"}
        </button>
      )}
    </div>
  );
}
