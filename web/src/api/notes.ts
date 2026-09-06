// N-track — notes / todo-lists API client. Mirrors the server's
// `routes/notes.rs` shapes. A note is an ordinary Markdown artifact
// (`kb-category=note`) so it has a normal id + permalink; these endpoints
// add body editing (toggle a checkbox, rewrite text). Local get/mutate
// helpers mirror `api/client.ts` (problem+json aware) — the same
// self-contained style `api/sessions.ts` / `api/versions.ts` use.

import { currentDaemonBase } from "./base";
import { isAbortError } from "./client";
// Wire types are the generated ts-rs bindings (routes/notes.rs is the
// source of truth; `just types` regenerates).
import type { NoteSummary as NoteSummaryWire } from "./generated/NoteSummary";
import type { NoteDetail } from "./generated/NoteDetail";
import type { CreateNoteBody } from "./generated/CreateNoteBody";
import type { CreateNoteResponse } from "./generated/CreateNoteResponse";
import type { PatchNoteBody } from "./generated/PatchNoteBody";
import type { ToggleNoteTaskBody } from "./generated/ToggleNoteTaskBody";
import type { AppendNoteTaskBody } from "./generated/AppendNoteTaskBody";
import type { NoteMutate } from "./generated/NoteMutate";
import type { ResolvedLink } from "./generated/ResolvedLink";
import type { BacklinkRef } from "./generated/BacklinkRef";
import type { BacklinksResponse } from "./generated/BacklinksResponse";
import type { NoteLinks } from "./generated/NoteLinks";
import type { WikilinkSuggestion } from "./generated/WikilinkSuggestion";
import type { SuggestResponse } from "./generated/SuggestResponse";

export { isAbortError };
export type {
  NoteDetail,
  CreateNoteBody,
  CreateNoteResponse,
  PatchNoteBody,
  NoteMutate,
  ResolvedLink,
  BacklinkRef,
  BacklinksResponse,
  NoteLinks,
  WikilinkSuggestion,
  SuggestResponse,
};

/// Deliberately NARROWER than the wire type: `kb` is absent only on the
/// per-kb route's rows; the SPA exclusively consumes the cross-kb
/// `/api/notes` (fetchNotesAll), whose fan-out always sets it.
/// (fetchKbNotes callers would have to widen — there are none today.)
export type NoteSummary = Omit<NoteSummaryWire, "kb"> & { kb: string };

export type NotesListResponse = { notes: NoteSummary[] };

type Problem = { title: string; detail?: string };

async function getJson<T>(path: string, signal?: AbortSignal): Promise<T> {
  const r = await fetch(`${currentDaemonBase()}${path}`, {
    headers: { Accept: "application/json" },
    signal,
  });
  if (!r.ok) throw await problem(r);
  return (await r.json()) as T;
}

async function mutateJson<T>(
  path: string,
  method: "POST" | "PATCH" | "DELETE",
  body?: unknown,
): Promise<T> {
  const headers: Record<string, string> = {
    Accept: "application/json",
    "X-Requested-By": "kb-spa",
  };
  if (body !== undefined) headers["Content-Type"] = "application/json";
  const r = await fetch(`${currentDaemonBase()}${path}`, {
    method,
    headers,
    body: body !== undefined ? JSON.stringify(body) : undefined,
  });
  if (!r.ok) throw await problem(r);
  // DELETE returns 204 with no body.
  if (r.status === 204) return undefined as T;
  return (await r.json()) as T;
}

async function problem(r: Response): Promise<Error> {
  const ct = r.headers.get("content-type") ?? "";
  if (ct.includes("application/problem+json")) {
    const p = (await r.json()) as Problem;
    return new Error(`${p.title}: ${p.detail ?? ""}`);
  }
  return new Error(`${r.status} ${r.statusText}`);
}

function listQuery(opts?: { folder?: string; status?: string }): string {
  const p = new URLSearchParams();
  if (opts?.folder) p.set("folder", opts.folder);
  if (opts?.status) p.set("status", opts.status);
  const qs = p.toString();
  return qs ? `?${qs}` : "";
}

export const fetchNotesAll = (
  opts?: { folder?: string; status?: string },
  signal?: AbortSignal,
) => getJson<NotesListResponse>(`/api/notes${listQuery(opts)}`, signal);

export const fetchKbNotes = (
  kb: string,
  opts?: { folder?: string; status?: string },
  signal?: AbortSignal,
) =>
  getJson<NotesListResponse>(
    `/api/kb/${encodeURIComponent(kb)}/notes${listQuery(opts)}`,
    signal,
  );

export const fetchNote = (kb: string, id: string, signal?: AbortSignal) =>
  getJson<NoteDetail>(
    `/api/kb/${encodeURIComponent(kb)}/notes/${encodeURIComponent(id)}`,
    signal,
  );

export const createNote = (kb: string, body: CreateNoteBody) =>
  mutateJson<CreateNoteResponse>(
    `/api/kb/${encodeURIComponent(kb)}/notes`,
    "POST",
    body,
  );

export const patchNote = (kb: string, id: string, patch: PatchNoteBody) =>
  mutateJson<NoteDetail>(
    `/api/kb/${encodeURIComponent(kb)}/notes/${encodeURIComponent(id)}`,
    "PATCH",
    patch,
  );

export const toggleNoteTask = (
  kb: string,
  id: string,
  index: number,
  on: boolean,
) =>
  mutateJson<NoteMutate>(
    `/api/kb/${encodeURIComponent(kb)}/notes/${encodeURIComponent(id)}/toggle`,
    "POST",
    { index, on } satisfies ToggleNoteTaskBody,
  );

export const appendNoteTask = (kb: string, id: string, text: string) =>
  mutateJson<NoteMutate>(
    `/api/kb/${encodeURIComponent(kb)}/notes/${encodeURIComponent(id)}/tasks`,
    "POST",
    { text } satisfies AppendNoteTaskBody,
  );

export const deleteNote = (kb: string, id: string) =>
  mutateJson<void>(
    `/api/kb/${encodeURIComponent(kb)}/notes/${encodeURIComponent(id)}`,
    "DELETE",
  );

// --- wikilinks / backlinks (notes as connective tissue) ---------------------

/// Inbound references to any artifact ("Linked from" / "Referenced in notes").
export const fetchBacklinks = (kb: string, id: string, signal?: AbortSignal) =>
  getJson<BacklinksResponse>(
    `/api/kb/${encodeURIComponent(kb)}/backlinks/${encodeURIComponent(id)}`,
    signal,
  );

/// A note's outgoing wikilinks + backlinks in one call (drives `kb notes
/// links`; the SPA reads outgoing from NoteDetail.links + backlinks here).
export const fetchNoteLinks = (kb: string, id: string, signal?: AbortSignal) =>
  getJson<NoteLinks>(
    `/api/kb/${encodeURIComponent(kb)}/notes/${encodeURIComponent(id)}/links`,
    signal,
  );

/// `[[` autocomplete candidates — title/basename matches across the corpus.
export const fetchWikilinkSuggest = (
  kb: string,
  q: string,
  limit?: number,
  signal?: AbortSignal,
) => {
  const p = new URLSearchParams({ q });
  if (limit) p.set("limit", String(limit));
  return getJson<SuggestResponse>(
    `/api/kb/${encodeURIComponent(kb)}/wikilinks/suggest?${p.toString()}`,
    signal,
  );
};
