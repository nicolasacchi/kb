// OL PRES3 - the presence poll, and the one hook every presence surface reads.
//
// `GET /api/kb/presence` is a relay of kb's live-touches. kb has no event that
// reaches this daemon, so the query is a #23-style polling exception
// (web-code/CLAUDE.md, "Presence in the tree"): a finite `staleTime`, a 10 s
// `refetchInterval`, no background polling, and it runs only while the pref is
// on AND the document is visible. The key is `["presence", repo]`, so the
// generic per-repo bridge in `api/queryClient.ts` (`queryKey[1] === repo`)
// already refetches it on `kbc:mirror.updated` - that is how attribution
// catches up within a beat of the touch instead of waiting for the interval.
// Home's RepoCard, the Spine chip and the tree all read the SAME key: one
// poll per repo, however many surfaces show it.

import { useEffect, useState, useSyncExternalStore } from "react";
import { useQuery } from "@tanstack/react-query";
import { fetchPresence } from "../api/client";
import {
  attribute,
  buildMarks,
  rollup,
  summarize,
  worktreeCounts,
  type DirRollup,
  type PresenceMark,
  type PresenceSnapshot,
  type PresenceSummary,
  type WorktreeCount,
} from "../lib/presence";
import {
  burstFor,
  openedAtFor,
  presenceVersion,
  subscribePresence,
  touchesFor,
} from "../lib/presenceStore";
import { loadPresenceOverlay, PRESENCE_PREF_EVENT } from "../lib/prefs";
import { usePrefersReducedMotion } from "./usePrefersReducedMotion";

export const PRESENCE_POLL_MS = 10_000;
/// Re-render cadence for fade levels, only while a mark is showing.
export const PRESENCE_TICK_MS = 5_000;

export function presenceKey(repo: string) {
  return ["presence", repo] as const;
}

/// Exported so a test pins the polling contract without mounting a tree.
export function presenceQueryOptions(repo: string, enabled: boolean) {
  return {
    queryKey: presenceKey(repo),
    queryFn: async ({ signal }: { signal?: AbortSignal }): Promise<PresenceSnapshot> => {
      const wire = await fetchPresence(repo, signal);
      return { ...wire, fetchedPerf: performance.now() };
    },
    staleTime: PRESENCE_POLL_MS,
    refetchInterval: PRESENCE_POLL_MS,
    enabled,
  } as const;
}

export function presenceEnabled(o: { pref: boolean; visible: boolean; repo: string | undefined }) {
  return o.pref && o.visible && !!o.repo;
}

export function useDocumentVisible(): boolean {
  const [visible, setVisible] = useState(
    () => typeof document === "undefined" || document.visibilityState !== "hidden",
  );
  useEffect(() => {
    const on = () => setVisible(document.visibilityState !== "hidden");
    document.addEventListener("visibilitychange", on);
    return () => document.removeEventListener("visibilitychange", on);
  }, []);
  return visible;
}

/// The pref, kept in step across the tree toggle, Home and the Spine chip.
export function usePresencePref(): boolean {
  const [on, setOn] = useState(() => loadPresenceOverlay());
  useEffect(() => {
    const sync = () => setOn(loadPresenceOverlay());
    window.addEventListener(PRESENCE_PREF_EVENT, sync);
    return () => window.removeEventListener(PRESENCE_PREF_EVENT, sync);
  }, []);
  return on;
}

/// `usePresence(repo, enabled)`: the query alone.
export function usePresence(repo: string | undefined, enabled: boolean) {
  const visible = useDocumentVisible();
  return useQuery(
    presenceQueryOptions(repo ?? "", presenceEnabled({ pref: enabled, visible, repo })),
  );
}

/// Re-renders its caller every `ms` while `active`. The caller reads
/// `performance.now()` itself at render; this only schedules the render.
export function usePerfTick(active: boolean, ms: number): void {
  const [, setN] = useState(0);
  useEffect(() => {
    if (!active) return undefined;
    const id = window.setInterval(() => setN((n) => n + 1), ms);
    return () => window.clearInterval(id);
  }, [active, ms]);
}

export interface PresenceView {
  pref: boolean;
  reduced: boolean;
  available: boolean;
  caption: string | undefined;
  marks: Map<string, PresenceMark>;
  dirs: Map<string, DirRollup>;
  worktrees: WorktreeCount[];
  burst: { count: number; at: number } | undefined;
  summary: PresenceSummary | null;
}

/// Everything the tree needs: marks (mirror touches x attribution), the
/// collapsed-dir roll-up, worktree counts and the pulse summary.
export function usePresenceView(repo: string | undefined): PresenceView {
  const pref = usePresencePref();
  const reduced = usePrefersReducedMotion();
  const q = usePresence(repo, pref);
  const snap = q.data;
  useSyncExternalStore(subscribePresence, presenceVersion, presenceVersion);
  const touches = repo && pref ? touchesFor(repo) : [];
  const tickOn = pref && !reduced && (touches.length > 0 || (snap?.files.length ?? 0) > 0);
  usePerfTick(tickOn, PRESENCE_TICK_MS);
  // Re-derive on every render: the inputs (store version, snapshot, tick) all
  // re-render this hook, and the derivation is a few hundred rows at most.
  const nowPerf = performance.now();
  const attributed = attribute(touches, snap);
  const marks = repo && pref ? buildMarks(attributed, openedAtFor(repo), nowPerf) : new Map();
  return {
    pref,
    reduced,
    available: snap?.available !== false,
    caption: snap && snap.available === false ? snap.caption : undefined,
    marks,
    dirs: rollup(marks),
    worktrees: pref ? worktreeCounts(snap, nowPerf) : [],
    burst: repo && pref ? burstFor(repo) : undefined,
    summary: pref ? summarize(snap, nowPerf) : null,
  };
}

/// Home's RepoCard line and the Spine chip: the pulse only, read from the
/// SAME `["presence", repo]` query (no second poll).
export function usePresenceSummary(repo: string | undefined): PresenceSummary | null {
  const pref = usePresencePref();
  const reduced = usePrefersReducedMotion();
  const q = usePresence(repo, pref);
  const snap = q.data;
  usePerfTick(pref && !reduced && (snap?.files.length ?? 0) > 0, PRESENCE_TICK_MS);
  return pref ? summarize(snap, performance.now()) : null;
}
