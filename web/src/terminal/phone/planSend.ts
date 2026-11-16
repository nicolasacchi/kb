// T18 — what the composer's Send does, as a pure function.
//
// The server is authoritative (it refuses a multi-line paste when the pane
// did not enable bracketed paste, a Enter while the bound session is blocked,
// and an Enter after a paste-only paste); this only picks which calls to make
// and which caption to show. Enter is never implied: it is sent only for a
// plain paste, and the runner still waits for the pane to be quiet first.

export interface SendCtx {
  harness: string;
  /** The pane's `bracket_paste_flag`; null when the terminal row is not known yet. */
  bracketPaste: boolean | null;
  /** Beat-derived: the bound session is blocked on the operator. */
  blocked: boolean;
}

export type SendPlan =
  | { kind: "empty" }
  | { kind: "refuse"; reason: "multiline-no-bracketed-paste" }
  | { kind: "paste-only"; text: string; why: "slash" | "at" }
  | { kind: "paste-enter"; text: string }
  | { kind: "paste-then-hold"; text: string };

/**
 * Rules, in this order: (1) blank is empty; (2) multi-line (after dropping ONE
 * trailing newline) with `bracketPaste === false` is refused (`null` proceeds:
 * the server refuses with 409/422 `multi-line-refused`); (3) text starting with
 * `/` or `@` is pasted only, whatever else is true (a menu or file picker
 * would take an Enter blindly); (4) a blocked session pastes then holds;
 * (5) otherwise paste, then Enter.
 */
export function planSend(text: string, ctx: SendCtx): SendPlan {
  if (text.trim() === "") return { kind: "empty" };
  const body = /\r?\n$/.test(text) ? text.replace(/\r?\n$/, "") : text;
  if (/[\r\n]/.test(body) && ctx.bracketPaste === false) {
    return { kind: "refuse", reason: "multiline-no-bracketed-paste" };
  }
  const first = text.trimStart().charAt(0);
  if (first === "/") return { kind: "paste-only", text, why: "slash" };
  if (first === "@") return { kind: "paste-only", text, why: "at" };
  if (ctx.blocked) return { kind: "paste-then-hold", text };
  return { kind: "paste-enter", text };
}

/** The captions the composer shows (Spec 2), in one place so tests can pin them. */
export const CAPTION = {
  pastedOnly: "Pasted, not sent (starts with / or @). Press Enter on the key bar.",
  holding: "Session is waiting on you; use the choice bar.",
  refuse:
    "This pane did not enable bracketed paste, so a multi-line message would be sent line by line. Send one line, or paste it into the Read view instead.",
  unbound: "No session bound: Enter is disabled",
  holdCtrlC: "Hold to send Ctrl-C",
  pastedNotSent: (detail: string): string => `Pasted, not sent: ${detail}`,
  enterRefused: (detail: string): string => `Enter not sent: ${detail}`,
} as const;
