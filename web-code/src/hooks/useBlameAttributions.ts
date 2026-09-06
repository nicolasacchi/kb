import { useQueries } from "@tanstack/react-query";
import { fetchWhyLine } from "../api/client";
import { distinctRegionShas } from "../lib/blameGutter";
import type { BlameRegion, LineWhyOut } from "../api/types";

/// A defensive cap on how many distinct shas one file's blame gets
/// prefetched attribution for — mirrors `routes.rs`'s own
/// `MAX_SYMBOL_MATCHES` precedent (a circuit breaker for a pathological
/// file, not a real-world limit: an ordinary source file has, at most, a
/// few dozen distinct authoring commits).
const MAX_PREFETCH_SHAS = 200;

export interface BlameAttributions {
  /// The FULL `/api/why?line=` response per sha — not just `.attribution`
  /// — so the why-panel can also read `.kb_context` (prompt excerpt +
  /// decisions) without a second fetch (`WhyPanel.tsx`).
  bySha: Map<string, LineWhyOut>;
  isLoading: boolean;
}

/// The disclosure ladder's lazy prefetch (W4.4, step 1): one
/// `GET /api/why?line=` call per DISTINCT sha in `regions` — queried by the
/// first region carrying that sha, but keyed (react-query dedup + this
/// hook's own cache) purely by `sha`, so every OTHER region sharing that
/// sha reuses the same result with no further network call (`why`'s
/// line-grade attribution depends only on the resolved commit, never the
/// exact line asked about — see `provenance::why`'s module doc). `enabled`
/// gates the whole prefetch on the reader's "provenance" toggle, same as
/// `useBlame` itself.
export function useBlameAttributions(
  repo: string | undefined,
  path: string | undefined,
  regions: BlameRegion[] | undefined,
  enabled: boolean,
): BlameAttributions {
  const shas = regions ? distinctRegionShas(regions).slice(0, MAX_PREFETCH_SHAS) : [];
  const lineForSha = new Map<string, number>();
  if (regions) {
    for (const r of regions) {
      if (!lineForSha.has(r.sha)) lineForSha.set(r.sha, r.final_start);
    }
  }

  const queries = useQueries({
    queries: shas.map((sha) => ({
      queryKey: ["why-line", repo, path, sha],
      queryFn: () => fetchWhyLine(repo as string, path as string, lineForSha.get(sha) as number),
      enabled: enabled && repo !== undefined && path !== undefined,
      staleTime: Infinity,
    })),
  });

  const bySha = new Map<string, LineWhyOut>();
  let isLoading = false;
  shas.forEach((sha, i) => {
    const q = queries[i];
    if (q?.data) bySha.set(sha, q.data);
    else if (q?.isLoading) isLoading = true;
  });

  return { bySha, isLoading };
}
