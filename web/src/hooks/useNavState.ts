import { useMemo } from "react";
import { useIdentity } from "./useArtifactHost";
import {
  NAV_ITEMS,
  resolveNavItems,
  type NavState,
  type ResolvedNavItem,
} from "../lib/navItems";

// The client state nav `badge` / `visible` predicates read. `blockedTerms` is a
// placeholder 0: the hub door unit replaces it with the blocked count over the
// terminals query (editing THIS file). It deliberately calls no TanStack hook
// here so nav surfaces keep rendering without a QueryClientProvider.
export function useNavState(): NavState {
  const terminalAdmitted = useIdentity()?.terminal_admitted === true;
  return useMemo(
    () => ({ terminalAdmitted, blockedTerms: 0 }),
    [terminalAdmitted],
  );
}

/// NAV_ITEMS resolved against the live nav state.
export function useNavItems(): ResolvedNavItem[] {
  const state = useNavState();
  return useMemo(() => resolveNavItems(NAV_ITEMS, state), [state]);
}
