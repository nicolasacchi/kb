// BHEAD — the one-shot `initialInput` ledger. Module-level so it survives a
// remount of the panel (breakpoint switch, route re-entry) within the page: a
// (terminal, text) pair is delivered at most once.

const DELIVERED = new Set<string>();

const keyOf = (termId: string, text: string): string => `${termId}\u0000${text}`;

export const initialInputDelivered = (termId: string, text: string): boolean => DELIVERED.has(keyOf(termId, text));

export function markInitialInputDelivered(termId: string, text: string): void {
  DELIVERED.add(keyOf(termId, text));
}

/** What gets pasted: one trailing newline stripped (the paste never presses Enter). */
export const initialInputText = (text: string): string => text.replace(/\r?\n$/, "");

/** Test seam. */
export function resetInitialInput(): void {
  DELIVERED.clear();
}
