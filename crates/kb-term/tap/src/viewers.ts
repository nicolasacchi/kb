// kb-term tap: the multi-viewer service. Numbers the post-transform stream
// into the ring, fans out to viewers over one unix-socket connection
// (tap.sock), and joins late viewers atomically: hello, profile, imagedefs,
// seed, then live bytes from exactly the offset the seed corresponds to.
//
// Quiescence (brief rule 4, T6 gate): a join waits until the twin has parsed
// through ring.head AND synchronized output is OFF AND the ground tracker is
// at ground AND no query-strip bytes are held; preferred additionally no
// output for KB_TAP_QUIESCE_MS (default 50), bounded by KB_TAP_JOIN_WAIT_MS
// (default 3000). If sync is still ON, the twin lags, or the twin broke, the
// viewer gets error{busy} and is closed; if merely noisy (twin caught up)
// it joins degraded and counts stats.degradedJoins. A join never falls
// inside a synchronized frame or mid-escape.

import { randomUUID } from 'node:crypto';
import { mkdirSync, unlinkSync, chmodSync } from 'node:fs';
import { connect, createServer, type Server, type Socket } from 'node:net';
import { join as pathJoin } from 'node:path';
import { buildMediaStub, type GfxItem, type ImageDef } from './graphics.ts';
import { GroundTracker } from './ground.ts';
import { Withholder } from './withhold.ts';
import { Ring } from './ring.ts';
import { buildSeed } from './seed.ts';
import type { Twin } from './twin.ts';
import {
  ENV_BINARY,
  ENV_CONTROL,
  ENV_TEXT,
  decodeTextFrame,
  encodeBinaryFrame,
  encodeEnvelope,
  encodeServerFrame,
  encodeStatsResponse,
  EnvelopeDecoder,
  FrameError,
  parseS2C,
  wrapS2C,
  type ControlMessage,
  type Envelope,
  type ServerFrame,
  type StatsResponse,
} from './wire.ts';

const LIVE_CHUNK = 64 * 1024;
const LAG_LIMIT = 8 * 1024 * 1024;
const CLIPBOARD_WINDOW_MS = 10_000;
const DEFAULT_QUISCE_MS = 50;
const DEFAULT_JOIN_WAIT_MS = 3000;

export type Profile = 'desktop' | 'phone';

/** Produces the seed bytes for a late joiner at the twin's current offset.
 * The production default builds T6's state-restore seed (serialize body +
 * tail) via `buildSeed`; tests inject a recording stub. */
export type SeedMaker = (twin: Twin, scrollback: number, atOffset: number) => Uint8Array;

export interface Transport {
  /** Hand the bytes to the transport/socket buffer and report backpressure.
   * Node semantics (and the mock transport models exactly this): `write()`
   * ALWAYS hands the bytes off -- a `false` return only means "stop writing
   * until 'drain'", never "the bytes were rejected". Callers must commit the
   * handoff before honouring the pause signal (see `serviceViewer`), or a
   * drained re-service would write the same range twice. */
  write(bytes: Uint8Array): boolean;
  close(): void;
  onDrain(cb: () => void): void;
  onClose(cb: () => void): void;
}

export interface TapServerOpts {
  session: string;
  harness: string;
  boundSid?: string;
  epoch?: string;
  runDir?: string;
  cols?: number;
  rows?: number;
  twin?: Twin | null;
  makeSeed?: SeedMaker;
  testMode?: boolean;
  rewind?: number;
  now?: () => number;
  quiesceMs?: number;
  joinWaitMs?: number;
  /** Held query-strip bytes (main wires the per-pane pipeline's pending here);
   * a join never counts as quiescent while these are undecided. */
  stripPending?: () => number;
  /** Test only: shrink the ring so >cap chunk handling is reachable. */
  ringCap?: number;
}

interface PendingControl {
  offset: number;
  frame: string;
}

interface Viewer {
  id: string;
  profile: Profile;
  connection: Connection;
  sentOffset: number;
  lastTypedAt: number;
  pending: PendingControl[];
  state: 'joining' | 'live';
}

