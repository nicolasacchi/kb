// RAILKNOW — THROWING readers for the Knowledge tab. They hit the same routes
// as the `jsonOrEmpty` readers in api/sessions.ts (which turn every failure
// into an empty list and so cannot give per-lane error isolation) and reuse
// the same generated wire types. 404 = the session is not captured yet.

import { currentDaemonBase } from "../../../api/base";
import { fetchRecall, type MemoryScope } from "../../../api/client";
import type { RecallResponse } from "../../../api/generated/RecallResponse";
import type { RecollectResponse } from "../../../api/generated/RecollectResponse";
import type { SessionMemoriesResponse } from "../../../api/generated/SessionMemoriesResponse";
import type { SessionRecallsResponse } from "../../../api/generated/SessionRecallsResponse";
import type { SessionResearchResponse } from "../../../api/generated/SessionResearchResponse";

export class KnowledgeHttpError extends Error {
  readonly status: number;
  constructor(status: number, message: string) {
    super(message);
    this.name = "KnowledgeHttpError";
    this.status = status;
  }
}

export type Lane<T> = { kind: "ok"; data: T } | { kind: "not-captured" };

async function getLane<T>(path: string, signal?: AbortSignal): Promise<Lane<T>> {
  const r = await fetch(`${currentDaemonBase()}${path}`, {
    headers: { Accept: "application/json" },
    signal,
  });
  if (r.status === 404) return { kind: "not-captured" };
  if (!r.ok) throw new KnowledgeHttpError(r.status, `${r.status} ${r.statusText}`.trim());
  let body: unknown;
  try {
    body = await r.json();
  } catch {
    throw new KnowledgeHttpError(r.status, "Unreadable response");
  }
  if (body === null || typeof body !== "object") throw new KnowledgeHttpError(r.status, "Unreadable response");
  return { kind: "ok", data: body as T };
}

const sidPath = (sid: string, leaf: string) => `/api/sessions/${encodeURIComponent(sid)}/${leaf}`;

export const fetchKnowledgeRecalls = (sid: string, signal?: AbortSignal) =>
  getLane<SessionRecallsResponse>(sidPath(sid, "recalls"), signal);
export const fetchKnowledgeMemories = (sid: string, signal?: AbortSignal) =>
  getLane<SessionMemoriesResponse>(sidPath(sid, "memories"), signal);
export const fetchKnowledgeResearch = (sid: string, signal?: AbortSignal) =>
  getLane<SessionResearchResponse>(sidPath(sid, "research"), signal);

/** The two session-detail fields the Similar lane needs (throwing reader). */
export interface KnowledgeDetail {
  first_user_prompt?: string | null;
  project_key?: string | null;
}
export const fetchKnowledgeDetail = (sid: string, signal?: AbortSignal) =>
  getLane<KnowledgeDetail>(`/api/sessions/${encodeURIComponent(sid)}`, signal);

export async function fetchKnowledgeSimilar(
  q: string,
  project: string | undefined,
  signal?: AbortSignal,
): Promise<RecollectResponse> {
  const params = new URLSearchParams({ q, limit: "5" });
  if (project) params.set("project", project);
  const r = await getLane<RecollectResponse>(`/api/sessions/recollect?${params.toString()}`, signal);
  if (r.kind === "not-captured") throw new KnowledgeHttpError(404, "recollect unavailable");
  return r.data;
}

/** Project memory search: the memory page's recall route, no weekly ledger. */
export function knowledgeRecall(q: string, scope: MemoryScope, signal?: AbortSignal): Promise<RecallResponse> {
  return fetchRecall({ q: q === "" ? undefined : q, scope, limit: 20 }, signal);
}
