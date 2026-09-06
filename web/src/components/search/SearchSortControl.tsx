import {
  defaultSearchDir,
  SEARCH_SORTS,
  type SearchSort,
  type SortDir,
} from "../../lib/searchSort";

// FS4 — the search results-header sort menu. Sorting is SERVER-SIDE: this
// controlled component just writes sort/dir to the URL; the daemon
// re-orders the matched pool before truncation (a client re-sort would
// corrupt the score-ranked window + the show-more pagination). `relevance`
// is the default (the score order); a direction toggle shows only for the
// axes where asc/desc both read naturally (words, reading progress) —
// time/title sorts use their implicit default direction.

type Props = {
  sort: SearchSort;
  dir: SortDir;
  onSort: (sort: SearchSort, dir: SortDir) => void;
};

export default function SearchSortControl({ sort, dir, onSort }: Props) {
  return (
    <div className="sort-control" role="group" aria-label="sort">
      <label className="sort-control__label">sort</label>
      <select
        className="sort-control__select"
        value={sort}
        onChange={(e) => {
          const key = e.target.value as SearchSort;
          onSort(key, defaultSearchDir(key));
        }}
      >
        {SEARCH_SORTS.map((o) => (
          <option key={o.key} value={o.key}>
            {o.label}
          </option>
        ))}
      </select>
      {(sort === "words" || sort === "progress") && (
        <button
          type="button"
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
