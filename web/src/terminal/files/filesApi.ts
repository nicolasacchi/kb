// FXUI — the Files tab's HTTP client: `POST …/files` (upload), `GET …/ls`
// (browse) and `GET …/file` (download), plus the REST paste. Same-origin, the
// terminal intent header on everything, no token. Uploads and downloads are
// imperative; the listing read is wrapped by `useFilesListing` (TanStack).
// Names and server messages are data: nothing here builds HTML.

import type { FileUploadAnswer } from "../../api/generated/FileUploadAnswer";
import type { TermLsAnswer } from "../../api/generated/TermLsAnswer";
import { TERMINAL_INTENT_HEADER, TERMINAL_INTENT_VALUE } from "../../api/terms";
import { DriveError, driveError, paste, url } from "../phone/driveApi";

/** The per-file cap, both directions (the server holds the same number). */
export const FILE_MAX_BYTES = 32 * 1024 * 1024;
export const FILE_CAP_MESSAGE = "Over the 32 MiB per-file limit.";

export type FilesRoot = "cwd" | "up";

const INTENT = { [TERMINAL_INTENT_HEADER]: TERMINAL_INTENT_VALUE };

/** Why a file cannot be uploaded, or null (checked before any request). */
export function fileRefusal(file: { size: number }): string | null {
  if (file.size === 0) return "That file is empty.";
  if (file.size > FILE_MAX_BYTES) return FILE_CAP_MESSAGE;
  return null;
}

export interface TransportRequest {
  url: string;
  body: Blob;
  headers: Record<string, string>;
  signal?: AbortSignal;
  onProgress?: (loaded: number, total: number) => void;
}
export interface TransportResponse {
  status: number;
  statusText: string;
  text: string;
}
export type UploadTransport = (req: TransportRequest) => Promise<TransportResponse>;

const abortError = (): DOMException => new DOMException("aborted", "AbortError");

/** XMLHttpRequest: the only way a browser reports UPLOAD progress. */
export const xhrTransport: UploadTransport = (req) =>
  new Promise((resolve, reject) => {
    if (req.signal?.aborted) {
      reject(abortError());
      return;
    }
    const xhr = new XMLHttpRequest();
    xhr.open("POST", req.url);
    for (const [k, v] of Object.entries(req.headers)) xhr.setRequestHeader(k, v);
    xhr.upload.onprogress = (e) => req.onProgress?.(e.loaded, e.lengthComputable ? e.total : req.body.size);
    xhr.onload = () => resolve({ status: xhr.status, statusText: xhr.statusText, text: xhr.responseText });
    xhr.onerror = () => reject(new DriveError(0, null, "Network error while uploading."));
    xhr.onabort = () => reject(abortError());
    req.signal?.addEventListener("abort", () => xhr.abort(), { once: true });
    xhr.send(req.body);
  });

/** `fetch`: no progress, but it is what the pane paste/drop path already uses. */
export const fetchTransport: UploadTransport = async (req) => {
  const res = await fetch(req.url, { method: "POST", headers: req.headers, body: req.body, signal: req.signal });
  return { status: res.status, statusText: res.statusText, text: await res.text() };
};

function problem(res: TransportResponse): DriveError {
  let type: string | null = null;
  let detail = `${res.status} ${res.statusText}`.trim();
  try {
    const p = JSON.parse(res.text) as { type?: string; detail?: string; title?: string };
    if (typeof p.type === "string") type = p.type;
    if (typeof p.detail === "string" && p.detail !== "") detail = p.detail;
    else if (typeof p.title === "string") detail = p.title;
  } catch {
    // A bodiless or non-JSON answer: the status line is the message.
  }
  return new DriveError(res.status, type, detail);
}

/** A typed, human line for an upload failure (the row and the toast say the same). */
export function uploadMessage(e: unknown): string {
  if (e instanceof DriveError) {
    if (e.status === 413 && e.type === "urn:kb:errors:terminal-upload-quota") {
      return "Upload area is full (128 MiB per session).";
    }
    if (e.status === 413) return FILE_CAP_MESSAGE;
    if (e.status === 503) return "The terminal's runner is not reachable. Try again in a moment.";
    return e.detail;
  }
  return "Could not upload the file.";
}

export interface UploadOptions {
  /** `&index=1`: inline text content into the kb stub. */
  index?: boolean;
  signal?: AbortSignal;
  onProgress?: (loaded: number, total: number) => void;
  transport?: UploadTransport;
}

/** `POST …/files?name=…[&index=1]` with the raw file as the body. */
export async function uploadFile(termId: string, file: File, opts: UploadOptions = {}): Promise<FileUploadAnswer> {
  const refusal = fileRefusal(file);
  if (refusal !== null) throw new DriveError(0, null, refusal);
  const q = `name=${encodeURIComponent(file.name === "" ? "file" : file.name)}${opts.index ? "&index=1" : ""}`;
  const res = await (opts.transport ?? xhrTransport)({
    url: `${url(termId, "files")}?${q}`,
    body: file,
    headers: {
      Accept: "application/json",
      "Content-Type": file.type === "" ? "application/octet-stream" : file.type,
      ...INTENT,
    },
    signal: opts.signal,
    onProgress: opts.onProgress,
  });
  if (res.status < 200 || res.status >= 300) throw problem(res);
  return JSON.parse(res.text) as FileUploadAnswer;
}

