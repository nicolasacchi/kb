// PHIN2 - the sticky Ctrl latch of the phone's direct typing, as a pure store.
//
// One tap arms it (applies to the NEXT key, from the key bar or the soft
// keyboard), a second tap within DOUBLE_TAP_MS of the arming tap locks it, any
// later tap turns it off. An armed latch is one-shot (`consume`), a locked one
// persists. The store knows nothing of the DOM, a socket or a composition: the
// proxy core and the key bar ask it, and the phone view releases it.

export type StickyState = "off" | "armed" | "locked";

/** A second tap within this window of the arming tap locks the latch. */
export const DOUBLE_TAP_MS = 350;

export interface StickyMods {
  getState(): StickyState;
  subscribe(fn: () => void): () => void;
  /** off -> armed; armed -> locked (fast second tap) or off (slow); locked -> off. */
  tap(): void;
  /** True when Ctrl applies to the key being sent now. Armed becomes off; locked stays locked. */
  consume(): boolean;
  /** Back to off; idempotent and silent when already off. */
  release(): void;
}

export function createStickyMods(now: () => number = () => Date.now()): StickyMods {
  let state: StickyState = "off";
  let armedAt = -Infinity;
  const subs = new Set<() => void>();
  const set = (s: StickyState): void => {
    if (s === state) return;
    state = s;
    for (const f of [...subs]) f();
  };
  return {
    getState: () => state,
    subscribe(fn) {
      subs.add(fn);
      return () => subs.delete(fn);
    },
    tap() {
      if (state === "off") {
        armedAt = now();
        set("armed");
      } else if (state === "armed") {
        set(now() - armedAt <= DOUBLE_TAP_MS ? "locked" : "off");
      } else {
        set("off");
      }
    },
    consume() {
      if (state === "off") return false;
      if (state === "armed") set("off");
      return true;
    },
    release() {
      set("off");
    },
  };
}

const CTRL_PUNCT: Record<string, string> = {
  "@": "\x00",
  " ": "\x00",
  "[": "\x1b",
  "\\": "\x1c",
  "]": "\x1d",
  "^": "\x1e",
  _: "\x1f",
  "?": "\x7f",
};

/**
 * Ctrl applied to the FIRST code point of `s`: a-z/A-Z become `c & 0x1f`,
 * `@`, space, `[ \ ] ^ _` and `?` their control bytes; anything else passes
 * through. `consumed` says whether a control byte was produced; the rest of
 * the string is untouched.
 */
export function applyCtrl(s: string): { out: string; consumed: boolean } {
  const first = Array.from(s)[0];
  if (first === undefined) return { out: s, consumed: false };
  const rest = s.slice(first.length);
  const code = first.charCodeAt(0);
  if (first.length === 1 && ((code >= 0x61 && code <= 0x7a) || (code >= 0x41 && code <= 0x5a))) {
    return { out: String.fromCharCode(code & 0x1f) + rest, consumed: true };
  }
  const p = CTRL_PUNCT[first];
  if (p !== undefined) return { out: p + rest, consumed: true };
  return { out: s, consumed: false };
}