interface Connection {
  transport: Transport;
  viewers: Set<string>;
  decoder: EnvelopeDecoder;
  pendingBacklog: number;
  blocked: boolean;
  closed: boolean;
}

class NetTransport implements Transport {
  constructor(private socket: Socket) {}
  write(bytes: Uint8Array): boolean {
    return this.socket.write(bytes);
  }
  close(): void {
    // end (not destroy): a busy frame written immediately before close must
    // reach the client before the FIN.
    this.socket.end();
  }
  onDrain(cb: () => void): void {
    this.socket.on('drain', cb);
  }
  onClose(cb: () => void): void {
    this.socket.on('close', cb);
  }
}

function sleep(ms: number): Promise<void> {
  const { promise, resolve } = Promise.withResolvers<void>();
  setTimeout(resolve, ms);
  return promise;
}

function envNumber(name: string, fallback: number): number {
  const raw = process.env[name];
  if (raw === undefined || raw === '') return fallback;
  const n = Number(raw);
  return Number.isFinite(n) && n >= 0 ? n : fallback;
}

/** Resume coordinates must be safe integers >= 0 ("bad-frame" otherwise). */
function validOffset(value: unknown, name: string): number {
  if (!Number.isSafeInteger(value) || (value as number) < 0) {
    throw new FrameError(`bad-frame: resumefrom.${name} must be a safe integer >= 0`);
  }
  return value as number;
}

function validEpoch(value: unknown): string {
  if (typeof value !== 'string') throw new FrameError('bad-frame: resumefrom.epoch must be a string');
  return value;
}

/** A viewer id that wrapS2C can carry: a 1..255-byte string. */
function validViewerId(value: unknown, t: string): string {
  if (typeof value !== 'string' || value.length === 0 || Buffer.byteLength(value, 'utf-8') > 255) {
    throw new FrameError(`bad-frame: ${t}.viewer must be a 1..255-byte string`);
  }
  return value;
}

export class TapServer {
  readonly ring: Ring;
  readonly withholder = new Withholder();
  epoch: string;
  readonly sockPath: string;
  stats = { joins: 0, degradedJoins: 0 };

  private readonly session: string;
  private readonly harness: string;
  private readonly boundSid: string | null;
  private cols: number;
  private rows: number;
  private readonly twin: Twin | null;
  private readonly makeSeed: SeedMaker;
  private readonly testMode: boolean;
  private readonly rewind: number;
  private readonly now: () => number;
  private readonly quiesceMs: number;
  private readonly joinWaitMs: number;
  private readonly getStripPending: () => number;

  private readonly viewers = new Map<string, Viewer>();
  private readonly connections = new Set<Connection>();
  private readonly defsByKey = new Map<string, ImageDef>();
  private readonly pendingJoins = new Set<Promise<unknown>>();
  private readonly ground = new GroundTracker();
  private twinOffset = 0;
  private twinTail: Promise<void> = Promise.resolve();
  private lastFeedAt = 0;
  private server: Server | null = null;
  /** Latched after any twin feed failure: joins then take the busy path
   * rather than lose the (twinOffset, head] gap. */
  private twinBroken = false;
  /** Reseed pause gate (see setTwinPaused): twin feeds queue behind it. */
  private twinGate: Promise<void> | null = null;
  private resumeGate: (() => void) | null = null;

