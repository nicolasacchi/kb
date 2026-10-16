// OL R4 — the pure pieces of a worklog row: the title (with the
// first-prompt-fallback rule) and the ONE quiet meta line. Display-only:
// nothing here is persisted or fed back into triage (invariant #11).
import type { SessionRow } from "../api/sessions";
import { sessionDisplayName } from "./sessionDisplayName";
import { humanizeDuration } from "./time";

export const PROMPT_TITLE_MAX = 80;

export type SessionRowTitle = {
  text: string;
  /** True when the title is just the opening prompt (no real name exists). */
  isPromptFallback: boolean;
};

/** The row title. When the name is the raw first prompt, truncate it at
 *  {@link PROMPT_TITLE_MAX} chars and flag it so the row renders it regular
 *  weight. Whitespace runs collapse so a multi-line prompt reads as one line. */
export function sessionRowTitle(row: SessionRow): SessionRowTitle {
  const name = sessionDisplayName(row);
  const prompt = row.first_user_prompt?.trim() ?? "";
  if (!prompt || name.trim() !== prompt) return { text: name, isPromptFallback: false };
  const flat = prompt.replace(/\s+/g, " ");
  const text = flat.length > PROMPT_TITLE_MAX ? `${flat.slice(0, PROMPT_TITLE_MAX - 1).trimEnd()}…` : flat;
  return { text, isPromptFallback: true };
}

const plural = (n: number, one: string, many: string) => `${n} ${n === 1 ? one : many}`;

/** The single facts line: project · harness · duration · N memories (+ commits/errors when present). */
export function sessionMetaParts(row: SessionRow): string[] {
  const parts: string[] = [];
  if (row.folder) parts.push(row.folder);
  if (row.harness) parts.push(row.harness);
  if (row.duration_ms > 0) {
    const d = humanizeDuration(row.duration_ms);
    if (d) parts.push(d);
  }
  if (row.memory_count > 0) parts.push(plural(row.memory_count, "memory", "memories"));
  if (row.commit_count > 0) parts.push(plural(row.commit_count, "commit", "commits"));
  if (row.error_count > 0) parts.push(plural(row.error_count, "error", "errors"));
  return parts;
}
