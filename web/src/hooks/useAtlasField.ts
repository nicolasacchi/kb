import { useQuery } from "@tanstack/react-query";
import {
  fetchAtlasField,
  fetchAtlasFieldDisagreement,
  type AtlasFieldDisagreementResponse,
} from "../api/atlasField";
import type { CanvasDoc } from "../lib/canvas";

// W3.F-c — the operator field's two server-state reads (the onion skin's
// data half). Query-key convention (documented in api/queryClient.ts):
//
//   ["atlasField", kb]                 the raw JSON Canvas sidecar
//   ["atlasField", kb, "disagreement"] the Procrustes-aligned displacements
//
// The disagreement key is deliberately NESTED under the sidecar's, so the
// ONE bridge line for `atlas.field.updated` (and the existing
// atlas.recompute/recluster pair — the machine half of the comparison
// moves too) invalidates both by PREFIX. staleTime Infinity like every
// other server-state key (#23); zero fetch/subscribe plumbing here and no
// new SSE connection (#24) — the bridge in queryClient.ts owns that.
//
// Both hooks are LAZY: AtlasView passes `undefined` until the operator
// turns the overlay on, so a plain atlas visit costs nothing extra (the
// same shape `useAtlasHistory` uses for the time-lapse).

export function useAtlasField(kb: string | undefined) {
  return useQuery({
    queryKey: ["atlasField", kb] as const,
    enabled: !!kb,
    queryFn: ({ signal }) => fetchAtlasField(kb as string, signal),
  });
}

export function useAtlasFieldDisagreement(kb: string | undefined) {
  return useQuery({
    queryKey: ["atlasField", kb, "disagreement"] as const,
    enabled: !!kb,
    queryFn: ({ signal }) =>
      fetchAtlasFieldDisagreement(kb as string, signal),
  });
}

export type { AtlasFieldDisagreementResponse, CanvasDoc };
