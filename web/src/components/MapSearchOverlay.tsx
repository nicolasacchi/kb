import { useEffect, useRef, useState } from "react";
import { ApiError, isAbortError, search } from "../api/client";
import { censusBump } from "../lib/census";

// W3.M-c — the atlas's own "dim in place" search: distinct from the
// gallery's `ids=` hard filter (invariant #35, which REMOVES rows and is
// capped at 500). This never removes a dot from the canvas — it only feeds
// a matched-id `Set<string>` up to AtlasView, which applies it as ONE MORE
// alpha multiplier alongside the existing dim-read / cluster-highlight /
// lasso-selection signals. "Not in the hit set" always reads as dimmed,
// never as gone.
//
// Scope is always `scope=one` against the kb the atlas is already showing
// (search's positional/options overload — api/client.ts). The result set
// is honestly capped at the server's own `SEARCH_MAX_LIMIT` (200,
// routes/search.rs) — the hint copy says so rather than implying the whole
// corpus was ranked.
export const MAP_SEARCH_DIM_LIMIT = 200;

export default function MapSearchOverlay({
  kb,
  onDimSet,
}: {
  kb: string;
  onDimSet: (ids: Set<string> | null) => void;
}) {
  const [q, setQ] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [matched, setMatched] = useState<number | null>(null);
  const abortRef = useRef<AbortController | null>(null);

  // A kb switch invalidates any in-flight/previous dim result — the caller
  // (AtlasView) resets its own `dimIds` on `[kb]`; this component is mounted
  // with `key={kb}` there, so React remounts it fresh on every kb change
  // (no extra effect needed for that). The abort-on-unmount below still
  // guards a request that outlives the remount.
  useEffect(() => {
    return () => abortRef.current?.abort();
  }, []);

  const clear = () => {
    abortRef.current?.abort();
    setQ("");
    setError(null);
    setMatched(null);
    onDimSet(null);
    censusBump("atlas.dimSearch.clear");
  };

  const run = async (query: string) => {
    const trimmed = query.trim();
    if (!trimmed) {
      clear();
      return;
    }
    abortRef.current?.abort();
    const ctrl = new AbortController();
    abortRef.current = ctrl;
    setBusy(true);
    setError(null);
    try {
      const resp = await search(trimmed, {
        kb,
        scope: "one",
        limit: MAP_SEARCH_DIM_LIMIT,
        signal: ctrl.signal,
      });
      const ids = new Set(resp.hits.map((h) => h.id));
      setMatched(ids.size);
      onDimSet(ids);
      censusBump("atlas.dimSearch.run");
    } catch (e) {
      if (isAbortError(e)) return;
      setMatched(null);
      onDimSet(null);
      setError(e instanceof ApiError ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <form
      className="atlas-map-search"
      role="search"
      aria-label="dim atlas dots by search"
      onSubmit={(e) => {
        e.preventDefault();
        void run(q);
      }}
    >
      <input
        type="search"
        value={q}
        onChange={(e) => setQ(e.target.value)}
        placeholder="dim by search…"
        aria-label="search to dim non-matching dots"
        className="atlas-map-search__input"
        data-kb-act="atlas-map-search-input"
      />
      <button
        type="submit"
        className="atlas-map-search__btn"
        disabled={busy || !q.trim()}
        data-kb-act="atlas-map-search-run"
      >
        {busy ? "searching…" : "dim by search"}
      </button>
      {(matched != null || error) && (
        <button
          type="button"
          className="atlas-map-search__btn"
          onClick={clear}
          data-kb-act="atlas-map-search-clear"
        >
          clear
        </button>
      )}
      {error && (
        <span className="atlas-map-search__error" role="alert">
          {error}
        </span>
      )}
      {matched != null && !error && (
        <span className="atlas-map-search__hint">
          highlighting the top {MAP_SEARCH_DIM_LIMIT} matches ({matched}{" "}
          found) — the rest dim
        </span>
      )}
    </form>
  );
}
