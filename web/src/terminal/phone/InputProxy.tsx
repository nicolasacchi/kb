// PHIN0 - the kb-owned input element of the phone Watch view (behind a hidden
// pref) and its debug strip. The element lives inside the grid viewport so it
// pans with the grid, sits on the cursor cell, and hands every event to the
// DOM-free `createProxyCore`, whose output is the existing WS input frames.
// xterm stays output-only here; nothing typed is stored (only the variant
// choice is, by the caller).

import { createElement, useEffect, useRef, useState, type MutableRefObject } from "react";
import {
  VARIANT_OPTIONS,
  attrsFor,
  createProxyCore,
  type DebugEvent,
  type ProxyCore,
  type ProxyVariant,
} from "./inputProxy";
import { tryWriteClipboardText } from "../clipboard";
import type { ConfirmFn } from "./driveApi";
import type { DirectLine } from "./directLine";
import type { StickyMods } from "./stickyMods";
import "./inputProxy.css";

/** The ring the strip shows: the last 20 events and the bytes sent, in memory only. */
export const DEBUG_RING = 20;

export interface DebugLog {
  push(e: DebugEvent): void;
  events(): readonly DebugEvent[];
  total(): number;
  subscribe(fn: () => void): () => void;
}

export function createDebugLog(cap = DEBUG_RING): DebugLog {
  let ring: DebugEvent[] = [];
  let total = 0;
  const subs = new Set<() => void>();
  return {
    push(e) {
      ring = [...ring, e].slice(-cap);
      total += e.n;
      for (const f of subs) f();
    },
    events: () => ring,
    total: () => total,
    subscribe(fn) {
      subs.add(fn);
      return () => subs.delete(fn);
    },
  };
}

/** What the phone view calls: focus from a tap, blur on Read. */
export interface ProxyHandle {
  focus(): void;
  blur(): void;
}

/** The slice of xterm the proxy follows (a fake in tests). */
export interface ProxyTerm {
  readonly cols: number;
  readonly rows: number;
  readonly element?: HTMLElement | undefined;
  readonly buffer: { readonly active: { readonly cursorX: number; readonly cursorY: number } };
  readonly modes?: { readonly bracketedPasteMode?: boolean; readonly applicationCursorKeysMode?: boolean };
  onWriteParsed?(cb: () => void): { dispose(): void };
  /** PHIN1: the terminal's own selection (Ctrl/Cmd+C copies it instead of sending ^C); absent in a fake. */
  hasSelection?(): boolean;
  getSelection?(): string;
}

export interface InputProxyProps {
  term: ProxyTerm;
  /** The grid viewport the element lives in (it is positioned against it). */
  viewport: HTMLElement;
  variant: ProxyVariant;
  send: (data: string) => boolean;
  handleRef: MutableRefObject<ProxyHandle | null>;
  log?: DebugLog | undefined;
  /** PHIN1: the element gained (true) or lost (false) focus; false again when it unmounts. */
  onFocusChange?: ((focused: boolean) => void) | undefined;
  /** PHIN2: the sticky Ctrl latch shared with the key bar; released when the element blurs. */
  mods?: StickyMods | undefined;
  /** PHIN2: the typed-line mirror shared with the key bar (else the core keeps its own). */
  line?: DirectLine | undefined;
  /** PHIN2: the harness and confirm host of the typed `/bg` guard (used when no shared `line` is given). */
  harness?: string | undefined;
  confirm?: ConfirmFn | null | undefined;
}