  constructor(opts: TapServerOpts) {
    this.session = opts.session;
    this.harness = opts.harness;
    this.boundSid = opts.boundSid ?? null;
    this.epoch = opts.epoch ?? randomUUID().replaceAll('-', '');
    const runDir = opts.runDir ?? process.env.KB_TERM_RUN_DIR ?? '/run/kb-term';
    this.sockPath = pathJoin(runDir, 's', this.session, 'tap.sock');
    this.cols = opts.cols ?? 120;
    this.rows = opts.rows ?? 40;
    this.twin = opts.twin ?? null;
    // T6's state-restore seed is the production default; tests inject a stub
    // maker (buildSeed requires a real twin's xterm internals, stub twins
    // cannot serve it).
    this.makeSeed = opts.makeSeed ?? ((twin, scrollback, atOffset) => {
      const seed = buildSeed(twin, {
        scrollback: scrollback === 20000 ? 20000 : 50000,
        atOffset,
        ground: this.ground,
      });
      return seed.bytes;
    });
    this.testMode = opts.testMode ?? process.env.KB_TAP_TEST === '1';
    this.rewind = opts.rewind ?? 0;
    this.now = opts.now ?? (() => Date.now());
    this.quiesceMs = opts.quiesceMs ?? envNumber('KB_TAP_QUIESCE_MS', DEFAULT_QUISCE_MS);
    this.joinWaitMs = opts.joinWaitMs ?? envNumber('KB_TAP_JOIN_WAIT_MS', DEFAULT_JOIN_WAIT_MS);
    this.getStripPending = opts.stripPending ?? (() => 0);
    this.lastFeedAt = this.now();
    this.ring = new Ring(opts.ringCap);
  }

  get head(): number {
    return this.ring.head;
  }
  get viewerCount(): number {
    return this.viewers.size;
  }

  /** In-flight `requestJoin` count (test seam: lets a test wait until a join
   * has started and until it has settled, instead of guessing with sleeps). */
  get pendingJoinCount(): number {
    return this.pendingJoins.size;
  }

  /** Feed one post-transform item batch (T4 graphics stage output). */
  feed(items: GfxItem[]): void {
    for (const item of items) {
      if (item.kind === 'bytes') {
        this.feedBytes(item.data);
      } else if (item.kind === 'imagedef') {
        const def = item.def;
        // Keep only the latest generation per id: superseded gens are not
        // twin-buffer state and must not accumulate within the epoch.
        for (const [key, prev] of this.defsByKey) {
          if (prev.id === def.id && prev.gen !== def.gen) this.defsByKey.delete(key);
        }
        this.defsByKey.set(`${def.id}:${def.gen}`, def);
        this.recordMarker((seq, offset) =>
          encodeServerFrame({ t: 'imagedef', seq, id: def.id, gen: def.gen, sha256: def.sha256, mime: def.mime, w: def.w, h: def.h, cols: def.cols, rows: def.rows, offset }),
        );
      } else if (item.kind === 'gfxreset') {
        const gen = item.gen;
        this.recordMarker((seq, offset) => encodeServerFrame({ t: 'gfxreset', seq, gen, offset }));
      } else {
        // mediadef: stub bytes only (the mediadef itself is its marker).
        this.feedBytes(buildMediaStub(item.mediaKind, item.sha256, item.len));
      }
    }
  }

  /**
   * Record an in-order control marker: assigns the seq via the ring, stores
   * the frame, and queues it for every live viewer at its offset.
   */
  recordMarker(build: (seq: number, offset: number) => string): number {
    const seq = this.ring.addMarker(build);
    for (const viewer of this.viewers.values()) {
      if (viewer.state !== 'live') continue;
      viewer.pending.push({ offset: this.ring.head, frame: build(seq, this.ring.head) });
    }
    return seq;
  }

  private feedBytes(data: Uint8Array): void {
    if (data.length === 0) return;
    const { bytes, clipboards } = this.withholder.push(data, this.ring.head);
    if (bytes.length > 0) {
      this.ring.push(bytes);
      const end = this.ring.head;
      if (this.twin !== null) {
        // Feed the transformed bytes themselves, not a ring re-read: a
        // chunk larger than the ring cap retains only its tail in the
        // ring, but the twin must parse the whole stream.
        const twin = this.twin;
        this.twinTail = this.twinTail
          .then(() => this.twinGate ?? Promise.resolve())
          .then(() => twin.feed(bytes))
          .then(() => {
            // Twin write callbacks fired in order (RACE RULE for the ground
            // tracker, shared with T6's gate): ground tracks exactly what
            // the twin has parsed through, before the offset advances.
            this.ground.push(bytes);
            if (end > this.twinOffset) this.twinOffset = end;
          })
          .catch(() => {
            // Twin feed failed: latch broken so joins take the busy path
            // instead of losing the (twinOffset, head] gap.
            this.twinBroken = true;
          });
      } else {
        this.twinOffset = end;
        this.ground.push(bytes);
      }
    }
    this.lastFeedAt = this.now();
    for (const ev of clipboards) {
      for (const viewer of this.viewers.values()) {
        if (viewer.state !== 'live') continue;
        if (this.now() - viewer.lastTypedAt > CLIPBOARD_WINDOW_MS) continue;
        viewer.pending.push({ offset: ev.offset, frame: encodeServerFrame({ t: 'clipboard', offset: ev.offset, data_b64: ev.data_b64 }) });
      }
    }
    for (const conn of this.connections) this.serviceConnection(conn);
  }

