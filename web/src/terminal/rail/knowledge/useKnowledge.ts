// RAILKNOW — one useQuery per lane (#23). The `["sessions"]` prefix receives
// session.captured / session.state invalidation; `["memories"]` receives the
// memory.* events. No polling, nothing read from terminal bytes.

import { useQuery } from "@tanstack/react-query";
import type { MemoryScope } from "../../../api/client";
import {
  fetchKnowledgeMemories,
  fetchKnowledgeRecalls,
  fetchKnowledgeResearch,
  fetchKnowledgeSimilar,
  knowledgeRecall,
} from "./knowledgeApi";

export type SessionLane = "recalls" | "memories" | "research" | "similar";

export const knowledgeKey = (lane: SessionLane, sid: string | undefined) =>
  ["sessions", "knowledge", lane, sid] as const;
export const knowledgeSearchKey = (scope: MemoryScope, q: string) => ["memories", "rail", scope, q] as const;

export function useKnowledgeRecalls(sid: string | undefined) {
  return useQuery({
    queryKey: knowledgeKey("recalls", sid),
    enabled: !!sid,
    staleTime: Infinity,
    queryFn: ({ signal }) => fetchKnowledgeRecalls(sid as string, signal),
  });
}

export function useKnowledgeMemories(sid: string | undefined) {
  return useQuery({
    queryKey: knowledgeKey("memories", sid),
    enabled: !!sid,
    staleTime: Infinity,
    queryFn: ({ signal }) => fetchKnowledgeMemories(sid as string, signal),
  });
}

export function useKnowledgeResearch(sid: string | undefined) {
  return useQuery({
    queryKey: knowledgeKey("research", sid),
    enabled: !!sid,
    staleTime: Infinity,
    queryFn: ({ signal }) => fetchKnowledgeResearch(sid as string, signal),
  });
}

export function useKnowledgeSimilar(sid: string | undefined, prompt: string | undefined, project: string | undefined) {
  return useQuery({
    queryKey: knowledgeKey("similar", sid),
    enabled: !!sid && !!prompt,
    staleTime: Infinity,
    queryFn: ({ signal }) => fetchKnowledgeSimilar(prompt as string, project, signal),
  });
}

export function useKnowledgeSearch(scope: MemoryScope, q: string) {
  return useQuery({
    queryKey: knowledgeSearchKey(scope, q),
    staleTime: Infinity,
    queryFn: ({ signal }) => knowledgeRecall(q, scope, signal),
  });
}
