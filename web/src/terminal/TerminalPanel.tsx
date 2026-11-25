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
// FUTERMFULL: full screen (`fullscreen.ts`: native where Esc can stay with the
// terminal, else a maximised overlay; never a take-size), the typing/click-to-type
// chip, Ctrl/Cmd+C copies only with a selection (else ^C), http(s) links open on
// Ctrl/Cmd-click, and Jump to latest while scrolled up.
//
// T17 — on a phone (`usePhonePanel()`) the SAME terminal, socket and
// reconnect state render through `PhoneTerminal` inside a `Sheet`. The
// instance lives here, above the mobile/desktop switch, so a rotation
// across the breakpoint re-parents the xterm element and never
// reconnects or re-seeds. A terminal created on a phone gets the DOM
// renderer only (no WebGL addon: context loss and memory pressure on
// phones, and a real phone GPU is untested), a 20,000-line scrollback
// cap and the persisted phone font; it stays that way if the viewport
// later grows (no mid-session addon load).

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { Sheet, type Detent } from "@kb/ui/Sheet";
import { usePhonePanel } from "../hooks/useIsMobile";
import { useSheetHistory } from "./phone/useSheetHistory";
import { Terminal, type IFunctionIdentifier } from "@xterm/xterm";
import "@xterm/xterm/css/xterm.css";
import { ImageAddon } from "@xterm/addon-image";
import { WebglAddon } from "@xterm/addon-webgl";
import { UnicodeGraphemesAddon } from "@xterm/addon-unicode-graphemes";
import { TermSession, type SessionState } from "./session";
import { mintTicket } from "../api/terms";
import { toast } from "../lib/toast";
import { installReplyGuards } from "./replyGuards";
import { profileMatchesBundle } from "./pins";
import { FIT_MAX_FULL, FIT_MAX_INLINE, FIT_MIN_FULL, FIT_MIN_INLINE, type FitResult } from "./fit";
import { fitHostNow } from "./fitHost";
import { createFitter, type Fitter } from "./fitter";
import { scrollPanelIntoView } from "./viewport";
import { measureCellRatio } from "./cellProbe";
import { fontGate, TERM_FONT_FAMILY } from "./fonts";
import { readTheme } from "./theme";
import { GridControls } from "./GridControls";
import { chipText, trustedOwner } from "./gridOwner";
import { CROPPED_DESKTOP_HINT, CROPPED_LABEL } from "./cropHint";
import type { WindowMeasure } from "./fitWindow";
import { THEME_APPLIED_EVENT } from "@kb/ui/themes/apply";
import { attachGfx, type GfxHandle, type OpenPicture } from "./gfx";
import { Lightbox } from "./Lightbox";
import { countHits, findNext, FIND_START, type FindHit } from "./find";
import { useFullscreen } from "./useFullscreen";
import { useTerminalPaste } from "./useTerminalPaste";
import { clipboardAction, inputLabel, scrolledUp } from "./input";
import { linksAtRow, openExternal, isMacLike, linkHint } from "./links";
import { useTerms } from "./useTerms";
import { useLiveSessions } from "../hooks/useLiveSessions";
import { cwdBase } from "../components/TermControls";
import { statusChips } from "./statusChips";
import { TermHeader } from "./TermHeader";
import { TermStateOverlay, announceText, overlayKind } from "./TermStateOverlay";
import { initialInputDelivered, initialInputText, markInitialInputDelivered } from "./initialInput";
import type { ImageDefFrame, ProfileFrame } from "./wire";
import type { TerminalPanelProps } from "./TerminalPanel.lazy";
import { osc52ClipboardText, tryWriteClipboardText, writeClipboardText } from "./clipboard";
import { PhoneTerminal, attachTerm, type PhoneMode } from "./phone/PhoneTerminal";
import { DriveDock } from "./phone/DriveDock";
import { terminalOwnsEscape } from "./phone/escape";
import { readFont } from "./phone/fontSteps";
import { readScreenReader, writeScreenReader } from "./screenReaderPref";
import { scrollbackFor } from "./phone/scrollback";
import { bindNetworkKicks } from "./networkKicks";
import { bellNeedsAttention, sanitizeOscTitle } from "../lib/termAttention";
import { createPortal } from "react-dom";
import "./terminal.css";

