// HUBPROMPT — the in-memory handoff from the launch dialog to the terminal
// page. Never `history.state`, a URL, storage or the query cache: the text may
// hold secrets, so it lives only in this module and is consumed once.

const STASH = new Map<string, string>();

export function stashFirstPrompt(termId: string, text: string): void {
  STASH.set(termId, text);
}

/** Returns the stashed text once, then undefined. */
export function takeFirstPrompt(termId: string): string | undefined {
  const t = STASH.get(termId);
  STASH.delete(termId);
  return t;
}

/** Test seam. */
export function resetFirstPromptStash(): void {
  STASH.clear();
}
