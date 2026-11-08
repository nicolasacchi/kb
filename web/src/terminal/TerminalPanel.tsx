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
// answered by the browser. The panel never sends a resize; `sizechanged`
// only updates the header caption. No `focus()` on mount (click
// focuses), no Escape binding, no global key handler.

import { useEffect, useRef, useState } from "react";
import { Terminal, type ITheme } from "@xterm/xterm";
import "@xterm/xterm/css/xterm.css";
import { ImageAddon } from "@xterm/addon-image";
import { WebglAddon } from "@xterm/addon-webgl";
import { UnicodeGraphemesAddon } from "@xterm/addon-unicode-graphemes";
import { TermSession, type SessionState } from "./session";
import { mintTicket } from "../api/terms";
import { installReplyGuards } from "./replyGuards";
import { profileMatchesBundle } from "./pins";
import { computeFontSize } from "./fit";
import { ImageDefStore } from "./imageDefs";
import { countHits, findNext, type FindHit } from "./find";
import { useTerms } from "./useTerms";
import type { ImageDefFrame, ProfileFrame } from "./wire";
import type { TerminalPanelProps } from "./TerminalPanel.lazy";
import "./terminal.css";

const TYPED_WINDOW_MS = 10_000;
const TRANSIENT_MS = 2_000;

/** e2e seam: `container.__kbTerm` exposes the live instance (no window global). */
type TermSeam = HTMLElement & { __kbTerm?: Terminal };

function readTheme(profile: ProfileFrame): ITheme | undefined {
  const theme = profile.theme;
  if (theme !== null && typeof theme === "object") {
    // Only string color slots are honored; unknown keys are dropped.
    const slots = theme as Record<string, unknown>;
    const out: ITheme = {};
    if (typeof slots.background === "string") out.background = slots.background;
    if (typeof slots.foreground === "string") out.foreground = slots.foreground;
    if (typeof slots.cursor === "string") out.cursor = slots.cursor;
    return out;
  }
  // No palette in the Profile: fall back to the app's own tokens.
  const css = getComputedStyle(document.documentElement);
  const background = css.getPropertyValue("--bg-panel-2").trim();
  const foreground = css.getPropertyValue("--ink").trim();
  const cursor = css.getPropertyValue("--accent").trim();
  const fallback: ITheme = {};
  if (background) fallback.background = background;
  if (foreground) fallback.foreground = foreground;
  if (cursor) fallback.cursor = cursor;
  return fallback;
}

/** Fit by fontSize only (never transform/zoom): ≤4 iterations on the screen width. */
function fitNow(term: Terminal, host: HTMLElement, cols: number): void {
  const screen = host.querySelector(".xterm-screen");
  const canvas = document.createElement("canvas");
  const ctx = canvas.getContext("2d");
  if (!ctx) return;
  const family = term.options.fontFamily || "monospace";
  ctx.font = `100px ${family}`;
  const ratio = ctx.measureText("W").width / 100;
  let fontSize = computeFontSize(host.clientWidth, cols, ratio);
  host.style.overflowX = "";
  for (let i = 0; i < 4; i += 1) {
    term.options.fontSize = fontSize;
    if (!screen || screen.scrollWidth <= host.clientWidth) return;
    if (fontSize <= 10) break;
    fontSize = Math.max(10, fontSize - 1);
  }
  if (screen && screen.scrollWidth > host.clientWidth) host.style.overflowX = "auto";
}

