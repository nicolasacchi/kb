// T14 — the pure stream/connection machine for one terminal panel.
//
// A reducer over injected effects (`mintTicket`, `openSocket`, `now`,
// `setTimer`, `random`, and the `TermSink` that reaches the xterm
// instance), so all join/resume/backoff behaviour is unit-testable in
// node without xterm. Frame semantics implement the kb-term/1
// contract as pinned by T7/T12:
//
//   * A `seed` frame arrives on a full join. Only then is
//     `term.reset()` queued (in write order) and `applied` set to
//     `seed.offset + seed.len` once the seed payload is written. Seed
//     payload binary frames all carry `offset == seed.offset`, so
//     they are written without advancing `applied` per chunk.
//   * On a resume (no seed), data frames advance `applied` exactly:
//     `off == applied` writes and advances; `off < applied` skips the
//     rewind overlap and writes the rest; `off > applied` is a GAP —
//     the socket is closed, the next join is FULL (no `from`), the
//     held `applied` is discarded, and `gaps++`.
//   * `hello.epoch` differing from the stored epoch discards the held
//     `applied`; the next connect omits `from`.
//   * `resize` is applied inside the write queue, after prior writes.
//   * `exit{code,offset}` ends the stream permanently (no reconnect).
//   * Reconnect delay is `min(10_000, 250 * 2^attempt)` ±20% jitter;
//     `attempt` resets only after a connection stayed live ≥ 5 s.
//     Every attempt mints a NEW ticket (single-use, ~30 s TTL).
//     Ticket 404 → `blocked` ("terminal disabled"); 403 → `blocked`
//     ("not permitted"); never retried. Close 1012 (server restart)
//     uses the same backoff as any unexpected close. Unmount closes
//     the socket with 1000, cancels timers, and mints no further
//     tickets.
//   * Liveness is the 1500 ms no-frame watchdog between socket open
//     and the first applied frame/seed of that connection. There is
//     no steady-state client timer: T12's relay pings the browser at
//     the WS layer every 25 s (browsers answer Pong automatically),
//     so `onclose` is the steady-state signal.

import {
  decodeBinaryFrame,
  encodeInput,
  parseTextFrame,
  WireProtocolError,
  type ImageDefFrame,
  type ProfileFrame,
  type ServerTextFrame,
} from "./wire";
import { TermsApiError, wsUrl } from "../api/terms";

export type Phase = "connecting" | "live" | "reconnecting" | "ended" | "blocked";

export interface SessionState {
  phase: Phase;
  epoch: string | null;
  applied: number;
  resets: number;
  gaps: number;
  attempt: number;
  caption: string;
  exitCode: number | null;
  blockedReason: string | null;
}

/** One live WebSocket, as the machine sees it. */
export interface SessionSocket {
  send(data: string): void;
  close(code?: number): void;
}

export interface SocketHandlers {
  onMessageBinary(frame: Uint8Array): void;
  onMessageText(text: string): void;
  onClose(code: number): void;
}

export interface SessionEffects {
  openSocket(url: string, handlers: SocketHandlers): SessionSocket;
  mintTicket(termId: string, signal?: AbortSignal): Promise<{ ticket: string }>;
  now(): number;
  setTimer(fn: () => void, ms: number): unknown;
  clearTimer(handle: unknown): void;
  random(): number;
}

/** The term-side surface the machine drives (xterm instance, or a fake in tests). */
export interface TermSink {
  write(data: Uint8Array, done: () => void): void;
  reset(): void;
  resize(cols: number, rows: number): void;
}

export interface SessionCallbacks {
  onState(state: SessionState): void;
  onProfile?(profile: ProfileFrame): void;
  onImageDef?(def: ImageDefFrame): void;
  onSizeChanged?(cols: number, rows: number, by: string): void;
  onClipboard?(dataB64: string): void;
  /** Transient caption-only notice (e.g. dropped input while not live). */
  onTransient?(message: string): void;
}

export interface TermSessionOptions {
  termId: string;
  sink: TermSink;
  effects: SessionEffects;
  callbacks: SessionCallbacks;
}

