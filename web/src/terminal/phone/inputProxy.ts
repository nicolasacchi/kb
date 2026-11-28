// PHIN0 - the phone's direct-typing input proxy, as a DOM-free state machine.
//
// A kb-owned element's value is diffed against a mirror and the delta goes out
// through the same WS input frames the desktop uses (`{"t":"input","data"}`).
// Nothing here touches the DOM, a socket or storage with typed text: the
// element, the send function and the log sink are all injected, so the golden
// tests run in node. Typed text lives only in memory (the mirror and the
// bounded debug ring the component keeps); the sole persisted value is the
// variant choice.

import { clipboardAction } from "../input";
import type { ConfirmFn } from "./driveApi";
import { createDirectLine, type DirectLine } from "./directLine";
import { applyCtrl, type StickyMods } from "./stickyMods";

/** The three switchable axes the operator uses to find what works on a given IME. */
export interface ProxyVariant {
  /** `every`: diff on every input event (composition included). `deferred`: interim text is only shown, one diff at compositionend. */
  send: "every" | "deferred";
  /** `zwsp`: the value holds two zero-width spaces so Backspace on an empty field is an event. */
  sentinel: "none" | "zwsp";
  attrs: "textarea" | "input-text" | "input-url" | "textarea-nofill";
}

export const DEFAULT_VARIANT: ProxyVariant = { send: "every", sentinel: "none", attrs: "textarea" };

export const VARIANT_OPTIONS = {
  send: ["every", "deferred"],
  sentinel: ["none", "zwsp"],
  attrs: ["textarea", "input-text", "input-url", "textarea-nofill"],
} as const;

export const PHONE_DIRECT_KEY = "kb.term.phoneDirect";
export const PHONE_VARIANT_KEY = "kb.term.phoneVariant";

/** Two zero-width spaces: one can be deleted without leaving the field empty. */
export const SENTINEL = "\u200b\u200b";
const ZWSP = "\u200b";

type StorageLike = Pick<Storage, "getItem" | "setItem" | "removeItem">;

/**
 * The hidden pref: `?direct=1` turns it on (and remembers it), `?direct=0`
 * turns it off (and forgets it), otherwise the remembered value decides.
 * A missing or throwing storage only costs the memory: it never turns the
 * pref on, and an explicit `?direct=1` still enables it for this page.
 */
export function phoneDirectEnabled(search: string, storage: StorageLike | null): boolean {
  let q: string | null = null;
  try {
    q = new URLSearchParams(search).get("direct");
  } catch {
    q = null;
  }
  if (q === "1") {
    try {
      storage?.setItem(PHONE_DIRECT_KEY, "1");
    } catch {
      // the pref just is not remembered
    }
    return true;
  }
  if (q === "0") {
    try {
      storage?.removeItem(PHONE_DIRECT_KEY);
    } catch {
      // nothing remembered to forget
    }
    return false;
  }
  try {
    return storage?.getItem(PHONE_DIRECT_KEY) === "1";
  } catch {
    return false;
  }
}

/** The debug strip's switch (`?inputdebug=1`); never persisted. */
export function inputDebugEnabled(search: string): boolean {
  try {
    return new URLSearchParams(search).get("inputdebug") === "1";
  } catch {
    return false;
  }
}

function isVariant(v: unknown): v is ProxyVariant {
  if (typeof v !== "object" || v === null) return false;
  const o = v as Record<string, unknown>;
  return (
    (VARIANT_OPTIONS.send as readonly unknown[]).includes(o.send) &&
    (VARIANT_OPTIONS.sentinel as readonly unknown[]).includes(o.sentinel) &&
    (VARIANT_OPTIONS.attrs as readonly unknown[]).includes(o.attrs)
  );
}

/** The remembered variant, or the default when absent, malformed or unreadable. */
export function readVariant(storage: StorageLike | null): ProxyVariant {
  try {
    const raw = storage?.getItem(PHONE_VARIANT_KEY);
    if (!raw) return DEFAULT_VARIANT;
    const v: unknown = JSON.parse(raw);
    return isVariant(v) ? { send: v.send, sentinel: v.sentinel, attrs: v.attrs } : DEFAULT_VARIANT;
  } catch {
    return DEFAULT_VARIANT;
  }
}

export function writeVariant(storage: StorageLike | null, v: ProxyVariant): void {
  try {
    storage?.setItem(PHONE_VARIANT_KEY, JSON.stringify(v));
  } catch {
    // not remembered
  }
}

