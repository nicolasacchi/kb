// V76-R2b — cache `GET /api/syntax` (syntax/1). The registry is
// build-time data: one fetch per SPA session, `staleTime: Infinity`.
import { useQuery } from "@tanstack/react-query";
import { fetchSyntax } from "../api/client";
import type { SyntaxOut } from "../api/types";

export function useSyntax() {
  return useQuery<SyntaxOut>({
    queryKey: ["syntax"],
    queryFn: fetchSyntax,
    staleTime: Infinity,
    gcTime: Infinity,
  });
}
