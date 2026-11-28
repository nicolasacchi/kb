// CMDPALETTE: send one catalogue entry through the EXISTING Drive paths
// (`sendText` / `sendKey` in phone/driveApi.ts). No new route and no new
// request shape: paste-mode is one paste WITHOUT Enter; run-mode is one paste
// request carrying `enter: true` (the server decides whether the Enter may go).

import { DriveError, sendKey, sendText, type Drive } from "../../phone/driveApi";
import type { Keystrokes } from "../catalog";
import { flattenToLine } from "./flatten";

export type SendOutcome =
  /** Everything was sent. */
  | { kind: "sent" }
  /** The Claude guard was declined; nothing (more) was sent. */
  | { kind: "declined" }
  /** Run-mode: the text is in the pane but the server refused the Enter. */
  | { kind: "refused"; detail: string }
  /** Nothing was sent (invalid entry) or a request failed. */
  | { kind: "error"; detail: string };

const INVALID: Record<string, string> = {
  "arg-control-char": "An argument holds a control character.",
  "bad-key": "This entry names a key the terminal does not accept.",
  "bad-item": "This entry is incomplete.",
};

/** Why `ks` cannot be sent, or null when it can. */
export function unsendable(ks: Keystrokes): string | null {
  if (ks.kind === "incomplete") return `Missing: ${ks.missing.join(", ")}`;
  if (ks.kind === "invalid") return INVALID[ks.reason] ?? "This entry cannot be sent.";
  return null;
}

function detailOf(e: unknown): string {
  if (e instanceof DriveError) return e.detail;
  return e instanceof Error ? e.message : "Send failed";
}

export async function sendEntry(d: Drive, ks: Keystrokes): Promise<SendOutcome> {
  const bad = unsendable(ks);
  if (bad !== null) return { kind: "error", detail: bad };
  try {
    if (ks.kind === "keys") {
      for (const key of ks.keys) {
        const r = await sendKey(d, key);
        if (r.kind === "declined") return { kind: "declined" };
      }
      return { kind: "sent" };
    }
    if (ks.kind !== "text") return { kind: "error", detail: "This entry cannot be sent." };
    const text = ks.enter ? ks.text : flattenToLine(ks.text);
    if (text === "") return { kind: "error", detail: "Nothing to send" };
    const r = await sendText(d, text, { enter: ks.enter });
    if (r.kind === "declined") return { kind: "declined" };
    if (r.kind === "pasted-not-sent") return { kind: "refused", detail: r.detail };
    return { kind: "sent" };
  } catch (e) {
    return { kind: "error", detail: detailOf(e) };
  }
}

/** The terminal pane's own input (xterm exports no focus API; this is its helper textarea). */
export function focusTerminalPane(): void {
  document.querySelector<HTMLElement>('[data-testid="kb-terminal"] .xterm-helper-textarea')?.focus();
}