  /** Pause/resume twin feeding for reseeds: while paused, twinOffset lags
   * ring.head, so joins take the busy path; on resume the queued chain
   * catches up and twinOffset advances to head. */
  setTwinPaused(paused: boolean): void {
    if (paused) {
      if (this.twinGate === null) {
        const { promise, resolve } = Promise.withResolvers<void>();
        this.resumeGate = resolve;
        this.twinGate = promise;
      }
    } else {
      this.resumeGate?.();
      this.resumeGate = null;
      this.twinGate = null;
    }
  }
  /** Apply a grid change (audit F6): resize the twin and stamp a resize
   * marker at the current head; later profiles carry the new grid. */
  setGrid(cols: number, rows: number): void {
    this.cols = cols;
    this.rows = rows;
    this.twin?.resize(cols, rows);
    this.recordMarker((seq, offset) => encodeServerFrame({ t: 'resize', seq, offset, cols, rows }));
  }


  // ---------------------------------------------------------- join / resume

  /** Synchronous join; `X = ring.head` captured atomically with registration. */
  joinNow(viewer: Viewer): void {
    if (this.twin === null) throw new Error('join without a twin');
    const X = this.ring.head;
    // Never seed past what the twin parsed: setting sentOffset to head while
    // twinOffset lags would lose the (twinOffset, head] gap (audit F3).
    if (this.twinOffset !== X || this.twinBroken) throw new Error('join beyond twin offset');
    const scrollback = viewer.profile === 'phone' ? 20000 : 50000;
    const seed = this.makeSeed(this.twin, scrollback, X);
    viewer.sentOffset = X;
    viewer.pending = [];
    viewer.state = 'live';
    const conn = viewer.connection;
    const profile: ServerFrame = {
      t: 'profile',
      xterm: '6.0.0',
      serialize: '0.14.0',
      width: 'unicode-graphemes@0.4.0',
      image: '0.9.0',
      cols: this.cols,
      rows: this.rows,
      scrollback,
      scroll_on_erase_in_display: true,
      cell_px_w: 9,
      cell_px_h: 18,
      theme: null,
    };
    const imagedefs = [...this.defsByKey.values()].map((def) =>
      encodeServerFrame({ t: 'imagedef', seq: 0, id: def.id, gen: def.gen, sha256: def.sha256, mime: def.mime, w: def.w, h: def.h, cols: def.cols, rows: def.rows, offset: X }),
    );
    this.writeText(conn, viewer.id, encodeServerFrame({ t: 'hello', proto: 'kb-term/1', session: this.session, epoch: this.epoch, harness: this.harness, bound_sid: this.boundSid }));
    this.writeText(conn, viewer.id, encodeServerFrame(profile));
    for (const frame of imagedefs) this.writeText(conn, viewer.id, frame);
    this.writeText(conn, viewer.id, encodeServerFrame({ t: 'seed', offset: X, len: seed.length }));
    for (let i = 0; i < seed.length; i += LIVE_CHUNK) {
      this.writeBinary(conn, viewer.id, X, seed.subarray(i, Math.min(seed.length, i + LIVE_CHUNK)));
    }
    this.stats.joins++;
  }

