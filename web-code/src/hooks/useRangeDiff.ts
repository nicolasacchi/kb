import { useQuery } from "@tanstack/react-query";
import { fetchRangeDiff } from "../api/client";

/// `GET /api/range-diff?repo=&old=&new=` (Phase C6) — `old`/`new` are RANGE
/// strings (`main..topic@{1}` vs `main..topic`), not plain revspecs.
export function useRangeDiff(
  repo: string | undefined,
  oldRange: string | undefined,
  newRange: string | undefined,
) {
  return useQuery({
    queryKey: ["range-diff", repo, oldRange, newRange],
    queryFn: () => fetchRangeDiff(repo as string, oldRange as string, newRange as string),
    enabled: repo !== undefined && !!oldRange && !!newRange,
  });
}