/** The element attributes of one attribute set. */
export function attrsFor(kind: ProxyVariant["attrs"]): { tag: "textarea" | "input"; attrs: Record<string, string> } {
  const base: Record<string, string> = {
    autocapitalize: "off",
    autocorrect: "off",
    spellcheck: "false",
    autocomplete: "off",
    enterkeyhint: "enter",
    inputmode: "text",
  };
  if (kind === "input-text") return { tag: "input", attrs: { ...base, type: "text" } };
  if (kind === "input-url") return { tag: "input", attrs: { ...base, type: "url" } };
  if (kind === "textarea-nofill") {
    return { tag: "textarea", attrs: { ...base, name: "kb-term-proxy", "data-lpignore": "true", "data-1p-ignore": "true" } };
  }
  return { tag: "textarea", attrs: base };
}

/**
 * What changed between two values: the common prefix is measured in CODE
 * POINTS (a surrogate pair is never split), `del` counts the code points of
 * `prev` after the prefix and `ins` is the rest of `next`.
 */
export function diffValue(prev: string, next: string): { del: number; ins: string } {
  const a = Array.from(prev);
  const b = Array.from(next);
  let i = 0;
  const n = Math.min(a.length, b.length);
  while (i < n && a[i] === b[i]) i += 1;
  return { del: a.length - i, ins: b.slice(i).join("") };
}

/** Normalise typed newlines to the Enter key's byte. */
export function newlinesToCr(s: string): string {
  return s.replace(/\r\n|\n/g, "\r");
}

/** DELs for the removed code points, then the inserted text (newlines as Enter). */
export function encodeDiff(d: { del: number; ins: string }): string {
  return "\x7f".repeat(d.del) + newlinesToCr(d.ins);
}

/** Control characters shown as visible escapes (the debug strip never holds a raw control). */
export function escapeCtl(s: string): string {
  let out = "";
  for (const ch of s) {
    const c = ch.codePointAt(0) ?? 0;
    if (ch === "\\") out += "\\\\";
    else if (c === 0x0d) out += "\\r";
    else if (c === 0x0a) out += "\\n";
    else if (c === 0x09) out += "\\t";
    else if (c === 0x1b) out += "\\e";
    else if (c === 0x7f) out += "\\x7f";
    else if (c < 0x20) out += `\\x${c.toString(16).padStart(2, "0")}`;
    else if (c === 0x200b) out += "\\u200b";
    else out += ch;
  }
  return out;
}

export interface DebugEvent {
  type: string;
  inputType: string;
  data: string;
  keyCode: number;
  composing: boolean;
  /** What went out for this event (escaped), `dropped` when the socket refused it. */
  bytes: string;
  /** UTF-8 bytes this event sent (0 when nothing went out). */
  n: number;
}

export interface BeforeInputEv {
  inputType: string;
  data: string | null;
  isComposing: boolean;
}
export interface InputEv extends BeforeInputEv {
  /** The element's value after the event. */
  value: string;
}
export interface KeyEv {
  key: string;
  keyCode: number;
  ctrlKey: boolean;
  altKey: boolean;
  metaKey: boolean;
  isComposing: boolean;
  /** PHIN1: absent = false (Ctrl+Shift+C / Ctrl+Shift+V are the clipboard chords). */
  shiftKey?: boolean;
}

export interface ProxyCoreOptions {
  /** One WS input frame; false when it was not sent (not live). */
  send: (data: string) => boolean;
  variant: ProxyVariant;
  log?: (e: DebugEvent) => void;
  /** The element's value must become this (sentinel restore / clear). */
  setValue?: (v: string) => void;
  /** Injectable clock for the Enter keydown + beforeinput dedupe. */
  now?: () => number;
  /** PHIN1: the element's current value, so an Enter during a composition can flush the composed text first. */
  getValue?: () => string;
  /** PHIN1: the terminal has a selection (Ctrl/Cmd+C then copies instead of sending ^C). */
  hasSelection?: () => boolean;
  /** PHIN1: copy the terminal selection to the clipboard. */
  copy?: () => void;
  /** PHIN2: the sticky Ctrl latch; applies to the next discrete insertion, never to a composition. */
  mods?: Pick<StickyMods, "consume">;
  /** PHIN2: the typed-line mirror shared with the key bar (default: the core's own, configured from `harness`/`confirm`). */
  line?: DirectLine;
  /** PHIN2: the harness and confirm host the typed `/bg` Enter guard asks (ignored when `line` is given). */
  harness?: string;
  confirm?: ConfirmFn | null;
}