function lsQuery(root: FilesRoot, path: string): string {
  return `root=${root}&path=${encodeURIComponent(path)}`;
}

/** `GET …/ls`: one directory, in the server's order (directories first). */
export async function listDir(termId: string, root: FilesRoot, path: string, signal?: AbortSignal): Promise<TermLsAnswer> {
  const res = await fetch(`${url(termId, "ls")}?${lsQuery(root, path)}`, {
    headers: { Accept: "application/json", ...INTENT },
    signal,
  });
  if (!res.ok) throw await driveError(res);
  return (await res.json()) as TermLsAnswer;
}

export interface DownloadOptions {
  /** The size the listing carried: over the cap is refused with no request. */
  bytes?: number;
  signal?: AbortSignal;
  onProgress?: (loaded: number, total: number) => void;
  /** Test seam: where the blob URL is created, clicked and revoked. */
  document?: Document;
  /** How long a SAVED blob URL lives before it is revoked (default 60 s). */
  revokeAfterMs?: number;
}

/** A synchronous revoke right after `click()` can cancel the save on Android browsers. */
export const REVOKE_DELAY_MS = 60_000;

/**
 * `GET <url>`, stream-read for progress, then blob -> object URL -> anchor with
 * `download` -> click. A body shorter than `Content-Length` is an error, never a
 * saved file. The URL is revoked immediately when the click fails, and after
 * `revokeAfterMs` (a delay, so a phone browser can finish the save) on success.
 */
async function downloadFrom(href: string, name: string, opts: DownloadOptions): Promise<void> {
  if (opts.bytes !== undefined && opts.bytes > FILE_MAX_BYTES) throw new DriveError(0, null, FILE_CAP_MESSAGE);
  const res = await fetch(href, {
    headers: { ...INTENT },
    signal: opts.signal,
  });
  if (!res.ok) throw await driveError(res);
  const declared = Number(res.headers.get("Content-Length") ?? "");
  const total = Number.isFinite(declared) && declared > 0 ? declared : (opts.bytes ?? 0);
  const chunks: Uint8Array[] = [];
  let loaded = 0;
  if (res.body) {
    const reader = res.body.getReader();
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      chunks.push(value);
      loaded += value.byteLength;
      opts.onProgress?.(loaded, total);
    }
  } else {
    const buf = new Uint8Array(await res.arrayBuffer());
    chunks.push(buf);
    loaded = buf.byteLength;
    opts.onProgress?.(loaded, total);
  }
  if (Number.isFinite(declared) && res.headers.has("Content-Length") && loaded !== declared) {
    throw new DriveError(0, null, "The download ended early; the file was not saved.");
  }
  const doc = opts.document ?? document;
  const objectUrl = URL.createObjectURL(new Blob(chunks as BlobPart[]));
  try {
    const a = doc.createElement("a");
    a.href = objectUrl;
    a.download = name;
    a.rel = "noopener";
    doc.body.appendChild(a);
    a.click();
    a.remove();
  } catch (err) {
    URL.revokeObjectURL(objectUrl);
    throw err;
  }
  setTimeout(() => URL.revokeObjectURL(objectUrl), opts.revokeAfterMs ?? REVOKE_DELAY_MS);
}

/** `GET …/file` (a working-tree or uploads file). */
export function downloadFile(
  termId: string,
  root: FilesRoot,
  path: string,
  name: string,
  opts: DownloadOptions = {},
): Promise<void> {
  return downloadFrom(`${url(termId, "file")}?${lsQuery(root, path)}`, name, opts);
}

/** `GET …/kept/{artifact_id}`: the original of a file kept in kb (same blob + anchor path). */
export function downloadKept(termId: string, artifactId: string, name: string, opts: DownloadOptions = {}): Promise<void> {
  return downloadFrom(url(termId, `kept/${encodeURIComponent(artifactId)}`), name, opts);
}

/** The absolute path of `name` inside the listed directory (`TermLsAnswer.dir`). */
export function absPath(dir: string, name: string): string {
  return dir.endsWith("/") ? `${dir}${name}` : `${dir}/${name}`;
}

/** Refuse a multi-line text; the REST paste is a single line, never Enter. */
export function singleLine(text: string): boolean {
  return !/[\r\n]/.test(text);
}

/** Any C0 control, DEL or C1 control (CR/LF included). */
const CONTROL = /[\u0000-\u001f\u007f-\u009f]/;
const SHELL_SAFE = /^[A-Za-z0-9_@%+=:,./-]+$/;

/**
 * Quote a path for a POSIX shell prompt: unchanged when every character is in
 * the conservative safe set, else single-quoted with each `'` written `'\''`.
 * Pure: it does not check for control characters (`pastePath` does).
 */
export function shellQuotePath(p: string): string {
  if (SHELL_SAFE.test(p)) return p;
  return `'${p.replace(/'/g, "'\\''")}'`;
}

/**
 * `POST …/paste` with `{text}` only: the shell-quoted path, no Enter, no
 * `enter` key. Refuses a line break or any other control character with no
 * request. Throws DriveError.
 */
export async function pastePath(termId: string, text: string): Promise<void> {
  if (!singleLine(text)) throw new DriveError(0, null, "That path has a line break and cannot be pasted.");
  if (CONTROL.test(text)) throw new DriveError(0, null, "That path has a control character and cannot be pasted.");
  await paste({ termId, harness: "", viewer: null, confirm: async () => false }, shellQuotePath(text));
}
