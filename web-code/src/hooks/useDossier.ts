// V72-G1.2 — ONE `entity/1` read, two renderers.
//
// The dossier CENTER (`components/entity/DossierView.tsx`) and the Dossier
// RAIL tab (`components/entity/DossierRail.tsx`) show the same entity's member
// table, and they must never disagree about it. The way to guarantee that is
// not discipline, it is arithmetic: there is one query, owned here and lifted
// into `routes/Reader.tsx`, and both components render the `DossierOut` it
// hands them. Neither fetches.
//
// That is the same "one projection, two renderers" rule kbc-tree/1 states for
// `GET /api/tree/2` (`kb-code-server/CLAUDE.md` invariant 17a) — its own
// stated top risk is two renderers of one projection drifting apart, and the
// fix there was to compute once and render twice. This is that shape for a
// second wire.
//
// `usagesPerKind` is part of the key on purpose: "show more" is a RE-FETCH
// with a higher per-kind cut, never a client-side reveal, because the rows a
// smaller cut did not return were never in the browser to reveal.

import { useQuery } from "@tanstack/react-query";
import { fetchDossier } from "../api/client";
import type { DossierOut } from "../api/types";
import { DEFAULT_USAGES_PER_KIND, readStateOf, type DossierReadState } from "../lib/dossier";

export interface UseDossierResult {
  data: DossierOut | undefined;
  /// The four READ states, resolved once (`lib/dossier.ts`'s `readStateOf`),
  /// so the center and the rail cannot render different ones.
  state: DossierReadState;
  error: unknown;
}

export function useDossier(
  repo: string,
  ent: string | null,
  inherited: boolean,
  usagesPerKind: number,
): UseDossierResult {
  const q = useQuery({
    queryKey: ["entity-dossier", repo, ent ?? "", inherited, usagesPerKind],
    queryFn: () =>
      fetchDossier({
        repo,
        ent: ent as string,
        inherited,
        // Omitted at its default so a plain open sends the shortest address
        // the route documents.
        usagesPerKind: usagesPerKind === DEFAULT_USAGES_PER_KIND ? undefined : usagesPerKind,
      }),
    enabled: repo !== "" && ent !== null && ent !== "",
    staleTime: 30_000,
  });
  return {
    data: q.data,
    state: readStateOf({ isLoading: q.isLoading, error: q.error, data: q.data }),
    error: q.error,
  };
}
