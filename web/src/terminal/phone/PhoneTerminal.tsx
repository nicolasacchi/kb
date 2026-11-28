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

import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState, type ReactNode } from "react";
import type { Terminal } from "@xterm/xterm";
import { ReadView } from "./ReadView";
import { phoneFullscreenAvailable } from "../fullscreen";
import { CROPPED_LABEL, CROPPED_PHONE_HINT, isCroppedX } from "../cropHint";
import { useFullscreen } from "../useFullscreen";
import { FONT_STEPS, stepFont } from "./scrollerCore";
import { readFont } from "./fontSteps";
import { applyFont, measureCellHeight, usePhoneScroller, type ScrollTerm } from "./usePhoneScroller";
import type { PhoneMeasure } from "./fitPhone";
import { InputDebugStrip, InputProxy, createDebugLog, type ProxyHandle } from "./InputProxy";
import { DEFAULT_VARIANT, inputDebugEnabled, phoneDirectEnabled, readVariant, writeVariant, type ProxyVariant } from "./inputProxy";
import { createFollowGate } from "./cursorFollow";
import { useConfirmOptional } from "../../components/ConfirmProvider";
import { DirectInputContext, type DirectInput } from "./directInput";
import { createDirectLine } from "./directLine";
import { createStickyMods } from "./stickyMods";
import { useCursorFollow } from "./useCursorFollow";
import { StatusChips, TermHeader } from "../TermHeader";
import type { StatusChip } from "../statusChips";
import { TermStateOverlay } from "../TermStateOverlay";
import "./phone.css";

const NO_CHIPS: StatusChip[] = [];

/** How long a Full tap waits for the browser to enter full screen before saying it did not. */
const FULLSCREEN_GRACE_MS = 1500;
const FULLSCREEN_FAILED = "Full screen is not available here.";

export type PhoneMode = "watch" | "read";

/** The slice of the xterm Terminal this view uses (a fake in the dom test). */
export type PhoneTerm = ScrollTerm & {
  readonly cols: number;
  readonly textarea?: HTMLTextAreaElement | undefined;
  readonly buffer: Terminal["buffer"];
  readonly options: { fontSize?: number; disableStdin?: boolean; scrollback?: number };
  /** PHIN0: the terminal's private modes (xterm 6); absent in a fake. */
  readonly modes?: { readonly bracketedPasteMode?: boolean; readonly applicationCursorKeysMode?: boolean };
  open(parent: HTMLElement): void;
  scrollToBottom(): void;
  onScroll?(cb: () => void): { dispose(): void };
  onWriteParsed?(cb: () => void): { dispose(): void };
  refresh?(start: number, end: number): void;
  /** PHIN1: cursor-follow also listens to the cursor moving (xterm 6 has it; a fake may not). */
  onCursorMove?(cb: () => void): { dispose(): void };
  /** PHIN1: the terminal's own selection, for the proxy's Ctrl/Cmd+C. */
  hasSelection?(): boolean;
  getSelection?(): string;
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
  /** BSTATE: the state overlay's words (the panel passes its whole session state). */
  exitCode?: number | null;
  blockedReason?: string | null;
  attempt?: number;
}

export interface PhoneTerminalProps {
  /** The harness name the title row shows (default "Terminal"). */
  harness?: string;
  term: PhoneTerm | null;
  profile: PhoneProfile | null;
  /** The panel creates the xterm into this host (it exists before the term does). */
  hostRef: { current: HTMLDivElement | null };
  mode: PhoneMode;
  onMode: (m: PhoneMode) => void;
  stream: PhoneStream;
  caption: string;
  onClose?: () => void;
  /** BSTATE: "Reconnect now" on the state overlay (the session's kick). */
  onReconnect?: () => void;
  /** BSTATE: true until the stream has been live once (first connect shows the connecting card). */
  overlayFirstConnect?: boolean;
  /** BSTATE: the sentence the one polite status region says (the panel derives it). */
  announce?: string;
  /** FUTERMLIST: extra title-bar controls (the End button, the terminal switcher). */
  headerExtra?: ReactNode;
  /** BHEAD: controls rendered before `headerExtra` in the extra row (palette, switcher). */
  headerActions?: ReactNode;
  /** BHEAD: the terminal's working-directory basename, shown beside the harness. */
  project?: string;
  /** BHEAD: the typed live status chips (the panel header's single home, D7). */
  chips?: StatusChip[];
  /**
   * T18 — the Drive dock, rendered below the Watch grid (kept mounted, only
   * hidden, in Read mode so a half-typed message survives a mode switch).
   * `measure` reports the visible Watch area and one cell for "Fit to phone".
   */
  renderDrive?: (api: { measure: () => PhoneMeasure | null; fullscreen: boolean }) => ReactNode;
  /**
   * PHIN0: one WS input frame (false when it was not sent). Only used behind
   * the hidden `?direct=1` / `kb.term.phoneDirect` pref; without it (or with
   * the pref off) the view is the read-only one, untouched.
   */
  sendInput?: (data: string) => boolean;
}

