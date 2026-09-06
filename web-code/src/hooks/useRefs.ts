import { useQuery } from "@tanstack/react-query";
import { fetchRefs } from "../api/client";

export function useRefs(repo: string | undefined) {
  return useQuery({
    queryKey: ["refs", repo],
    queryFn: () => fetchRefs(repo as string),
    enabled: repo !== undefined,
  });
}