export function TerminalPanel(props: TerminalPanelProps) {
  const hostRef = useRef<HTMLDivElement | null>(null);
  const termRef = useRef<Terminal | null>(null);
  const sessionRef = useRef<TermSession | null>(null);
  const lastTypedRef = useRef(0);
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
  const gridRef = useRef<{ cols: number; rows: number } | null>(null);
  const [scrollback, setScrollback] = useState(0);
  const [widthActive, setWidthActive] = useState(false);
  const [transient, setTransient] = useState<string | null>(null);
  const [findOpen, setFindOpen] = useState(false);
  const [findQuery, setFindQuery] = useState("");
  const [findNote, setFindNote] = useState("");
  const findHitRef = useRef<FindHit>({ line: 0, col: -1, length: 0 });
  const imageDefsRef = useRef(new ImageDefStore());
  const { terms } = useTerms();
  const rowState = terms.find((t) => t.id === props.termId)?.state ?? null;

  useEffect(() => {
    const host = hostRef.current;
    if (!host) return;
    let dead = false;
    let transientTimer: number | null = null;
    let fitTimer: number | null = null;

    const writeClipboardIfRecent = (text: string): void => {
      if (Date.now() - lastTypedRef.current > TYPED_WINDOW_MS) return;
      try {
        void navigator.clipboard?.writeText(text);
      } catch {
        // Clipboard failures are caught silently.
      }
    };

    const createTerminal = (profile: ProfileFrame): boolean => {
      if (!profileMatchesBundle(profile)) {
        sessionRef.current?.block("viewer build differs from session profile");
        return false;
      }
      const term = new Terminal({
        cols: profile.cols,
        rows: profile.rows,
        scrollback: profile.scrollback,
        scrollOnEraseInDisplay: true,
        allowProposedApi: true,
        convertEol: false,
        cursorBlink: false,
        // Initial size only; fitNow clamps from the token-derived width.
        fontSize: Math.max(
          10,
          parseFloat(getComputedStyle(document.documentElement).getPropertyValue("--fs-md")) || 14,
        ),
        fontFamily: "JetBrains Mono, monospace",
        theme: readTheme(profile),
      });
      term.open(host);
      term.loadAddon(new UnicodeGraphemesAddon());
      const key = Object.keys(term.unicode.versions ?? {}).find((k) => /grapheme/i.test(k));
      if (!key) {
        sessionRef.current?.block("width table unavailable");
        term.dispose();
        return false;
      }
      term.unicode.activeVersion = key;
      term.loadAddon(new ImageAddon({ enableSizeReports: false }));
      try {
        const webgl = new WebglAddon();
        webgl.onContextLoss(() => webgl.dispose());
        term.loadAddon(webgl);
      } catch {
        // DOM renderer fallback.
      }
      installReplyGuards({
        registerCsiHandler: (prefix, intermediates, final, handler) =>
          term.parser.registerCsiHandler({ prefix, intermediates, final }, (params) =>
            handler(params.toArray()),
          ),
        registerOscHandler: (code, handler) => term.parser.registerOscHandler(code, handler),
        registerDcsHandler: (prefix, intermediates, final, handler) =>
          term.parser.registerDcsHandler({ prefix, intermediates, final }, (params, data) =>
            handler(params.toArray(), data ?? ""),
          ),
      });
      term.parser.registerOscHandler(52, (data) => {
        if (data.includes("?")) return true;
        const parts = data.split(";");
        if (parts.length >= 2) {
          try {
            writeClipboardIfRecent(atob(parts[1]));
          } catch {
            // Not clipboard content — ignored.
          }
        }
        return false;
      });
      term.attachCustomKeyEventHandler((ev) => {
        if (
          ev.type === "keydown" &&
          ev.key === "Enter" &&
          ev.shiftKey &&
          !ev.ctrlKey &&
          !ev.metaKey &&
          !ev.altKey
        ) {
          if (sessionRef.current?.sendInput("\u001b[13;2u") ?? false) {
            lastTypedRef.current = Date.now();
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
      host.addEventListener("click", () => term.focus());
      fitNow(term, host, profile.cols);
      termRef.current = term;
      (host as TermSeam).__kbTerm = term;
      gridRef.current = { cols: profile.cols, rows: profile.rows };
      setGrid(gridRef.current);
      setScrollback(profile.scrollback);
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
        if (term && (term.cols !== cols || term.rows !== rows)) term.resize(cols, rows);
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
            profile.scrollback !== existing.options.scrollback
          ) {
            existing.dispose();
            termRef.current = null;
            (host as TermSeam).__kbTerm = undefined;
            if (createTerminal(profile)) sessionRef.current?.rejoinFull();
          }
        },
        onImageDef: (def: ImageDefFrame) => {
          imageDefsRef.current.put(def);
        },
        onSizeChanged: (cols, rows, by) => {
          if (dead) return;
          setTransient(`grid ${cols}x${rows} by ${by}`);
          if (transientTimer !== null) window.clearTimeout(transientTimer);
          transientTimer = window.setTimeout(() => setTransient(null), TRANSIENT_MS);
        },
        onClipboard: (dataB64) => {
          try {
            writeClipboardIfRecent(atob(dataB64));
          } catch {
            // Not clipboard content — ignored.
          }
        },
      },
    });
    sessionRef.current = session;
    session.start();

    const observer = new ResizeObserver(() => {
      if (fitTimer !== null) window.clearTimeout(fitTimer);
      fitTimer = window.setTimeout(() => {
        const term = termRef.current;
        if (term && gridRef.current) fitNow(term, host, gridRef.current.cols);
      }, 100);
    });
    observer.observe(host);

    return () => {
      dead = true;
      observer.disconnect();
      if (fitTimer !== null) window.clearTimeout(fitTimer);
      if (transientTimer !== null) window.clearTimeout(transientTimer);
      session.dispose();
      sessionRef.current = null;
      const term = termRef.current;
      termRef.current = null;
      (host as TermSeam).__kbTerm = undefined;
      term?.dispose();
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [props.termId]);

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

  return (
    <div className="kb-term" data-testid="kb-terminal">
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
            {grid.cols}x{grid.rows}
          </span>
        ) : null}
        <span className="kb-term__caption" data-testid="kb-term-caption">
          {transient ?? state.caption}
        </span>
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
          onClick={() => {
            sessionRef.current?.dispose();
            sessionRef.current = null;
            termRef.current?.dispose();
            termRef.current = null;
          }}
        >
          Close
        </button>
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
    </div>
  );
}
