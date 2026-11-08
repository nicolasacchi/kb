// kb-term tap: the multi-viewer service. Numbers the post-transform stream
// into the ring, fans out to viewers over one unix-socket connection
// (tap.sock), and joins late viewers atomically: hello, profile, imagedefs,
// seed, then live bytes from exactly the offset the seed corresponds to.
//
// Quiescence (brief rule 4): a join waits until the twin has parsed through
// ring.head AND synchronized output is OFF; preferred additionally no output
// for KB_TAP_QUIESCE_MS (default 50), bounded by KB_TAP_JOIN_WAIT_MS
// (default 3000). If sync is still ON at the bound the viewer gets
// error{busy} and is closed; if merely noisy it joins degraded and counts
// stats.degradedJoins. A join never falls inside a synchronized frame.

import { randomUUID } from 'node:crypto';
import { mkdirSync, unlinkSync, chmodSync } from 'node:fs';
import { createServer, type Server, type Socket } from 'node:net';
import { join as pathJoin } from 'node:path';
import { buildMediaStub, type GfxItem, type ImageDef } from './graphics.ts';
import { Ring } from './ring.ts';
import type { Twin } from './twin.ts';
import { Withholder } from './withhold.ts';
import {
  ENV_BINARY,
  ENV_CONTROL,
  ENV_TEXT,
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

/** Produces the seed bytes for a late joiner. T6's makeSeed lands here. */
export type SeedMaker = (twin: Twin, scrollback: number) => Uint8Array;

const defaultSeed: SeedMaker = (twin, _scrollback) => new TextEncoder().encode(twin.serialize.serialize());

export interface Transport {
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
    this.socket.destroy();
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

export class TapServer {
  readonly ring = new Ring();
  readonly withholder = new Withholder();
  epoch: string;
  readonly sockPath: string;
  stats = { joins: 0, degradedJoins: 0 };

  private readonly session: string;
  private readonly harness: string;
  private readonly boundSid: string | null;
  private readonly cols: number;
  private readonly rows: number;
  private readonly twin: Twin | null;
  private readonly makeSeed: SeedMaker;
  private readonly testMode: boolean;
  private readonly rewind: number;
  private readonly now: () => number;
  private readonly quiesceMs: number;
  private readonly joinWaitMs: number;

  private readonly viewers = new Map<string, Viewer>();
  private readonly connections = new Set<Connection>();
  private readonly defsByKey = new Map<string, ImageDef>();
  private twinOffset = 0;
  private twinTail: Promise<void> = Promise.resolve();
  private lastFeedAt = 0;
  private server: Server | null = null;

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
    this.makeSeed = opts.makeSeed ?? defaultSeed;
    this.testMode = opts.testMode ?? process.env.KB_TAP_TEST === '1';
    this.rewind = opts.rewind ?? 0;
    this.now = opts.now ?? (() => Date.now());
    this.quiesceMs = opts.quiesceMs ?? envNumber('KB_TAP_QUIESCE_MS', DEFAULT_QUISCE_MS);
    this.joinWaitMs = opts.joinWaitMs ?? envNumber('KB_TAP_JOIN_WAIT_MS', DEFAULT_JOIN_WAIT_MS);
    this.lastFeedAt = this.now();
  }

  get head(): number {
    return this.ring.head;
  }

  get viewerCount(): number {
    return this.viewers.size;
  }

  /** Feed one post-transform item batch (T4 graphics stage output). */
  feed(items: GfxItem[]): void {
    for (const item of items) {
      if (item.kind === 'bytes') {
        this.feedBytes(item.data);
      } else if (item.kind === 'imagedef') {
        const def = item.def;
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
      const start = this.ring.push(bytes);
      const end = this.ring.head;
      if (this.twin !== null) {
        const chunk = this.ring.read(start, bytes.length);
        const twin = this.twin;
        this.twinTail = this.twinTail
          .then(() => twin.feed(chunk ?? new Uint8Array(0)))
          .then(() => {
            if (end > this.twinOffset) this.twinOffset = end;
          })
          .catch(() => {
            // Twin feed failed: twinOffset stalls; joins degrade/busy.
          });
      } else {
        this.twinOffset = end;
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

  // ---------------------------------------------------------- join / resume

  /** Synchronous join; `X = ring.head` captured atomically with registration. */
  joinNow(viewer: Viewer): void {
    if (this.twin === null) throw new Error('join without a twin');
    const X = this.ring.head;
    const scrollback = viewer.profile === 'phone' ? 20000 : 50000;
    const seed = this.makeSeed(this.twin, scrollback);
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

  /** Quiescence wait; only awaits, then runs the synchronous joinNow. */
  async requestJoin(viewer: Viewer): Promise<{ ok: boolean; reason?: string }> {
    const deadline = this.now() + this.joinWaitMs;
    let degraded = false;
    for (;;) {
      const quiescent = !this.withholder.syncOn && this.twinOffset === this.ring.head && this.now() - this.lastFeedAt >= this.quiesceMs;
      if (quiescent) break;
      if (this.now() >= deadline) {
        if (this.withholder.syncOn) {
          this.writeText(viewer.connection, viewer.id, encodeServerFrame({ t: 'error', code: 'busy', msg: 'join deferred: frame in progress' }));
          this.closeViewer(viewer);
          return { ok: false, reason: 'busy' };
        }
        degraded = true;
        break;
      }
      await sleep(Math.min(10, Math.max(1, deadline - this.now())));
    }
    this.joinNow(viewer);
    if (degraded) this.stats.degradedJoins++;
    return { ok: true };
  }

  /** The resume decision table (one row per brief rule 5). */
  handleResume(viewer: Viewer, offset: number, mseq: number, epoch: string): void {
    if (epoch !== this.epoch) {
      this.requestJoin(viewer);
      return;
    }
    if (offset > this.ring.head) {
      this.writeText(viewer.connection, viewer.id, encodeServerFrame({ t: 'error', code: 'offset-ahead', msg: 'resume offset ahead of stream' }));
      this.requestJoin(viewer);
      return;
    }
    if (offset < this.ring.tail || this.ring.head - offset > LAG_LIMIT) {
      this.requestJoin(viewer);
      return;
    }
    // In range, synchronous registration: hello only, then replay.
    const from = Math.max(this.ring.tail, offset - Math.max(0, this.rewind));
    viewer.sentOffset = from;
    viewer.pending = this.ring
      .markersAfter(mseq)
      .filter((m) => m.offset >= from)
      .map((m) => ({ offset: m.offset, frame: m.frame }));
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
        if (!this.writeBinary(conn, viewer.id, viewer.sentOffset, bytes)) return;
        viewer.sentOffset += bytes.length;
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
    conn.pendingBacklog += buf.length;
    const accepted = conn.transport.write(buf);
    if (!accepted) conn.blocked = true;
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
    if (env.kind === ENV_CONTROL) {
      let msg: ControlMessage;
      try {
        msg = JSON.parse(Buffer.from(env.body).toString('utf-8')) as ControlMessage;
      } catch {
        throw new FrameError('bad-frame: malformed control envelope');
      }
      this.handleControl(conn, msg);
      return;
    }
    if (env.kind !== ENV_TEXT) return;
    const { id, frame } = parseS2C(env.body);
    const text = Buffer.from(frame).toString('utf-8');
    let value: { t?: unknown; offset?: unknown; mseq?: unknown; epoch?: unknown };
    try {
      value = JSON.parse(text) as { t?: unknown };
    } catch {
      throw new FrameError('bad-frame: malformed text frame');
    }
    if (value.t === 'resumefrom') {
      const viewer = this.viewers.get(id);
      if (viewer === undefined) return;
      this.handleResume(viewer, Number(value.offset ?? 0), Number(value.mseq ?? 0), String(value.epoch ?? ''));
      return;
    }
    // ping and any other C->S text frame: accepted, no reply.
  }

  private handleControl(conn: Connection, msg: ControlMessage): void {
    if (msg.t === 'viewer-open') {
      const existing = this.viewers.get(msg.viewer);
      if (existing !== undefined) {
        existing.connection.viewers.delete(msg.viewer);
      }
      const viewer: Viewer = {
        id: msg.viewer,
        profile: msg.profile,
        connection: conn,
        sentOffset: 0,
        lastTypedAt: 0,
        pending: [],
        state: 'joining',
      };
      this.viewers.set(msg.viewer, viewer);
      conn.viewers.add(msg.viewer);
      if (msg.resume === null) {
        this.requestJoin(viewer).catch(() => {
          // A failed join surfaces as a busy/closed viewer; nothing to retry.
        });
      } else {
        this.handleResume(viewer, msg.resume.offset, msg.resume.mseq, msg.resume.epoch);
      }
      return;
    }
    if (msg.t === 'viewer-close') {
      const viewer = this.viewers.get(msg.viewer);
      if (viewer !== undefined) this.closeViewer(viewer);
      return;
    }
    if (msg.t === 'typed') {
      const viewer = this.viewers.get(msg.viewer);
      if (viewer !== undefined) viewer.lastTypedAt = this.now();
      return;
    }
    if (msg.t === 'stats') {
      if (!this.testMode) return;
      const body: StatsResponse = { t: 'stats', joins: this.stats.joins, degradedJoins: this.stats.degradedJoins, viewers: this.viewers.size };
      conn.transport.write(encodeEnvelope(ENV_CONTROL, Buffer.from(encodeStatsResponse(body), 'utf-8')));
    }
  }

  /** Feed raw bytes into one attached connection (the socket data path). */
  feedTransport(conn: Connection, chunk: Uint8Array): void {
    for (const env of conn.decoder.push(chunk)) this.handleEnvelope(conn, env);
  }

  // ------------------------------------------------------------- real socket

  /** Listen on `${runDir}/s/<session>/tap.sock` with mode 0600. */
  async listen(): Promise<void> {
    mkdirSync(pathJoin(this.sockPath, '..'), { recursive: true });
    try {
      unlinkSync(this.sockPath);
    } catch {
      // No stale socket: fresh boot.
    }
    const previous = process.umask();
    process.umask(0o177); // no wider-mode window: 0600 from birth
    try {
      const { promise, resolve, reject } = Promise.withResolvers<void>();
      this.server = createServer((socket: Socket) => {
        socket.on('error', () => socket.destroy());
        const transport = new NetTransport(socket);
        const conn = this.attachTransport(transport);
        socket.on('data', (chunk: Buffer) => this.feedTransport(conn, chunk));
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