/** The window in which a beforeinput Enter after a keydown Enter is the same press. */
export const ENTER_PAIR_MS = 300;

const ARROWS: Record<string, string> = { ArrowUp: "\x1b[A", ArrowDown: "\x1b[B", ArrowRight: "\x1b[C", ArrowLeft: "\x1b[D" };
/** PHIN2: the arrows with the sticky Ctrl latch applied. */
const CTRL_ARROWS: Record<string, string> = { ArrowUp: "\x1b[1;5A", ArrowDown: "\x1b[1;5B", ArrowRight: "\x1b[1;5C", ArrowLeft: "\x1b[1;5D" };

export interface ProxyCore {
  /** The value the element must start with. */
  initialValue: string;
  onBeforeInput(e: BeforeInputEv): boolean;
  onInput(e: InputEv): void;
  onCompositionStart(): void;
  onCompositionEnd(e: { data: string | null; value: string }): void;
  onKeyDown(e: KeyEv): boolean;
  /** Returns true when the event was consumed (the caller prevents default). */
  onPaste(text: string, bracketed: boolean): boolean;
  /** Forget the mirror and put the element back to its resting value. */
  reset(): void;
  readonly composing: boolean;
  /** UTF-8 bytes actually sent so far. */
  readonly sentBytes: number;
}