export function InputProxy(props: InputProxyProps) {
  const { term, viewport, variant, handleRef } = props;
  const elRef = useRef<HTMLTextAreaElement | HTMLInputElement | null>(null);
  const sendRef = useRef(props.send);
  sendRef.current = props.send;
  const termRef = useRef(term);
  termRef.current = term;
  const logRef = useRef(props.log);
  logRef.current = props.log;
  const focusRef = useRef(props.onFocusChange);
  focusRef.current = props.onFocusChange;
  const modsRef = useRef(props.mods);
  modsRef.current = props.mods;
  const confirmRef = useRef(props.confirm);
  confirmRef.current = props.confirm;
  const lineProp = props.line;
  const harnessProp = props.harness;
  const hasConfirm = props.confirm != null;

  // The element at the cursor cell, refreshed on every parsed write.
  useEffect(() => {
    const el = elRef.current;
    if (!el) return;
    const prevPos = viewport.style.position;
    const pos = getComputedStyle(viewport).position;
    const wasStatic = pos === "static" || pos === "";
    if (wasStatic) viewport.style.position = "relative";
    const place = (): void => {
      const t = termRef.current;
      const screen = t.element?.querySelector(".xterm-screen");
      if (!screen || t.cols <= 0 || t.rows <= 0) return;
      const s = screen.getBoundingClientRect();
      const v = viewport.getBoundingClientRect();
      const cw = s.width / t.cols;
      const ch = s.height / t.rows;
      const b = t.buffer.active;
      el.style.left = `${Math.round(s.left - v.left + b.cursorX * cw)}px`;
      el.style.top = `${Math.round(s.top - v.top + b.cursorY * ch)}px`;
    };
    place();
    const sub = termRef.current.onWriteParsed?.(place);
    return () => {
      sub?.dispose();
      if (wasStatic) viewport.style.position = prevPos;
    };
  }, [viewport, term, variant]);

  // The state machine, wired to native events (React's onBeforeInput is not the DOM one).
  useEffect(() => {
    const el = elRef.current;
    if (!el) return;
    const put = (v: string): void => {
      el.value = v;
      try {
        el.setSelectionRange(v.length, v.length);
      } catch {
        // an input type without a selection API
      }
    };
    const core: ProxyCore = createProxyCore({
      send: (d) => sendRef.current(d),
      variant,
      log: (e) => logRef.current?.push(e),
      setValue: put,
      getValue: () => el.value,
      hasSelection: () => termRef.current.hasSelection?.() === true,
      copy: () => {
        const text = termRef.current.getSelection?.() ?? "";
        if (text !== "") void tryWriteClipboardText(text);
      },
      mods: { consume: () => modsRef.current?.consume() === true },
      ...(lineProp ? { line: lineProp } : {}),
      ...(harnessProp !== undefined ? { harness: harnessProp } : {}),
      confirm: hasConfirm ? (o) => (confirmRef.current ? confirmRef.current(o) : Promise.resolve(false)) : null,
    });
    put(core.initialValue);
    const onBefore = (e: Event): void => {
      const ie = e as InputEvent;
      if (core.onBeforeInput({ inputType: ie.inputType, data: ie.data, isComposing: ie.isComposing })) e.preventDefault();
    };
    const onInput = (e: Event): void => {
      const ie = e as InputEvent;
      core.onInput({ inputType: ie.inputType, data: ie.data, isComposing: ie.isComposing, value: el.value });
    };
    const onStart = (): void => core.onCompositionStart();
    const onEnd = (e: Event): void => core.onCompositionEnd({ data: (e as CompositionEvent).data, value: el.value });
    const onKey = (e: Event): void => {
      const ke = e as KeyboardEvent;
      if (
        core.onKeyDown({
          key: ke.key,
          keyCode: ke.keyCode,
          ctrlKey: ke.ctrlKey,
          altKey: ke.altKey,
          metaKey: ke.metaKey,
          isComposing: ke.isComposing,
          shiftKey: ke.shiftKey,
        })
      ) {
        e.preventDefault();
      }
    };
    const onPaste = (e: Event): void => {
      e.preventDefault();
      const text = (e as ClipboardEvent).clipboardData?.getData("text/plain") ?? "";
      core.onPaste(text, termRef.current.modes?.bracketedPasteMode === true);
    };
    const onBlur = (): void => {
      core.reset();
      modsRef.current?.release();
      focusRef.current?.(false);
    };
    const onFocus = (): void => focusRef.current?.(true);
    const on: Array<[string, (e: Event) => void]> = [
      ["beforeinput", onBefore],
      ["input", onInput],
      ["compositionstart", onStart],
      ["compositionend", onEnd],
      ["keydown", onKey],
      ["paste", onPaste],
      ["blur", onBlur],
      ["focus", onFocus],
    ];
    for (const [n, f] of on) el.addEventListener(n, f);
    handleRef.current = {
      focus: () => el.focus({ preventScroll: true }),
      blur: () => el.blur(),
    };
    // A variant change re-attaches while the element may still hold focus: no focus event follows.
    if (document.activeElement === el) focusRef.current?.(true);
    return () => {
      for (const [n, f] of on) el.removeEventListener(n, f);
      handleRef.current = null;
      focusRef.current?.(false);
    };
  }, [variant, handleRef, lineProp, harnessProp, hasConfirm]);

  const { tag, attrs } = attrsFor(variant.attrs);
  return createElement(tag as string, {
    ...(attrs as Record<string, unknown>),
    ref: elRef,
    className: "kb-term-input",
    "data-kb-term-input": "",
    "data-testid": "kb-term-input",
    "aria-label": "Terminal input",
    ...(tag === "textarea" ? { rows: 1 } : {}),
  });
}

function useLog(log: DebugLog): { events: readonly DebugEvent[]; total: number } {
  const [, tick] = useState(0);
  useEffect(() => log.subscribe(() => tick((n) => n + 1)), [log]);
  return { events: log.events(), total: log.total() };
}

const AXES: Array<{ key: keyof ProxyVariant; label: string }> = [
  { key: "send", label: "send" },
  { key: "sentinel", label: "sentinel" },
  { key: "attrs", label: "attrs" },
];

export function InputDebugStrip(props: { log: DebugLog; variant: ProxyVariant; onVariant: (v: ProxyVariant) => void }) {
  const { log, variant, onVariant } = props;
  const { events, total } = useLog(log);
  // Buttons never take focus from the proxy: the keyboard must stay up.
  const keep = (e: { preventDefault(): void }): void => e.preventDefault();
  const text = events
    .map((e) => `${e.type}${e.inputType ? `/${e.inputType}` : ""} d=${e.data} k=${e.keyCode}${e.composing ? " C" : ""} -> ${e.bytes}`)
    .join("\n");
  return (
    <div className="kb-term-input-debug" data-testid="kb-term-input-debug">
      <div className="kb-term-input-debug__row">
        <span data-testid="kb-term-input-debug-total">bytes: {total}</span>
        {AXES.map((a) => (
          <label key={a.key}>
            {a.label}{" "}
            <select
              data-testid={`kb-term-input-variant-${a.key}`}
              value={variant[a.key]}
              onChange={(e) => onVariant({ ...variant, [a.key]: e.target.value } as ProxyVariant)}
            >
              {VARIANT_OPTIONS[a.key].map((o) => (
                <option key={o} value={o}>
                  {o}
                </option>
              ))}
            </select>
          </label>
        ))}
        <button
          type="button"
          className="kb-term-phone__btn"
          data-testid="kb-term-input-copy"
          onPointerDown={keep}
          onMouseDown={keep}
          onClick={() => {
            const json = JSON.stringify({ variant, total, events });
            void navigator.clipboard?.writeText(json).catch(() => undefined);
          }}
        >
          Copy log
        </button>
      </div>
      <pre className="kb-term-input-debug__log" data-testid="kb-term-input-debug-log">
        {text}
      </pre>
    </div>
  );
}