const FIRST_FRAME_WATCHDOG_MS = 1500;
const LIVE_BEFORE_RESET_MS = 5000;
const BASE_BACKOFF_MS = 250;
const MAX_BACKOFF_MS = 10_000;

export class TermSession {
  private readonly termId: string;
  private readonly sink: TermSink;
  private readonly fx: SessionEffects;
  private readonly cb: SessionCallbacks;

  private phase: Phase = "connecting";
  private epoch: string | null = null;
  private applied = 0;
  private resets = 0;
  private gaps = 0;
  private attempt = 0;
  private exitCode: number | null = null;
  private blockedReason: string | null = null;

  private socket: SessionSocket | null = null;
  private socketGen = 0;
  private disposed = false;
  private mintAbort: AbortController | null = null;

  private watchdog: unknown = null;
  private liveResetTimer: unknown = null;
  private reconnectTimer: unknown = null;

  // Seed bookkeeping: the seed payload chunks all carry seed.offset.
  private seeding = false;
  private seedOffset = 0;
  private seedLen = 0;
  private seedWritten = 0;

  // Write-order queue: reset/resize apply after prior writes' callbacks.
  private writeQueue: Promise<void> = Promise.resolve();

  constructor(opts: TermSessionOptions) {
    this.termId = opts.termId;
    this.sink = opts.sink;
    this.fx = opts.effects;
    this.cb = opts.callbacks;
  }

  getState(): SessionState {
    let caption: string;
    if (this.phase === "blocked") caption = `blocked: ${this.blockedReason ?? "unknown"}`;
    else if (this.phase === "ended") caption = `exited (code ${this.exitCode ?? 0})`;
    else if (this.phase === "reconnecting") {
      const nextMs = Math.min(MAX_BACKOFF_MS, BASE_BACKOFF_MS * 2 ** this.attempt);
      caption = `reconnecting in ${Math.max(1, Math.ceil(nextMs / 1000))}s`;
    } else caption = this.phase;
    return {
      phase: this.phase,
      epoch: this.epoch,
      applied: this.applied,
      resets: this.resets,
      gaps: this.gaps,
      attempt: this.attempt,
      caption,
      exitCode: this.exitCode,
      blockedReason: this.blockedReason,
    };
  }

  start(): void {
    if (this.disposed || this.phase === "ended" || this.phase === "blocked") return;
    this.phase = "connecting";
    void this.connect();
  }

  /** Unmount: close 1000, cancel timers, mint no further tickets. */
  dispose(): void {
    this.disposed = true;
    this.clearAllTimers();
    if (this.mintAbort) {
      this.mintAbort.abort();
      this.mintAbort = null;
    }
    const s = this.socket;
    this.socket = null;
    if (s) s.close(1000);
  }

  /**
   * Send one keystroke payload. Returns false (and does not queue)
   * when the stream is not live — the caller drops it with a
   * transient caption.
   */
  sendInput(data: string): boolean {
    if (this.phase !== "live" || this.socket === null || this.disposed) {
      this.cb.onTransient?.("input dropped (not live)");
      return false;
    }
    this.socket.send(encodeInput(data));
    return true;
  }

  /** Discard stream position and rejoin FULL (used when the Profile grid changes). */
  rejoinFull(): void {
    if (this.disposed || this.phase === "ended" || this.phase === "blocked") return;
    this.applied = 0;
    this.clearAllTimers();
    const s = this.socket;
    this.socket = null;
    if (s) s.close(1000);
    this.phase = "connecting";
    void this.connect();
  }

  // ------------------------------------------------------------ connect

