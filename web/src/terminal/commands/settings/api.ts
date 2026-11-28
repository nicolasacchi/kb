// CMDSETTINGS: the SPA client for `/api/terms/commands` (the command
// catalogue overlay; server side: crates/kb-server/src/terminal/command_store.rs).
//
// Same gate as every terminal route: same-origin (the browser adds Origin; it
// is never set here), the `X-Kb-Intent: terminal` header on every call, no
// token. PUT and DELETE carry `If-Match: "<rev>"` (the ETag the GET returned);
// a stale one is a 412 whose problem body names the current `rev`.

import { currentDaemonBase } from "../../../api/base";
import type { EffectiveCatalogue } from "../../../api/generated/EffectiveCatalogue";
import type { OverlayPut } from "../../../api/generated/OverlayPut";
import { TERMINAL_INTENT_HEADER, TERMINAL_INTENT_VALUE } from "../../../api/terms";

export const COMMANDS_STALE_TYPE = "urn:kb:errors:commands-stale";
export const BAD_CATALOGUE_TYPE = "urn:kb:errors:bad-catalogue";

export interface ServerDefect {
  code: string;
  path: string;
}

/** A non-2xx answer: the problem+json fields the editor reacts to. */
export class CommandsApiError extends Error {
  readonly status: number;
  readonly type: string | null;
  readonly errors: ServerDefect[];
  /** Present on a 412: the revision the server holds now. */
  readonly rev: number | null;
  constructor(status: number, type: string | null, message: string, errors: ServerDefect[], rev: number | null) {
    super(message);
    this.name = "CommandsApiError";
    this.status = status;
    this.type = type;
    this.errors = errors;
    this.rev = rev;
  }
  get stale(): boolean {
    return this.status === 412;
  }
}

async function failure(res: Response): Promise<CommandsApiError> {
  let type: string | null = null;
  let message = `${res.status} ${res.statusText}`.trim();
  let errors: ServerDefect[] = [];
  let rev: number | null = null;
  try {
    const p = (await res.json()) as {
      type?: unknown;
      title?: unknown;
      detail?: unknown;
      errors?: unknown;
      rev?: unknown;
    };
    if (typeof p.type === "string") type = p.type;
    if (typeof p.title === "string") {
      message = typeof p.detail === "string" && p.detail !== "" ? `${p.title}: ${p.detail}` : p.title;
    }
    if (Array.isArray(p.errors)) {
      errors = p.errors.filter(
        (e): e is ServerDefect =>
          typeof e === "object" &&
          e !== null &&
          typeof (e as ServerDefect).code === "string" &&
          typeof (e as ServerDefect).path === "string",
      );
    }
    if (typeof p.rev === "number") rev = p.rev;
  } catch {
    // Bodiless or unparseable: the status line is the message.
  }
  return new CommandsApiError(res.status, type, message, errors, rev);
}

const url = () => `${currentDaemonBase()}/api/terms/commands`;

function headers(extra?: Record<string, string>): Record<string, string> {
  return { Accept: "application/json", [TERMINAL_INTENT_HEADER]: TERMINAL_INTENT_VALUE, ...extra };
}

export async function fetchCommands(signal?: AbortSignal): Promise<EffectiveCatalogue> {
  const res = await fetch(url(), { headers: headers(), signal });
  if (!res.ok) throw await failure(res);
  return (await res.json()) as EffectiveCatalogue;
}

/** Replace the overlay. `rev` is the revision the caller saw. */
export async function putOverlay(overlay: OverlayPut, rev: number, signal?: AbortSignal): Promise<EffectiveCatalogue> {
  const res = await fetch(url(), {
    method: "PUT",
    headers: headers({ "Content-Type": "application/json", "If-Match": `"${rev}"` }),
    body: JSON.stringify({ harnesses: overlay.harnesses, hidden: overlay.hidden }),
    signal,
  });
  if (!res.ok) throw await failure(res);
  return (await res.json()) as EffectiveCatalogue;
}

/** Reset to the shipped defaults (an empty overlay at rev + 1). */
export async function resetOverlay(rev: number, signal?: AbortSignal): Promise<EffectiveCatalogue> {
  const res = await fetch(url(), {
    method: "DELETE",
    headers: headers({ "If-Match": `"${rev}"` }),
    signal,
  });
  if (!res.ok) throw await failure(res);
  return (await res.json()) as EffectiveCatalogue;
}
