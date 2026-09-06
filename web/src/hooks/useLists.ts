import { useCallback, useMemo } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import {
  addListEntry,
  createList,
  deleteList,
  fetchList,
  fetchLists,
  patchList,
  patchListEntry,
  removeListEntry,
  type ListDetailResponse,
  type ListEntry,
  type ListEntryCreateBody,
  type ListPatchBody,
  type ListSummary,
} from "../api/client";

// RL-track (v0.18) — reading-list hooks, invariant #23 shaped: server
// state rides the query cache (["lists"] index, ["list", kb, id] detail);
// the SSE bridge owns ALL invalidation, so these hooks carry zero
// subscribe plumbing.
//
// Mutation strategy:
//   reorder + read-override   optimistic with rollback (the useAnchors
//                             pattern) — latency-sensitive, idempotent;
//                             an invalidation round-trip makes a drag or
//                             a checkbox feel broken.
//   everything else           write-through-on-response — the daemon
//                             mints ids and resolves titles/minutes, so
//                             we splice its response into the cache and
//                             let the list.* SSE event reconcile.

const INDEX_KEY = ["lists"] as const;
const EMPTY: ListSummary[] = [];

export type UseListsResult = {
  lists: ListSummary[];
  pinned: ListSummary[];
  active: ListSummary[];
  archived: ListSummary[];
  loading: boolean;
  error: string | null;
  create: (kb: string, title: string, description?: string) => Promise<ListSummary>;
  setPinned: (l: ListSummary, pinned: boolean) => Promise<void>;
  setArchived: (l: ListSummary, archived: boolean) => Promise<void>;
  remove: (l: ListSummary) => Promise<void>;
  refresh: () => void;
};

export function useLists(): UseListsResult {
  const queryClient = useQueryClient();
  const q = useQuery({
    queryKey: INDEX_KEY,
    queryFn: ({ signal }) => fetchLists(signal).then((r) => r.lists),
  });
  const lists = q.data ?? EMPTY;

  const { pinned, active, archived } = useMemo(() => {
    const pinned: ListSummary[] = [];
    const active: ListSummary[] = [];
    const archived: ListSummary[] = [];
    for (const l of lists) {
      if (l.archived) archived.push(l);
      else if (l.pinned) pinned.push(l);
      else active.push(l);
    }
    return { pinned, active, archived };
  }, [lists]);

  const invalidateIndex = useCallback(
    () => void queryClient.invalidateQueries({ queryKey: INDEX_KEY }),
    [queryClient],
  );

  const create = useCallback(
    async (kb: string, title: string, description?: string) => {
      // `pinned` is a plain serde-default bool daemon-side — omitted
      // rather than null (null fails bool deserialization).
      const created = await createList(kb, {
        title,
        description: description || null,
      });
      queryClient.setQueryData<ListSummary[]>(INDEX_KEY, (prev = EMPTY) => [
        created,
        ...prev,
      ]);
      return created;
    },
    [queryClient],
  );

  const patchHeader = useCallback(
    async (l: ListSummary, body: ListPatchBody) => {
      const updated = await patchList(l.kb, l.id, body);
      queryClient.setQueryData<ListSummary[]>(INDEX_KEY, (prev = EMPTY) =>
        prev.map((x) => (x.kb === l.kb && x.id === l.id ? updated : x)),
      );
    },
    [queryClient],
  );

  const setPinned = useCallback(
    (l: ListSummary, pinned: boolean) => patchHeader(l, { pinned }),
    [patchHeader],
  );
  const setArchived = useCallback(
    (l: ListSummary, archived: boolean) => patchHeader(l, { archived }),
    [patchHeader],
  );

  const remove = useCallback(
    async (l: ListSummary) => {
      await deleteList(l.kb, l.id);
      queryClient.setQueryData<ListSummary[]>(INDEX_KEY, (prev = EMPTY) =>
        prev.filter((x) => !(x.kb === l.kb && x.id === l.id)),
      );
    },
    [queryClient],
  );

  return {
    lists,
    pinned,
    active,
    archived,
    loading: q.isPending,
    error: q.error ? String(q.error) : null,
    create,
    setPinned,
    setArchived,
    remove,
    refresh: invalidateIndex,
  };
}

export type UseListDetailResult = {
  list: ListSummary | null;
  entries: ListEntry[];
  loading: boolean;
  error: string | null;
  addEntry: (body: ListEntryCreateBody) => Promise<ListEntry>;
  updateNote: (eid: string, note: string | null) => Promise<void>;
  setReadOverride: (
    eid: string,
    override: "read" | "unread" | null,
  ) => Promise<void>;
  /// Move the entry one slot up/down (the ↑/↓ buttons + Alt+arrows).
  moveBy: (eid: string, delta: -1 | 1) => Promise<void>;
  /// Drop the entry before/after a sibling (drag-and-drop).
  moveBefore: (eid: string, siblingId: string) => Promise<void>;
  removeEntry: (eid: string) => Promise<void>;
  patchHeader: (body: ListPatchBody) => Promise<void>;
  removeList: () => Promise<void>;
};

