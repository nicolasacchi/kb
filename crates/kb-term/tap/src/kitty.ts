// kb-term tap: kitty graphics control data and chunk reassembly (KBT-GFX). Pure, no I/O.
//
// Implements kb-term v2 stage 5 input handling: parses kitty APC `G` control data
// and reassembles chunked `a=t` PNG transfers. Only `f=100` direct PNG is
// accepted; all other actions/formats are counted drops (never a path open).
// `a=d` of any form is a reset: it frees nothing and advances no per-image gen.

import { createHash } from 'node:crypto';

export const MAX_IMAGE_BYTES = 32 * 1024 * 1024;
export const MAX_APC_BODY = 8192;

export type DropReason =
  | 'unsupportedFormat'
  | 'unsupportedMedium'
  | 'compressed'
  | 'directPlacement'
  | 'nonVirtualPlacement'
  | 'queryAction'
  | 'imageNumberUnsupported'
  | 'orphanChunk'
  | 'badBase64'
  | 'badPng'
  | 'oversize'
  | 'otherAction';

export type KittyResult =
  | { kind: 'transmit'; id: number; png: Uint8Array; sha256: string; w: number; h: number }
  | { kind: 'place'; id: number; cols: number; rows: number }
  | { kind: 'reset'; scope: 'all' | 'id'; id?: number }
  | { kind: 'drop'; reason: DropReason };

export interface ParsedKitty {
  keys: Map<string, string>;
  payload: Uint8Array;
}

const SEMI = 0x3b;
const COMMA = 0x2c;
const EQ = 0x3d;

/** Parse kitty APC body (bytes after ESC _ G, before the terminator). */
export function parseKittyControl(body: Uint8Array): ParsedKitty {
  let semi = -1;
  for (let i = 0; i < body.length; i++) {
    if (body[i] === SEMI) { semi = i; break; }
  }
  const ctrlEnd = semi === -1 ? body.length : semi;
  const payload = semi === -1 ? new Uint8Array(0) : body.subarray(semi + 1);
  const keys = new Map<string, string>();
  // Decode control part as latin1 and split on commas.
  let partStart = 0;
  const pushPart = (end: number) => {
    if (end <= partStart) return;
    let eq = -1;
    for (let i = partStart; i < end; i++) {
      if (body[i] === EQ) { eq = i; break; }
    }
    if (eq === -1) return; // bare key without '=': ignore
    if (eq === partStart) return; // empty key: ignore
    let k = '';
    for (let i = partStart; i < eq; i++) k += String.fromCharCode(body[i]);
    let v = '';
    for (let i = eq + 1; i < end; i++) v += String.fromCharCode(body[i]);
    keys.set(k, v); // duplicate key: last wins
  };
  for (let i = 0; i < ctrlEnd; i++) {
    if (body[i] === COMMA) { pushPart(i); partStart = i + 1; }
  }
  pushPart(ctrlEnd);
  return { keys, payload };
}

const B64_OK = /^[A-Za-z0-9+/=]*$/;

function parseId(s: string | undefined): number | null {
  if (s === undefined) return null;
  if (!/^[0-9]+$/.test(s)) return null;
  const n = Number(s);
  if (!Number.isSafeInteger(n) || n < 1 || n > 4294967295) return null;
  return n;
}

function parseUint(s: string | undefined): number {
  if (s === undefined) return 0;
  if (!/^[0-9]+$/.test(s)) return 0;
  const n = Number(s);
  if (!Number.isSafeInteger(n) || n < 0) return 0;
  return n;
}

function readPngSize(png: Uint8Array): { w: number; h: number } | null {
  if (png.length < 24) return null;
  const sig = [137, 80, 78, 71, 13, 10, 26, 10];
  for (let i = 0; i < 8; i++) if (png[i] !== sig[i]) return null;
  if (png[12] !== 0x49 || png[13] !== 0x48 || png[14] !== 0x44 || png[15] !== 0x52) return null; // IHDR
  const dv = new DataView(png.buffer, png.byteOffset, png.byteLength);
  const w = dv.getUint32(16, false);
  const h = dv.getUint32(20, false);
  if (w < 1 || w > 16384 || h < 1 || h > 16384) return null;
  return { w, h };
}

