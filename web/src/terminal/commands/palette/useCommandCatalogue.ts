// CMDPALETTE: the effective command catalogue (`GET /api/terms/commands`),
// key ["term","commands"], invalidated by the `term.commands` SSE kind through
// the bridge in api/queryClient.ts (#23: the bridge owns invalidation, the
// hook carries none). Enabled only when the terminal tier is admitted. While
// there is no data (loading or failed) the shipped defaults answer, flagged
// so the UI can say so.

import { useMemo } from "react";
import { useQuery } from "@tanstack/react-query";
import { currentDaemonBase } from "../../../api/base";
import type { EffectiveCatalogue } from "../../../api/generated/EffectiveCatalogue";
import { TERMINAL_INTENT_HEADER, TERMINAL_INTENT_VALUE } from "../../../api/terms";
import { useIdentity } from "../../../hooks/useArtifactHost";
import { effectiveCatalogue, type Effective } from "../catalog";

export const COMMANDS_KEY = ["term", "commands"] as const;

export async function fetchCommands(signal?: AbortSignal): Promise<EffectiveCatalogue> {
  const res = await fetch(`${currentDaemonBase()}/api/terms/commands`, {
    headers: { Accept: "application/json", [TERMINAL_INTENT_HEADER]: TERMINAL_INTENT_VALUE },
    signal,
  });
  if (!res.ok) throw new Error(`commands ${res.status}`);
  return (await res.json()) as EffectiveCatalogue;
}

export interface CommandCatalogue {
  catalogue: Effective;
  /** True when the shipped defaults stand in for the daemon's answer. */
  fallback: boolean;
}

export function useCommandCatalogue(): CommandCatalogue {
  const identity = useIdentity();
  const q = useQuery({
    queryKey: COMMANDS_KEY,
    queryFn: ({ signal }) => fetchCommands(signal),
    staleTime: Infinity,
    enabled: identity?.terminal_admitted === true,
  });
  const shipped = useMemo(() => effectiveCatalogue(null), []);
  if (q.data === undefined || q.isError) return { catalogue: shipped, fallback: true };
  return { catalogue: q.data as unknown as Effective, fallback: false };
}