export function createProxyCore(opts: ProxyCoreOptions): ProxyCore {
  const { variant } = opts;
  const now = opts.now ?? (() => Date.now());
  const rest = variant.sentinel === "zwsp" ? SENTINEL : "";
  let mirror = rest;
  let composing = false;
  let enterAt = -Infinity;
  let sentBytes = 0;
  const enc = new TextEncoder();
  const line = opts.line ?? createDirectLine();
  if (!opts.line) line.configure(opts.harness ?? "", opts.confirm ?? null);

  const log = (type: string, inputType: string, data: string | null, keyCode: number, comp: boolean, bytes: string, n = 0): void => {
    opts.log?.({ type, inputType, data: escapeCtl(data ?? ""), keyCode, composing: comp, bytes: escapeCtl(bytes), n });
  };

  /** Send once; a refusal is logged and leaves every mirror untouched. */
  const emit = (
    bytes: string,
    type: string,
    inputType: string,
    data: string | null,
    keyCode: number,
    comp: boolean,
    track: string = bytes,
  ): boolean => {
    if (bytes === "") {
      log(type, inputType, data, keyCode, comp, "");
      return true;
    }
    if (!opts.send(bytes)) {
      log(type, inputType, data, keyCode, comp, "dropped");
      return false;
    }
    const n = enc.encode(bytes).length;
    sentBytes += n;
    line.feed(track);
    log(type, inputType, data, keyCode, comp, bytes, n);
    return true;
  };

  const settle = (): void => {
    if (composing) return;
    if (opts.setValue) opts.setValue(rest);
    mirror = rest;
  };

  /** Diff the element's value against the mirror and send the delta. */
  const flush = (
    value: string,
    type: string,
    inputType: string,
    data: string | null,
    comp: boolean,
    settleAfter = true,
    ctrlOk = !comp,
  ): void => {
    const next = value;
    const d = diffValue(mirror, next);
    // A zero-width space the user (or the IME) inserted is never typed text.
    const ins = variant.sentinel === "zwsp" ? d.ins.split(ZWSP).join("") : d.ins;
    // PHIN2: Ctrl rides a discrete insertion only; composed text goes out plain and the latch stays armed.
    const ctrl = ctrlOk && ins !== "" && opts.mods?.consume() === true;
    const bytes = "\x7f".repeat(d.del) + (ctrl ? applyCtrl(newlinesToCr(ins)).out : newlinesToCr(ins));
    if (!emit(bytes, type, inputType, data, 0, comp)) return;
    mirror = next;
    if (settleAfter) settle();
  };

  /** PHIN2: one Enter through the shared guard (a typed `/bg` on claude asks first). */
  const sendEnter = (type: string, inputType: string, data: string | null, keyCode: number, comp: boolean): void => {
    if (!line.pending) opts.mods?.consume();
    line.enter(() => emit("\r", type, inputType, data, keyCode, comp));
  };

  return {
    initialValue: rest,
    get composing() {
      return composing;
    },
    get sentBytes() {
      return sentBytes;
    },
    onBeforeInput(e) {
      const t = e.inputType;
      if (t === "insertLineBreak" || t === "insertParagraph") {
        if (now() - enterAt <= ENTER_PAIR_MS) {
          enterAt = -Infinity;
          log("beforeinput", t, e.data, 0, e.isComposing, "dup");
          return true;
        }
        if ((composing || e.isComposing) && opts.getValue) {
          // PHIN1: the composed text goes out BEFORE the carriage return. The composition
          // is over for the diff; the element keeps its value, so the compositionend that
          // follows diffs against an equal mirror and sends nothing.
          composing = false;
          flush(opts.getValue(), "beforeinput", t, e.data, false, false, false);
        }
        sendEnter("beforeinput", t, e.data, 0, e.isComposing);
        return true;
      }
      if (t === "deleteContentBackward" && !composing && !e.isComposing && mirror === "") {
        // Nothing to delete locally, so no input event will follow: the byte is ours to send.
        emit("\x7f", "beforeinput", t, e.data, 0, false);
        return true;
      }
      log("beforeinput", t, e.data, 0, e.isComposing, "");
      return false;
    },
    onInput(e) {
      const comp = composing || e.isComposing;
      if (comp && variant.send === "deferred") {
        log("input", e.inputType, e.value, 0, true, "interim");
        return;
      }
      flush(e.value, "input", e.inputType, e.data, comp);
    },
    onCompositionStart() {
      composing = true;
      log("compositionstart", "", "", 0, true, "");
    },
    onCompositionEnd(e) {
      composing = false;
      flush(e.value, "compositionend", "", e.data, false, true, false);
    },
    onKeyDown(e) {
      if (e.keyCode === 229 || e.isComposing || composing) {
        log("keydown", "", e.key, e.keyCode, true, "");
        return false;
      }
      if ((e.ctrlKey || e.metaKey) && !e.altKey) {
        // PHIN1: the desktop's clipboard chords. Copy sends nothing; paste is left to the
        // browser's paste event (onPaste), so the key itself must not be consumed.
        const act = clipboardAction(
          { type: "keydown", key: e.key, ctrlKey: e.ctrlKey, metaKey: e.metaKey, shiftKey: e.shiftKey === true, altKey: false },
          opts.hasSelection?.() ?? false,
        );
        if (act === "copy") {
          opts.copy?.();
          log("keydown", "", e.key, e.keyCode, false, "");
          return true;
        }
        if (act === "paste" || e.key.toLowerCase() === "v") {
          log("keydown", "", e.key, e.keyCode, false, "");
          return false;
        }
      }
      let bytes: string | null = null;
      if (e.key === "Enter") {
        enterAt = now();
        sendEnter("keydown", "", e.key, e.keyCode, false);
        return true;
      } else if (e.key === "Backspace") bytes = "\x7f";
      else if (e.key === "Tab") bytes = "\t";
      else if (e.key === "Escape") bytes = "\x1b";
      else if (ARROWS[e.key] !== undefined && !e.ctrlKey && !e.altKey && !e.metaKey) bytes = ARROWS[e.key] ?? null;
      else if (e.ctrlKey && !e.altKey && !e.metaKey && /^[a-zA-Z]$/.test(e.key)) {
        bytes = String.fromCharCode(e.key.toLowerCase().charCodeAt(0) & 0x1f);
      }
      if (bytes === null) {
        log("keydown", "", e.key, e.keyCode, false, "");
        return false;
      }
      // PHIN2: a latched Ctrl applies to this special key too (arrows take the modified form, the rest go plain).
      if (!e.ctrlKey && !e.altKey && !e.metaKey && opts.mods?.consume() === true) bytes = CTRL_ARROWS[e.key] ?? bytes;
      emit(bytes, "keydown", "", e.key, e.keyCode, false);
      return true;
    },
    onPaste(text, bracketed) {
      const body = newlinesToCr(text);
      if (body === "") return true;
      emit(bracketed ? `\x1b[200~${body}\x1b[201~` : body, "paste", "", text, 0, false, body);
      return true;
    },
    reset() {
      composing = false;
      enterAt = -Infinity;
      opts.setValue?.(rest);
      mirror = rest;
      line.reset();
    },
  };
}
