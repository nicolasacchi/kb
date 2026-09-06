import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import {
  appendNoteTask,
  createNote,
  deleteNote,
  fetchBacklinks,
  fetchNote,
  fetchNotesAll,
  patchNote,
  toggleNoteTask,
  type BacklinkRef,
  type CreateNoteBody,
  type NoteDetail,
  type NoteSummary,
  type PatchNoteBody,
} from "../api/notes";
import { sse } from "../api/sse";
import { flipTask } from "../lib/flipTask";

// N-track notes. TQ3: the query cache is the shared store — the /notes
// view and every contextual panel read the one ["notes"] entry
// (request-deduped), and the SSE bridge (api/queryClient.ts)
// invalidates it on note.* events and gap resync. The old module-level
// store + subscriber set + HMR-dispose apparatus is gone.

const NOTES_KEY = ["notes"] as const;
const EMPTY: NoteSummary[] = [];

export type UseNotesResult = {
  notes: NoteSummary[];
  loading: boolean;
  error: string | null;
  /// kb → folder → notes, with the scope notepad first within each folder.
  byScope: Map<string, Map<string, NoteSummary[]>>;
  statuses: string[];
  refresh: () => void;
  create: (kb: string, body: CreateNoteBody) => Promise<void>;
};

export function useNotes(): UseNotesResult {
  const queryClient = useQueryClient();
  const q = useQuery({
    queryKey: NOTES_KEY,
    queryFn: ({ signal }) =>
      fetchNotesAll(undefined, signal).then((r) => r.notes),
  });
  const notes = q.data ?? EMPTY;

  const byScope = useMemo(() => {
    const map = new Map<string, Map<string, NoteSummary[]>>();
    for (const n of notes) {
      let folders = map.get(n.kb);
      if (!folders) {
        folders = new Map();
        map.set(n.kb, folders);
      }
      const list = folders.get(n.folder) ?? [];
      list.push(n);
      folders.set(n.folder, list);
    }
    // notepad first, then by recency within each scope.
    for (const folders of map.values()) {
      for (const list of folders.values()) {
        list.sort((a, b) => {
          if (a.is_notepad !== b.is_notepad) return a.is_notepad ? -1 : 1;
          return (b.updated_at ?? 0) - (a.updated_at ?? 0);
        });
      }
    }
    return map;
  }, [notes]);

  const statuses = useMemo(() => {
    const set = new Set<string>();
    for (const n of notes) if (n.status) set.add(n.status);
    return Array.from(set).sort();
  }, [notes]);

  const refresh = useCallback(
    () => void queryClient.invalidateQueries({ queryKey: NOTES_KEY }),
    [queryClient],
  );
  const create = useCallback(
    async (kb: string, body: CreateNoteBody) => {
      await createNote(kb, body);
      // The daemon's note.created event reconciles through the bridge;
      // invalidate immediately anyway so the new note appears without
      // waiting for the watcher's debounce.
      void queryClient.invalidateQueries({ queryKey: NOTES_KEY });
    },
    [queryClient],
  );

  return {
    notes,
    loading: q.isPending,
    error: q.error ? String(q.error) : null,
    byScope,
    statuses,
    refresh,
    create,
  };
}

// Single-note hook — ["note", kb, id] + optimistic mutations against
// the cached detail. The SSE wiring stays IN this hook (not the
// bridge) on purpose: the `dirty` guard suppresses inbound reconciles
// while the editor holds unsaved text, and that's per-editor state a
// global bridge can't know. The bridge therefore never invalidates
// ["note", …] keys.

export type UseNoteResult = {
  note: NoteDetail | null;
  loading: boolean;
  error: string | null;
  setDirty: (dirty: boolean) => void;
  save: (patch: PatchNoteBody) => Promise<void>;
  toggleTask: (index: number, on: boolean) => Promise<void>;
  appendTask: (text: string) => Promise<void>;
  remove: () => Promise<void>;
  refresh: () => void;
};