export interface KittyStats {
  apcs: number;
  transmits: number;
  places: number;
  resets: number;
  chunks: number;
  abortedTransfer: number;
  unsupportedFormat: number;
  unsupportedMedium: number;
  compressed: number;
  directPlacement: number;
  nonVirtualPlacement: number;
  queryAction: number;
  imageNumberUnsupported: number;
  orphanChunk: number;
  badBase64: number;
  badPng: number;
  oversize: number;
  otherAction: number;
}

function emptyKittyStats(): KittyStats {
  return {
    apcs: 0, transmits: 0, places: 0, resets: 0, chunks: 0, abortedTransfer: 0,
    unsupportedFormat: 0, unsupportedMedium: 0, compressed: 0, directPlacement: 0,
    nonVirtualPlacement: 0, queryAction: 0, imageNumberUnsupported: 0, orphanChunk: 0,
    badBase64: 0, badPng: 0, oversize: 0, otherAction: 0,
  };
}

interface Pending {
  id: number;
  parts: string[];
  b64Len: number;
}

export class KittyReassembler {
  private pending: Pending | null = null;
  private counts: KittyStats = emptyKittyStats();
  private readonly maxImageBytes: number;

  constructor(opts?: { maxImageBytes?: number }) {
    this.maxImageBytes = opts?.maxImageBytes ?? MAX_IMAGE_BYTES;
  }

  get stats(): KittyStats {
    return { ...this.counts };
  }

  /** Discard any pending transfer (APC aborted/oversized in the graphics layer). No counters. */
  abortPending(): void {
    this.pending = null;
  }

  push(parsed: ParsedKitty): KittyResult | null {
    this.counts.apcs++;
    const keys = parsed.keys;
    const hasA = keys.has('a');
    const a = hasA ? keys.get('a')! : 't';

    // Continuation chunk: no 'a' key while a transfer is pending.
    if (!hasA && this.pending !== null) {
      return this.pushContinuation(parsed);
    }
    // No 'a' and no pending: pure m/q shape is an orphan; an explicit
    // default-action transmit (carries i/f/m with other keys) starts a transfer.
    if (!hasA && this.pending === null) {
      const onlyMq = [...keys.keys()].every((k) => k === 'm' || k === 'q');
      if (onlyMq) {
        this.counts.orphanChunk++;
        return { kind: 'drop', reason: 'orphanChunk' };
      }
      // Fall through as a default-action transmit start.
    }

    switch (a) {
      case 'd': {
        const d = keys.get('d');
        this.counts.resets++;
        if (d === 'A' || d === 'a') return { kind: 'reset', scope: 'all' };
        const id = parseId(keys.get('i'));
        return id !== null ? { kind: 'reset', scope: 'id', id } : { kind: 'reset', scope: 'id' };
      }
      case 'p': {
        if (keys.get('U') !== '1') {
          this.counts.nonVirtualPlacement++;
          return { kind: 'drop', reason: 'nonVirtualPlacement' };
        }
        const id = parseId(keys.get('i'));
        if (id === null) {
          this.counts.imageNumberUnsupported++;
          return { kind: 'drop', reason: 'imageNumberUnsupported' };
        }
        this.counts.places++;
        return { kind: 'place', id, cols: parseUint(keys.get('c')), rows: parseUint(keys.get('r')) };
      }
      case 'T': {
        this.counts.directPlacement++;
        return { kind: 'drop', reason: 'directPlacement' };
      }
      case 'q': {
        this.counts.queryAction++;
        return { kind: 'drop', reason: 'queryAction' };
      }
      case 't': {
        return this.pushTransmitStart(parsed);
      }
      default: {
        this.counts.otherAction++;
        return { kind: 'drop', reason: 'otherAction' };
      }
    }
  }

