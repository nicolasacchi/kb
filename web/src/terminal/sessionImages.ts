// T21 - the transcript's picture REFERENCES for the Images strip.
//
// kb stores references only (T20): an `Item::Image` in `session-view/1`
// carries a mime and, when the transcript named one, the sha256 of the
// picture. This module reads them out of `GET /api/sessions/{sid}/view`
// (turns only) and keeps ONLY the small refs in the query cache: no picture
// bytes ever enter TanStack (invariant #23); the bytes, when kb can serve
// them at all, come through `MediaCache` as blob URLs held in memory.

import { useQuery } from "@tanstack/react-query";
import { currentDaemonBase } from "../api/base";
import type { SessionViewResponse } from "../api/generated/SessionViewResponse";

/** One picture a transcript turn refers to. */
export interface TranscriptImageRef {
  mime: string;
  /** sha256 of the picture's bytes when the transcript named one. */
  sha256: string | null;
  /** The stable `t-<uuid12>` id of the turn that carries it. */
  turnId: string;
  /** The turn's RFC 3339 timestamp, when it has one. */
  ts: string | null;
}

const SHA256_RE = /^[0-9a-f]{64}$/;

/** Every `Item::Image` of `view`, in turn order. */
export function extractImageRefs(view: Partial<SessionViewResponse>): TranscriptImageRef[] {
  const out: TranscriptImageRef[] = [];
  for (const turn of view.turns ?? []) {
    for (const item of turn.items) {
      if (item.item !== "Image") continue;
      out.push({
        mime: item.mime,
        sha256: item.sha256 !== null && SHA256_RE.test(item.sha256) ? item.sha256 : null,
        turnId: turn.id,
        ts: turn.ts,
      });
    }
  }
  return out;
}

export async function fetchSessionImageRefs(
  sid: string,
  signal?: AbortSignal,
): Promise<TranscriptImageRef[]> {
  const r = await fetch(
    `${currentDaemonBase()}/api/sessions/${encodeURIComponent(sid)}/view?fields=turns&turns=all`,
    { headers: { Accept: "application/json" }, signal },
  );
  if (!r.ok) throw new Error(`session view: HTTP ${r.status}`);
  return extractImageRefs((await r.json()) as Partial<SessionViewResponse>);
}

// The key rides the `["sessions"]` prefix, so the bridge's own
// `session.captured`/`session.deleted` invalidation reaches it; no new SSE
// kind and no refetch interval (#23).
export const SESSION_IMAGES_KEY = (sid: string) => ["sessions", "images", sid] as const;

export function useSessionImageRefs(
  sid: string,
  enabled: boolean,
): { refs: TranscriptImageRef[]; loading: boolean; error: string | null } {
  const q = useQuery({
    queryKey: SESSION_IMAGES_KEY(sid),
    queryFn: ({ signal }) => fetchSessionImageRefs(sid, signal),
    staleTime: Infinity,
    enabled: enabled && sid !== "",
  });
  return {
    refs: q.data ?? [],
    loading: enabled && q.isPending,
    error: q.error ? String(q.error) : null,
  };
}