export function useNote(kb: string, id: string): UseNoteResult {
  const queryClient = useQueryClient();
  const [dirty, setDirty] = useState(false);
  const key = useMemo(() => ["note", kb, id] as const, [kb, id]);
  // In-flight mutation counter. note.updated events can arrive SECONDS
  // after the write that caused them (watcher debounce + reindex), so an
  // event from an EARLIER write can land mid-mutation — its invalidation
  // refetch would return the pre-mutation body and clobber the
  // optimistic patch. Mutations bump this; the SSE guard below skips
  // invalidation while it's non-zero (the mutation's own reconcile +
  // its eventual note.updated bring the truth).
  const mutating = useRef(0);

  const q = useQuery({
    queryKey: key,
    enabled: !!kb && !!id,
    queryFn: ({ signal }) => fetchNote(kb, id, signal),
  });

  useEffect(() => {
    const matches = (data: unknown) => {
      const d = data as { kb?: string; id?: string } | null;
      return d?.kb === kb && d?.id === id;
    };
    const offs = ["note.updated", "note.deleted"].map((kind) =>
      sse.subscribeEvent(kind, (evt) => {
        // Don't refetch while the editor is dirty (it would clobber the
        // in-progress edit) or while a mutation is in flight (see
        // `mutating` above).
        if (matches(evt) && !dirty && mutating.current === 0) {
          void queryClient.invalidateQueries({ queryKey: key });
        }
      }),
    );
    return () => {
      for (const off of offs) off();
    };
  }, [queryClient, key, kb, id, dirty]);

  const setNoteData = useCallback(
    (fn: (prev: NoteDetail) => NoteDetail) => {
      queryClient.setQueryData<NoteDetail>(key, (prev) =>
        prev ? fn(prev) : prev,
      );
    },
    [queryClient, key],
  );

  const save = useCallback(
    async (patch: PatchNoteBody) => {
      mutating.current += 1;
      try {
        const next = await patchNote(kb, id, patch);
        queryClient.setQueryData<NoteDetail>(key, next);
      } finally {
        mutating.current -= 1;
      }
    },
    [queryClient, key, kb, id],
  );

  const toggleTask = useCallback(
    async (index: number, on: boolean) => {
      mutating.current += 1;
      try {
        // Cancel any in-flight refetch — a stale response landing after
        // the optimistic patch would un-flip the checkbox. NOT awaited:
        // the abort itself is synchronous, and the optimistic flip must
        // apply in the SAME tick as the click (React restores a
        // controlled checkbox to its prop value right after the handler;
        // a deferred patch leaves a visible un-flip).
        void queryClient.cancelQueries({ queryKey: key });
        // Optimistic: flip locally, then reconcile from the server
        // response.
        setNoteData((prev) => ({
          ...prev,
          body_md: flipTask(prev.body_md, index, on),
        }));
        const r = await toggleNoteTask(kb, id, index, on);
        setNoteData((prev) => ({
          ...prev,
          body_md: r.body_md,
          task_done: r.task_done,
          task_total: r.task_total,
        }));
      } finally {
        mutating.current -= 1;
      }
    },
    [queryClient, key, setNoteData, kb, id],
  );

  const appendTask = useCallback(
    async (text: string) => {
      mutating.current += 1;
      try {
        const r = await appendNoteTask(kb, id, text);
        setNoteData((prev) => ({
          ...prev,
          body_md: r.body_md,
          task_done: r.task_done,
          task_total: r.task_total,
        }));
      } finally {
        mutating.current -= 1;
      }
    },
    [setNoteData, kb, id],
  );

  const remove = useCallback(async () => {
    await deleteNote(kb, id);
    queryClient.removeQueries({ queryKey: key });
  }, [queryClient, key, kb, id]);

  const refresh = useCallback(
    () => void queryClient.invalidateQueries({ queryKey: key }),
    [queryClient, key],
  );

  return {
    note: q.data ?? null,
    loading: q.isPending,
    error: q.error ? String(q.error) : null,
    setDirty,
    save,
    toggleTask,
    appendTask,
    remove,
    refresh,
  };
}

// Scope-derived view over the shared notes query — the contextual panel
// reads this so it shares the single cross-kb fetch.

export type UseScopeNotesResult = {
  notepad: NoteSummary | null;
  adhoc: NoteSummary[];
  loading: boolean;
};

// Backlinks ("Linked from" / "Referenced in notes") for any artifact id.
// Own SSE wiring (like useNote): a note.* anywhere can add/remove an edge to
// this artifact, so invalidate on those. Keyed ["backlinks", kb, id]; the
// query is cheap (one reverse-edge scan + a few get_by_id).

export function useBacklinks(
  kb: string | null,
  id: string | null,
): { backlinks: BacklinkRef[]; loading: boolean } {
  const queryClient = useQueryClient();
  const key = useMemo(() => ["backlinks", kb, id] as const, [kb, id]);
  const q = useQuery({
    queryKey: key,
    enabled: !!kb && !!id,
    queryFn: ({ signal }) =>
      fetchBacklinks(kb as string, id as string, signal).then((r) => r.backlinks),
  });
  useEffect(() => {
    if (!kb || !id) return;
    // Also listen to `artifact.indexed`: the wikilink EDGE that produces a
    // backlink is written by the indexer's edge-record hook AFTER the
    // note.created/updated that triggered it (and a NON-note artifact can link
    // here too, firing no note.* event). artifact.indexed fires once the
    // reindex — and the edge — has committed, so it's the reliable refresh.
    const offs = ["note.created", "note.updated", "note.deleted", "artifact.indexed"].map(
      (kind) =>
        sse.subscribeEvent(kind, () => {
          void queryClient.invalidateQueries({ queryKey: key });
        }),
    );
    return () => {
      for (const off of offs) off();
    };
  }, [queryClient, key, kb, id]);
  return { backlinks: q.data ?? EMPTY_BACKLINKS, loading: q.isPending };
}

const EMPTY_BACKLINKS: BacklinkRef[] = [];

export function useScopeNotes(
  kb: string | null,
  folder: string | null,
): UseScopeNotesResult {
  const { notes, loading } = useNotes();
  const scope = folder ?? "";
  return useMemo(() => {
    if (!kb) return { notepad: null, adhoc: [], loading };
    const inScope = notes.filter((n) => n.kb === kb && n.folder === scope);
    const notepad = inScope.find((n) => n.is_notepad) ?? null;
    const adhoc = inScope
      .filter((n) => !n.is_notepad)
      .sort((a, b) => (b.updated_at ?? 0) - (a.updated_at ?? 0));
    return { notepad, adhoc, loading };
  }, [notes, kb, scope, loading]);
}