function safeLocalStorage(): Storage | null {
  try {
    return typeof window === "undefined" ? null : window.localStorage;
  } catch {
    return null;
  }
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
  const { term, profile, hostRef, mode, onMode, stream, caption, onClose, renderDrive, harness, headerExtra, headerActions, project, chips, sendInput, onReconnect, overlayFirstConnect, announce } = props;
  const [wrap, setWrap] = useState<HTMLDivElement | null>(null);
  const [viewport, setViewport] = useState<HTMLDivElement | null>(null);
  const [font, setFont] = useState<number>(() => readFont());
  const [behind, setBehind] = useState(false);
  // The grid is wider than the screen (Watch pans it until Fit to phone).
  const [cropped, setCropped] = useState(false);
  const termRef = useRef(term);
  termRef.current = term;
  // PHIN0: the hidden direct-typing pref, read once per mount (`?direct=` also writes it).
  const [directPref] = useState<boolean>(() =>
    typeof window === "undefined" ? false : phoneDirectEnabled(window.location.search, safeLocalStorage()),
  );
  const proxyOn = directPref && sendInput !== undefined && mode === "watch";
  const [debugLog] = useState(() =>
    directPref && typeof window !== "undefined" && inputDebugEnabled(window.location.search) ? createDebugLog() : null,
  );
  const [variant, setVariant] = useState<ProxyVariant>(() => (directPref ? readVariant(safeLocalStorage()) : DEFAULT_VARIANT));
  const proxyHandle = useRef<ProxyHandle | null>(null);
  // PHIN2: the sticky Ctrl latch and the typed-line mirror live as long as the view and are shared by
  // the proxy and the key bar (through the context below), so both feed ONE ordered input path.
  const confirm = useConfirmOptional();
  const [mods] = useState(() => createStickyMods());
  const [line] = useState(() => createDirectLine());
  line.configure(harness || "", confirm);
  const sendRef = useRef(sendInput);
  sendRef.current = sendInput;
  const send = useCallback(
    (d: string): boolean => {
      const ok = sendRef.current?.(d) ?? false;
      if (!ok) mods.release();
      return ok;
    },
    [mods],
  );
  const direct = useMemo<DirectInput | null>(
    () =>
      proxyOn
        ? {
            send,
            mods,
            appCursor: () => termRef.current?.modes?.applicationCursorKeysMode === true,
            harness: harness || "",
            confirm,
            line,
          }
        : null,
    [proxyOn, send, mods, harness, confirm, line],
  );
  // The latch never outlives what it was armed for: Read mode, a stream that is not live, unmount.
  useEffect(() => {
    if (!proxyOn || stream.phase !== "live") mods.release();
  }, [proxyOn, stream.phase, mods]);
  useEffect(() => () => mods.release(), [mods]);
  // PHIN1: the Watch box (what cursor-follow keeps the cursor inside), the proxy's focus
  // and the 2 s "the operator is panning" gate shared by the scroller and the follow.
  const [watch, setWatch] = useState<HTMLDivElement | null>(null);
  const [proxyFocused, setProxyFocused] = useState(false);
  const [followGate] = useState(() => createFollowGate({ now: () => performance.now() }));
  // FUTERMFULL — element fullscreen of the sheet's root, only where the browser
  // has it (Android Chrome). No overlay here (the 100dvh sheet is the maximum)
  // and no Escape lock (the phone has no Esc key to hold).
  const [root, setRoot] = useState<HTMLDivElement | null>(null);
  const fs = useFullscreen(root, "phone", { overlay: false, lockEscape: false });
  const canFullscreen = typeof document !== "undefined" && phoneFullscreenAvailable(document);
  // A refused requestFullscreen (no user activation, a policy) leaves no state change at all:
  // say so after a short grace instead of failing silently.
  const [fsNote, setFsNote] = useState<string | null>(null);
  const fsOnRef = useRef(false);
  fsOnRef.current = fs.on;
  useEffect(() => {
    if (fs.on) setFsNote(null);
  }, [fs.on]);
  const fsTimer = useRef<number | null>(null);
  useEffect(
    () => () => {
      if (fsTimer.current !== null) window.clearTimeout(fsTimer.current);
    },
    [],
  );
  const toggleFullscreen = (): void => {
    const entering = !fs.on;
    if (fsTimer.current !== null) window.clearTimeout(fsTimer.current);
    fsTimer.current = null;
    setFsNote(null);
    fs.toggle();
    if (!entering) return;
    fsTimer.current = window.setTimeout(() => {
      fsTimer.current = null;
      if (!fsOnRef.current) setFsNote(FULLSCREEN_FAILED);
    }, FULLSCREEN_GRACE_MS);
  };

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
    setCropped(isCroppedX({ scrollWidth: viewport.scrollWidth, clientWidth: wrap.clientWidth }));
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

  // A grid change (Fit to phone, another viewer's take) changes the width the pan
  // has to cover; the screen repaints after the resize, so look again shortly after.
  useEffect(() => {
    const id = window.setTimeout(refresh, 100);
    return () => window.clearTimeout(id);
  }, [profile?.cols, profile?.rows, refresh]);

  usePhoneScroller({
    term: mode === "watch" ? term : null,
    wrap,
    viewport,
    onLongPress: () => onMode("read"),
    onActivity: refresh,
    onFont: setFont,
    ...(proxyOn ? { onTapFocus: () => proxyHandle.current?.focus(), onManual: () => followGate.noteManual() } : {}),
  });

  // PHIN1: with direct typing on and the proxy focused, the cursor cell stays visible above the keyboard.
  useCursorFollow({ term, wrap, watch, enabled: proxyOn && proxyFocused, gate: followGate });

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

  // T18 — what "Fit to phone" fills: the wrapper's visible width and the Watch
  // area's allocated height, over one rendered cell. Measured on demand (a click),
  // never observed: nothing here may drive a resize by itself.
  const measure = useCallback((): PhoneMeasure | null => {
    const t = termRef.current;
    if (!t || !wrap) return null;
    const screen = t.element?.querySelector(".xterm-screen");
    if (!screen || t.cols <= 0) return null;
    return {
      visibleWidth: wrap.clientWidth,
      visibleHeight: wrap.parentElement?.clientHeight ?? 0,
      cellWidth: screen.getBoundingClientRect().width / t.cols,
      cellHeight: measureCellHeight(t),
    };
  }, [wrap]);

  const fontIdx = FONT_STEPS.indexOf(font as (typeof FONT_STEPS)[number]);
  return (
    <div className="kb-term-phone" data-kb-term-root="" data-testid="kb-terminal" ref={setRoot}>
      <TermHeader
        compact
        harness={harness || "Terminal"}
        project={project}
        phase={stream.phase}
        chips={chips ?? NO_CHIPS}
        actions={headerActions}
        extra={headerExtra}
      >
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
        {canFullscreen ? (
          <button
            type="button"
            className="kb-term-phone__btn"
            data-testid="kb-term-phone-fullscreen"
            aria-pressed={fs.on}
            aria-label={fs.on ? "Exit full screen" : "Full screen"}
            onClick={toggleFullscreen}
          >
            {fs.on ? "Exit" : "Full"}
          </button>
        ) : null}
        {onClose ? (
          <button type="button" className="kb-term-phone__btn" data-testid="kb-term-close" onClick={onClose}>
            Close
          </button>
        ) : null}
      </TermHeader>
      <div className="kb-term-phone__sub">
        <span className="kb-term-phone__caption" data-testid="kb-term-caption">
          {caption}
        </span>
        <StatusChips chips={chips ?? NO_CHIPS} />
        {fsNote !== null ? (
          <span className="kb-term-phone__crop" data-testid="kb-term-phone-fs-note" role="status">
            {fsNote}
          </span>
        ) : null}
        {cropped && mode === "watch" ? (
          <span className="kb-term-phone__crop" data-testid="kb-term-phone-cropped" title={CROPPED_PHONE_HINT}>
            {CROPPED_LABEL}: pan, or Fit to phone
          </span>
        ) : null}
      </div>
      <div className="kb-term-phone__watch" hidden={mode === "read"} ref={setWatch}>
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
            {proxyOn && term && viewport ? (
              <InputProxy
                key={`${variant.send}/${variant.sentinel}/${variant.attrs}`}
                term={term}
                viewport={viewport}
                variant={variant}
                send={send}
                mods={mods}
                line={line}
                harness={harness || ""}
                confirm={confirm}
                handleRef={proxyHandle}
                onFocusChange={setProxyFocused}
                log={debugLog ?? undefined}
              />
            ) : null}
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
        <div className="kb-term__announce" role="status" aria-live="polite" data-testid="kb-term-announce">
          {announce ?? ""}
        </div>
        <TermStateOverlay
          phase={stream.phase}
          caption={caption}
          exitCode={stream.exitCode ?? null}
          blockedReason={stream.blockedReason ?? null}
          attempt={stream.attempt ?? 0}
          firstConnect={overlayFirstConnect ?? true}
          onReconnect={onReconnect}
          onBack={onClose}
          compact
        />
      </div>
      {proxyOn && debugLog ? (
        <InputDebugStrip
          log={debugLog}
          variant={variant}
          onVariant={(v) => {
            setVariant(v);
            writeVariant(safeLocalStorage(), v);
          }}
        />
      ) : null}
      {mode === "read" && term ? (
        <ReadView
          read={() => termRef.current?.buffer.normal ?? term.buffer.normal}
          fontSize={font}
          cols={profile?.cols ?? term.cols}
          onWatch={() => onMode("watch")}
        />
      ) : null}
      {renderDrive ? (
        <div hidden={mode === "read"}>
          <DirectInputContext.Provider value={direct}>{renderDrive({ measure, fullscreen: fs.on })}</DirectInputContext.Provider>
        </div>
      ) : null}
    </div>
  );
}