  private async connect(): Promise<void> {
    if (this.disposed || this.phase === "ended" || this.phase === "blocked") return;
    const from = this.epoch !== null && this.applied > 0 ? this.applied : undefined;
    const abort = new AbortController();
    this.mintAbort = abort;
    let ticket: string;
    try {
      const result = await this.fx.mintTicket(this.termId, abort.signal);
      ticket = result.ticket;
    } catch (e) {
      this.mintAbort = null;
      if (this.disposed) return;
      if (e instanceof TermsApiError) {
        if (e.status === 404) return this.block("terminal disabled");
        if (e.status === 403) return this.block("not permitted");
      }
      // Any other failure (incl. 429 ticket-table-full) is retryable.
      this.scheduleReconnect();
      return;
    }
    this.mintAbort = null;
    if (this.disposed || this.phase === "ended" || this.phase === "blocked") return;
    const url = wsUrl(this.termId, ticket, from);
    const gen = ++this.socketGen;
    const socket = this.fx.openSocket(url, {
      onMessageBinary: (frame) => {
        if (gen === this.socketGen) this.onBinary(frame);
      },
      onMessageText: (text) => {
        if (gen === this.socketGen) this.onText(text);
      },
      onClose: (code) => {
        if (gen === this.socketGen) this.onClose(code);
      },
    });
    this.socket = socket;
    this.armWatchdog(gen);
    this.emit();
  }

  private scheduleReconnect(): void {
    if (this.disposed || this.phase === "ended" || this.phase === "blocked") return;
    this.phase = "reconnecting";
    this.clearWatchdog();
    this.clearLiveReset();
    const base = Math.min(MAX_BACKOFF_MS, BASE_BACKOFF_MS * 2 ** this.attempt);
    // ±20% jitter: base * [0.8, 1.2); random() = 0.5 gives exactly base.
    const delay = Math.round(base * (1 + (this.fx.random() - 0.5) * 0.4));
    this.reconnectTimer = this.fx.setTimer(() => {
      this.reconnectTimer = null;
      this.attempt += 1;
      void this.connect();
    }, delay);
    this.emit();
  }

  /** Terminal-side block (pin/width mismatch): stop permanently, no retry. */
  block(reason: string): void {
    this.phase = "blocked";
    this.blockedReason = reason;
    this.clearAllTimers();
    if (this.mintAbort) {
      this.mintAbort.abort();
      this.mintAbort = null;
    }
    const s = this.socket;
    this.socket = null;
    if (s) s.close(1000);
    this.emit();
  }

  private onClose(_code: number): void {
    this.socket = null;
    if (this.disposed || this.phase === "ended" || this.phase === "blocked") return;
    // 1012 (server restart) and every other unexpected close take the
    // same bounded backoff. The socket-generated watchdog going off
    // closes through here too (see armWatchdog).
    this.scheduleReconnect();
  }

  /** Close the live socket for a gap/protocol full rejoin. */
  private closeForRejoin(): void {
    this.socketGen += 1;
    this.clearWatchdog();
    const s = this.socket;
    this.socket = null;
    if (s) s.close(1000);
  }

  // ------------------------------------------------------------ frames

  private armWatchdog(gen: number): void {
    this.clearWatchdog();
    this.watchdog = this.fx.setTimer(() => {
      this.watchdog = null;
      if (this.disposed || gen !== this.socketGen || this.socket === null) return;
      if (this.phase === "ended" || this.phase === "blocked") return;
      // No first applied frame/seed within 1500 ms of open: treat as a
      // dead connection and rejoin (resume from `applied` when known).
      this.closeForRejoin();
      this.scheduleReconnect();
    }, FIRST_FRAME_WATCHDOG_MS);
  }

  private clearWatchdog(): void {
    if (this.watchdog !== null) {
      this.fx.clearTimer(this.watchdog);
      this.watchdog = null;
    }
  }

  private clearLiveReset(): void {
    if (this.liveResetTimer !== null) {
      this.fx.clearTimer(this.liveResetTimer);
      this.liveResetTimer = null;
    }
  }

  private clearAllTimers(): void {
    this.clearWatchdog();
    this.clearLiveReset();
    if (this.reconnectTimer !== null) {
      this.fx.clearTimer(this.reconnectTimer);
      this.reconnectTimer = null;
    }
  }

