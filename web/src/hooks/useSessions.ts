// v0.14 S5 — sessions list + per-session detail hooks. TQ3: both ride
// the query cache.
//
// The list is keyset-cursor-paginated (cursor + cursor_id, X1) via
// useInfiniteQuery. Its SSE wiring stays IN this hook (not the bridge)
// on purpose: a fresh capture should land at the TOP, so the deliberate
// reconciliation is collapse-to-page-1 (drop the deeper pages, refetch
// page 1) rather than the bridge's refetch-all-pages-in-place — the
// bridge therefore carries no ["sessions"] mapping.

import { useCallback, useEffect, useMemo } from "react";
import {
  useInfiniteQuery,
  useQuery,
  useQueryClient,
  type InfiniteData,
} from "@tanstack/react-query";
import {
  fetchArtifactSessions,
  fetchCommitFiles,
  fetchSession,
  fetchSessionByArtifact,
  fetchSessionComments,
  fetchSessionCommits,
  fetchSessionDecisions,
  fetchSessionFiles,
  fetchSessionFolders,
  fetchSessionMemories,
  fetchSessionRecalls,
  fetchSessionOutline,
  fetchSessionPresence,
  fetchSessionProjects,
  fetchSessionReadings,
  fetchSessionRecollect,
  fetchSessionResearch,
  fetchSessionThreads,
  fetchSessionsPage,
  fetchSessionTouches,
  fetchWhy,
  type ArtifactSessionOut,
  type ByArtifactResponse,
  type CommitFilesResponse,
  type CommitOut,
  type DecisionOut,
  type FolderOut,
  type OutlineRow,
  type PresenceResponse,
  type ProjectOut,
  type RecollectResponse,
  type ResearchOut,
  type SessionCommentsResponse,
  type ThreadOut,
  type SessionDetail,
  type SessionFileOut,
  type SessionMemoryHit,
  type SessionRecallHit,
  type SessionReadingRow,
  type SessionRow,
  type SessionsListResponse,
  type TouchesResponse,
  type WhyResponse,
} from "../api/sessions";
import { sse } from "../api/sse";
import { presenceLiveSet, type PresenceLiveSet } from "../lib/sessionPresence";

const SESSION_EVENTS = ["session.captured", "session.deleted"] as const;
// U1 — page size for the /sessions infinite-scroll consumer. Matches
// the daemon's DEFAULT_LIMIT so the SPA stays in step with the
// cursor-paginated /api/sessions response shape (T5).
const PAGE_SIZE = 50;
const KEY = ["sessions"] as const;
const EMPTY: SessionRow[] = [];

type PageParam = { cursor?: number; cursor_id?: string };

export type UseSessions = {
  rows: SessionRow[];
  loading: boolean;
  loadingMore: boolean;
  error: string | null;
  refresh: () => void;
  loadMore: () => void;
  hasMore: boolean;
};

