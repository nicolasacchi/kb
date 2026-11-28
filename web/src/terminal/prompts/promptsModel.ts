// PQUI: pure helpers for the queue (no React, no fetch).
import type { PromptItem, PromptList } from "./promptsApi";

/** The server's cap on one prompt, in bytes after trim. */
export const MAX_PROMPT_BYTES = 8192;
/** Show the byte counter once the text is this close to the cap. */
export const COUNT_NEAR_BYTES = 7000;

const SLUG = /^[a-z0-9_-]{1,64}$/;

/** A project key the server accepts, else null (no request is ever made for null). */
export function validProject(slug: string | null | undefined): string | null {
  return typeof slug === "string" && SLUG.test(slug) ? slug : null;
}

export function byteLength(text: string): number {
  return new TextEncoder().encode(text).length;
}

/** True when `text` may be added/saved: non-blank and within the cap (trimmed, as the server does). */
export function textOk(text: string): boolean {
  const t = text.trim();
  return t !== "" && byteLength(t) <= MAX_PROMPT_BYTES;
}

export interface Split {
  open: PromptItem[];
  done: PromptItem[];
}

/** Open items first in stored order; done items apart, also in stored order. */
export function splitItems(list: PromptList | undefined): Split {
  const open: PromptItem[] = [];
  const done: PromptItem[] = [];
  for (const it of list?.items ?? []) (it.done_at === null ? open : done).push(it);
  return { open, done };
}

/**
 * The PATCH `position` that swaps an OPEN item with its open neighbour: the
 * neighbour's index in the FULL list (the server removes then inserts, so the
 * item lands exactly there). Null at the ends or for a done/unknown item.
 */
export function moveTarget(list: PromptList, id: string, dir: -1 | 1): number | null {
  const open = list.items.filter((i) => i.done_at === null);
  const at = open.findIndex((i) => i.id === id);
  const neighbour = open[at + dir];
  if (at < 0 || neighbour === undefined) return null;
  return list.items.findIndex((i) => i.id === neighbour.id);
}
