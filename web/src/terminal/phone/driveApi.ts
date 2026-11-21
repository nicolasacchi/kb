// T18 — the phone Drive REST client.
//
// Every request here is a POST to `/api/terms/{id}/…` carrying the
// terminal intent header (and, for JSON bodies, the viewer id the tap's
// `hello` frame gave this panel, so the tap can tell WHICH device typed and
// release an OSC 52 clipboard write only to it). Same-origin: the browser
// adds `Origin`; this module never sets it and sends no token (the gate
// refuses token callers).
//
// ONE `sendKey` / `sendText` wrapper owns the Claude background guard, so a
// button, a menu entry and the composer cannot each grow their own check.
// Terminal bytes and pictures never enter TanStack Query and nothing here
// polls or subscribes.

import { currentDaemonBase } from "../../api/base";
import { TERMINAL_INTENT_HEADER, TERMINAL_INTENT_VALUE } from "../../api/terms";
import type { ConfirmOptions } from "../../components/ConfirmProvider";
import { needsConfirm } from "./guards";
import type { KeyName } from "./keys";

/** The largest picture the relay accepts (the server holds the same number). */
export const UPLOAD_MAX_BYTES = 8 * 1024 * 1024;

/** The picture types the relay accepts, as the request `Content-Type`. */
export const UPLOAD_TYPES = ["image/png", "image/jpeg", "image/gif", "image/webp"] as const;

/** A non-2xx answer (or a client-side refusal, status 0) from a Drive call. */
export class DriveError extends Error {
  readonly status: number;
  /** problem+json `type` (e.g. `urn:kb:errors:terminal-conflict`), or null. */
  readonly type: string | null;
  /** The problem `detail`, verbatim (what the runner said), or the status line. */
  readonly detail: string;
  constructor(status: number, type: string | null, detail: string) {
    super(detail);
    this.name = "DriveError";
    this.status = status;
    this.type = type;
    this.detail = detail;
  }
}

async function driveError(res: Response): Promise<DriveError> {
  let type: string | null = null;
  let detail = `${res.status} ${res.statusText}`.trim();
  try {
    const p = (await res.json()) as { type?: string; detail?: string; title?: string };
    if (typeof p.type === "string") type = p.type;
    if (typeof p.detail === "string" && p.detail !== "") detail = p.detail;
    else if (typeof p.title === "string") detail = p.title;
  } catch {
    // A bodiless answer (terminal disabled): the status line is the message.
  }
  return new DriveError(res.status, type, detail);
}

function url(termId: string, op: string): string {
  return `${currentDaemonBase()}/api/terms/${encodeURIComponent(termId)}/${op}`;
}

async function postJson<T>(termId: string, op: string, body: Record<string, unknown>): Promise<T> {
  const res = await fetch(url(termId, op), {
    method: "POST",
    headers: {
      Accept: "application/json",
      "Content-Type": "application/json",
      [TERMINAL_INTENT_HEADER]: TERMINAL_INTENT_VALUE,
    },
    body: JSON.stringify(body),
  });
  if (!res.ok) throw await driveError(res);
  return (await res.json()) as T;
}

/** `viewer` is sent only when the tap named one (an older tap does not). */
function withViewer(body: Record<string, unknown>, viewer: string | null): Record<string, unknown> {
  return viewer === null ? body : { ...body, viewer };
}

export type ConfirmFn = (opts: ConfirmOptions) => Promise<boolean>;

/** What every Drive call needs. `confirm` is the one `useConfirm()` host. */
export interface Drive {
  termId: string;
  harness: string;
  viewer: string | null;
  confirm: ConfirmFn;
}

/** `POST …/paste`. Throws `DriveError` (422 `multi-line-refused`, …). */
export async function paste(d: Drive, text: string): Promise<void> {
  await postJson(d.termId, "paste", withViewer({ text }, d.viewer));
}

/** `POST …/enter`: the runner waits for a quiet pane and refuses (409) while the session is blocked. */
export async function pressEnter(d: Drive): Promise<void> {
  await postJson(d.termId, "enter", withViewer({}, d.viewer));
}

/** `POST …/keys` with ONE named key (no guard: use [`sendKey`]). */
export async function postKey(d: Drive, key: KeyName): Promise<void> {
  await postJson(d.termId, "keys", withViewer({ keys: [key] }, d.viewer));
}

export interface GridData {
  cols: number;
  rows: number;
  owner: string | null;
}

/** `POST …/take-size`: the explicit "Fit to phone". 409 `grid-owned` carries the owner in `detail`. */
export async function takeSize(d: Drive, cols: number, rows: number): Promise<GridData> {
  return postJson<GridData>(d.termId, "take-size", withViewer({ cols, rows }, d.viewer));
}

/**
 * `POST …/release-size`: give the grid back. The body is exactly `{}`: the
 * route rejects unknown fields, so (unlike take-size) no viewer rides along.
 * 409 `grid-owned` when the relay does not own the grid.
 */
export async function releaseSize(d: Pick<Drive, "termId">): Promise<void> {
  await postJson(d.termId, "release-size", {});
}

export type SendKeyResult = { kind: "sent" } | { kind: "declined" };

/** Send one key. The Claude guard asks first (every `Left`); declining sends nothing. */
export async function sendKey(d: Drive, key: KeyName): Promise<SendKeyResult> {
  const copy = needsConfirm(d.harness, { kind: "key", key });
  if (copy !== null && !(await d.confirm(copy))) return { kind: "declined" };
  await postKey(d, key);
  return { kind: "sent" };
}

export type SendTextResult =
  | { kind: "declined" }
  | { kind: "sent" }
  /** The text is in the pane but the Enter did not go (`conflict`: the runner refused it with 409). */
  | { kind: "pasted-not-sent"; detail: string; conflict: boolean };

/**
 * Paste `text`; with `enter`, then press Enter. The Claude guard asks BEFORE any
 * request. A paste failure throws (nothing was written); an Enter failure after
 * a successful paste is a result, not a throw, because the text already is in
 * the pane and the caller must not offer a resend.
 */
export async function sendText(
  d: Drive,
  text: string,
  opts: { enter: boolean },
): Promise<SendTextResult> {
  const copy = needsConfirm(d.harness, { kind: "text", text });
  if (copy !== null && !(await d.confirm(copy))) return { kind: "declined" };
  await paste(d, text);
  if (!opts.enter) return { kind: "sent" };
  try {
    await pressEnter(d);
  } catch (e) {
    if (e instanceof DriveError) {
      return { kind: "pasted-not-sent", detail: e.detail, conflict: e.status === 409 };
    }
    throw e;
  }
  return { kind: "sent" };
}

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
export async function uploadImage(termId: string, file: File): Promise<UploadResult> {
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
  });
  if (!res.ok) throw await driveError(res);
  return (await res.json()) as UploadResult;
}