// A1 — `folder` (a full cwd path) narrows the list to one project; `query`
// (P6) is a keyword search. W3.A adds `project` (the P1-derived-key axis —
// `sessionsUrl.ts` is the ONLY emitter, `folder` stays legacy-inbound) and
// `substance` (S1's csv triage filter). The query key carries every axis so
// each view is a distinct cache entry, and the SSE refresh below
// invalidates them all.
export function useSessions(
  folder?: string,
  query?: string,
  // The gallery join consumer passes `enabled=false` until it has detected a
  // session artifact in view, so non-session corpora never fire /api/sessions.
  enabled = true,
  project?: string,
  substance?: string,
  harness?: string,
): UseSessions {
  const queryClient = useQueryClient();
  const key =
    folder || query || project || substance || harness
      ? ([...KEY, folder ?? "", query ?? "", project ?? "", substance ?? "", harness ?? ""] as const)
      : KEY;
  const q = useInfiniteQuery({
    queryKey: key,
    enabled,
    initialPageParam: {} as PageParam,
    queryFn: ({ pageParam, signal }) =>
      fetchSessionsPage(
        { ...pageParam, limit: PAGE_SIZE, folder, q: query, project, substance, harness },
        signal,
      ),
    getNextPageParam: (last: SessionsListResponse): PageParam | undefined =>
      last.next_cursor != null
        ? { cursor: last.next_cursor, cursor_id: last.next_cursor_id }
        : undefined,
  });

  const rows = useMemo(() => {
    const pages = q.data?.pages ?? [];
    // Defensive dedup: the daemon's started_at < cursor filter is
    // exclusive, but two same-second rows straddling a page boundary
    // can repeat. Drop dupes by (kb, artifact_id).
    const seen = new Set<string>();
    const out: SessionRow[] = [];
    for (const page of pages) {
      for (const r of page.sessions) {
        const k = `${r.kb}:${r.artifact_id}`;
        if (seen.has(k)) continue;
        seen.add(k);
        out.push(r);
      }
    }
    return out.length > 0 ? out : EMPTY;
  }, [q.data]);

  // Collapse to page 1 + refetch — the deliberate reconciliation for
  // list-shape changes (see module header). Collapses THIS folder view's
  // cache, then invalidates the whole `["sessions", …]` prefix so every
  // folder view refetches on the next capture.
  const refresh = useCallback(() => {
    queryClient.setQueryData<InfiniteData<SessionsListResponse, PageParam>>(
      key,
      (data) =>
        data && data.pages.length > 1
          ? { pages: data.pages.slice(0, 1), pageParams: data.pageParams.slice(0, 1) }
          : data,
    );
    void queryClient.invalidateQueries({ queryKey: KEY });
  }, [queryClient, key]);

  useEffect(() => {
    const offs = SESSION_EVENTS.map((t) =>
      sse.subscribeEvent(t, () => refresh()),
    );
    offs.push(sse.onResync(() => refresh()));
    return () => offs.forEach((off) => off());
  }, [refresh]);

  const { hasNextPage, isFetchingNextPage, fetchNextPage } = q;
  const loadMore = useCallback(() => {
    if (hasNextPage && !isFetchingNextPage) void fetchNextPage();
  }, [hasNextPage, isFetchingNextPage, fetchNextPage]);

  return {
    rows,
    loading: q.isPending,
    loadingMore: q.isFetchingNextPage,
    error: q.error ? String(q.error) : null,
    refresh,
    loadMore,
    hasMore: q.hasNextPage ?? false,
  };
}

const EMPTY_FOLDERS: FolderOut[] = [];
const EMPTY_THREADS: ThreadOut[] = [];

// P7 — narrative threads for the /sessions threads view. Refreshes on session
// SSE events. `enabled` gates the fetch to when the threads view is active.
export function useSessionThreads(enabled: boolean): {
  threads: ThreadOut[];
  loading: boolean;
} {
  const queryClient = useQueryClient();
  const q = useQuery({
    queryKey: ["session-threads"] as const,
    enabled,
    queryFn: ({ signal }) => fetchSessionThreads(signal),
  });
  useEffect(() => {
    if (!enabled) return;
    const refresh = () =>
      void queryClient.invalidateQueries({ queryKey: ["session-threads"] });
    const offs = SESSION_EVENTS.map((t) => sse.subscribeEvent(t, refresh));
    offs.push(sse.onResync(refresh));
    return () => offs.forEach((off) => off());
  }, [queryClient, enabled]);
  return { threads: q.data ?? EMPTY_THREADS, loading: enabled && q.isPending };
}

// A1 — the folder facet for the /sessions filter dropdown. Refreshes on the
// same session SSE events as the list (a new capture can add a folder).
export function useSessionFolders(enabled = true): FolderOut[] {
  const queryClient = useQueryClient();
  const q = useQuery({
    queryKey: ["session-folders"] as const,
    enabled,
    queryFn: ({ signal }) => fetchSessionFolders(signal),
  });
  useEffect(() => {
    const refresh = () =>
      void queryClient.invalidateQueries({ queryKey: ["session-folders"] });
    const offs = SESSION_EVENTS.map((t) => sse.subscribeEvent(t, refresh));
    offs.push(sse.onResync(refresh));
    return () => offs.forEach((off) => off());
  }, [queryClient]);
  return q.data ?? EMPTY_FOLDERS;
}

