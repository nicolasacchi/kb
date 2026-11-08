// kb-term tap: the kb-term/1 wire contract in TypeScript (text frames,
// binary frames, the tap.sock envelope codec). Hand mirror of the Rust
// `crates/kb-term/src/wire.rs`; the shared golden directory pins both
// byte-for-byte, so the two must agree exactly.
//
// Text frames are UTF-8 JSON objects with discriminator "t"; key order is
// declaration order. Binary frames are [8-byte big-endian u64 offset][payload].
// Offsets are JS-safe integers (< 2^53); a header offset >= 2^53 is a
// protocol error in both languages. Text decode is hand-written: an unknown
// "t" yields {kind:'unknown', raw} holding the raw input string, so
// re-encode returns that exact string; malformed JSON is an error.

export const MAX_OFFSET = 1n << 53n;
/** Largest legal tap.sock envelope length; the u32 length counts the kind
 * byte plus the body. The 1 MiB cap was added by round-1 review (brief
 * section 6 sets no cap of its own), raised by 512 in round 3 so the
 * largest deliverable clipboard envelope still fits: the withhold OSC 52
 * budget is 1 MiB of base64 plus the clipboard JSON (~56 bytes), the S->C
 * id prefix (<= 255 bytes) and the kind byte. Both codecs (this file and
 * `crates/kb-term/src/wire.rs`) apply this identical constant. */
export const MAX_ENVELOPE_LEN = 1_048_576 + 512;

/** Protocol error for a malformed kb-term/1 frame (the "bad-frame" code). */
export class FrameError extends Error {
  constructor(message: string) {
    super(message);
    this.name = 'FrameError';
  }
}

export type ServerFrame =
  | { t: 'hello'; proto: string; session: string; epoch: string; harness: string; bound_sid: string | null }
  | {
      t: 'profile';
      xterm: string;
      serialize: string;
      width: string;
      image: string;
      cols: number;
      rows: number;
      scrollback: number;
      scroll_on_erase_in_display: boolean;
      cell_px_w: number;
      cell_px_h: number;
      theme: null;
    }
  | { t: 'imagedef'; seq: number; id: number; gen: number; sha256: string; mime: string; w: number; h: number; cols: number; rows: number; offset: number }
  | { t: 'seed'; offset: number; len: number }
  | { t: 'resize'; seq: number; offset: number; cols: number; rows: number }
  | { t: 'sizechanged'; seq: number; cols: number; rows: number; by: string; offset: number }
  | { t: 'gfxreset'; seq: number; gen: number; offset: number }
  | { t: 'exit'; seq: number; code: number | null; offset: number }
  | { t: 'clipboard'; offset: number; data_b64: string }
  | { t: 'error'; code: string; msg: string };

export type ClientFrame =
  | { t: 'resumefrom'; offset: number; mseq: number; epoch: string }
  | { t: 'takesize'; cols: number; rows: number }
  | { t: 'input'; data: string }
  | { t: 'ping' };

export interface ResumeInfo {
  offset: number;
  mseq: number;
  epoch: string;
}

export type ControlMessage =
  | { t: 'viewer-open'; viewer: string; profile: 'desktop' | 'phone'; resume: ResumeInfo | null }
  | { t: 'viewer-close'; viewer: string }
  | { t: 'typed'; viewer: string }
  | { t: 'stats' };

export interface StatsResponse {
  t: 'stats';
  joins: number;
  degradedJoins: number;
  viewers: number;
}

export type TextFrame =
  | { kind: 'known'; frame: ServerFrame | ClientFrame | ControlMessage }
  | { kind: 'unknown'; raw: string };

/** Encode a server frame to its exact golden bytes. */
export function encodeServerFrame(frame: ServerFrame): string {
  return JSON.stringify(frame);
}

/** Encode a client frame to its exact golden bytes. */
export function encodeClientFrame(frame: ClientFrame): string {
  return JSON.stringify(frame);
}

/** Encode a control message to its exact golden bytes. */
export function encodeControlMessage(msg: ControlMessage): string {
  return JSON.stringify(msg);
}
/** Encode a stats response to its exact golden bytes (joins, degradedJoins, viewers in field order). */
export function encodeStatsResponse(resp: StatsResponse): string {
  return JSON.stringify(resp);
}
const SERVER_T: Record<string, true> = {
  hello: true,
  profile: true,
  imagedef: true,
  seed: true,
  resize: true,
  sizechanged: true,
  gfxreset: true,
  exit: true,
  clipboard: true,
  error: true,
};
const CLIENT_T: Record<string, true> = { resumefrom: true, takesize: true, input: true, ping: true };
const CONTROL_T: Record<string, true> = { 'viewer-open': true, 'viewer-close': true, typed: true, stats: true };

