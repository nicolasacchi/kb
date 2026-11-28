// PQUI: Send = paste into THIS terminal WITHOUT Enter, then mark the item done.
//
// The text goes through the palette's ONE flattening rule (`flattenToLine`)
// and `sendText(..., {enter:false})`: with bracketed paste off a newline in
// pasted text can press Enter, so a multi-line prompt is never pasted raw, and
// `pressEnter` / `enter:true` are never called from here. The paste happens at
// most ONCE: the done-marking that follows may retry, the paste never does.

import { DriveError, sendText, type Drive, type SendTextResult } from "../phone/driveApi";
import { flattenToLine } from "../commands/palette/flatten";
import { PromptsApiError, type PromptList } from "./promptsApi";

export const FLATTENED_CAPTION = "Sent as one line (line breaks removed)";
export const MARK_FAILED_CAPTION = "Pasted, but could not mark done";

export type SendPromptOutcome =
  /** Nothing was pasted (the Claude guard was declined). */
  | { kind: "declined" }
  /** Nothing was pasted and nothing was marked. */
  | { kind: "error"; detail: string }
  /** Pasted. `marked` is false when done-marking failed (the caption says so). */
  | { kind: "sent"; flattened: boolean; marked: boolean };

export interface SendPromptDeps {
  drive: Drive;
  item: { id: string; text: string };
  /** One PATCH `{done:true}` at the CACHED rev; throws `PromptsApiError`. */
  markDone: (id: string) => Promise<unknown>;
  /** Re-read the queue (also refreshes the cached rev). */
  refetch: () => Promise<PromptList>;
  send?: (d: Drive, text: string, o: { enter: boolean }) => Promise<SendTextResult>;
}

function detailOf(e: unknown): string {
  if (e instanceof DriveError) return e.detail;
  return e instanceof Error ? e.message : "Send failed";
}

/** Mark done; on a 412 refetch once and, if the item is still open, try once more. */
async function markWithRetry(deps: SendPromptDeps): Promise<boolean> {
  const { item } = deps;
  try {
    await deps.markDone(item.id);
    return true;
  } catch (e) {
    if (!(e instanceof PromptsApiError) || !e.stale) return false;
  }
  try {
    const fresh = await deps.refetch();
    const now = fresh.items.find((i) => i.id === item.id);
    if (now === undefined) return false;
    if (now.done_at !== null) return true;
    await deps.markDone(item.id);
    return true;
  } catch {
    return false;
  }
}

export async function sendPrompt(deps: SendPromptDeps): Promise<SendPromptOutcome> {
  const text = deps.item.text;
  const line = flattenToLine(text);
  if (line === "") return { kind: "error", detail: "Nothing to send" };
  const flattened = line !== text.trim();
  try {
    const r = await (deps.send ?? sendText)(deps.drive, line, { enter: false });
    if (r.kind === "declined") return { kind: "declined" };
  } catch (e) {
    return { kind: "error", detail: detailOf(e) };
  }
  return { kind: "sent", flattened, marked: await markWithRetry(deps) };
}
