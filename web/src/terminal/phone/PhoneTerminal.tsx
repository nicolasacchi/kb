// T17 — the phone view of one terminal: Watch (pointer scroller, pinch,
// horizontal pan over the real session grid) and Read (native scroll and
// selection over a snapshot of the buffer).
//
// Instance ownership: the `Terminal`, its socket and the reconnect state
// live in `TerminalPanel` ABOVE the mobile/desktop switch. This view only
// attaches the xterm element into its own host (`attachTerm`) and never
// disposes it, so a rotation across the mobile breakpoint re-parents the
// view without a reconnect or a re-seed.
//
// Read-only by construction: stdin is disabled, the helper textarea can
// never raise the soft keyboard, and nothing here writes to the socket or
// resizes the grid (the grid owner changes with the later composer unit).
// The renderer is the DOM one on a phone (the panel skips the WebGL addon
// when it creates the terminal on a phone-sized viewport).

import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import type { Terminal } from "@xterm/xterm";
import { ReadView } from "./ReadView";
import { FONT_STEPS, stepFont } from "./scrollerCore";
import { readFont } from "./fontSteps";
import { applyFont, usePhoneScroller, type ScrollTerm } from "./usePhoneScroller";
import "./phone.css";

export type PhoneMode = "watch" | "read";

/** The slice of the xterm Terminal this view uses (a fake in the dom test). */
export type PhoneTerm = ScrollTerm & {
  readonly cols: number;
  readonly textarea?: HTMLTextAreaElement | undefined;
  readonly buffer: Terminal["buffer"];
  readonly options: { fontSize?: number; disableStdin?: boolean; scrollback?: number };
  open(parent: HTMLElement): void;
  scrollToBottom(): void;
  onScroll?(cb: () => void): { dispose(): void };
  onWriteParsed?(cb: () => void): { dispose(): void };
  refresh?(start: number, end: number): void;
};

export interface PhoneProfile {
  cols: number;
  rows: number;
}

export interface PhoneStream {
  phase: string;
  applied: number;
  resets: number;
  gaps: number;
}

export interface PhoneTerminalProps {
  term: PhoneTerm | null;
  profile: PhoneProfile | null;
  /** The panel creates the xterm into this host (it exists before the term does). */
  hostRef: { current: HTMLDivElement | null };
  mode: PhoneMode;
  onMode: (m: PhoneMode) => void;
  stream: PhoneStream;
  caption: string;
  onClose?: () => void;
}

/**
 * Put the xterm into `host`: `open()` exactly once for a terminal that has
 * never been opened, else move its existing element (xterm must not be
 * opened twice).
 */
export function attachTerm(term: Pick<PhoneTerm, "element" | "open">, host: HTMLElement): void {
  const el = term.element;
  if (!el) {
    term.open(host);
  } else if (el.parentElement !== host) {
    host.appendChild(el);
  }
}

function setAttr(el: HTMLElement, name: string, value: string | number | undefined): void {
  if (value === undefined) {
    if (el.hasAttribute(name)) el.removeAttribute(name);
    return;
  }
  const s = String(value);
  if (el.getAttribute(name) !== s) el.setAttribute(name, s);
}