/** A parsed JSON value with a string discriminator. Narrows without a cast. */
function hasStringT(value: unknown): value is { t: string } {
  return value !== null && typeof value === 'object' && 't' in value && typeof (value as { t: unknown }).t === 'string';
}

type FieldObj = Record<string, unknown>;

const HARNESSES = new Set(['claude', 'codex', 'opencode', 'grok', 'kimi', 'omp']);

function badField(t: string, key: string, want: string): never {
  throw new FrameError(`bad-frame: ${t}.${key} must be ${want}`);
}

function reqStr(o: FieldObj, t: string, key: string): string {
  const v = o[key];
  if (typeof v !== 'string') badField(t, key, 'a string');
  return v as string;
}

function reqBool(o: FieldObj, t: string, key: string): boolean {
  const v = o[key];
  if (typeof v !== 'boolean') badField(t, key, 'a boolean');
  return v as boolean;
}

function reqUint(o: FieldObj, t: string, key: string, max: number): number {
  const v = o[key];
  if (typeof v !== 'number' || !Number.isSafeInteger(v) || v < 0 || v > max) {
    badField(t, key, `a safe unsigned integer <= ${String(max)}`);
  }
  return v as number;
}

function reqI32(o: FieldObj, t: string, key: string, nullable: boolean): number | null {
  const v = o[key];
  // A missing nullable field decodes as None, matching serde's Option<i32>.
  if (nullable && (v === null || v === undefined)) return null;
  if (typeof v !== 'number' || !Number.isInteger(v) || v < -0x80000000 || v > 0x7fffffff) {
    badField(t, key, 'a 32-bit integer');
  }
  return v as number;
}

function reqNullableStr(o: FieldObj, t: string, key: string): string | null {
  const v = o[key];
  // A missing nullable field decodes as None, matching serde's Option<String>.
  if (v === null || v === undefined) return null;
  if (typeof v !== 'string') badField(t, key, 'a string or null');
  return v as string;
}


/** deny_unknown_fields mirror (Rust ClientFrame/ControlMessage); server frames tolerate extras. */
function onlyKeys(o: FieldObj, t: string, allowed: Record<string, true>): void {
  for (const k of Object.keys(o)) {
    if (!Object.hasOwn(allowed, k)) throw new FrameError(`bad-frame: ${t} has unknown field ${k}`);
  }
}

function validateServerFrame(o: FieldObj, t: string): void {
  const u64 = (k: string): number => reqUint(o, t, k, Number.MAX_SAFE_INTEGER);
  const u32 = (k: string): number => reqUint(o, t, k, 0xffffffff);
  const u16 = (k: string): number => reqUint(o, t, k, 0xffff);
  switch (t) {
    case 'hello': {
      reqStr(o, t, 'proto');
      reqStr(o, t, 'session');
      reqStr(o, t, 'epoch');
      const h = reqStr(o, t, 'harness');
      if (!HARNESSES.has(h)) throw new FrameError('bad-frame: hello.harness is not a known harness');
      reqNullableStr(o, t, 'bound_sid');
      return;
    }
    case 'profile': {
      reqStr(o, t, 'xterm');
      reqStr(o, t, 'serialize');
      reqStr(o, t, 'width');
      reqStr(o, t, 'image');
      u16('cols');
      u16('rows');
      u32('scrollback');
      reqBool(o, t, 'scroll_on_erase_in_display');
      u16('cell_px_w');
      u16('cell_px_h');
      // theme is Rust Option<Value>: any JSON value, absent = None.
      return;
    }
    case 'imagedef': {
      u64('seq');
      u32('id');
      u32('gen');
      reqStr(o, t, 'sha256');
      reqStr(o, t, 'mime');
      u32('w');
      u32('h');
      u16('cols');
      u16('rows');
      u64('offset');
      return;
    }
    case 'seed':
      u64('offset');
      u64('len');
      return;
    case 'resize':
      u64('seq');
      u64('offset');
      u16('cols');
      u16('rows');
      return;
    case 'sizechanged':
      u64('seq');
      u16('cols');
      u16('rows');
      reqStr(o, t, 'by');
      u64('offset');
      return;
    case 'gfxreset':
      u64('seq');
      u32('gen');
      u64('offset');
      return;
    case 'exit':
      u64('seq');
      reqI32(o, t, 'code', true);
      u64('offset');
      return;
    case 'clipboard':
      u64('offset');
      reqStr(o, t, 'data_b64');
      return;
    case 'error':
      reqStr(o, t, 'code');
      reqStr(o, t, 'msg');
      return;
    default:
      throw new FrameError(`bad-frame: unexpected server kind ${t}`);
  }
}

