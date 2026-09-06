import { useQuery } from "@tanstack/react-query";
import { fetchUsers } from "../api/client";

// v0.34 W — GET /api/users. Config-static + observed set; no users.*
// SSE event exists, so fetch-once (staleTime Infinity, default) is fine
// and the SSE bridge never invalidates ["users"].
export function useUsers() {
  return useQuery({
    queryKey: ["users"] as const,
    queryFn: ({ signal }) => fetchUsers(signal),
  });
}
