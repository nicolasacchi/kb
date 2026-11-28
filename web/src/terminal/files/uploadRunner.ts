// FXUI — the imperative half of the upload queue: a module-level store per
// terminal (so a rail-tab switch does not drop an upload in flight) that runs
// `uploadQueue.ts` one file at a time through `filesApi.uploadFile`. On success
// the file lands in Recent and the `up` listing is invalidated; on failure the
// row AND a toast say why. A cancel aborts the request of the in-flight file.

import type { QueryClient } from "@tanstack/react-query";
import { useSyncExternalStore } from "react";
import { toast } from "../../lib/toast";
import { displayName } from "./displayName";
import { fileRefusal, uploadFile, uploadMessage, type UploadTransport } from "./filesApi";
import { addRecent } from "./recentStore";
import * as Q from "./uploadQueue";

interface Entry {
  state: Q.QueueState;
  abort: Map<number, AbortController>;
  running: boolean;
}

const entries = new Map<string, Entry>();
const listeners = new Set<() => void>();
let snap = new Map<string, Q.QueueState>();
let transportOverride: UploadTransport | undefined;

const entry = (t: string): Entry => {
  let e = entries.get(t);
  if (!e) {
    e = { state: Q.emptyQueue, abort: new Map(), running: false };
    entries.set(t, e);
  }
  return e;
};

function set(t: string, state: Q.QueueState): void {
  entry(t).state = state;
  snap = new Map(snap).set(t, state);
  for (const l of listeners) l();
}

/** Test seam: swap the transport for every upload. */
export function setUploadTransport(t: UploadTransport | undefined): void {
  transportOverride = t;
}

export function resetUploads(): void {
  for (const e of entries.values()) for (const a of e.abort.values()) a.abort();
  entries.clear();
  snap = new Map();
  for (const l of listeners) l();
}

async function pump(termId: string, qc: QueryClient): Promise<void> {
  const e = entry(termId);
  if (e.running) return;
  e.running = true;
  try {
    for (;;) {
      const item = Q.nextToSend(e.state);
      if (!item) return;
      const ac = new AbortController();
      e.abort.set(item.id, ac);
      set(termId, Q.start(e.state, item.id));
      try {
        const res = await uploadFile(termId, item.file, {
          index: item.index,
          signal: ac.signal,
          transport: transportOverride,
          onProgress: (loaded, total) => set(termId, Q.progress(entry(termId).state, item.id, loaded, total)),
        });
        set(termId, Q.succeed(entry(termId).state, item.id, res));
        addRecent(termId, res);
        void qc.invalidateQueries({ queryKey: ["term", "files", termId, "up"] });
      } catch (err) {
        const cur = entry(termId).state.items.find((i) => i.id === item.id);
        if (cur?.status === "cancelled" || (err instanceof DOMException && err.name === "AbortError")) {
          set(termId, Q.cancel(entry(termId).state, item.id));
        } else {
          const msg = uploadMessage(err);
          set(termId, Q.fail(entry(termId).state, item.id, msg));
          toast.err(`${displayName(item.file.name)}: ${msg}`);
        }
      } finally {
        e.abort.delete(item.id);
      }
    }
  } finally {
    e.running = false;
  }
}

/** Queue files for upload (oversize and empty ones are refused here with a toast, no request). */
export function enqueueFiles(termId: string, files: File[], index: boolean, qc: QueryClient): void {
  const ok: File[] = [];
  for (const f of files) {
    const refusal = fileRefusal(f);
    if (refusal !== null) toast.err(`${displayName(f.name)}: ${refusal}`);
    else ok.push(f);
  }
  if (ok.length === 0) return;
  set(termId, Q.enqueue(entry(termId).state, ok, index));
  void pump(termId, qc);
}

export function cancelUpload(termId: string, id: number): void {
  const e = entry(termId);
  set(termId, Q.cancel(e.state, id));
  e.abort.get(id)?.abort();
}

export function clearUploads(termId: string): void {
  set(termId, Q.clearFinished(entry(termId).state));
}

const subscribe = (l: () => void): (() => void) => {
  listeners.add(l);
  return () => listeners.delete(l);
};

export function useUploadQueue(termId: string): Q.QueueState {
  return useSyncExternalStore(
    subscribe,
    () => snap.get(termId) ?? Q.emptyQueue,
    () => Q.emptyQueue,
  );
}
