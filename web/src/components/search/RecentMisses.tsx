import { useQuery } from "@tanstack/react-query";
import { fetchZeroHitQueries } from "../../api/client";
import { censusBump } from "../../lib/census";

const LIMIT = 8;

export type RecentMissesProps = {
  kb?: string;
  onRetry: (q: string) => void;
};

// W1.search — zero-hit memory (item 1.d). On the EMPTY-QUERY search page
// (no query, no filters — not the zero-HITS state, which gets the fuller
// ZeroHitRecovery treatment), a quiet row of past queries that returned
// nothing, as one-click retry chips. Backed by `GET /api/kb/{kb}/queries
// ?zero_hit=true` — an in-process ring (`kb_core::history::QueriesRing`)
// that resets on every daemon restart, so this stays a light,
// always-refetch read (`staleTime: 0`, matching `useRecentQueries`'
// choice for the same ring family) rather than a durable cache.
export default function RecentMisses({ kb, onRetry }: RecentMissesProps) {
  const q = useQuery({
    queryKey: ["zeroHitQueries", kb, LIMIT] as const,
    enabled: !!kb,
    queryFn: ({ signal }) => fetchZeroHitQueries(kb as string, LIMIT, signal),
    staleTime: 0,
  });
  const groups = q.data ?? [];
  if (groups.length === 0) return null;
  return (
    <div className="kb-zero-memory" role="group" aria-label="recent misses">
      <div className="kb-zero-memory__h">recent misses</div>
      <div className="kb-zero-memory__chips">
        {groups.map((g) => (
          <button
            key={g.query}
            type="button"
            className="kb-zero-memory__chip"
            onClick={() => {
              censusBump("search.zero_hit.retry");
              onRetry(g.query);
            }}
          >
            {g.query}
          </button>
        ))}
      </div>
    </div>
  );
}
