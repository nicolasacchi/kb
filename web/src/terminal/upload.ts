// FUTERMPASTE — the ONE home of picture upload (invariant #30): the type and
// size gate, `POST …/upload`, and reading picture files out of a paste or a
// drop. The phone Composer and the desktop panel's paste/drop hook both use
// it; neither forks a check. The caller PASTES the returned path (never Enter).

import { TERMINAL_INTENT_HEADER, TERMINAL_INTENT_VALUE } from "../api/terms";
import { DriveError, driveError, url } from "./phone/driveApi";

/** The largest picture the relay accepts (the server holds the same number). */
export const UPLOAD_MAX_BYTES = 8 * 1024 * 1024;

/** The picture types the relay accepts, as the request `Content-Type`. */
export const UPLOAD_TYPES = ["image/png", "image/jpeg", "image/gif", "image/webp"] as const;

/** The server's stored-picture reply (`kb-server`'s `UploadData`; hand-written, not generated). */
export interface UploadResult {
  path: string;
  bytes: number;
  mime: string;
  sha256: string;
}

/** Why a file cannot be uploaded, or null when it can (checked before any request). */
export function uploadRefusal(file: { size: number; type: string }): string | null {
  if (!(UPLOAD_TYPES as readonly string[]).includes(file.type)) {
    return "Only PNG, JPEG, GIF and WebP pictures can be sent.";
  }
  if (file.size > UPLOAD_MAX_BYTES) {
    return "That picture is over the 8 MiB limit.";
  }
  return null;
}

/**
 * `POST …/upload` with the raw file as the body. The runner stores it and
 * answers an absolute path; the caller PASTES the path (never Enter). Rejects
 * a wrong type or an oversize file before fetching.
 */
export async function uploadImage(termId: string, file: File, signal?: AbortSignal): Promise<UploadResult> {
  const refusal = uploadRefusal(file);
  if (refusal !== null) throw new DriveError(0, null, refusal);
  const res = await fetch(url(termId, "upload"), {
    method: "POST",
    headers: {
      Accept: "application/json",
      "Content-Type": file.type,
      [TERMINAL_INTENT_HEADER]: TERMINAL_INTENT_VALUE,
    },
    body: file,
    signal,
  });
  if (!res.ok) throw await driveError(res);
  return (await res.json()) as UploadResult;
}

/**
 * The picture files of a paste, or `[]` when the paste is not a picture paste.
 * A clipboard that carries `text/plain` is a TEXT paste even when it also
 * carries an image (a spreadsheet cell, a web selection): it is left alone.
 */
export function pastedPictureFiles(dt: DataTransfer | null): File[] {
  if (dt === null) return [];
  if (Array.from(dt.types ?? []).includes("text/plain")) return [];
  const out: File[] = [];
  for (const item of Array.from(dt.items ?? [])) {
    if (item.kind !== "file" || !item.type.startsWith("image/")) continue;
    const f = item.getAsFile();
    if (f !== null) out.push(f);
  }
  if (out.length > 0) return out;
  // Some browsers fill `files` but not `items`.
  return Array.from(dt.files ?? []).filter((f) => f.type.startsWith("image/"));
}

/** Every file of a drop (the gate then refuses the non-pictures with a message). */
export function droppedFiles(dt: DataTransfer | null): File[] {
  return dt === null ? [] : Array.from(dt.files ?? []);
}

/** Whether a drag carries files (a text drag is left to xterm). */
export function dragHasFiles(dt: DataTransfer | null): boolean {
  return dt !== null && Array.from(dt.types ?? []).includes("Files");
}