  private pushTransmitStart(parsed: ParsedKitty): KittyResult | null {
    const keys = parsed.keys;
    // Format/medium/compressed/id/base64 gates (fail closed; never open a named path).
    // A dropped start does NOT abort a pending transfer: only a transmit that
    // actually starts a transfer (m=1 pending created, or single-chunk transmit)
    // aborts the previous pending one, counted abortedTransfer.
    if (keys.get('f') !== '100') {
      this.counts.unsupportedFormat++;
      return { kind: 'drop', reason: 'unsupportedFormat' };
    }
    const t = keys.get('t');
    if (t !== undefined && t !== 'd') {
      this.counts.unsupportedMedium++;
      return { kind: 'drop', reason: 'unsupportedMedium' };
    }
    if (keys.has('o')) {
      this.counts.compressed++;
      return { kind: 'drop', reason: 'compressed' };
    }
    const id = parseId(keys.get('i'));
    if (id === null) {
      this.counts.imageNumberUnsupported++;
      return { kind: 'drop', reason: 'imageNumberUnsupported' };
    }
    const b64 = latin1String(parsed.payload);
    if (!B64_OK.test(b64)) {
      this.counts.badBase64++;
      return { kind: 'drop', reason: 'badBase64' };
    }
    const more = keys.get('m') === '1';
    if (more) {
      if (exactDecodedLen(b64.length, trailingPad(b64)) > this.maxImageBytes) {
        this.counts.oversize++;
        return { kind: 'drop', reason: 'oversize' };
      }
      if (this.pending !== null) {
        this.counts.abortedTransfer++;
        this.pending = null;
      }
      this.pending = { id, parts: [b64], b64Len: b64.length };
      this.counts.chunks++;
      return null;
    }
    const res = this.finishTransmit(id, b64);
    if (res.kind === 'transmit' && this.pending !== null) {
      this.counts.abortedTransfer++;
      this.pending = null;
    }
    return res;
  }

  private pushContinuation(parsed: ParsedKitty): KittyResult | null {
    const keys = parsed.keys;
    const pend = this.pending!; // non-null by caller
    const b64 = latin1String(parsed.payload);
    if (!B64_OK.test(b64)) {
      this.pending = null;
      this.counts.badBase64++;
      return { kind: 'drop', reason: 'badBase64' };
    }
    const more = keys.get('m') === '1';
    if (more) {
      const totalLen = pend.b64Len + b64.length;
      const pad = b64.length > 0
        ? trailingPad(b64)
        : (pend.parts.length > 0 ? trailingPad(pend.parts[pend.parts.length - 1]!) : 0);
      if (exactDecodedLen(totalLen, pad) > this.maxImageBytes) {
        this.pending = null;
        this.counts.oversize++;
        return { kind: 'drop', reason: 'oversize' };
      }
      pend.parts.push(b64);
      pend.b64Len = totalLen;
      this.counts.chunks++;
      return null;
    }
    const full = pend.parts.join('') + b64;
    const id = pend.id;
    this.pending = null;
    return this.finishTransmit(id, full);
  }

  private finishTransmit(id: number, fullB64: string): KittyResult {
    if (fullB64.length % 4 !== 0) {
      this.counts.badBase64++;
      return { kind: 'drop', reason: 'badBase64' };
    }
    const png = new Uint8Array(Buffer.from(fullB64, 'base64'));
    if (png.length > this.maxImageBytes) {
      this.counts.oversize++;
      return { kind: 'drop', reason: 'oversize' };
    }
    const size = readPngSize(png);
    if (size === null) {
      this.counts.badPng++;
      return { kind: 'drop', reason: 'badPng' };
    }
    const sha256 = createHash('sha256').update(png).digest('hex');
    this.counts.transmits++;
    return { kind: 'transmit', id, png, sha256, w: size.w, h: size.h };
  }
}

function latin1String(b: Uint8Array): string {
  let s = '';
  for (let i = 0; i < b.length; i += 8192) {
    const part: number[] = [];
    for (let j = i; j < Math.min(b.length, i + 8192); j++) part.push(b[j]);
    s += String.fromCharCode(...part);
  }
  return s;
}

function trailingPad(b64: string): number {
  let n = 0;
  for (let i = b64.length - 1; i >= 0 && b64[i] === '='; i--) n++;
  return n;
}

// Exact decoded length for a base64 string (or concatenated prefix) of
// totalLen chars whose trailing padding is pad: 3*floor(len/4) - pad.
// The final finish step checks the true decoded length; this incremental
// check must not use the ceil upper bound, which drops a valid image whose
// true length equals the cap up to 2 bytes early.
function exactDecodedLen(totalLen: number, pad: number): number {
  const v = 3 * Math.floor(totalLen / 4) - pad;
  return v < 0 ? 0 : v;
}
