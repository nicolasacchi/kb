// kb-term tap: the reference viewer (T14 ports it to the SPA) plus the
// reconnect backoff. Pure frame consumer: it never touches a socket, so the
// SPA port and the tests share one implementation. A transport driver feeds
// it decoded frames and acts on its signals (close on gap/bad-frame,
// reconnect with a resumefrom, resolve the watchdog).
//
// Offsets rule (brief rule 4.4): after `seed{offset:X,len:L}` the next binary
// frames until exactly L payload bytes are consumed are seed bytes with
// header == X (not offset-checked, not deduped); a frame straddling L is
// bad-frame; seed bytes never advance `applied` beyond X. At the seed end
// `applied = X`; later binary frames are stream data from X.

import { decodeBinaryFrame, decodeTextFrame, type ServerFrame } from './wire.ts';

export interface RefViewerOpts {
  /** No-frame watchdog in ms (default 1500). */
  watchdogMs?: number;
  /** Current epoch; a hello with a different epoch resets the viewer. */
  epoch?: string;
  /** Monotonic clock in ms (injected in tests). */
  now?: () => number;
}

interface SeedState {
  offset: number;
  remaining: number;
}

export class RefViewer {
  /** Absolute offset of the next stream byte the viewer needs. */
  applied = 0;
  /** Highest marker seq applied. */
  mseq = 0;
  /** True once a hello with a new epoch downgraded this viewer to a full join. */
  fullJoin = false;
  /** Count of binary frames whose header exceeded `applied`. */
  gaps = 0;
  /** Duplicate stream bytes dropped (headers below `applied`). */
  dupBytes = 0;
  /** Set on bad-frame / gap: the transport driver must close and reconnect. */
  closed = false;
  /** The bad-frame reason, when `closed` for a protocol error. */
  badFrame: string | null = null;
  /** Every stream byte applied, in order, for sha256 comparisons. */
  readonly bytes: Uint8Array[] = [];
  /** Every marker (imagedef/resize/gfxreset/exit/sizechanged) applied, in order. */
  readonly markers: { seq: number; t: string }[] = [];
  /** Error frame codes seen, in order. */
  readonly errors: string[] = [];
  /** Clipboard data_b64 payloads, in order. */
  readonly clipboards: string[] = [];

  private seed: SeedState | null = null;
  private readonly watchdogMs: number;
  private epoch: string | null;
  private readonly now: () => number;
  private lastFrameAt: number;

  constructor(opts: RefViewerOpts = {}) {
    this.watchdogMs = opts.watchdogMs ?? 1500;
    this.epoch = opts.epoch ?? null;
    this.now = opts.now ?? (() => Date.now());
    this.lastFrameAt = this.now();
  }

  /** Feed one text frame string. */
  onText(raw: string): void {
    this.lastFrameAt = this.now();
    if (this.closed) return;
    let decoded;
    try {
      decoded = decodeTextFrame(raw);
    } catch (e) {
      this.closed = true;
      this.badFrame = e instanceof Error ? e.message : 'bad-frame';
      return;
    }
    if (decoded.kind === 'unknown') return; // unknown frames are ignored, not errors
    const frame = decoded.frame;
    if (!('t' in frame)) return;
    this.onServerFrame(frame as ServerFrame);
  }

  /** Feed one binary frame (header + payload). */
  onBinary(bytes: Uint8Array): void {
    this.lastFrameAt = this.now();
    if (this.closed) return;
    let decoded;
    try {
      decoded = decodeBinaryFrame(bytes);
    } catch (e) {
      this.closed = true;
      this.badFrame = e instanceof Error ? e.message : 'bad-frame';
      return;
    }
    const { offset, payload } = decoded;
    if (this.seed !== null) {
      if (payload.length > this.seed.remaining) {
        this.closed = true;
        this.badFrame = 'bad-frame: seed frame straddles the seed length';
        return;
      }
      this.seed.remaining -= payload.length;
      if (this.seed.remaining === 0) {
        this.applied = this.seed.offset;
        this.seed = null;
      }
      return;
    }
    if (offset > this.applied) {
      this.gaps++;
      this.closed = true; // GAP: never apply, close, reconnect with resumefrom
      return;
    }
    const dupLen = Math.min(payload.length, this.applied - offset);
    this.dupBytes += dupLen;
    const fresh = payload.subarray(dupLen);
    if (fresh.length > 0) {
      this.bytes.push(new Uint8Array(fresh));
      this.applied += fresh.length;
    }
  }

  /** True when no well-formed frame arrived within the watchdog window. */
  checkWatchdog(): boolean {
    return this.now() - this.lastFrameAt > this.watchdogMs;
  }

  private onServerFrame(frame: ServerFrame): void {
    switch (frame.t) {
      case 'hello': {
        if (this.epoch !== null && frame.epoch !== this.epoch) {
          this.applied = 0;
          this.mseq = 0;
          this.fullJoin = true;
          this.seed = null;
        }
        this.epoch = frame.epoch;
        break;
      }
      case 'seed': {
        // A zero-length seed ends immediately: the viewer is at the seed
        // offset with no seed bytes to consume (without this the viewer
        // would stay in the seed branch and the first live frame would
        // look like a seed straddle).
        if (frame.len === 0) {
          this.applied = frame.offset;
          this.seed = null;
        } else {
          this.seed = { offset: frame.offset, remaining: frame.len };
        }
        break;
      }
      case 'imagedef':
      case 'resize':
      case 'gfxreset':
      case 'sizechanged':
      case 'exit': {
        const seq = 'seq' in frame ? frame.seq : 0;
        if (seq <= this.mseq) return; // idempotent by seq: already applied
        this.mseq = seq;
        this.markers.push({ seq, t: frame.t });
        break;
      }
      case 'clipboard': {
        this.clipboards.push(frame.data_b64);
        break;
      }
      case 'error': {
        this.errors.push(frame.code);
        if (frame.code === 'bad-frame') {
          this.closed = true;
          this.badFrame = frame.msg;
        }
        break;
      }
      default:
        break;
    }
  }
}

/** Nominal backoff: min(5000, 100 * 2^n). */
export function backoffNominal(n: number): number {
  return Math.min(5000, 100 * 2 ** n);
}

/**
 * Reconnect backoff state. `n` resets to 0 only after 10 consecutive
 * well-formed frames on one connection.
 */
export class Backoff {
  private n = 0;
  private good = 0;

  /** Nominal delay for the next retry (ms). */
  nominal(): number {
    return backoffNominal(this.n);
  }

  /** Actual delay with injected jitter: nominal * (0.75 + 0.25*rand). */
  delay(rand: () => number = Math.random): number {
    return this.nominal() * (0.75 + 0.25 * rand());
  }

  /** A connection failed immediately: the next nominal doubles. */
  onFailure(): void {
    this.n++;
    this.good = 0;
  }

  /** One well-formed frame arrived on the current connection. */
  onGoodFrame(): void {
    this.good++;
    if (this.good >= 10) {
      this.n = 0;
      this.good = 0;
    }
  }
}