  /** Start a tracked join (see the `pendingJoinCount` test seam). */
  private trackJoin(viewer: Viewer): void {
    const p = this.requestJoin(viewer);
    this.pendingJoins.add(p);
    const cleanup = () => {
      this.pendingJoins.delete(p);
    };
    // `requestJoin` itself reports failure by value; a stray rejection must
    // not surface as an unhandled rejection on this cleanup chain either.
    p.then(cleanup, cleanup);
    p.catch(() => {
      // A failed join surfaces as a busy/closed viewer; nothing to retry.
    });
  }

  /** Quiescence wait; only awaits, then runs the synchronous joinNow. */
  async requestJoin(viewer: Viewer): Promise<{ ok: boolean; reason?: string }> {
    const deadline = this.now() + this.joinWaitMs;
    let degraded = false;
    for (;;) {
      // One gate, T6's rule: twin parsed through head, synchronized output
      // off, ground (not mid-escape), no stripper-held bytes, plus the
      // silence preference. twinOffset is the parsed-byte counter (the
      // QuiescentJoinGate's equivalent: ground advances with twin writes).
      const quiescent = !this.withholder.syncOn && this.twinOffset === this.ring.head && this.ground.atGround() && this.getStripPending() === 0 && this.now() - this.lastFeedAt >= this.quiesceMs;
      if (quiescent) break;
      if (this.now() >= deadline) {
        // Degraded join is only sound when the twin is caught up: joining
        // otherwise would set sentOffset past twinOffset and lose the gap
        // (and a broken twin can never catch up). Those cases take the busy
        // path (error + close) instead of degrading into data loss.
        if (this.withholder.syncOn || this.twinOffset !== this.ring.head || this.twinBroken) {
          this.writeText(viewer.connection, viewer.id, encodeServerFrame({ t: 'error', code: 'busy', msg: 'join deferred: frame in progress' }));
          this.closeViewer(viewer);
          // A dropped join is terminal: end the connection once no viewer
          // remains on it, so the client sees the busy frame and the socket
          // closes (the header contract: "error{busy} and is closed").
          if (viewer.connection.viewers.size === 0) viewer.connection.transport.close();
          return { ok: false, reason: 'busy' };
        }
        degraded = true;
        break;
      }
      await sleep(Math.min(10, Math.max(1, deadline - this.now())));
    }
    // The viewer may have gone away (viewer-close) or been converted to
    // 'live' by a resumefrom while the quiescence wait was in flight; a
    // full join must not run for a dead or already-live viewer.
    if (viewer.state !== 'joining' || this.viewers.get(viewer.id) !== viewer) {
      return { ok: false, reason: 'gone' };
    }
    try {
      this.joinNow(viewer);
    } catch {
      // joinNow threw (no twin, oversize seed?): unregister, never park.
      this.closeViewer(viewer);
      return { ok: false, reason: 'error' };
    }
    if (degraded) this.stats.degradedJoins++;
    return { ok: true };
  }

  /** The resume decision table (one row per brief rule 5). */
  handleResume(viewer: Viewer, offset: number, mseq: number, epoch: string): void {
    if (epoch !== this.epoch) {
      this.trackJoin(viewer);
      return;
    }
    if (offset > this.ring.head) {
      this.writeText(viewer.connection, viewer.id, encodeServerFrame({ t: 'error', code: 'offset-ahead', msg: 'resume offset ahead of stream' }));
      this.trackJoin(viewer);
      return;
    }
    if (offset < this.ring.tail || this.ring.head - offset > LAG_LIMIT) {
      this.trackJoin(viewer);
      return;
    }
    // In range, synchronous registration: hello only, then replay.
    const from = Math.max(this.ring.tail, offset - Math.max(0, this.rewind));
    viewer.sentOffset = from;
    // Replay exactly markersAfter(mseq), in order. No offset filter: a
    // marker with offset < from but seq > mseq was never applied by this
    // viewer and is still owed (serviceViewer emits it before the first
    // byte at `from` because its offset <= sentOffset).
    viewer.pending = this.ring.markersAfter(mseq).map((m) => ({ offset: m.offset, frame: m.frame }));
    viewer.state = 'live';
    this.writeText(viewer.connection, viewer.id, encodeServerFrame({ t: 'hello', proto: 'kb-term/1', session: this.session, epoch: this.epoch, harness: this.harness, bound_sid: this.boundSid }));
    this.serviceViewer(viewer);
  }