export type UseSessionDetail = {
  detail: SessionDetail | null;
  memories: SessionMemoryHit[];
  /// MI-W4.2c — the PULL side: memories this session's `kb-recall` hook
  /// actually injected (as opposed to `memories`, which is the WRITE side
  /// — memories this session produced).
  recalls: SessionRecallHit[];
  touches: TouchesResponse | null;
  readings: SessionReadingRow[];
  files: SessionFileOut[];
  decisions: DecisionOut[];
  commits: CommitOut[];
  research: ResearchOut[];
  comments: SessionCommentsResponse;
  loading: boolean;
};

const EMPTY_MEMORIES: SessionMemoryHit[] = [];
const EMPTY_RECALLS: SessionRecallHit[] = [];
const EMPTY_READINGS: SessionReadingRow[] = [];
const EMPTY_FILES: SessionFileOut[] = [];
const EMPTY_DECISIONS: DecisionOut[] = [];
const EMPTY_COMMITS: CommitOut[] = [];
const EMPTY_RESEARCH: ResearchOut[] = [];
const EMPTY_COMMENTS_RES: SessionCommentsResponse = {
  artifacts: [],
  total: 0,
  raised: [],
};

export function useSessionDetail(sessionId: string | null): UseSessionDetail {
  const q = useQuery({
    queryKey: ["session", sessionId] as const,
    enabled: !!sessionId,
    queryFn: async ({ signal }) => {
      const sid = sessionId as string;
      const [
        detail,
        memories,
        recalls,
        touches,
        readings,
        files,
        decisions,
        commits,
        research,
        comments,
      ] = await Promise.all([
        fetchSession(sid, signal),
        fetchSessionMemories(sid, signal),
        fetchSessionRecalls(sid, signal),
        fetchSessionTouches(sid, signal),
        fetchSessionReadings(sid, signal),
        fetchSessionFiles(sid, signal),
        fetchSessionDecisions(sid, signal),
        fetchSessionCommits(sid, signal),
        fetchSessionResearch(sid, signal),
        fetchSessionComments(sid, signal),
      ]);
      return {
        detail,
        memories,
        recalls,
        touches,
        readings,
        files,
        decisions,
        commits,
        research,
        comments,
      };
    },
  });

  return {
    detail: q.data?.detail ?? null,
    memories: q.data?.memories ?? EMPTY_MEMORIES,
    recalls: q.data?.recalls ?? EMPTY_RECALLS,
    touches: q.data?.touches ?? null,
    readings: q.data?.readings ?? EMPTY_READINGS,
    files: q.data?.files ?? EMPTY_FILES,
    decisions: q.data?.decisions ?? EMPTY_DECISIONS,
    commits: q.data?.commits ?? EMPTY_COMMITS,
    research: q.data?.research ?? EMPTY_RESEARCH,
    comments: q.data?.comments ?? EMPTY_COMMENTS_RES,
    loading: !!sessionId && q.isPending,
  };
}

// MI-W4.6 — the provenance thread's 3rd hop, lazy per-commit: only fetched
// once a commit row is expanded (`enabled`), never as part of the composite
// `useSessionDetail` fetch above (a session can have many commits, and most
// provenance panels never expand more than one).
export function useCommitFiles(
  sessionId: string | null,
  sha: string | null,
  enabled: boolean,
): { data: CommitFilesResponse | null; loading: boolean; error: string | null } {
  const q = useQuery({
    queryKey: ["session-commit-files", sessionId, sha] as const,
    enabled: enabled && !!sessionId && !!sha,
    queryFn: ({ signal }) => fetchCommitFiles(sessionId as string, sha as string, signal),
  });
  return {
    data: q.data ?? null,
    loading: q.isPending && enabled,
    error: q.error ? String(q.error) : null,
  };
}