function validateClientFrame(o: FieldObj, t: string): void {
  const u64 = (k: string): number => reqUint(o, t, k, Number.MAX_SAFE_INTEGER);
  const u16 = (k: string): number => reqUint(o, t, k, 0xffff);
  switch (t) {
    case 'resumefrom':
      onlyKeys(o, t, { t: true, offset: true, mseq: true, epoch: true });
      u64('offset');
      u64('mseq');
      reqStr(o, t, 'epoch');
      return;
    case 'takesize':
      onlyKeys(o, t, { t: true, cols: true, rows: true });
      u16('cols');
      u16('rows');
      return;
    case 'input':
      onlyKeys(o, t, { t: true, data: true });
      reqStr(o, t, 'data');
      return;
    case 'ping':
      onlyKeys(o, t, { t: true });
      return;
    default:
      throw new FrameError(`bad-frame: unexpected client kind ${t}`);
  }
}

function validateControlMessage(o: FieldObj, t: string): void {
  const u64 = (k: string): number => reqUint(o, t, k, Number.MAX_SAFE_INTEGER);
  switch (t) {
    case 'viewer-open': {
      onlyKeys(o, t, { t: true, viewer: true, profile: true, resume: true });
      reqStr(o, t, 'viewer');
      const profile = reqStr(o, t, 'profile');
      if (profile !== 'desktop' && profile !== 'phone') {
        throw new FrameError('bad-frame: viewer-open.profile must be desktop|phone');
      }
      const resume = o['resume'];
      // A missing resume decodes as None, matching serde's Option<ResumeInfo>.
      if (resume !== null && resume !== undefined) {
        if (typeof resume !== 'object' || resume === null || Array.isArray(resume)) {
          throw new FrameError('bad-frame: viewer-open.resume must be null or an object');
        }
        const r = resume as FieldObj;
        onlyKeys(r, `${t}.resume`, { offset: true, mseq: true, epoch: true });
        reqUint(r, `${t}.resume`, 'offset', Number.MAX_SAFE_INTEGER);
        reqUint(r, `${t}.resume`, 'mseq', Number.MAX_SAFE_INTEGER);
        reqStr(r, `${t}.resume`, 'epoch');
      }
      return;
    }
    case 'viewer-close':
    case 'typed':
      onlyKeys(o, t, { t: true, viewer: true });
      reqStr(o, t, 'viewer');
      return;
    case 'stats':
      onlyKeys(o, t, { t: true, joins: true, degradedJoins: true, viewers: true });
      for (const k of ['joins', 'degradedJoins', 'viewers']) {
        if (k in o) u64(k);
      }
      return;
    default:
      throw new FrameError(`bad-frame: unexpected control kind ${t}`);
  }
}

/** Decode one text frame; unknown `t` yields the raw string; malformed JSON throws. */
export function decodeTextFrame(raw: string): TextFrame {
  let value: unknown;
  try {
    value = JSON.parse(raw);
  } catch {
    throw new FrameError('bad-frame: malformed JSON');
  }
  if (!hasStringT(value)) {
    throw new FrameError('bad-frame: frame has no string discriminator');
  }
  const t = value.t;
  const obj = value as FieldObj;
  if (SERVER_T[t]) {
    validateServerFrame(obj, t);
    return { kind: 'known', frame: obj as unknown as ServerFrame };
  }
  if (CLIENT_T[t]) {
    validateClientFrame(obj, t);
    return { kind: 'known', frame: obj as unknown as ClientFrame };
  }
  if (CONTROL_T[t]) {
    validateControlMessage(obj, t);
    return { kind: 'known', frame: obj as unknown as ControlMessage };
  }
  return { kind: 'unknown', raw };
}
export function encodeBinaryFrame(offset: number, payload: Uint8Array): Uint8Array {
  if (!Number.isSafeInteger(offset) || offset < 0) throw new FrameError(`bad-frame: unsafe offset ${String(offset)}`);
  const out = new Uint8Array(8 + payload.length);
  new DataView(out.buffer).setBigUint64(0, BigInt(offset));
  out.set(payload, 8);
  return out;
}