export function PhoneTerminal(props: PhoneTerminalProps) {
  const { term, profile, hostRef, mode, onMode, stream, caption, onClose } = props;
  const [wrap, setWrap] = useState<HTMLDivElement | null>(null);
  const [viewport, setViewport] = useState<HTMLDivElement | null>(null);
  const [font, setFont] = useState<number>(() => readFont());
  const [behind, setBehind] = useState(false);
  const termRef = useRef(term);
  termRef.current = term;

  // Attach (open once, else re-parent) and force read-only while mounted.
  useLayoutEffect(() => {
    const host = hostRef.current;
    if (!term || !host) return;
    attachTerm(term, host);
    const prevStdin = term.options.disableStdin;
    term.options.disableStdin = true;
    const ta = term.textarea;
    const prev = ta
      ? { readOnly: ta.readOnly, inputMode: ta.inputMode, tabIndex: ta.tabIndex }
      : null;
    if (ta) {
      ta.readOnly = true;
      ta.inputMode = "none";
      ta.tabIndex = -1;
    }
    return () => {
      // The terminal outlives this view: give it back as we found it. (It may
      // already be disposed when the panel itself is closing.)
      try {
        term.options.disableStdin = prevStdin ?? false;
      } catch {
        // disposed terminal: nothing left to restore
      }
      if (ta && prev) {
        ta.readOnly = prev.readOnly;
        ta.inputMode = prev.inputMode;
        ta.tabIndex = prev.tabIndex;
      }
    };
  }, [term, hostRef]);

  // The test-contract attributes: written only when the value changed.
  const refresh = useCallback((): void => {
    const t = termRef.current;
    if (!t || !viewport || !wrap) return;
    const b = t.buffer.active;
    setAttr(viewport, "data-kb-term-vy", b.viewportY);
    setAttr(viewport, "data-kb-term-base", b.baseY);
    setAttr(viewport, "data-kb-term-len", b.length);
    setAttr(viewport, "data-kb-term-font", t.options.fontSize);
    setAttr(viewport, "data-kb-term-cols", t.cols);
    setAttr(viewport, "data-kb-term-rows", t.rows);
    setAttr(viewport, "data-kb-term-sl", Math.round(wrap.scrollLeft));
    setBehind(b.viewportY < b.baseY);
  }, [viewport, wrap]);

  useEffect(() => {
    if (!term) return;
    refresh();
    const subs = [term.onScroll?.(refresh), term.onWriteParsed?.(refresh)];
    wrap?.addEventListener("scroll", refresh, { passive: true });
    return () => {
      for (const s of subs) s?.dispose();
      wrap?.removeEventListener("scroll", refresh);
    };
  }, [term, wrap, refresh]);

  usePhoneScroller({
    term: mode === "watch" ? term : null,
    wrap,
    viewport,
    onLongPress: () => onMode("read"),
    onActivity: refresh,
    onFont: setFont,
  });

  // Apply the chosen font to the grid (Watch only: a hidden xterm cannot
  // measure its cells) and re-measure the wrapper height.
  useEffect(() => {
    if (!term || mode !== "watch") return;
    if (applyFont(term, wrap, font)) refresh();
    term.refresh?.(0, Math.max(0, term.rows - 1));
    refresh();
  }, [term, wrap, font, mode, refresh]);

  // Keyboard never resizes the pty: lay out from the visual viewport only.
  useEffect(() => {
    const vv = typeof window === "undefined" ? undefined : window.visualViewport;
    if (!vv || !viewport) return;
    const root = viewport.closest<HTMLElement>("[data-kb-term-root]");
    if (!root) return;
    const sync = (): void => {
      root.style.setProperty("--kb-term-vvh", `${Math.round(vv.height)}px`);
    };
    sync();
    vv.addEventListener("resize", sync);
    vv.addEventListener("scroll", sync);
    return () => {
      vv.removeEventListener("resize", sync);
      vv.removeEventListener("scroll", sync);
      root.style.removeProperty("--kb-term-vvh");
    };
  }, [viewport]);

  const fontIdx = FONT_STEPS.indexOf(font as (typeof FONT_STEPS)[number]);
  return (
    <div className="kb-term-phone" data-kb-term-root="" data-testid="kb-terminal">
      <div className="kb-term-phone__bar">
        <span className="kb-term-phone__caption" data-testid="kb-term-caption">
          {caption}
        </span>
        <button
          type="button"
          className="kb-term-phone__btn"
          data-testid="kb-term-phone-mode"
          aria-pressed={mode === "read"}
          onClick={() => onMode(mode === "watch" ? "read" : "watch")}
        >
          {mode === "watch" ? "Read" : "Watch"}
        </button>
        <button
          type="button"
          className="kb-term-phone__btn"
          data-testid="kb-term-phone-font-down"
          aria-label="Smaller font"
          disabled={fontIdx === 0}
          onClick={() => setFont((f) => stepFont(f, -1))}
        >
          A-
        </button>
        <button
          type="button"
          className="kb-term-phone__btn"
          data-testid="kb-term-phone-font-up"
          aria-label="Larger font"
          disabled={fontIdx === FONT_STEPS.length - 1}
          onClick={() => setFont((f) => stepFont(f, 1))}
        >
          A+
        </button>
        {onClose ? (
          <button type="button" className="kb-term-phone__btn" data-testid="kb-term-close" onClick={onClose}>
            Close
          </button>
        ) : null}
      </div>
      <div className="kb-term-phone__watch" hidden={mode === "read"}>
        <div className="kb-term-phone__wrap" ref={setWrap}>
          <div
            className="kb-term-phone__viewport"
            data-kb-term-viewport=""
            data-kb-term-phase={stream.phase}
            data-kb-term-offset={stream.applied}
            data-kb-term-resets={stream.resets}
            data-kb-term-gaps={stream.gaps}
            ref={setViewport}
          >
            <div className="kb-term-phone__host" ref={hostRef} />
          </div>
        </div>
        {behind && mode === "watch" ? (
          <button
            type="button"
            className="kb-term-phone__btn kb-term-phone__jump"
            data-testid="kb-term-phone-jump"
            onClick={() => {
              term?.scrollToBottom();
              refresh();
            }}
          >
            Jump to latest
          </button>
        ) : null}
      </div>
      {mode === "read" && term ? (
        <ReadView
          read={() => termRef.current?.buffer.normal ?? term.buffer.normal}
          fontSize={font}
          cols={profile?.cols ?? term.cols}
          onWatch={() => onMode("watch")}
        />
      ) : null}
    </div>
  );
}