const EMPTY_ARTIFACT_SESSIONS: ArtifactSessionOut[] = [];

// A7 — the sessions that touched a given artifact, for the reader panel.
// Lazy (enabled only when a kb+id is known); refreshes on session SSE.
export function useArtifactSessions(
  kb: string | null,
  artifactId: string | null,
): { sessions: ArtifactSessionOut[]; loading: boolean } {
  const queryClient = useQueryClient();
  const enabled = !!kb && !!artifactId;
  const q = useQuery({
    queryKey: ["artifact-sessions", kb, artifactId] as const,
    enabled,
    queryFn: ({ signal }) =>
      fetchArtifactSessions(kb as string, artifactId as string, signal),
  });
  useEffect(() => {
    if (!enabled) return;
    const refresh = () =>
      void queryClient.invalidateQueries({
        queryKey: ["artifact-sessions", kb, artifactId],
      });
    const offs = SESSION_EVENTS.map((t) => sse.subscribeEvent(t, refresh));
    offs.push(sse.onResync(refresh));
    return () => offs.forEach((off) => off());
  }, [queryClient, kb, artifactId, enabled]);
  return {
    sessions: q.data ?? EMPTY_ARTIFACT_SESSIONS,
    loading: enabled && q.isPending,
  };
}

// R2 — "why is this file the way it is": the touching sessions + their
// prompt/decisions/commits. Keyed on the artifact's source-relative path (the
// /api/why join key). Lazy + SSE-refreshed, like useArtifactSessions.
export function useWhy(
  kb: string | null,
  sourceRelative: string | null,
): { why: WhyResponse | null; loading: boolean } {
  const queryClient = useQueryClient();
  const enabled = !!kb && !!sourceRelative;
  const q = useQuery({
    queryKey: ["why", kb, sourceRelative] as const,
    enabled,
    queryFn: ({ signal }) => fetchWhy(sourceRelative as string, signal),
  });
  useEffect(() => {
    if (!enabled) return;
    const refresh = () =>
      void queryClient.invalidateQueries({
        queryKey: ["why", kb, sourceRelative],
      });
    const offs = SESSION_EVENTS.map((t) => sse.subscribeEvent(t, refresh));
    offs.push(sse.onResync(refresh));
    return () => offs.forEach((off) => off());
  }, [queryClient, kb, sourceRelative, enabled]);
  return { why: q.data ?? null, loading: enabled && q.isPending };
}

const EMPTY_PROJECTS: ProjectOut[] = [];

// W3.C/P6 — the projects-home facet: one card per registry entry /
// auto-project. Plain query under the `["sessions"]` carve-out prefix (#23)
// — rides `useSessions`' own `session.captured`/`session.deleted`
// invalidation wherever it's mounted, exactly like `useSessionReplay`; no
// second SSE subscription (#24).
export function useSessionProjects(enabled = true): {
  projects: ProjectOut[];
  loading: boolean;
} {
  const q = useQuery({
    queryKey: ["sessions", "projects"] as const,
    enabled,
    queryFn: ({ signal }) => fetchSessionProjects(signal),
  });
  return { projects: q.data ?? EMPTY_PROJECTS, loading: enabled && q.isPending };
}

// W3.E/S3 — the by-artifact join: is this artifact a session capture, and is
// it the newest one? Same carve-out-prefix, no-own-subscription pattern as
// `useSessionProjects`/`useSessionReplay`.
export function useSessionByArtifact(
  kb: string | null | undefined,
  artifactId: string | null | undefined,
): { data: ByArtifactResponse | null; loading: boolean } {
  const enabled = !!kb && !!artifactId;
  const q = useQuery({
    queryKey: ["sessions", "by-artifact", kb ?? "", artifactId ?? ""] as const,
    enabled,
    queryFn: ({ signal }) =>
      fetchSessionByArtifact(kb as string, artifactId as string, signal),
  });
  return { data: q.data ?? null, loading: enabled && q.isPending };
}

