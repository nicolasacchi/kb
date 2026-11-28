// PQUI: the `["term","prompts",project]` query and its mutations. staleTime
// Infinity, never polls: the `term.prompts` SSE event invalidates it through
// the shared bridge (api/queryClient.ts), and every successful mutation writes
// the answered list straight into the cache. Components never call fetch.
import { useMemo } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { addPrompt, deletePrompt, fetchPrompts, patchPrompt, termPromptsKey, type PromptList, type PromptPatch } from "./promptsApi";

export function useTermPrompts(project: string | null) {
  return useQuery({
    queryKey: termPromptsKey(project ?? ""),
    queryFn: ({ signal }) => fetchPrompts(project as string, signal),
    enabled: project !== null,
    staleTime: Infinity,
  });
}

export interface PromptActions {
  add: (text: string) => Promise<PromptList>;
  patch: (id: string, patch: PromptPatch) => Promise<PromptList>;
  remove: (id: string) => Promise<PromptList>;
  /** The cached list (the rev PATCH/DELETE send), or undefined before the first read. */
  current: () => PromptList | undefined;
  /** Drop the cache entry's freshness and read the list again. */
  refetch: () => Promise<PromptList>;
}

/** Every mutation sends the CACHED rev and caches the answer. */
export function usePromptActions(project: string): PromptActions {
  const qc = useQueryClient();
  return useMemo(() => {
    const key = termPromptsKey(project);
    const current = () => qc.getQueryData<PromptList>(key);
    const put = (l: PromptList): PromptList => {
      qc.setQueryData(key, l);
      return l;
    };
    return {
      add: async (text) => put(await addPrompt(project, text)),
      patch: async (id, p) => put(await patchPrompt(project, id, p, current()?.rev ?? 0)),
      remove: async (id) => put(await deletePrompt(project, id, current()?.rev ?? 0)),
      current,
      refetch: async () => put(await fetchPrompts(project)),
    };
  }, [qc, project]);
}
