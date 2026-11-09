// T14 — OSC 52 gated clipboard write: the pane program's set is written
// to the clipboard ONLY if this panel typed within the last 10 s; reads
// (`?`) are swallowed; failures are caught silently. Extracted from
// `TerminalPanel.tsx` so the policy is unit-testable without xterm.

export const TYPED_WINDOW_MS = 10_000;

/**
 * Decide the clipboard text for one OSC 52 payload, or null when it must
 * NOT be written (read query, stale/no prior typing, malformed base64).
 */
export function osc52ClipboardText(
  data: string,
  lastTypedMs: number,
  nowMs: number,
): string | null {
  if (data.includes("?")) return null;
  if (nowMs - lastTypedMs > TYPED_WINDOW_MS) return null;
  const parts = data.split(";");
  if (parts.length < 2) return null;
  try {
    return atob(parts[1]);
  } catch {
    return null;
  }
}

/** Write `text` to the clipboard, swallowing every failure silently. */
export async function writeClipboardText(text: string): Promise<void> {
  try {
    await navigator.clipboard?.writeText(text);
  } catch {
    // Clipboard failures are caught silently.
  }
}

/** Combined: gate on recent typing, then write; never throws. */
export async function writeClipboardIfRecent(
  data: string,
  lastTypedMs: number,
  nowMs: number,
): Promise<void> {
  const text = osc52ClipboardText(data, lastTypedMs, nowMs);
  if (text !== null) await writeClipboardText(text);
}