type RecollectSessionOutList = RecollectResponse["sessions"];
const EMPTY_RECOLLECT: RecollectSessionOutList = [];

const EMPTY_OUTLINE: OutlineRow[] = [];

// W3.E/S5 — the outline projection, fetched lazily only when a caller
// actually needs to resolve a `?turn=N` ordinal (ArtifactPane gates
// `enabled` on "a numeric turn param is present"). Same carve-out-prefix,
// no-own-subscription pattern as the other by-sid session queries.
export function useSessionOutline(
  sessionId: string | null | undefined,
  enabled: boolean,
): { outline: OutlineRow[]; loading: boolean } {
  const gated = enabled && !!sessionId;
  const q = useQuery({
    queryKey: ["sessions", "outline", sessionId ?? ""] as const,
    enabled: gated,
    queryFn: ({ signal }) => fetchSessionOutline(sessionId as string, signal),
  });
  return { outline: q.data ?? EMPTY_OUTLINE, loading: gated && q.isPending };
}

const EMPTY_PRESENCE_SET: PresenceLiveSet = presenceLiveSet(undefined);

// W7 (R15/LF-1) — the Tier-1 presence probe: `GET /api/sessions/presence`
// polled 30s, ONLY while `enabled` (the caller scopes this to "the /sessions
// page is mounted" or "an open session artifact page is mounted" — LF-1's
// "from the /sessions page and an open session page ONLY" rule). Query key
// rides the `["sessions"]` prefix on purpose: `useSessions`' own carve-out
// invalidates that whole prefix on `session.captured`, so a fresh capture
// ALSO kicks an early presence refetch for free — no separate SSE wiring
// needed here.
//
// Deliberately NOT `refetchIntervalInBackground` — React Query's default
// pauses `refetchInterval` while the tab/window is unfocused, which is
// exactly LF-1's `document.visibilityState !== "visible"` pause rule; this
// hook doesn't have to re-derive it (same free-lunch the `ambient.tsx`
// #23 precedent documents, minus the explicit background override that
// route deliberately opts INTO).
export function useSessionPresence(enabled: boolean): {
  presenceSet: PresenceLiveSet;
  enabled: boolean;
} {
  const q = useQuery({
    queryKey: ["sessions", "presence"] as const,
    enabled,
    queryFn: ({ signal }) => fetchSessionPresence(signal),
    staleTime: 0,
    // Once the daemon answers disabled (unconfigured dir, or the loopback-only
    // gate 403→EMPTY fallback — the prod posture, D-LF1), stop polling: the
    // answer cannot change without a daemon restart.
    refetchInterval: (query) =>
      query.state.data?.enabled === false ? false : 30_000,
  });
  const data: PresenceResponse | undefined = q.data;
  const presenceSet = useMemo(
    () => (data?.live ? presenceLiveSet(data.live) : EMPTY_PRESENCE_SET),
    [data],
  );
  return { presenceSet, enabled: data?.enabled ?? false };
}

// W3.C/P5 — lane B of the two-lane sessions search (the "deep search"
// affordance): semantic search over the R1 digests, scoped to the current
// project. Debounced by the CALLER (mirrors the existing 250ms metadata-lane
// debounce in sessions.tsx); nested under the carve-out prefix so a fresh
// capture invalidates a stale hit list too. `enabled` is ANDed with the ≥3
// char gate here so a caller doesn't have to duplicate it.
export function useSessionRecollect(
  q: string,
  project: string | undefined,
  enabled: boolean,
): { hits: RecollectSessionOutList; loading: boolean } {
  const gated = enabled && q.trim().length >= 3;
  const query = useQuery({
    queryKey: ["sessions", "recollect", project ?? "", q] as const,
    enabled: gated,
    queryFn: ({ signal }) =>
      fetchSessionRecollect(q, { project, limit: 5 }, signal),
  });
  return {
    hits: query.data?.sessions ?? EMPTY_RECOLLECT,
    loading: gated && query.isPending,
  };
}