  private markLive(): void {
    this.clearWatchdog();
    this.phase = "live";
    this.clearLiveReset();
    const gen = this.socketGen;
    // `attempt` resets only after the connection stayed live ≥ 5 s.
    this.liveResetTimer = this.fx.setTimer(() => {
      this.liveResetTimer = null;
      if (this.phase === "live" && gen === this.socketGen) this.attempt = 0;
    }, LIVE_BEFORE_RESET_MS);
    this.emit();
  }

  private enqueueWrite(data: Uint8Array): void {
    this.writeQueue = this.writeQueue.then(() => {
      const { promise, resolve } = Promise.withResolvers<void>();
      this.sink.write(data, resolve);
      return promise;
    });
  }

  private enqueueReset(): void {
    this.writeQueue = this.writeQueue.then(() => {
      this.sink.reset();
    });
  }

  private enqueueResize(cols: number, rows: number): void {
    this.writeQueue = this.writeQueue.then(() => {
      this.sink.resize(cols, rows);
    });
  }

  private onBinary(frame: Uint8Array): void {
    let decoded: { offset: number; payload: Uint8Array };
    try {
      decoded = decodeBinaryFrame(frame);
    } catch (e) {
      if (e instanceof WireProtocolError) {
        this.gaps += 1;
        this.applied = 0;
        this.closeForRejoin();
        this.scheduleReconnect();
      }
      return;
    }
    const { offset, payload } = decoded;
    if (this.seeding) {
      this.enqueueWrite(payload);
      this.seedWritten += payload.length;
      if (this.seedWritten >= this.seedLen) {
        this.seeding = false;
        this.applied = this.seedOffset + this.seedLen;
        this.markLive();
      }
    } else if (offset === this.applied) {
      this.clearWatchdog();
      this.enqueueWrite(payload);
      this.applied += payload.length;
    } else if (offset < this.applied) {
      const overlap = Math.min(this.applied - offset, payload.length);
      if (overlap < payload.length) {
        this.enqueueWrite(payload.subarray(overlap));
        this.applied += payload.length - overlap;
      }
      this.clearWatchdog();
    } else {
      // off > applied: GAP — close and rejoin FULL (no `from`).
      this.gaps += 1;
      this.applied = 0;
      this.closeForRejoin();
      this.scheduleReconnect();
      return;
    }
    this.emit();
  }

  private onText(text: string): void {
    let frame: ServerTextFrame;
    try {
      frame = parseTextFrame(text);
    } catch {
      // Malformed server text: ignore (never thrown to the term).
      return;
    }
    switch (frame.t) {
      case "hello": {
        if (this.epoch !== null && frame.epoch !== this.epoch) {
          // Epoch changed: discard the held offset; the next connect
          // omits `from` (full join). The server sends a seed on this
          // connection when the epoch is stale, so we keep reading.
          this.applied = 0;
        }
        this.epoch = frame.epoch;
        break;
      }
      case "profile":
        this.cb.onProfile?.(frame);
        break;
      case "imagedef":
        this.cb.onImageDef?.(frame);
        break;
      case "seed": {
        this.enqueueReset();
        this.seeding = true;
        this.seedOffset = frame.offset;
        this.seedLen = frame.len;
        this.seedWritten = 0;
        this.resets += 1;
        this.clearWatchdog();
        if (frame.len === 0) {
          this.seeding = false;
          this.applied = frame.offset;
          this.markLive();
        }
        break;
      }
      case "resize":
        this.enqueueResize(frame.cols, frame.rows);
        break;
      case "sizechanged":
        this.cb.onSizeChanged?.(frame.cols, frame.rows, frame.by);
        break;
      case "gfxreset":
        break;
      case "clipboard":
        this.cb.onClipboard?.(frame.data_b64);
        break;
      case "error":
        break;
      case "exit": {
        // Remaining data up to `offset` is already queued ahead of this
        // frame (in-order delivery); end permanently, never reconnect.
        this.phase = "ended";
        this.exitCode = frame.code;
        this.clearAllTimers();
        const s = this.socket;
        this.socket = null;
        if (s) s.close(1000);
        break;
      }
      case "unknown":
      default:
        break;
    }
    this.emit();
  }

  private emit(): void {
    this.cb.onState(this.getState());
  }
}
