// T14 — the desktop terminal panel for `/sessions/:sid`.
//
// Lazy chunk: this module (and only this one) imports `@xterm/*` and
// `@xterm/xterm/css/xterm.css`; it is fetched only after the user
// clicks "Open terminal" (see `TerminalPanel.lazy.ts`). The terminal
// is built from the Profile frame (never constants), the Unicode
// graphemes width table activates before the first write, and the
// stream/connection machine (`session.ts`) drives join/resume against
// `GET /api/terms/{id}/ws?ticket=&from=`.
//
// Reply guards (`replyGuards.ts`) ensure no terminal query is ever
// answered by the browser. FUTERMFIT: the panel fits the whole grid by font
// size (both axes, `fitHost.ts`) and only ever sends a grid change from the
// explicit "Fit to window" / "Release grid" clicks (`GridControls.tsx`); a
// window resize, opening the panel and focusing it refit the FONT only.
// `sizechanged` updates the chip and the caption. No `focus()` on mount
// (click focuses), no Escape binding, no global key handler.
//
// T17 — on a phone (`useIsMobile()`) the SAME terminal, socket and
// reconnect state render through `PhoneTerminal` inside a `Sheet`. The
// instance lives here, above the mobile/desktop switch, so a rotation
// across the breakpoint re-parents the xterm element and never
// reconnects or re-seeds. A terminal created on a phone gets the DOM
// renderer only (no WebGL addon: context loss and memory pressure on
// phones, and a real phone GPU is untested), a 20,000-line scrollback
// cap and the persisted phone font; it stays that way if the viewport
// later grows (no mid-session addon load).

import { useCallback, useEffect, useRef, useState } from "react";
import { Sheet, type Detent } from "@kb/ui/Sheet";
import { useIsMobile } from "../hooks/useIsMobile";
import { Terminal, type IFunctionIdentifier } from "@xterm/xterm";
import "@xterm/xterm/css/xterm.css";
import { ImageAddon } from "@xterm/addon-image";
import { WebglAddon } from "@xterm/addon-webgl";
import { UnicodeGraphemesAddon } from "@xterm/addon-unicode-graphemes";
import { TermSession, type SessionState } from "./session";
import { mintTicket } from "../api/terms";
import { installReplyGuards } from "./replyGuards";
import { profileMatchesBundle } from "./pins";
import { FIT_MAX_INLINE, FIT_MIN_INLINE, type FitResult } from "./fit";
import { fitHostNow } from "./fitHost";
import { measureCellRatio } from "./cellProbe";
import { fontGate, TERM_FONT_FAMILY } from "./fonts";
import { readTheme } from "./theme";
import { GridControls } from "./GridControls";
import { chipText } from "./gridOwner";
import type { WindowMeasure } from "./fitWindow";
import { THEME_APPLIED_EVENT } from "@kb/ui/themes/apply";
import { attachGfx, type GfxHandle, type OpenPicture } from "./gfx";
import { Lightbox } from "./Lightbox";
import { countHits, findNext, type FindHit } from "./find";
import { useTerms } from "./useTerms";
import type { ImageDefFrame, ProfileFrame } from "./wire";
import type { TerminalPanelProps } from "./TerminalPanel.lazy";
import { osc52ClipboardText, writeClipboardText } from "./clipboard";
import { PhoneTerminal, attachTerm, type PhoneMode } from "./phone/PhoneTerminal";
import { DriveDock } from "./phone/DriveDock";
import { terminalOwnsEscape } from "./phone/escape";
import { readFont } from "./phone/fontSteps";
import { scrollbackFor } from "./phone/scrollback";
import { bindNetworkKicks } from "./networkKicks";
import { createPortal } from "react-dom";
import "./terminal.css";

const TRANSIENT_MS = 2_000;

/** e2e seam: `container.__kbTerm` exposes the live instance (no window global). */
type TermSeam = HTMLElement & { __kbTerm?: Terminal };

/** xterm 6 parser params are `(number | number[])[]` (nested arrays are
 *  sub-parameters); the reply-guard seam works on a flat `number[]`. */
function flattenParams(params: (number | number[])[]): number[] {
  const out: number[] = [];
  for (const p of params) {
    if (Array.isArray(p)) out.push(...p);
    else out.push(p);
  }
  return out;
}

