import { useQuery } from "@tanstack/react-query";
import { fetchSessionDiff } from "../api/client";

/// `GET /api/session-diff` — LOOPBACK-ONLY server-side (`router.rs`'s
/// transcripts sub-router); a non-loopback SPA gets a 404 `ApiError`, which
/// `retry: false` treats as a terminal "unavailable" rather than retrying —
/// both `routes/SessionDiff.tsx` (the full page) and `WhyPanel`'s
/// best-effort "sibling files" section (`lib/sessionDiff.ts`'s
/// `siblingFiles`) read `isError` to degrade quietly rather than surface a
/// hard failure (see each caller's own doc).
export function useSessionDiff(sessionId: string | undefined, repo: string | undefined, enabled = true) {
  return useQuery({
    queryKey: ["session-diff", sessionId ?? null, repo ?? null],
    queryFn: () => fetchSessionDiff(sessionId as string, repo),
    enabled: enabled && sessionId !== undefined,
    retry: false,
  });
}