/** Decode a binary frame; rejects short frames and offsets >= 2^53. */
export function decodeBinaryFrame(frame: Uint8Array): { offset: number; payload: Uint8Array } {
  if (frame.length < 8) throw new FrameError('bad-frame: short binary frame');
  const offset = new DataView(frame.buffer, frame.byteOffset).getBigUint64(0);
  if (offset >= MAX_OFFSET) throw new FrameError('bad-frame: offset out of range');
  return { offset: Number(offset), payload: frame.subarray(8) };
}

// ---------------------------------------------------------------- tap.sock envelope
// [u32 BE length][u8 kind][body]; length counts the kind byte plus the body.

export const ENV_TEXT = 0;
export const ENV_BINARY = 1;
export const ENV_CONTROL = 2;

export interface Envelope {
  kind: number;
  body: Uint8Array;
}

/** Encode one envelope. `kind` is 0 (text JSON), 1 (binary frame), 2 (control). */
export function encodeEnvelope(kind: number, body: Uint8Array): Uint8Array {
  if (kind !== 0 && kind !== 1 && kind !== 2) throw new FrameError(`bad-frame: bad envelope kind ${String(kind)}`);
  if (1 + body.length > MAX_ENVELOPE_LEN) throw new FrameError('bad-frame: envelope exceeds maximum length');
  const out = new Uint8Array(4 + 1 + body.length);
  new DataView(out.buffer).setUint32(0, 1 + body.length);
  out[4] = kind;
  out.set(body, 5);
  return out;
}

/** S->C body: [u8 idlen][id utf8][frame]. */
export function wrapS2C(id: string, frame: Uint8Array): Uint8Array {
  const idBytes = Buffer.from(id, 'utf-8');
  if (idBytes.length > 255) throw new FrameError('bad-frame: viewer id too long');
  const out = new Uint8Array(1 + idBytes.length + frame.length);
  out[0] = idBytes.length;
  out.set(idBytes, 1);
  out.set(frame, 1 + idBytes.length);
  return out;
}

/** Parse an S->C body. */
export function parseS2C(body: Uint8Array): { id: string; frame: Uint8Array } {
  if (body.length < 1) throw new FrameError('bad-frame: empty s2c body');
  const idLen = body[0];
  if (body.length < 1 + idLen) throw new FrameError('bad-frame: truncated s2c id');
  const id = Buffer.from(body.subarray(1, 1 + idLen)).toString('utf-8');
  return { id, frame: body.subarray(1 + idLen) };
}

/** Streaming envelope decoder: feed bytes, get back complete envelopes. */
export class EnvelopeDecoder {
  private buf: Uint8Array = new Uint8Array(0);

  push(chunk: Uint8Array): Envelope[] {
    const joined = new Uint8Array(this.buf.length + chunk.length);
    joined.set(this.buf);
    joined.set(chunk, this.buf.length);
    this.buf = joined;
    const out: Envelope[] = [];
    for (;;) {
      if (this.buf.length < 4) break;
      // The length can be validated as soon as its 4 bytes are buffered:
      // an oversize (or zero) claim fails here, before the body is waited
      // for (and never buffered).
      const dv = new DataView(this.buf.buffer, this.buf.byteOffset);
      const len = dv.getUint32(0);
      if (len === 0) throw new FrameError('bad-frame: zero-length envelope');
      if (len > MAX_ENVELOPE_LEN) throw new FrameError('bad-frame: envelope exceeds maximum length');
      if (this.buf.length < 4 + len) break;
      const kind = this.buf[4];
      if (kind !== 0 && kind !== 1 && kind !== 2) throw new FrameError(`bad-frame: bad envelope kind ${String(kind)}`);
      out.push({ kind, body: this.buf.subarray(5, 4 + len) });
      this.buf = this.buf.subarray(4 + len);
    }
    return out;
  }
}