export function TerminalPanel(props: TerminalPanelProps) {
  const isMobile = useIsMobile();
  const isMobileRef = useRef(isMobile);
  isMobileRef.current = isMobile;
  // Whether the live terminal was created for a phone (renderer, scrollback).
  const createdPhoneRef = useRef(false);
  const [detent, setDetent] = useState<Detent>("full");
  const [phoneMode, setPhoneMode] = useState<PhoneMode>("watch");
  const [termInst, setTermInst] = useState<Terminal | null>(null);
  const hostRef = useRef<HTMLDivElement | null>(null);
  const phoneHostRef = useRef<HTMLDivElement | null>(null);
  const termRef = useRef<Terminal | null>(null);
  const sessionRef = useRef<TermSession | null>(null);
  const lastTypedRef = useRef(0);
  // T18 — the viewer id the tap's hello named for THIS socket (the Drive layer reports it on its REST input calls).
  const [viewer, setViewer] = useState<string | null>(null);
  const [state, setState] = useState<SessionState>({
    phase: "connecting",
    epoch: null,
    applied: 0,
    resets: 0,
    gaps: 0,
    attempt: 0,
    caption: "connecting",
    exitCode: null,
    blockedReason: null,
  });
  const [grid, setGrid] = useState<{ cols: number; rows: number } | null>(null);
  // FUTERMFIT — the last fit (font and crop, for the chip and the hint), the
  // in-panel status line, and the profile palette the theme re-read starts from.
  const [fit, setFit] = useState<FitResult | null>(null);
  const [status, setStatus] = useState<string | null>(null);
  const profileThemeRef = useRef<unknown>(null);
  const gridRef = useRef<{ cols: number; rows: number } | null>(null);
  const [scrollback, setScrollback] = useState(0);
  const [widthActive, setWidthActive] = useState(false);
  const [transient, setTransient] = useState<string | null>(null);
  const [findOpen, setFindOpen] = useState(false);
  const [findQuery, setFindQuery] = useState("");
  const [findNote, setFindNote] = useState("");
  const findHitRef = useRef<FindHit>({ line: 0, col: -1, length: 0 });
  // T15 — the picture layer for the CURRENT xterm instance (null before the
  // Profile builds the terminal and after teardown).
  const gfxRef = useRef<GfxHandle | null>(null);
  const [gfx, setGfx] = useState<GfxHandle | null>(null);
  const [picture, setPicture] = useState<OpenPicture | null>(null);
  const { terms } = useTerms();
  const termRow = terms.find((t) => t.id === props.termId);
  const rowState = termRow?.state ?? null;
  const gridOwner = termRow?.grid_owner ?? null;

  // Fit the live terminal into the desktop host (font only; never a take-size).
  const fitDesktop = (term: Terminal, host: HTMLElement): void => {
    const g = gridRef.current;
    if (!g) return;
    const r = fitHostNow(term, host, g, { min: FIT_MIN_INLINE, max: FIT_MAX_INLINE });
    if (!r) return;
    setFit((prev) =>
      prev && prev.fontSize === r.fontSize && prev.cropped.x === r.cropped.x && prev.cropped.y === r.cropped.y ? prev : r,
    );
  };
  const say = useCallback((message: string): void => setStatus(message), []);
  const measureWindow = useCallback((): WindowMeasure | null => {
    const host = hostRef.current;
    const term = termRef.current;
    if (!host || !term || !(host.clientWidth > 0)) return null;
    const r = measureCellRatio(document, term.options.fontFamily || "monospace");
    return { width: host.clientWidth, height: host.clientHeight, cellW: r.cellW, cellH: r.cellH };
  }, []);

  useEffect(() => {
    let dead = false;
    let transientTimer: number | null = null;

    const writeClipboardIfRecent = (data: string): void => {
      const text = osc52ClipboardText(data, lastTypedRef.current, Date.now());
      if (text !== null) void writeClipboardText(text);
    };

    const disposeGfx = (): void => {
      gfxRef.current?.dispose();
      gfxRef.current = null;
      setGfx(null);
      setPicture(null);
    };

    const createTerminal = (profile: ProfileFrame): boolean => {
      const phone = isMobileRef.current;
      // The view mounts before the Profile arrives, so its host exists.
      const host = phone ? phoneHostRef.current : hostRef.current;
      if (!host) return false;
      if (!profileMatchesBundle(profile)) {
        sessionRef.current?.block("viewer build differs from session profile");
        return false;
      }
      const term = new Terminal({
        cols: profile.cols,
        rows: profile.rows,
        scrollback: scrollbackFor(profile, phone),
        scrollOnEraseInDisplay: true,
        allowProposedApi: true,
        convertEol: false,
        cursorBlink: false,
        // Initial size only; `fitDesktop` picks the real one from the host.
        fontSize: phone
          ? readFont()
          : Math.max(
              FIT_MIN_INLINE,
              parseFloat(getComputedStyle(document.documentElement).getPropertyValue("--fs-md")) || 14,
            ),
        fontFamily: TERM_FONT_FAMILY,
        theme: readTheme(profile.theme),
      });
      profileThemeRef.current = profile.theme;
      // The host paints the terminal's own background (no second black).
      host.style.background = term.options.theme?.background ?? "";
      attachTerm(term, host);
      term.loadAddon(new UnicodeGraphemesAddon());
      const key = term.unicode.versions.find((k) => /grapheme/i.test(k));
      if (!key) {
        sessionRef.current?.block("width table unavailable");
        term.dispose();
        return false;
      }
      term.unicode.activeVersion = key;
      term.loadAddon(new ImageAddon({ enableSizeReports: false }));
      if (!phone) {
        try {
          const webgl = new WebglAddon();
          webgl.onContextLoss(() => webgl.dispose());
          term.loadAddon(webgl);
        } catch {
          // DOM renderer fallback.
        }
      }
      // T15: the placeholder renderer, media cache and tap-to-open layer.
      const handle = attachGfx(term, {
        compact: scrollbackFor(profile, phone) <= 20_000,
        bg: term.options.theme?.background ?? "",
        onOpenPicture: (p) => setPicture(p),
      });
      gfxRef.current = handle;
      setGfx(handle);
      // xterm's IFunctionIdentifier: prefix/intermediates are OPTIONAL
      // and validated when present (prefix 0x3c-0x3f, intermediates
      // 0x20-0x2f); every other value — including the empty string —
      // throws. Omit empty keys instead of sending them as "".
      const functionId = (prefix: string, intermediates: string, final: string): IFunctionIdentifier => {
        const id: IFunctionIdentifier = { final };
        if (prefix !== "") id.prefix = prefix;
        if (intermediates !== "") id.intermediates = intermediates;
        return id;
      };
      installReplyGuards({
        registerCsiHandler: (prefix, intermediates, final, handler) =>
          term.parser.registerCsiHandler(functionId(prefix, intermediates, final), (params) =>
            handler(flattenParams(params)),
          ),
        registerOscHandler: (code, handler) => term.parser.registerOscHandler(code, handler),
        registerDcsHandler: (prefix, intermediates, final, handler) =>
          term.parser.registerDcsHandler(functionId(prefix, intermediates, final), (data, param) =>
            handler(flattenParams(param), data),
          ),
      });
      term.parser.registerOscHandler(52, (data) => {
        if (data.includes("?")) return true;
        // Set: gated write, then swallow (return true) so xterm's
        // default OSC 52 handling never reaches the clipboard.
        writeClipboardIfRecent(data);
        return true;
      });
      term.attachCustomKeyEventHandler((ev) => {
        // xterm consults this handler for keydown, keypress AND keyup. A
        // Shift+Enter must be swallowed on all three (the keypress would
        // otherwise still emit a bare \r), but the CSI-u is sent once, on
        // the keydown.
        if (
          ev.key === "Enter" &&
          ev.shiftKey &&
          !ev.ctrlKey &&
          !ev.metaKey &&
          !ev.altKey
        ) {
          if (ev.type === "keydown") {
            ev.preventDefault();
            if (sessionRef.current?.sendInput("\u001b[13;2u") ?? false) {
              lastTypedRef.current = Date.now();
            }
          }
          return false;
        }
        return true;
      });
      term.onData((data) => {
        if (sessionRef.current?.sendInput(data) ?? false) lastTypedRef.current = Date.now();
      });
      term.onBinary((bytes) => {
        if (sessionRef.current?.sendInput(bytes) ?? false) {
          lastTypedRef.current = Date.now();
        }
      });
      // Phone: the font is the persisted step (PhoneTerminal owns it); the
      // grid is never fitted (the session grid pans instead).
      createdPhoneRef.current = phone;
      termRef.current = term;
      setTermInst(term);
      gridRef.current = { cols: profile.cols, rows: profile.rows };
      setGrid(gridRef.current);
      if (!phone) fitDesktop(term, host);
      setScrollback(scrollbackFor(profile, phone));
      setWidthActive(true);
      return true;
    };

    const sink = {
      write: (data: Uint8Array, done: () => void): void => {
        const term = termRef.current;
        if (!term) {
          done();
          return;
        }
        term.write(data, done);
      },
      reset: (): void => {
        termRef.current?.reset();
      },
      resize: (cols: number, rows: number): void => {
        const term = termRef.current;
        if (!term) return;
        if (term.cols !== cols || term.rows !== rows) term.resize(cols, rows);
        // Keep the chip, the observer's fit input and the fontSize in
        // step with the grid the size owner just set.
        gridRef.current = { cols, rows };
        setGrid(gridRef.current);
        // Phone: the grid is never fitted (the session grid pans instead).
        const host = isMobileRef.current ? null : hostRef.current;
        if (host) fitDesktop(term, host);
      },
    };

    const session = new TermSession({
      termId: props.termId,
      sink,
      effects: {
        openSocket: (url, handlers) => {
          const ws = new WebSocket(url);
          ws.binaryType = "arraybuffer";
          ws.onmessage = (ev: MessageEvent) => {
            const data = ev.data as ArrayBuffer | string;
            if (data instanceof ArrayBuffer) handlers.onMessageBinary(new Uint8Array(data));
            else handlers.onMessageText(String(data));
          };
          ws.onclose = (ev: CloseEvent) => handlers.onClose(ev.code);
          ws.onerror = () => {
            // Errors surface as onclose; nothing additional to do.
          };
          return { send: (d) => ws.send(d), close: (code = 1000) => ws.close(code) };
        },
        mintTicket: (id, signal) => mintTicket(id, signal),
        now: () => Date.now(),
        setTimer: (fn, ms) => window.setTimeout(fn, ms),
        clearTimer: (h) => window.clearTimeout(h as number),
        random: () => Math.random(),
      },
      callbacks: {
        onState: (s) => {
          if (!dead) setState(s);
          if (s.phase === "live") gfxRef.current?.markLive();
        },
        onTransient: (message) => {
          if (dead) return;
          setTransient(message);
          if (transientTimer !== null) window.clearTimeout(transientTimer);
          transientTimer = window.setTimeout(() => setTransient(null), TRANSIENT_MS);
        },
        onProfile: (profile) => {
          if (dead) return;
          const existing = termRef.current;
          if (!existing) {
            createTerminal(profile);
            return;
          }
          if (
            profile.cols !== existing.cols ||
            profile.rows !== existing.rows ||
            scrollbackFor(profile, createdPhoneRef.current) !== existing.options.scrollback
          ) {
            disposeGfx();
            existing.dispose();
            termRef.current = null;
            setTermInst(null);
            if (createTerminal(profile)) sessionRef.current?.rejoinFull();
          }
        },
        onImageDef: (def: ImageDefFrame) => {
          gfxRef.current?.onImageDef(def);
        },
        onGfxReset: (gen: number) => {
          gfxRef.current?.onGfxReset(gen);
        },
        onFullJoin: () => {
          gfxRef.current?.onFullJoin();
        },
        onSizeChanged: (cols, rows, by) => {
          if (dead) return;
          setTransient(`grid ${cols}x${rows} by ${by}`);
          if (transientTimer !== null) window.clearTimeout(transientTimer);
          transientTimer = window.setTimeout(() => setTransient(null), TRANSIENT_MS);
        },
        onClipboard: (dataB64) => {
          writeClipboardIfRecent(`c;${dataB64}`);
        },
        onViewer: (v) => {
          if (!dead) setViewer(v);
        },
      },
    });
    sessionRef.current = session;
    // FUTERMFIT: the first `new Terminal` must measure the real webfont, so the
    // stream opens after the (bounded) font gate. A face that arrives after
    // the bound re-assigns the family (a changed value makes xterm re-measure)
    // and refits.
    const gate = fontGate(typeof document !== "undefined" ? document.fonts : undefined);
    void gate.ready.then((inTime) => {
      if (dead) return;
      session.start();
      if (inTime) return;
      void gate.done.then(() => {
        const term = termRef.current;
        if (dead || !term) return;
        term.options.fontFamily = `${TERM_FONT_FAMILY}, monospace`;
        term.clearTextureAtlas();
        const host = isMobileRef.current ? null : hostRef.current;
        if (host) fitDesktop(term, host);
      });
    });

    // T17: a returning network (`online`, a bfcache `pageshow`, the tab
    // turning visible) skips a pending reconnect backoff once.
    const unbindKicks = bindNetworkKicks(window, document, () => session.kick());

    return () => {
      dead = true;
      unbindKicks();
      if (transientTimer !== null) window.clearTimeout(transientTimer);
      session.dispose();
      sessionRef.current = null;
      disposeGfx();
      const term = termRef.current;
      termRef.current = null;
      term?.dispose();
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [props.termId]);

  // Desktop host wiring (click focuses, refit on resize, the e2e seam). A
  // phone view owns its own host; switching modes re-parents the SAME
  // xterm element and never touches the socket.
  useEffect(() => {
    const term = termInst;
    const host = hostRef.current;
    if (!term || isMobile || !host) return;
    attachTerm(term, host);
    (host as TermSeam).__kbTerm = term;
    const onHostClick = (): void => term.focus();
    host.addEventListener("click", onHostClick);
    let fitTimer: number | null = null;
    const refit = (): void => fitDesktop(term, host);
    refit();
    const observer = new ResizeObserver(() => {
      if (fitTimer !== null) window.clearTimeout(fitTimer);
      fitTimer = window.setTimeout(refit, 100);
    });
    observer.observe(host);
    // A closed <details> hands the host zero width; opening it refits.
    const details = host.closest("details");
    details?.addEventListener("toggle", refit);
    return () => {
      observer.disconnect();
      details?.removeEventListener("toggle", refit);
      if (fitTimer !== null) window.clearTimeout(fitTimer);
      host.removeEventListener("click", onHostClick);
      (host as TermSeam).__kbTerm = undefined;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [termInst, isMobile]);

  // The tokens the theme reads are per-theme: re-read when the SPA theme changes.
  useEffect(() => {
    const term = termInst;
    if (!term) return;
    const onTheme = (): void => {
      term.options.theme = readTheme(profileThemeRef.current);
      const host = isMobileRef.current ? phoneHostRef.current : hostRef.current;
      const bg = term.options.theme.background ?? "";
      if (host) host.style.background = bg;
      gfxRef.current?.setTheme(bg);
    };
    window.addEventListener(THEME_APPLIED_EVENT, onTheme);
    return () => window.removeEventListener(THEME_APPLIED_EVENT, onTheme);
  }, [termInst]);

  const closePanel = (): void => {
    if (props.onClose) {
      props.onClose();
      return;
    }
    sessionRef.current?.dispose();
    sessionRef.current = null;
    gfxRef.current?.dispose();
    gfxRef.current = null;
    setGfx(null);
    setPicture(null);
    termRef.current?.dispose();
    termRef.current = null;
    setTermInst(null);
    setGrid(null);
  };

  const runFind = (dir: 1 | -1): void => {
    const term = termRef.current;
    if (!term) return;
    const buf = term.buffer.active;
    const source = {
      length: buf.length,
      lineToString: (y: number): string => buf.getLine(y)?.translateToString(true) ?? "",
    };
    if (!findQuery) {
      setFindNote("");
      return;
    }
    const hit = findNext(source, findQuery, findHitRef.current, dir);
    if (!hit) {
      setFindNote(`0/${countHits(source, findQuery)}`);
      return;
    }
    findHitRef.current = hit;
    const viewportY = hit.line - buf.baseY;
    if (viewportY < 0 || viewportY >= term.rows) {
      term.scrollLines(viewportY - Math.floor(term.rows / 2));
    }
    term.select(hit.col, Math.min(Math.max(hit.line - buf.baseY, 0), term.rows - 1), hit.length);
    setFindNote(`${countHits(source, findQuery)} matches`);
  };

  if (isMobile) {
    // Portaled so a collapsed ancestor <details> can never hide a modal sheet
    // (and strand its body scroll lock) when the viewport crosses the breakpoint.
    return createPortal(
      <Sheet
        open
        detent={detent}
        onDetentChange={setDetent}
        full="92vh"
        title="Terminal"
        onClose={closePanel}
        ignoreEscape={terminalOwnsEscape}
      >
        <PhoneTerminal
          term={termInst}
          profile={grid}
          hostRef={phoneHostRef}
          mode={phoneMode}
          onMode={setPhoneMode}
          stream={state}
          caption={transient ?? state.caption}
          onClose={closePanel}
          renderDrive={({ measure }) => (
            <DriveDock
              termId={props.termId}
              harness={props.harness}
              viewer={viewer}
              row={termRow}
              measure={measure}
              onTyped={() => {
                lastTypedRef.current = Date.now();
              }}
            />
          )}
        />
      </Sheet>,
      document.body,
    );
  }

  return (
    <div className={`kb-term${state.phase === "live" ? "" : " kb-term--idle"}`} data-testid="kb-terminal">
      <div className="kb-term__bar">
        <span className="kb-term__harness" data-testid="kb-term-harness">
          {props.harness}
        </span>
        {rowState ? (
          <span className="kb-term__chip" data-testid="kb-term-state">
            {rowState}
          </span>
        ) : null}
        {grid ? (
          <span className="kb-term__grid" data-testid="kb-term-grid">
            {chipText(grid, gridOwner)}
          </span>
        ) : null}
        {fit ? (
          <span className="kb-term__grid" data-testid="kb-term-font">
            font {fit.fontSize}px
          </span>
        ) : null}
        {fit && (fit.cropped.x || fit.cropped.y) ? (
          <span className="kb-term__hint" data-testid="kb-term-cropped" title="The grid is larger than the panel at the smallest font: scroll the panel">
            Cropped
          </span>
        ) : null}
        <span className="kb-term__caption" data-testid="kb-term-caption">
          {transient ?? state.caption}
        </span>
        <GridControls
          termId={props.termId}
          harness={props.harness}
          viewer={viewer}
          owner={gridOwner}
          measureWindow={measureWindow}
          say={say}
        />
        <button
          type="button"
          className="kb-term__btn"
          data-testid="kb-term-find-toggle"
          onClick={() => setFindOpen((v) => !v)}
        >
          Find
        </button>
        <button
          type="button"
          className="kb-term__btn"
          data-testid="kb-term-close"
          onClick={closePanel}
        >
          Close
        </button>
      </div>
      <div className="kb-term__status" role="status" aria-live="polite" data-testid="kb-term-status">
        {status}
      </div>
      {findOpen ? (
        <div className="kb-term__findbar">
          <input
            type="text"
            className="kb-term__findinput"
            data-testid="kb-term-find-input"
            value={findQuery}
            onChange={(e) => setFindQuery(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") runFind(e.shiftKey ? -1 : 1);
            }}
            aria-label="Find in terminal"
          />
          <span className="kb-term__findnote" data-testid="kb-term-find-note">
            {findNote}
          </span>
        </div>
      ) : null}
      <div
        className="kb-term__view"
        data-kb-term=""
        data-kb-term-phase={state.phase}
        data-kb-term-offset={state.applied}
        data-kb-term-resets={state.resets}
        data-kb-term-gaps={state.gaps}
        data-kb-term-width={widthActive ? "unicode-graphemes" : undefined}
        data-kb-term-scrollback={scrollback > 0 ? scrollback : undefined}
        ref={hostRef}
      />
      {gfx !== null ? (
        <Lightbox picture={picture} media={gfx.media} onClose={() => setPicture(null)} />
      ) : null}
    </div>
  );
}
