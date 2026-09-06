// MI-W4.1 — shared daemon-wide decay-policy query. Was previously a
// component-local `useEffect`/`useState` fetch inside `DecayRail`; hoisted
// to a `useQuery` (TQ3 — invariant #23's `staleTime: Infinity` default, no
// hand-rolled fetch/subscribe plumbing) so `DecayRail`, every row's
// `DecaySparkline` (the floor reference line), and the MI-W4.3 lineage
// viewer all share ONE cached read instead of each re-fetching. Any number
// of components calling this hook with the same query key still issues
// exactly one network request — TanStack Query's normal dedup, not
// something this hook has to implement itself.

import { useQuery, useQueryClient } from "@tanstack/react-query";
import { fetchMemoryPolicy, setMemoryPolicy, type DecayPolicy, type MemoryPolicyOut } from "../api/client";

export const MEMORY_POLICY_KEY = ["memory-policy"] as const;

export type UseMemoryPolicy = {
  policy: DecayPolicy | null;
  /** The active policy's salience floor; `null` for `loose` (never drops). */
  dropThreshold: number | null;
  loading: boolean;
  error: string | null;
  /** Optimistic flip with rollback on failure — mirrors the pre-hook
   * `DecayRail.flipPolicy` behaviour exactly. */
  flip: (next: DecayPolicy) => Promise<void>;
};

export function useMemoryPolicy(): UseMemoryPolicy {
  const queryClient = useQueryClient();
  const q = useQuery({
    queryKey: MEMORY_POLICY_KEY,
    queryFn: ({ signal }) => fetchMemoryPolicy(signal),
  });

  const flip = async (next: DecayPolicy) => {
    const prev = queryClient.getQueryData<MemoryPolicyOut>(MEMORY_POLICY_KEY);
    queryClient.setQueryData<MemoryPolicyOut>(MEMORY_POLICY_KEY, {
      policy: next,
      drop_threshold: prev?.drop_threshold,
    });
    try {
      const res = await setMemoryPolicy(next);
      queryClient.setQueryData<MemoryPolicyOut>(MEMORY_POLICY_KEY, res);
    } catch (e) {
      queryClient.setQueryData<MemoryPolicyOut>(MEMORY_POLICY_KEY, prev);
      throw e;
    }
  };

  return {
    policy: q.data?.policy ?? null,
    dropThreshold: q.data?.drop_threshold ?? null,
    loading: q.isPending,
    error: q.isError ? String(q.error) : null,
    flip,
  };
}
