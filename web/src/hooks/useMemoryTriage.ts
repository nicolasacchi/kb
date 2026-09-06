// FIX2 (kb-core/triage.rs threshold-duplication risk) — shared
// `GET /api/memory/triage` query, mirroring `useMemoryPolicy.ts`'s shape:
// any number of components calling this hook with the same query key still
// issue exactly one network request (TanStack Query's normal dedup, not
// something this hook implements itself). Exists so the recall quadrant
// scatter's wire-supplied `high_salience_threshold`/`dormant_days` — the
// SAME response `HygieneQueue` already fetches unconditionally on
// `/memory` — never costs a second round-trip.

import { useQuery } from "@tanstack/react-query";
import { fetchMemoryTriage } from "../api/client";

export const MEMORY_TRIAGE_KEY = ["memory-triage"] as const;

export function useMemoryTriage() {
  return useQuery({
    queryKey: MEMORY_TRIAGE_KEY,
    queryFn: ({ signal }) => fetchMemoryTriage({}, signal),
  });
}