  // ------------------------------------------------------------ connection IO

  /** Attach a transport (the real unix socket in production, a mock in tests). */
  attachTransport(transport: Transport): Connection {
    const conn: Connection = { transport, viewers: new Set(), decoder: new EnvelopeDecoder(), pendingBacklog: 0, blocked: false, closed: false };
    this.connections.add(conn);
    transport.onDrain(() => {
      conn.pendingBacklog = 0;
      conn.blocked = false;
      if (!conn.closed) this.serviceConnection(conn);
    });
    transport.onClose(() => this.closeConnection(conn));
    return conn;
  }

  private serviceConnection(conn: Connection): void {
    if (conn.closed) return;
    for (const id of [...conn.viewers]) {
      const viewer = this.viewers.get(id);
      if (viewer === undefined) continue;
      this.serviceViewer(viewer);
      if (conn.closed) return;
    }
  }

  private serviceViewer(viewer: Viewer): void {
    if (viewer.state !== 'live') return;
    const conn = viewer.connection;
    if (conn.closed) return;
    for (;;) {
      if (this.ring.head - viewer.sentOffset > LAG_LIMIT || viewer.sentOffset < this.ring.tail) {
        this.dropViewer(viewer, 'too-far-behind');
        return;
      }
      if (conn.pendingBacklog > LAG_LIMIT) {
        for (const id of [...conn.viewers]) {
          const v = this.viewers.get(id);
          if (v !== undefined) this.dropViewer(v, 'too-far-behind');
        }
        return;
      }
      if (conn.blocked) return;
      const next = viewer.pending[0];
      if (next !== undefined && next.offset <= viewer.sentOffset) {
        viewer.pending.shift();
        if (!this.writeText(conn, viewer.id, next.frame)) return;
        continue;
      }
      const bytes = this.ring.read(viewer.sentOffset, LIVE_CHUNK);
      if (bytes !== null && bytes.length > 0) {
        // Commit the handoff BEFORE honouring the pause signal: under the
        // Transport contract write() always hands the bytes off, so a
        // drained re-service must not re-read (and re-write) this range.
        // (The marker path above already shifts before write for the same
        // reason.) `blocked` in write() is the pause signal only.
        const ok = this.writeBinary(conn, viewer.id, viewer.sentOffset, bytes);
        viewer.sentOffset += bytes.length;
        if (!ok) return;
        continue;
      }
      return;
    }
  }

  private writeText(conn: Connection, id: string, frame: string): boolean {
    return this.write(conn, encodeEnvelope(ENV_TEXT, wrapS2C(id, Buffer.from(frame, 'utf-8'))));
  }

  private writeBinary(conn: Connection, id: string, offset: number, payload: Uint8Array): boolean {
    return this.write(conn, encodeEnvelope(ENV_BINARY, wrapS2C(id, encodeBinaryFrame(offset, payload))));
  }

  private write(conn: Connection, buf: Uint8Array): boolean {
    // Backlog = bytes buffered in the socket but not yet flushed: only a
    // write that returns false (Node's backpressure signal, followed by a
    // 'drain') adds anything. The bytes are already handed off (see the
    // Transport contract); `blocked` is the pause signal only. Accepted
    // bytes are flushed by the socket and must not accumulate over the
    // connection's lifetime.
    const accepted = conn.transport.write(buf);
    if (!accepted) {
      conn.pendingBacklog += buf.length;
      conn.blocked = true;
    }
    return accepted;
  }

  private dropViewer(viewer: Viewer, code: string): void {
    const conn = viewer.connection;
    try {
      this.writeText(conn, viewer.id, encodeServerFrame({ t: 'error', code, msg: `viewer dropped: ${code}` }));
    } catch {
      // Transport already gone; the close below still unregisters the viewer.
    }
    conn.viewers.delete(viewer.id);
    this.viewers.delete(viewer.id);
  }