export function useListDetail(
  kb: string | undefined,
  id: string | undefined,
): UseListDetailResult {
  const queryClient = useQueryClient();
  const key = useMemo(() => ["list", kb, id] as const, [kb, id]);
  const q = useQuery({
    queryKey: key,
    enabled: !!kb && !!id,
    queryFn: ({ signal }) => fetchList(kb as string, id as string, signal),
  });

  const patchDetail = useCallback(
    (fn: (d: ListDetailResponse) => ListDetailResponse) =>
      queryClient.setQueryData<ListDetailResponse>(key, (d) =>
        d ? fn(d) : d,
      ),
    [queryClient, key],
  );
  const spliceEntry = useCallback(
    (entry: ListEntry) =>
      patchDetail((d) => ({
        ...d,
        entries: d.entries.map((e) => (e.id === entry.id ? entry : e)),
      })),
    [patchDetail],
  );
  const invalidateBoth = useCallback(() => {
    void queryClient.invalidateQueries({ queryKey: key });
    void queryClient.invalidateQueries({ queryKey: INDEX_KEY });
  }, [queryClient, key]);

  const addEntry = useCallback(
    async (body: ListEntryCreateBody) => {
      if (!kb || !id) throw new Error("no list");
      const entry = await addListEntry(kb, id, body);
      patchDetail((d) => {
        const entries = [...d.entries];
        const at = Math.min(entry.position, entries.length);
        entries.splice(at, 0, entry);
        return {
          ...d,
          entries: entries.map((e, i) => ({ ...e, position: i })),
        };
      });
      invalidateBoth();
      return entry;
    },
    [kb, id, patchDetail, invalidateBoth],
  );

  const updateNote = useCallback(
    async (eid: string, note: string | null) => {
      if (!kb || !id) return;
      const entry = await patchListEntry(kb, id, eid, { note });
      spliceEntry(entry);
    },
    [kb, id, spliceEntry],
  );

  const setReadOverride = useCallback(
    async (eid: string, override: "read" | "unread" | null) => {
      if (!kb || !id) return;
      // Optimistic: the dot flips inside the click's React batch. On
      // "clear" we can't re-derive locally — keep the current state and
      // let the response/SSE reconcile.
      const before = queryClient.getQueryData<ListDetailResponse>(key);
      patchDetail((d) => ({
        ...d,
        entries: d.entries.map((e) =>
          e.id === eid
            ? {
                ...e,
                read_override: override ?? undefined,
                read_state: override ?? e.read_state,
              }
            : e,
        ),
      }));
      try {
        const entry = await patchListEntry(kb, id, eid, {
          read_override: override ?? "clear",
        });
        spliceEntry(entry);
        void queryClient.invalidateQueries({ queryKey: INDEX_KEY });
      } catch (e) {
        queryClient.setQueryData(key, before);
        console.warn("[lists] read-override failed", e);
      }
    },
    [kb, id, key, queryClient, patchDetail, spliceEntry],
  );

  const moveTo = useCallback(
    async (eid: string, body: { before?: string; after?: string }) => {
      if (!kb || !id) return;
      const before = queryClient.getQueryData<ListDetailResponse>(key);
      // Optimistic local reorder (dense renumber mirrors the daemon's).
      patchDetail((d) => {
        const entries = d.entries.filter((e) => e.id !== eid);
        const moving = d.entries.find((e) => e.id === eid);
        if (!moving) return d;
        const anchorId = body.before ?? body.after;
        let at = entries.findIndex((e) => e.id === anchorId);
        if (at < 0) return d;
        if (body.after) at += 1;
        entries.splice(at, 0, moving);
        return {
          ...d,
          entries: entries.map((e, i) => ({ ...e, position: i })),
        };
      });
      try {
        await patchListEntry(kb, id, eid, body);
      } catch (e) {
        queryClient.setQueryData(key, before);
        console.warn("[lists] move failed", e);
      }
    },
    [kb, id, key, queryClient, patchDetail],
  );

  const moveBy = useCallback(
    async (eid: string, delta: -1 | 1) => {
      const entries = q.data?.entries ?? [];
      const idx = entries.findIndex((e) => e.id === eid);
      const target = entries[idx + delta];
      if (idx < 0 || !target) return;
      await moveTo(
        eid,
        delta === -1 ? { before: target.id } : { after: target.id },
      );
    },
    [q.data, moveTo],
  );

  const moveBefore = useCallback(
    (eid: string, siblingId: string) => moveTo(eid, { before: siblingId }),
    [moveTo],
  );

  const removeEntry = useCallback(
    async (eid: string) => {
      if (!kb || !id) return;
      const before = queryClient.getQueryData<ListDetailResponse>(key);
      patchDetail((d) => ({
        ...d,
        entries: d.entries
          .filter((e) => e.id !== eid)
          .map((e, i) => ({ ...e, position: i })),
      }));
      try {
        await removeListEntry(kb, id, eid);
        void queryClient.invalidateQueries({ queryKey: INDEX_KEY });
      } catch (e) {
        queryClient.setQueryData(key, before);
        console.warn("[lists] remove entry failed", e);
      }
    },
    [kb, id, key, queryClient, patchDetail],
  );

  const patchHeader = useCallback(
    async (body: ListPatchBody) => {
      if (!kb || !id) return;
      const updated = await patchList(kb, id, body);
      patchDetail((d) => ({ ...d, list: updated }));
      void queryClient.invalidateQueries({ queryKey: INDEX_KEY });
    },
    [kb, id, queryClient, patchDetail],
  );

  const removeList = useCallback(async () => {
    if (!kb || !id) return;
    await deleteList(kb, id);
    void queryClient.invalidateQueries({ queryKey: INDEX_KEY });
  }, [kb, id, queryClient]);

  return {
    list: q.data?.list ?? null,
    entries: q.data?.entries ?? [],
    loading: q.isPending,
    error: q.error ? String(q.error) : null,
    addEntry,
    updateNote,
    setReadOverride,
    moveBy,
    moveBefore,
    removeEntry,
    patchHeader,
    removeList,
  };
}
