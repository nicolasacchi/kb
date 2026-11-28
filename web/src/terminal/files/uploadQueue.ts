// FXUI — the pure upload-queue state machine. Files go ONE AT A TIME, in the
// order they were added. No I/O here: `uploadRunner.ts` drives it.

import type { FileUploadAnswer } from "../../api/generated/FileUploadAnswer";

export type UploadStatus = "queued" | "uploading" | "done" | "error" | "cancelled";

export interface QueueItem {
  id: number;
  file: File;
  /** Whether "Index content in kb" was ticked when this file was added. */
  index: boolean;
  status: UploadStatus;
  loaded: number;
  total: number;
  error: string | null;
  result: FileUploadAnswer | null;
  /** True when the user pressed Cancel but the request had already finished: the file IS stored. */
  late: boolean;
}

export interface QueueState {
  items: QueueItem[];
  nextId: number;
}

export const emptyQueue: QueueState = { items: [], nextId: 1 };

export function enqueue(s: QueueState, files: File[], index: boolean): QueueState {
  let id = s.nextId;
  const added: QueueItem[] = files.map((file) => ({
    id: id++,
    file,
    index,
    status: "queued",
    loaded: 0,
    total: file.size,
    error: null,
    result: null,
    late: false,
  }));
  return { items: [...s.items, ...added], nextId: id };
}

const patch = (s: QueueState, id: number, f: (i: QueueItem) => QueueItem): QueueState => ({
  ...s,
  items: s.items.map((i) => (i.id === id ? f(i) : i)),
});

/** The item to send next: none while one is uploading, else the first queued. */
export function nextToSend(s: QueueState): QueueItem | null {
  if (s.items.some((i) => i.status === "uploading")) return null;
  return s.items.find((i) => i.status === "queued") ?? null;
}

export const start = (s: QueueState, id: number): QueueState =>
  patch(s, id, (i) => (i.status === "queued" ? { ...i, status: "uploading", loaded: 0 } : i));

export const progress = (s: QueueState, id: number, loaded: number, total: number): QueueState =>
  patch(s, id, (i) => (i.status === "uploading" ? { ...i, loaded, total: total > 0 ? total : i.total } : i));

export const succeed = (s: QueueState, id: number, result: FileUploadAnswer): QueueState =>
  patch(s, id, (i) => (i.status === "uploading" ? { ...i, status: "done", loaded: i.total, result } : i));

/**
 * A request that finished AFTER Cancel: the file exists on the runner, so the
 * row becomes `done` (flagged `late`) instead of saying "cancelled". Only a
 * cancelled row can land; any other status is untouched.
 */
export const landed = (s: QueueState, id: number, result: FileUploadAnswer): QueueState =>
  patch(s, id, (i) => (i.status === "cancelled" ? { ...i, status: "done", loaded: i.total, result, error: null, late: true } : i));

/** True when every byte had been sent before an abort: the server may still have stored it. */
export const fullySent = (i: QueueItem): boolean => i.total > 0 && i.loaded >= i.total;

/** An error never stalls the queue: the next queued file is still sent. */
export const fail = (s: QueueState, id: number, error: string): QueueState =>
  patch(s, id, (i) => (i.status === "uploading" || i.status === "queued" ? { ...i, status: "error", error } : i));

/** Cancel a queued or in-flight item (the runner aborts the request for the latter). */
export const cancel = (s: QueueState, id: number): QueueState =>
  patch(s, id, (i) =>
    i.status === "queued" || i.status === "uploading" ? { ...i, status: "cancelled", error: null } : i,
  );

/** Drop every finished row (done, error, cancelled); queued and in-flight stay. */
export const clearFinished = (s: QueueState): QueueState => ({
  ...s,
  items: s.items.filter((i) => i.status === "queued" || i.status === "uploading"),
});