  private closeViewer(viewer: Viewer): void {
    viewer.connection.viewers.delete(viewer.id);
    this.viewers.delete(viewer.id);
  }

  private closeConnection(conn: Connection): void {
    if (conn.closed) return;
    conn.closed = true;
    for (const id of [...conn.viewers]) this.viewers.delete(id);
    conn.viewers.clear();
    this.connections.delete(conn);
  }

  private handleEnvelope(conn: Connection, env: Envelope): void {
    // Single-validator parsing: wire.decodeTextFrame enforces the required
    // fields plus onlyKeys (Rust deny_unknown_fields) for client/control
    // frames, so a viewer-open with an unknown field (or a resumefrom with
    // trailing junk) is a FrameError here exactly as in wire.decodeTextFrame
    // and Rust -- no second hand-rolled validator to drift. A FrameError
    // propagates to feedTransport, which closes only this offender; an
    // unknown `t` and a known frame of the other family are ignored (no
    // reply), as before.
    if (env.kind === ENV_CONTROL) {
      const decoded = decodeTextFrame(Buffer.from(env.body).toString('utf-8'));
      if (decoded.kind === 'unknown') return;
      const msg = decoded.frame as ControlMessage;
      if (msg.t !== 'viewer-open' && msg.t !== 'viewer-close' && msg.t !== 'typed' && msg.t !== 'stats') return;
      this.handleControl(conn, msg);
      return;
    }
    if (env.kind !== ENV_TEXT) return;
    const { id, frame } = parseS2C(env.body);
    const decoded = decodeTextFrame(Buffer.from(frame).toString('utf-8'));
    if (decoded.kind === 'unknown') return;
    const f = decoded.frame;
    if (f.t === 'resumefrom') {
      const viewer = this.viewers.get(id);
      if (viewer === undefined) return;
      this.handleResume(viewer, validOffset(f.offset, 'offset'), validOffset(f.mseq, 'mseq'), validEpoch(f.epoch));
      return;
    }
    // ping and any other C->S text frame: accepted, no reply.
  }

  private handleControl(conn: Connection, msg: ControlMessage): void {
    // msg passed wire.decodeTextFrame in handleEnvelope (required fields and
    // types checked); the id-length/profile checks below are still needed
    // because the wire validator does not bound the viewer-id length.
    if (msg.t === 'viewer-open') {
      const id = validViewerId((msg as { viewer?: unknown }).viewer, 'viewer-open');
      const profile = (msg as { profile?: unknown }).profile;
      if (profile !== 'desktop' && profile !== 'phone') {
        throw new FrameError('bad-frame: viewer-open.profile must be desktop|phone');
      }
      const rawResume = (msg as { resume?: unknown }).resume;
      let resume: { offset: number; mseq: number; epoch: string } | null = null;
      // A missing resume means a fresh join (Rust Option<ResumeInfo>).
      if (rawResume !== null && rawResume !== undefined) {
        if (typeof rawResume !== 'object' || rawResume === null) {
          throw new FrameError('bad-frame: viewer-open.resume must be null or an object');
        }
        const r = rawResume as { offset?: unknown; mseq?: unknown; epoch?: unknown };
        resume = { offset: validOffset(r.offset, 'resume.offset'), mseq: validOffset(r.mseq, 'resume.mseq'), epoch: validEpoch(r.epoch) };
      }
      const existing = this.viewers.get(id);
      if (existing !== undefined) {
        existing.connection.viewers.delete(id);
      }
      const viewer: Viewer = {
        id,
        profile,
        connection: conn,
        sentOffset: 0,
        // Never typed: -Infinity keeps the clipboard window closed even on
        // tiny fake clocks (a 0 sentinel would fake a recent typing there).
        lastTypedAt: -Infinity,
        pending: [],
        state: 'joining',
      };
      this.viewers.set(id, viewer);
      conn.viewers.add(id);
      if (resume === null) {
        this.trackJoin(viewer);
      } else {
        this.handleResume(viewer, resume.offset, resume.mseq, resume.epoch);
      }
      return;
    }
    if (msg.t === 'viewer-close') {
      const id = validViewerId((msg as { viewer?: unknown }).viewer, 'viewer-close');
      const viewer = this.viewers.get(id);
      if (viewer !== undefined) this.closeViewer(viewer);
      return;
    }
    if (msg.t === 'typed') {
      const id = validViewerId((msg as { viewer?: unknown }).viewer, 'typed');
      const viewer = this.viewers.get(id);
      if (viewer !== undefined) viewer.lastTypedAt = this.now();
      return;
    }
    if (msg.t === 'stats') {
      if (!this.testMode) return;
      const body: StatsResponse = { t: 'stats', joins: this.stats.joins, degradedJoins: this.stats.degradedJoins, viewers: this.viewers.size };
      conn.transport.write(encodeEnvelope(ENV_CONTROL, Buffer.from(encodeStatsResponse(body), 'utf-8')));
    }
  }

