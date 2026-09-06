import { useQuery } from "@tanstack/react-query";
import { fetchScopes } from "../api/client";

/// Phase N — `GET /api/scopes`. Global (not per-repo); boot config is
/// static for the daemon lifetime, so Infinity staleTime is fine.
export function scopesQueryKey() {
  return ["scopes"] as const;
}

export function useScopes() {
  return useQuery({
    queryKey: scopesQueryKey(),
    queryFn: () => fetchScopes(),
  });
}
