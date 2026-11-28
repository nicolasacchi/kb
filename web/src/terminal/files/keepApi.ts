// FXPHONE — "Keep in kb" for a file already on the runner: `POST …/files/keep`.
// Imperative `fetch` (no TanStack, no SSE), same-origin, the terminal intent
// header, no token. The answer is the server's `KeepAnswer`; every refusal is a
// typed `DriveError` that `keepMessage` turns into one honest line for the row
// and the toast. Nothing here builds HTML.

import type { KeepAnswer } from "../../api/generated/KeepAnswer";
import { TERMINAL_INTENT_HEADER, TERMINAL_INTENT_VALUE } from "../../api/terms";
import { DriveError, driveError, url } from "../phone/driveApi";
import { FILE_CAP_MESSAGE, FILE_MAX_BYTES, type FilesRoot } from "./filesApi";

export interface KeepRequest {
  root: FilesRoot;
  /** Relative to the listed root (the same string the download route takes). */
  path: string;
  /** Inline text content into the kb stub. Default false. */
  index?: boolean;
}

/** `POST …/files/keep` with `{path, root, index}`; `index` is always a boolean. */
export async function keepFile(termId: string, req: KeepRequest, signal?: AbortSignal): Promise<KeepAnswer> {
  const res = await fetch(url(termId, "files/keep"), {
    method: "POST",
    headers: {
      Accept: "application/json",
      "Content-Type": "application/json",
      [TERMINAL_INTENT_HEADER]: TERMINAL_INTENT_VALUE,
    },
    body: JSON.stringify({ path: req.path, root: req.root, index: req.index === true }),
    signal,
  });
  if (!res.ok) throw await driveError(res);
  return (await res.json()) as KeepAnswer;
}

/** Why a listed file cannot be kept, or null (checked before any request). */
export function keepRefusal(file: { bytes: number }): string | null {
  return file.bytes > FILE_MAX_BYTES ? FILE_CAP_MESSAGE : null;
}

/** A typed, human line for a keep failure (the row and the toast say the same). */
export function keepMessage(e: unknown): string {
  if (!(e instanceof DriveError)) return "Could not keep the file in kb.";
  switch (e.type) {
    case "urn:kb:errors:terminal-files-not-configured":
      return "Keeping files in kb is not configured on this server.";
    case "urn:kb:errors:terminal-file-changed":
      return "The file changed while it was being read. Try again.";
    case "urn:kb:errors:terminal-upload-quota":
      return "The kb file area is full (quota reached). Free some space and try again.";
    default:
      break;
  }
  if (e.status === 413) return FILE_CAP_MESSAGE;
  if (e.status === 503) return "The terminal's runner is not reachable. Try again in a moment.";
  // 400 / 403 / 404 / 422 and anything else: the server's own words, verbatim.
  return e.detail;
}

/** A 200 answer whose kb step did not store the file (skipped / failed), in words, or null when stored. */
export function keepAnswerProblem(a: KeepAnswer): string | null {
  if (a.kb.status === "stored") return null;
  const why = a.kb.reason ? `: ${a.kb.reason}` : "";
  return a.kb.status === "failed" ? `kb keep failed${why}` : `Not kept in kb${why}`;
}
