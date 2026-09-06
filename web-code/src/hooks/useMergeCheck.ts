import { useQuery } from "@tanstack/react-query";
import { fetchMergeCheck } from "../api/client";

/// `GET /api/merge-check?repo=&from=&to=` (Phase G1) — dry-run merge
/// readiness, shared by the Compare page's card (always fetched once both
/// refs are known) and the Branches page's per-row LAZY check (`enabled`
/// gated on the operator actually clicking "Check" — see
/// `components/history/MergeCheckButton.tsx`).
export function useMergeCheck(
  repo: string | undefined,
  from: string | undefined,
  to: string | undefined,
  enabled = true,
) {
  return useQuery({
    queryKey: ["merge-check", repo, from, to],
    queryFn: () => fetchMergeCheck(repo as string, from as string, to as string),
    enabled: enabled && repo !== undefined && !!from && !!to,
  });
}