  /** Feed raw bytes into one attached connection (the socket data path). A
   * malformed or oversize envelope closes only this connection. */
  feedTransport(conn: Connection, chunk: Uint8Array): void {
    try {
      for (const env of conn.decoder.push(chunk)) this.handleEnvelope(conn, env);
    } catch {
      conn.transport.close();
      this.closeConnection(conn);
    }
  }

  // ------------------------------------------------------------- real socket

  /**
   * Brief section 6: remove a stale socket file on boot only if nothing
   * answers on it. If a live tap answers, refuse (never steal its path).
   */
  private async refuseWhenLive(): Promise<void> {
    const { promise, resolve } = Promise.withResolvers<boolean>();
    const probe = connect(this.sockPath);
    const done = (v: boolean) => {
      probe.destroy();
      resolve(v);
    };
    probe.once('connect', () => done(true));
    probe.once('error', () => done(false));
    // No answer within 2 s: assume live; the refusal is the safe side.
    probe.setTimeout(2000, () => done(true));
    const live = await promise;
    if (live) throw new Error(`tap socket ${this.sockPath} is served by a live tap`);
    try {
      unlinkSync(this.sockPath);
    } catch {
      // No stale socket: fresh boot.
    }
  }

  /** Listen on `${runDir}/s/<session>/tap.sock` with mode 0600. */
  async listen(): Promise<void> {
    mkdirSync(pathJoin(this.sockPath, '..'), { recursive: true });
    await this.refuseWhenLive();
    const previous = process.umask();
    process.umask(0o177); // no wider-mode window: 0600 from birth
    try {
      const { promise, resolve, reject } = Promise.withResolvers<void>();
      this.server = createServer((socket: Socket) => {
        socket.on('error', () => socket.destroy());
        const transport = new NetTransport(socket);
        const conn = this.attachTransport(transport);
        socket.on('data', (chunk: Buffer) => {
          try {
            this.feedTransport(conn, chunk);
          } catch {
            // feedTransport already closes only the offender; this is belt
            // and braces against an unexpected throw.
            try {
              conn.transport.close();
            } catch {
              // Already gone.
            }
            this.closeConnection(conn);
          }
        });
      });
      this.server.on('error', reject);
      this.server.listen(this.sockPath, () => {
        this.server?.off('error', reject);
        resolve();
      });
      await promise;
    } finally {
      process.umask(previous);
    }
    try {
      chmodSync(this.sockPath, 0o600);
    } catch {
      // umask already guarantees the mode; chmod is belt and braces.
    }
  }

  async close(): Promise<void> {
    for (const conn of [...this.connections]) {
      try {
        conn.transport.close();
      } catch {
        // Already gone.
      }
      conn.closed = true;
    }
    this.connections.clear();
    this.viewers.clear();
    const server = this.server;
    this.server = null;
    if (server !== null) {
      await new Promise<void>((resolve) => server.close(() => resolve()));
      try {
        unlinkSync(this.sockPath);
      } catch {
        // Already removed.
      }
    }
  }
}
