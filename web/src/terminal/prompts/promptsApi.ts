// PQUI: the SPA client for `/api/term-prompts/{project}[/{item}]` (the
// daemon-owned prompt queue; server side: crates/kb-server/src/terminal/prompts.rs).
//
// Same gate as every terminal route: same-origin (the browser adds Origin; it
// is never set here), the `X-Kb-Intent: terminal` header on every call, no
// token. PATCH and DELETE carry `If-Match: "<rev>"`; a stale one is a 412
// whose problem body names the current `rev`. Prompt text travels ONLY in a
// JSON body: never in a URL, a log line or an error message built here.

import { currentDaemonBase } from "../../api/base";
import type { PromptList } from "../../api/generated/PromptList";
import { TERMINAL_INTENT_HEADER, TERMINAL_INTENT_VALUE } from "../../api/terms";

export type { PromptList };
export type { PromptItem } from "../../api/generated/PromptItem";

/** Query key of one project's queue; `["term","prompts"]` is the SSE prefix. */
export const termPromptsKey = (project: string) => ["term", "prompts", project] as const;

export const PROMPTS_STALE_TYPE = "urn:kb:errors:prompts-stale";

/** A non-2xx answer: the problem+json fields the UI reacts to. */
export class PromptsApiError extends Error {
  readonly status: number;
  readonly type: string | null;
  /** The problem `detail` (the server never echoes prompt text into it). */
  readonly detail: string;
  /** Present on a 412: the revision the server holds now. */
  readonly rev: number | null;
  constructor(status: number, type: string | null, detail: string, rev: number | null) {
    super(detail);
    this.name = "PromptsApiError";
    this.status = status;
    this.type = type;
    this.detail = detail;
    this.rev = rev;
  }
  get stale(): boolean {
    return this.status === 412;
  }
}

async function failure(res: Response): Promise<PromptsApiError> {
  let type: string | null = null;
  let detail = `${res.status} ${res.statusText}`.trim();
  let rev: number | null = null;
  try {
    const p = (await res.json()) as { type?: unknown; title?: unknown; detail?: unknown; rev?: unknown };
    if (typeof p.type === "string") type = p.type;
    if (typeof p.detail === "string" && p.detail !== "") detail = p.detail;
    else if (typeof p.title === "string") detail = p.title;
    if (typeof p.rev === "number") rev = p.rev;
  } catch {
    // Bodiless or unparseable: the status line is the message.
  }
  return new PromptsApiError(res.status, type, detail, rev);
}

const base = (project: string) => `${currentDaemonBase()}/api/term-prompts/${encodeURIComponent(project)}`;
const itemUrl = (project: string, id: string) => `${base(project)}/${encodeURIComponent(id)}`;

function headers(extra?: Record<string, string>): Record<string, string> {
  return { Accept: "application/json", [TERMINAL_INTENT_HEADER]: TERMINAL_INTENT_VALUE, ...extra };
}

async function answer(res: Response): Promise<PromptList> {
  if (!res.ok) throw await failure(res);
  return (await res.json()) as PromptList;
}

export async function fetchPrompts(project: string, signal?: AbortSignal): Promise<PromptList> {
  return answer(await fetch(base(project), { headers: headers(), signal }));
}

/** Append `text` (201 + the whole queue). */
export async function addPrompt(project: string, text: string, signal?: AbortSignal): Promise<PromptList> {
  return answer(
    await fetch(base(project), {
      method: "POST",
      headers: headers({ "Content-Type": "application/json" }),
      body: JSON.stringify({ text }),
      signal,
    }),
  );
}

export interface PromptPatch {
  text?: string;
  done?: boolean;
  position?: number;
}

/** Edit, mark done/open or move one item. `rev` is the revision the caller saw. */
export async function patchPrompt(
  project: string,
  id: string,
  patch: PromptPatch,
  rev: number,
  signal?: AbortSignal,
): Promise<PromptList> {
  return answer(
    await fetch(itemUrl(project, id), {
      method: "PATCH",
      headers: headers({ "Content-Type": "application/json", "If-Match": `"${rev}"` }),
      body: JSON.stringify(patch),
      signal,
    }),
  );
}

export async function deletePrompt(project: string, id: string, rev: number, signal?: AbortSignal): Promise<PromptList> {
  return answer(
    await fetch(itemUrl(project, id), {
      method: "DELETE",
      headers: headers({ "If-Match": `"${rev}"` }),
      signal,
    }),
  );
}
