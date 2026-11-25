import { useContext, useMemo } from "react";
import { QueryClient, QueryClientContext, useQuery } from "@tanstack/react-query";
import { useIdentity } from "./useArtifactHost";
import {
  NAV_ITEMS,
  resolveNavItems,
  type NavState,
  type ResolvedNavItem,
} from "../lib/navItems";
import type { TermRow } from "../api/terms";
import { termsQueryOptions } from "../terminal/useTerms";

/// Terminals that need the operator: blocked rows that are still alive.
export function blockedTermCount(rows: readonly TermRow[] | undefined): number {
  if (!rows) return 0;
  let n = 0;
  for (const r of rows) if (r.blocked === true && !r.pane_dead) n += 1;
  return n;
}

// Nav surfaces (and their tests) may render without a QueryClientProvider, and
// hooks cannot be conditional. With no provider the query is DISABLED and runs
// against this private client, which never fetches and holds nothing.
const FALLBACK_CLIENT = new QueryClient();

// The client state nav `badge` / `visible` predicates read. `blockedTerms`
// counts blocked rows of the shared `["terms"]` query (same options as
// `useTerms`, no polling; the SSE bridge keeps it fresh); it only runs for a
// terminal-admitted caller.
export function useNavState(): NavState {
  const terminalAdmitted = useIdentity()?.terminal_admitted === true;
  const provided = useContext(QueryClientContext);
  const q = useQuery(
    {
      ...termsQueryOptions,
      enabled: terminalAdmitted && provided !== undefined,
      select: blockedTermCount,
    },
    provided ?? FALLBACK_CLIENT,
  );
  const blockedTerms = terminalAdmitted ? (q.data ?? 0) : 0;
  return useMemo(
    () => ({ terminalAdmitted, blockedTerms }),
    [terminalAdmitted, blockedTerms],
  );
}

/// NAV_ITEMS resolved against the live nav state.
export function useNavItems(): ResolvedNavItem[] {
  const state = useNavState();
  return useMemo(() => resolveNavItems(NAV_ITEMS, state), [state]);
}