const TRANSIENT_MS = 2_000;
/** One `["terms"]` refetch this long after the last grid-change frame (W1). */
const TERMS_REFRESH_MS = 300;
/** Verification passes run this long after a fit, once the renderer has painted. */
const FIT_SETTLE_MS = 60;

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
  const isMobile = usePhonePanel();
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
  // FUTERMATTN: the program's own OSC 0/2 title, sanitized (text only), and the bell hook.
  const [progTitle, setProgTitle] = useState("");
  const onBellRef = useRef(props.onBell);
  onBellRef.current = props.onBell;
  const [findOpen, setFindOpen] = useState(false);
  const [findQuery, setFindQuery] = useState("");
  const [findNote, setFindNote] = useState("");
  const findHitRef = useRef<FindHit>({ ...FIND_START });
  // FUTERMFULL — the panel element (state, so the full-screen control binds once it exists),
  // whether the terminal has focus, whether the view is scrolled up, and the
  // refit the full-screen change asks for.
  const [panelEl, setPanelEl] = useState<HTMLDivElement | null>(null);
  const [focused, setFocused] = useState(false);
  const [behind, setBehind] = useState(false);
  const fullRef = useRef(false);
  const refitRef = useRef<(() => void) | null>(null);
  const fs = useFullscreen(panelEl, props.termId, {
    onChange: (on) => {
      fullRef.current = on;
      if (on) termRef.current?.focus();
      // The panel's box changes with the next layout: refit the FONT (never a take-size).
      window.setTimeout(() => refitRef.current?.(), 60);
    },
  });
  // W2 — opening the panel (Open terminal, a launch, Resume) brings its bar just
  // under the app bar, once per mount; a re-render or a stream event never scrolls.
  const scrolledOnOpenRef = useRef(false);
  useEffect(() => {
    if (!panelEl || isMobile || scrolledOnOpenRef.current) return;
    scrolledOnOpenRef.current = true;
    scrollPanelIntoView(panelEl);
  }, [panelEl, isMobile]);
  // T15 — the picture layer for the CURRENT xterm instance (null before the
  // Profile builds the terminal and after teardown).
  const gfxRef = useRef<GfxHandle | null>(null);
  const [gfx, setGfx] = useState<GfxHandle | null>(null);
  const [picture, setPicture] = useState<OpenPicture | null>(null);
  const { terms } = useTerms();
  const queryClient = useQueryClient();
  const termsTimerRef = useRef<number | null>(null);
  const termRow = terms.find((t) => t.id === props.termId);
  const project = termRow?.cwd ? cwdBase(termRow.cwd) : undefined;
  // BHEAD: the typed status of the bound session, from the shared live-status
  // query (never from terminal bytes). An unbound sid ("") has no chips.
  const { rows: liveRows } = useLiveSessions();
  const liveStatus = props.sid ? liveRows.find((r) => r.session_id === props.sid)?.status : undefined;
  const chips = useMemo(() => statusChips(liveStatus), [liveStatus]);
  // The row's owner as the list states it (null = free). It retires the hint
  // below whenever it changes.
  const listOwner: string | null | undefined = termRow ? (termRow.grid_owner ?? null) : undefined;
  // undefined = the list cannot vouch for it: no row (still loading or the read
  // failed) or a row for another grid size than the live one (a grid change the
  // list has not caught up with). UNKNOWN is not the same as free.
  const vouchedOwner: string | null | undefined = trustedOwner(termRow, grid);
  // The runner's answer to our own take/release names the owner NOW; the list
  // row is the source otherwise, and any change of the row's owner retires the
  // hint (a local attach that took the grid after us must show).
  const [ownerHint, setOwnerHint] = useState<{ owner: string | null } | null>(null);
  useEffect(() => setOwnerHint(null), [listOwner]);
  const gridOwner: string | null | undefined = ownerHint ? ownerHint.owner : vouchedOwner;
  const onOwner = useCallback((owner: string | null): void => setOwnerHint({ owner }), []);

  // BSTATE — a hint names the owner of ONE grid on ONE connection: a new grid
  // size or a connection that is no longer live retires it, and while the
  // stream is not live the chip says the size only (the owner is unknown).
  const everLiveRef = useRef(false);
  if (state.phase === "live") everLiveRef.current = true;
  const firstConnect = !everLiveRef.current;
  const ownerStale = ["reconnecting", "ended", "expired", "blocked", "sleeping"].includes(state.phase);
  const gridCols = grid?.cols;
  const gridRows = grid?.rows;
  useEffect(() => setOwnerHint(null), [gridCols, gridRows]);
  useEffect(() => {
    if (ownerStale) setOwnerHint(null);
  }, [ownerStale]);
  const chipOwner: string | null | undefined = ownerStale ? undefined : gridOwner;

  // BSTATE — the ONE polite region: a short sentence per state change.
  const [announce, setAnnounce] = useState("");
  const reconnectedRef = useRef(false);
  useEffect(() => {
    if (state.phase === "live") {
      setAnnounce(reconnectedRef.current ? "Reconnected" : "");
      reconnectedRef.current = false;
      return;
    }
    if (state.phase === "sleeping") {
      setAnnounce("");
      return;
    }
    if (overlayKind(state.phase, firstConnect) === "reconnecting" || state.phase === "expired") {
      reconnectedRef.current = true;
    }
    setAnnounce(announceText(state.phase, firstConnect, state.exitCode, state.blockedReason));
  }, [state.phase, state.exitCode, state.blockedReason, firstConnect]);

  // BSTATE — the overlay never takes focus. Only when the reader had focus ON
  // the overlay (its button) as it went away does the terminal get it back.
  const overlayFocusRef = useRef(false);
  const [screenReader, setScreenReader] = useState<boolean>(() => readScreenReader());
  const onScreenReader = useCallback((on: boolean) => {
    setScreenReader(on);
    writeScreenReader(on);
    // Live: the same Terminal, no remount, no geometry message.
    if (termRef.current) termRef.current.options.screenReaderMode = on;
  }, []);
  const overlayOn = !isMobile && overlayKind(state.phase, firstConnect) !== null;
  useEffect(() => {
    if (overlayOn || !overlayFocusRef.current) return;
    overlayFocusRef.current = false;
    const active = document.activeElement;
    if (!active || active === document.body) termRef.current?.focus();
  }, [overlayOn]);

  // Fit the live terminal into the desktop host (font only; never a take-size).
  // The fitter plans a new grid or box from scratch and verifies after the
  // renderer has painted (`fitter.ts`); only a refit of the same situation keeps
  // a font that already fits.
  const fitterRef = useRef<Fitter<Terminal, HTMLElement> | null>(null);
  const lastFitFontRef = useRef<number | null>(null);
  const fitDesktop = (term: Terminal, host: HTMLElement): void => {
    if (!gridRef.current) return;
    if (!fitterRef.current) {
      const bounds = (): { min: number; max: number } =>
        fullRef.current ? { min: FIT_MIN_FULL, max: FIT_MAX_FULL } : { min: FIT_MIN_INLINE, max: FIT_MAX_INLINE };
      fitterRef.current = createFitter<Terminal, HTMLElement>({
        run: (t, h, fresh) => {
          const g = gridRef.current;
          return g ? fitHostNow(t, h, g, { ...bounds(), fresh }) : null;
        },
        key: (h) => {
          const g = gridRef.current;
          const b = bounds();
          return `${g?.cols}x${g?.rows}|${h.clientWidth}x${h.clientHeight}|${b.min}-${b.max}`;
        },
        schedule: (fn) => window.setTimeout(fn, FIT_SETTLE_MS),
        cancel: (handle) => window.clearTimeout(handle as number),
        onResult: (r) => {
          // A new font moves every cell: the picture layer re-lays out (the
          // renderer hears no event for a late font change on its own).
          if (lastFitFontRef.current !== r.fontSize) {
            lastFitFontRef.current = r.fontSize;
            gfxRef.current?.notifyMetricsChanged();
          }
          setFit((prev) =>
            prev && prev.fontSize === r.fontSize && prev.cropped.x === r.cropped.x && prev.cropped.y === r.cropped.y
              ? prev
              : r,
          );
        },
      });
    }
    fitterRef.current.refit(term, host);
  };
  // A grid change this panel did not cause (another tab, a local attach) leaves
  // the list row stale: refetch it once, after the burst of frames settles.
  const refreshTermsSoon = (): void => {
    if (termsTimerRef.current !== null) window.clearTimeout(termsTimerRef.current);
    termsTimerRef.current = window.setTimeout(() => {
      termsTimerRef.current = null;
      void queryClient.invalidateQueries({ queryKey: ["terms"] });
    }, TERMS_REFRESH_MS);
  };
  const say = useCallback((message: string): void => setStatus(message), []);
  // FUMINPANEL: the program's title belongs to one live connection: a reconnect or an
  // ended session must not keep showing the previous run's title.
  useEffect(() => {
    if (state.phase !== "live") setProgTitle("");
  }, [state.phase]);
  // BHEAD: `initialInput` is delivered ONCE per (terminal, text), when the
  // connection first reaches live, through the paste path and WITHOUT Enter
  // (one trailing newline is stripped). The delivered set is module-level, so a
  // reconnect, a breakpoint remount or a prop re-render never sends it again.
  const initialInput = props.initialInput;
  useEffect(() => {
    if (state.phase !== "live" || !termInst || !initialInput) return;
    const text = initialInputText(initialInput);
    if (text === "" || initialInputDelivered(props.termId, initialInput)) return;
    if (isMobileRef.current) {
      // The phone's xterm has stdin disabled: send the paste to the session directly.
      const bracketed = (termInst as { modes?: { bracketedPasteMode?: boolean } }).modes?.bracketedPasteMode === true;
      const body = text.replace(/\r?\n/g, "\r");
      if (!(sessionRef.current?.sendInput(bracketed ? `\u001b[200~${body}\u001b[201~` : body) ?? false)) return;
    } else {
      termInst.paste(text);
    }
    markInitialInputDelivered(props.termId, initialInput);
  }, [state.phase, termInst, initialInput, props.termId]);
  // FUTERMPASTE: a picture paste or drop on the desktop host uploads and pastes its path.
  const pastePath = useCallback((text: string): void => termRef.current?.paste(text), []);
  useTerminalPaste({
    hostRef,
    termId: props.termId,
    active: termInst !== null && !isMobile,
    paste: pastePath,
    say,
  });
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
        cursorInactiveStyle: "outline",
        // Initial size only; `fitDesktop` picks the real one from the host.
        fontSize: phone
          ? readFont()
          : Math.max(
              FIT_MIN_INLINE,
              parseFloat(getComputedStyle(document.documentElement).getPropertyValue("--fs-md")) || 14,
            ),
        fontFamily: TERM_FONT_FAMILY,
        theme: readTheme(profile.theme),
        // BANSI — nothing illegible whatever a programme paints; bold stays
        // bold (the bright slots are explicit); box/block glyphs are drawn.
        minimumContrastRatio: 4.5,
        drawBoldTextInBrightColors: false,
        customGlyphs: true,
        // BA11Y — xterm's own accessibility tree, a browser-local choice.
        screenReaderMode: readScreenReader(),
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
        // Copy only with a selection (else Ctrl/Cmd+C stays ^C); Ctrl/Cmd+Shift+C
        // always copies and +V leaves the paste to the browser's paste event.
        const action = clipboardAction(ev, term.hasSelection());
        if (action === "copy") {
          if (ev.type === "keydown") {
            ev.preventDefault();
            const sel = term.getSelection();
            if (sel) {
              // Copied: clear the selection once the write LANDED, so the next
              // Ctrl/Cmd+C has none and is ^C. A refused write keeps it and says so.
              void tryWriteClipboardText(sel).then((ok) => {
                if (dead) return;
                if (ok) term.clearSelection();
                else toast.err("Could not copy to the clipboard");
              });
            }
          }
          return false;
        }
        if (action === "paste") return false;
        return true;
      });
      term.registerLinkProvider({
        provideLinks: (y, cb) => {
          const buf = term.buffer.active;
          const found = linksAtRow(
            {
              length: buf.length,
              row: (i) => {
                const l = buf.getLine(i);
                return l ? { text: l.translateToString(true), wrapped: l.isWrapped } : null;
              },
            },
            y - 1,
            term.cols,
          );
          cb(
            found.length === 0
              ? undefined
              : found.map((l) => ({
                  range: { start: l.start, end: l.end },
                  text: l.url,
                  activate: (e: MouseEvent, text: string) => {
                    if (e.ctrlKey || e.metaKey) openExternal(text, window);
                  },
                  // A plain click only selects: say how a link opens (native tooltip; the
                  // phone opens a link by tap in its own scroller instead).
                  hover: () => {
                    if (term.element) term.element.title = linkHint(isMacLike(navigator));
                  },
                  leave: () => {
                    if (term.element) term.element.title = "";
                  },
                })),
          );
        },
      });
      const syncBehind = (): void => setBehind(scrolledUp(term.buffer.active));
      term.onScroll(syncBehind);
      term.onWriteParsed(syncBehind);
      setBehind(false);
      term.onTitleChange((t) => setProgTitle(sanitizeOscTitle(t)));
      term.onBell(() => {
        const active = document.activeElement;
        const termFocused = document.hasFocus() && !!(active && term.element?.contains(active));
        if (bellNeedsAttention({ hidden: document.hidden, termFocused })) onBellRef.current?.();
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
        // The list row's size/owner may now be behind: one debounced refetch.
        refreshTermsSoon();
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
          ws.onclose = (ev: CloseEvent) => handlers.onClose(ev.code, ev.reason);
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
    const unbindKicks = bindNetworkKicks(window, document, () => session.kick(), (hidden) => session.setHidden(hidden));

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
      fitterRef.current?.dispose();
      fitterRef.current = null;
      if (termsTimerRef.current !== null) window.clearTimeout(termsTimerRef.current);
      termsTimerRef.current = null;
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
    const onFocusIn = (): void => setFocused(true);
    const onFocusOut = (): void => setFocused(false);
    host.addEventListener("focusin", onFocusIn);
    host.addEventListener("focusout", onFocusOut);
    let fitTimer: number | null = null;
    const refit = (): void => fitDesktop(term, host);
    refitRef.current = refit;
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
      host.removeEventListener("focusin", onFocusIn);
      host.removeEventListener("focusout", onFocusOut);
      setFocused(false);
      refitRef.current = null;
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
    // Leave full screen first (native, key lock, overlay): the effect cleanup
    // repeats it on unmount, so a closed panel never leaves one behind.
    fs.exit();
    // On iOS the sheet's own history entry is popped first; the popstate then
    // re-enters here with the entry gone and closes for real.
    if (isMobileRef.current && popSheetEntry()) return;
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

  // iOS Safari has no CloseWatcher: the sheet takes one history entry of its
  // own so edge-swipe Back closes it instead of leaving the page.
  const popSheetEntry = useSheetHistory(isMobile, closePanel);

  // Closing Find hands the keyboard back to the terminal.
  const closeFind = (): void => {
    setFindOpen(false);
    termRef.current?.focus();
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
        full="100dvh"
        title="Terminal"
        titleHidden
        onClose={closePanel}
        ignoreEscape={terminalOwnsEscape}
      >
        <PhoneTerminal
          harness={props.harness}
          headerExtra={props.headerExtra}
          headerActions={props.headerActions}
          project={project}
          chips={chips}
          term={termInst}
          profile={grid}
          hostRef={phoneHostRef}
          mode={phoneMode}
          onMode={setPhoneMode}
          stream={state}
          caption={transient ?? state.caption}
          onClose={closePanel}
          onReconnect={() => sessionRef.current?.kick()}
          overlayFirstConnect={firstConnect}
          announce={announce}
          sendInput={(d) => {
            const ok = sessionRef.current?.sendInput(d) ?? false;
            if (ok) lastTypedRef.current = Date.now();
            return ok;
          }}
          renderDrive={({ measure, fullscreen }) => (
            <DriveDock
              grid={grid}
              compact={fullscreen}
              termId={props.termId}
              harness={props.harness}
              viewer={viewer}
              row={termRow}
              measure={measure}
              onTyped={() => {
                lastTypedRef.current = Date.now();
              }}
              ownerHint={ownerHint}
              onOwner={onOwner}
            />
          )}
        />
      </Sheet>,
      document.body,
    );
  }

  return (
    <div
      className={`kb-term${state.phase === "live" ? "" : " kb-term--idle"}${focused ? " kb-term--typing" : ""}${fs.max ? " kb-term--max" : ""}`}
      data-testid="kb-terminal"
      data-kb-term-fullscreen={fs.on ? "" : undefined}
      ref={setPanelEl}
    >
      <TermHeader
        harness={props.harness}
        project={project}
        phase={state.phase}
        caption={transient ?? state.caption}
        progTitle={progTitle}
        chips={chips}
        actions={props.headerActions}
        compact={false}
        more={
          <>
            <span
              className={`kb-term__chip kb-term__input${focused ? " kb-term__input--on" : ""}`}
              data-testid="kb-term-input"
            >
              {inputLabel(focused)}
            </span>
            {grid ? (
              <span className="kb-term__grid" data-testid="kb-term-grid">
                {chipText(grid, chipOwner)}
              </span>
            ) : null}
            {fit ? (
              <span className="kb-term__grid" data-testid="kb-term-font">
                font {fit.fontSize}px
              </span>
            ) : null}
            <label className="kb-term__sr" data-testid="kb-term-screen-reader-label">
              <input
                type="checkbox"
                data-testid="kb-term-screen-reader"
                checked={screenReader}
                onChange={(e) => onScreenReader(e.target.checked)}
              />
              Screen reader mode
            </label>
            {fit && (fit.cropped.x || fit.cropped.y) ? (
              <span className="kb-term__hint" data-testid="kb-term-cropped" title={CROPPED_DESKTOP_HINT}>
                {CROPPED_LABEL}
              </span>
            ) : null}
          </>
        }
      >
        <GridControls
          termId={props.termId}
          harness={props.harness}
          viewer={viewer}
          owner={gridOwner ?? null}
          measureWindow={measureWindow}
          onBeforeFit={() => {
            // Measure the box the reader can see: bring the panel up first (not in full screen).
            if (!fullRef.current) scrollPanelIntoView(panelEl, { instant: true });
          }}
          say={say}
          onOwner={onOwner}
        />
        <button
          type="button"
          className="kb-term__btn"
          data-testid="kb-term-find-toggle"
          onClick={() => (findOpen ? closeFind() : setFindOpen(true))}
        >
          Find
        </button>
        <button
          type="button"
          className="kb-term__btn"
          data-testid="kb-term-fullscreen"
          aria-pressed={fs.on}
          onClick={fs.toggle}
        >
          {fs.on ? "Exit full screen" : "Full screen"}
        </button>
        <button
          type="button"
          className="kb-term__btn"
          data-testid="kb-term-close"
          onClick={closePanel}
        >
          Close
        </button>
      </TermHeader>
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
            autoFocus
            onChange={(e) => {
              setFindQuery(e.target.value);
              // A new query starts from the top, never from the old hit.
              findHitRef.current = { ...FIND_START };
              setFindNote("");
            }}
            onKeyDown={(e) => {
              if (e.key === "Enter") runFind(e.shiftKey ? -1 : 1);
              if (e.key === "Escape") closeFind();
            }}
            aria-label="Find in terminal"
          />
          <span className="kb-term__findnote" data-testid="kb-term-find-note">
            {findNote}
          </span>
        </div>
      ) : null}
      <div className="kb-term__announce" role="status" aria-live="polite" data-testid="kb-term-announce">
        {announce}
      </div>
      <div
        className="kb-term__pane"
        role="region"
        aria-label={`${props.harness} terminal${project ? ` in ${project}` : ""}`}
        onFocus={(e) => {
          overlayFocusRef.current = !!(e.target as Element).closest?.(".kb-term-state-overlay");
        }}
        onBlur={(e) => {
          // Focus left the overlay: the terminal is not owed it back. An
          // element removed while focused (the overlay unmounting) keeps its flag.
          const to = e.relatedTarget as Element | null;
          if (!(e.target as Element).isConnected) return;
          if (!to || !to.closest?.(".kb-term-state-overlay")) overlayFocusRef.current = false;
        }}
      >
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
        <TermStateOverlay
          phase={state.phase}
          caption={state.caption}
          exitCode={state.exitCode}
          blockedReason={state.blockedReason}
          firstConnect={firstConnect}
          onReconnect={() => sessionRef.current?.kick()}
          onBack={closePanel}
          compact={false}
        />
      </div>
      {behind ? (
        <button
          type="button"
          className="kb-term__btn kb-term__jump"
          data-testid="kb-term-jump"
          onClick={() => {
            termRef.current?.scrollToBottom();
            termRef.current?.focus();
          }}
        >
          Jump to latest
        </button>
      ) : null}
      {gfx !== null ? (
        <Lightbox picture={picture} media={gfx.media} onClose={() => setPicture(null)} />
      ) : null}
    </div>
  );
}
