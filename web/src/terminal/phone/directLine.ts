// PHIN2 - the typed-line mirror and the one guarded Enter of direct typing.
//
// The mirror holds the code points sent since the last Enter, Ctrl-C, Ctrl-U or
// Esc (Backspace pops one). It is shared by the soft-keyboard proxy and the key
// bar, so a `/bg` typed on one and confirmed with Enter on the other is still
// seen. This is a CONVENTION guard, like the one in `guards.ts` it asks: a
// pasted line or a moved cursor makes the mirror lossy. Typed text lives in
// memory only and is never persisted.

import type { ConfirmFn } from "./driveApi";
import { needsConfirm } from "./guards";

export interface DirectLine {
  /** The harness and the confirm host the Enter guard asks (call on every render; cheap). */
  configure(harness: string, confirm: ConfirmFn | null): void;
  text(): string;
  /** Account for bytes that went out (text, DEL, \r, control bytes). Escape sequences are ignored. */
  feed(bytes: string): void;
  /** Clear the mirror. Does nothing while an Enter confirm is pending (the operator keeps the text). */
  reset(): void;
  readonly pending: boolean;
  /**
   * An Enter. `sendCr` runs now when the line needs no confirm, else after the
   * operator confirms; declined, or no confirm host, drops it (fail closed) and
   * keeps the line. While a confirm is pending further Enters are dropped.
   * `sendCr` returns false when nothing went out (the line is then kept).
   */
  enter(sendCr: () => boolean): void;
}

export function createDirectLine(): DirectLine {
  let cps: string[] = [];
  let pending = false;
  let harness = "";
  let confirm: ConfirmFn | null = null;
  return {
    configure(h, c) {
      harness = h;
      confirm = c;
    },
    text: () => cps.join(""),
    feed(bytes) {
      if (bytes.length > 1 && (bytes.startsWith("\x1b[") || bytes.startsWith("\x1bO"))) return;
      for (const ch of bytes) {
        const c = ch.codePointAt(0) ?? 0;
        if (c === 0x7f) cps.pop();
        else if (c === 0x0d || c === 0x03 || c === 0x15 || c === 0x1b) cps = [];
        else if (c >= 0x20) cps.push(ch);
      }
    },
    reset() {
      if (!pending) cps = [];
    },
    get pending() {
      return pending;
    },
    enter(sendCr) {
      if (pending) return;
      const copy = needsConfirm(harness, { kind: "text", text: cps.join("") });
      if (copy === null) {
        if (sendCr()) cps = [];
        return;
      }
      if (confirm === null) return;
      pending = true;
      confirm(copy).then(
        (ok) => {
          pending = false;
          if (ok && sendCr()) cps = [];
        },
        () => {
          pending = false;
        },
      );
    },
  };
}
