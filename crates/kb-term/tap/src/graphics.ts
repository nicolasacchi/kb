// kb-term tap: graphics extraction (KBT-GFX). Zero npm dependencies.
//
// Removes kitty APC `G` sequences from the viewer byte stream, stores each PNG
// once by sha256 (ImageStore), and emits small in-process events in trigger
// order: bytes / imagedef / gfxreset / mediadef (IIP/SIXEL stubbed). Placeholder
// TEXT (U+10EEEE cells, SGR, CR CR LF ESC[2K) passes verbatim. No APC `G` body
// byte ever reaches the viewer stream. Runs after T3's DCS-tmux unwrap; also
// works on plain (unwrapped) input.
//
// Known limitation (v1): a real unbracketed IIP/SIXEL advances the cursor in
// xterm.js but not in tmux; mirroring that cursor rule in the twin belongs to
// the twin stage (T5/T6), not here.

import { createHash } from 'node:crypto';
import { KittyReassembler, parseKittyControl } from './kitty.ts';
import type { ImageStore } from './imagestore.ts';
import { MAX_APC_BODY, MAX_IMAGE_BYTES } from './kitty.ts';

export { MAX_APC_BODY, MAX_IMAGE_BYTES };
export const MAX_MEDIA_BYTES = 8 * 1024 * 1024;

export interface ImageDef {
  id: number;
  gen: number;
  sha256: string;
  mime: string;
  w: number;
  h: number;
  cols: number;
  rows: number;
}

export type GfxItem =
  | { kind: 'bytes'; data: Uint8Array }
  | { kind: 'imagedef'; def: ImageDef }
  | { kind: 'gfxreset'; gen: number }
  | { kind: 'mediadef'; mediaKind: 'iip' | 'sixel'; sha256: string; len: number };

export class GfxStats {
  apcs = 0;
  transmits = 0;
  places = 0;
  resets = 0;
  chunks = 0;
  abortedTransfer = 0;
  unsupportedFormat = 0;
  unsupportedMedium = 0;
  compressed = 0;
  directPlacement = 0;
  nonVirtualPlacement = 0;
  queryAction = 0;
  imageNumberUnsupported = 0;
  orphanChunk = 0;
  badBase64 = 0;
  badPng = 0;
  oversize = 0;
  otherAction = 0;
  oversizeApc = 0;
  abortedApc = 0;
  oversizeMedia = 0;
  abortedMedia = 0;
  iip = 0;
  sixel = 0;
}

const ESC = 0x1b;
const BEL = 0x07;
const CAN = 0x18;
const SUB = 0x1a;
const BS = 0x5c; // backslash

type State = 'ground' | 'sniff' | 'apc' | 'skipApc' | 'media' | 'skipMedia';

type SniffPhase = 'afterEsc' | 'afterUnderscore' | 'iip' | 'sixel';

const IIP_EXPECT = ['1', '3', '3', '7', ';', 'F', 'i', 'l', 'e', '='];

class GrowBuf {
  private buf = new Uint8Array(64);
  private len = 0;
  get length(): number { return this.len; }
  reset(): void { this.len = 0; }
  push(b: number): void {
    if (this.len >= this.buf.length) {
      const nb = new Uint8Array(Math.max(64, this.buf.length * 2));
      nb.set(this.buf.subarray(0, this.len));
      this.buf = nb;
    }
    this.buf[this.len++] = b;
  }
  pushBytes(b: Uint8Array): void {
    if (this.len + b.length > this.buf.length) {
      const nb = new Uint8Array(Math.max(64, (this.len + b.length) * 2));
      nb.set(this.buf.subarray(0, this.len));
      this.buf = nb;
    }
    this.buf.set(b, this.len);
    this.len += b.length;
  }
  copy(): Uint8Array {
    return this.buf.slice(0, this.len);
  }
}

export class GraphicsExtractor {
  stats: GfxStats;
  private readonly store: ImageStore;
  private readonly kitty: KittyReassembler;
  private readonly maxApcBody: number;
  private readonly maxMediaBytes: number;

  private state: State = 'ground';
  private sniffBuf: number[] = [];
  private sniffPhase: SniffPhase = 'afterEsc';
  private iipStep = 0;
  private apcBody = new GrowBuf();
  private apcPendEsc = false;
  private skipPendEsc = false;
  private mediaBuf = new GrowBuf();
  private mediaKind: 'iip' | 'sixel' = 'iip';
  private mediaPendEsc = false;
  private skipMediaKind: 'iip' | 'sixel' = 'iip';

  private pendingPlace = new Map<number, { cols: number; rows: number }>();
  private lastEmitted = new Map<number, { gen: number; sha256: string; cols: number; rows: number }>();
  private streamGen = 0;

  constructor(store: ImageStore, opts?: { stats?: GfxStats; maxImageBytes?: number; maxApcBody?: number; maxMediaBytes?: number }) {
    this.store = store;
    this.stats = opts?.stats ?? new GfxStats();
    this.kitty = new KittyReassembler({ maxImageBytes: opts?.maxImageBytes ?? MAX_IMAGE_BYTES });
    this.maxApcBody = opts?.maxApcBody ?? MAX_APC_BODY;
    this.maxMediaBytes = opts?.maxMediaBytes ?? MAX_MEDIA_BYTES;
    for (const e of store.entries()) {
      if (e.cols !== null && e.rows !== null) {
        this.lastEmitted.set(e.id, { gen: e.gen, sha256: e.sha256, cols: e.cols, rows: e.rows });
      }
    }
  }

  push(chunk: Uint8Array): GfxItem[] {
    if (chunk.length === 0) return [];
    // Fast path: ground state, nothing held, no ESC in the chunk: zero-copy.
    if (this.state === 'ground' && this.sniffBuf.length === 0 && chunk.indexOf(ESC) === -1) {
      return [{ kind: 'bytes', data: chunk }];
    }
    const items: GfxItem[] = [];
    const out = new GrowBuf();
    const flushOut = () => {
      if (out.length > 0) {
        items.push({ kind: 'bytes', data: out.copy() });
        out.reset();
      }
    };
    for (let i = 0; i < chunk.length; i++) {
      const b = chunk[i];
      switch (this.state) {
        case 'ground': {
          if (b === ESC) {
            this.state = 'sniff';
            this.sniffBuf = [ESC];
            this.sniffPhase = 'afterEsc';
          } else {
            out.push(b);
          }
          break;
        }
        case 'sniff': {
          this.feedSniff(b, out, items);
          break;
        }
        case 'apc': {
          this.feedApc(b, out, items);
          break;
        }
        case 'skipApc': {
          if (this.skipPendEsc) {
            this.skipPendEsc = false;
            if (b === BS) {
              this.state = 'ground'; // terminator swallowed
            } else {
              // ESC-not-backslash: re-process as a new sequence start.
              this.state = 'sniff';
              this.sniffBuf = [ESC];
              this.sniffPhase = 'afterEsc';
              this.feedSniff(b, out, items);
            }
          } else if (b === ESC) {
            this.skipPendEsc = true;
          } else if (b === CAN || b === SUB) {
            this.state = 'ground'; // swallowed, ends skip
          } else {
            // swallowed silently
          }
          break;
        }
        case 'media': {
          this.feedMedia(b, out, items);
          break;
        }
        case 'skipMedia': {
          if (this.skipPendEsc) {
            this.skipPendEsc = false;
            if (b === BS) {
              this.state = 'ground';
            } else {
              this.state = 'sniff';
              this.sniffBuf = [ESC];
              this.sniffPhase = 'afterEsc';
              this.feedSniff(b, out, items);
            }
          } else if (b === ESC) {
            this.skipPendEsc = true;
          } else if (b === CAN || b === SUB) {
            this.state = 'ground';
          } else if (b === BEL && this.skipMediaKind === 'iip') {
            this.state = 'ground';
          } else {
            // swallowed silently
          }
          break;
        }
      }
    }
    flushOut();
    return items;
  }

  flush(): GfxItem[] {
    this.state = 'ground';
    this.sniffBuf = [];
    this.sniffPhase = 'afterEsc';
    this.apcBody.reset();
    this.apcPendEsc = false;
    this.skipPendEsc = false;
    this.mediaBuf.reset();
    this.mediaPendEsc = false;
    this.kitty.abortPending();
    return [];
  }

  // -- sniff: ESC held, deciding between kitty APC / IIP / SIXEL / pass-through --

  private feedSniff(b: number, out: GrowBuf, items: GfxItem[]): void {
    switch (this.sniffPhase) {
      case 'afterEsc': {
        if (b === 0x5f) { // '_'
          this.sniffBuf.push(b);
          this.sniffPhase = 'afterUnderscore';
        } else if (b === 0x5d) { // ']'
          this.sniffBuf.push(b);
          this.sniffPhase = 'iip';
          this.iipStep = 0;
        } else if (b === 0x50) { // 'P'
          this.sniffBuf.push(b);
          this.sniffPhase = 'sixel';
        } else {
          this.flushSniffAsBytes(out, b);
        }
        break;
      }
      case 'afterUnderscore': {
        if (b === 0x47) { // 'G': kitty APC starts; introducer never emitted
          this.state = 'apc';
          this.apcBody.reset();
          this.apcPendEsc = false;
          this.sniffBuf = [];
        } else {
          this.flushSniffAsBytes(out, b);
        }
        break;
      }
      case 'iip': {
        const exp = IIP_EXPECT[this.iipStep]!;
        // Tolerate a single optional space before ';' and '=' (iTerm docs style).
        if ((exp === ';' || exp === '=') && b === 0x20) {
          this.sniffBuf.push(b);
          break;
        }
        if (String.fromCharCode(b) === exp) {
          this.sniffBuf.push(b);
          this.iipStep++;
          if (this.iipStep === IIP_EXPECT.length) {
            this.mediaBuf.reset();
            for (const x of this.sniffBuf) this.mediaBuf.push(x);
            this.mediaKind = 'iip';
            this.mediaPendEsc = false;
            this.state = 'media';
            this.sniffBuf = [];
          }
        } else {
          this.flushSniffAsBytes(out, b);
        }
        break;
      }
      case 'sixel': {
        if (b >= 0x30 && b <= 0x39 || b === 0x3b) { // digit or ';'
          this.sniffBuf.push(b);
        } else if (b === 0x71) { // 'q'
          this.sniffBuf.push(b);
          this.mediaBuf.reset();
          for (const x of this.sniffBuf) this.mediaBuf.push(x);
          this.mediaKind = 'sixel';
          this.mediaPendEsc = false;
          this.state = 'media';
          this.sniffBuf = [];
        } else {
          this.flushSniffAsBytes(out, b);
        }
        break;
      }
    }
  }

  /** Held prefix is not a kitty/IIP/SIXEL start: emit it verbatim, then handle `b` fresh. */
  private flushSniffAsBytes(out: GrowBuf, b: number): void {
    for (const x of this.sniffBuf) out.push(x);
    this.sniffBuf = [];
    this.state = 'ground';
    if (b === ESC) {
      this.state = 'sniff';
      this.sniffBuf = [ESC];
      this.sniffPhase = 'afterEsc';
    } else {
      out.push(b);
    }
  }

  // -- kitty APC capture --

  private feedApc(b: number, out: GrowBuf, items: GfxItem[]): void {
    if (this.apcPendEsc) {
      this.apcPendEsc = false;
      if (b === BS) {
        const body = this.apcBody.copy();
        this.state = 'ground';
        this.apcBody.reset();
        this.handleApc(body, out, items);
        return;
      }
      // Aborting ESC: discard, re-process as a new sequence start.
      this.stats.abortedApc++;
      this.kitty.abortPending();
      this.state = 'sniff';
      this.sniffBuf = [ESC];
      this.sniffPhase = 'afterEsc';
      this.apcBody.reset();
      this.feedSniff(b, out, items);
      return;
    }
    if (b === ESC) {
      this.apcPendEsc = true;
      return;
    }
    if (b === CAN || b === SUB) {
      this.stats.abortedApc++;
      this.kitty.abortPending();
      this.state = 'ground';
      this.apcBody.reset();
      return; // CAN/SUB swallowed (incl. T3 forwarded-then-aborted ghosts)
    }
    this.apcBody.push(b);
    if (this.apcBody.length > this.maxApcBody) {
      this.stats.oversizeApc++;
      this.state = 'skipApc';
      this.skipPendEsc = false;
      this.apcBody.reset();
      // Body bytes never emitted: nothing to output.
    }
  }

  private handleApc(body: Uint8Array, out: GrowBuf, items: GfxItem[]): void {
    const flushOut = () => {
      if (out.length > 0) {
        items.push({ kind: 'bytes', data: out.copy() });
        out.reset();
      }
    };
    const parsed = parseKittyControl(body);
    const res = this.kitty.push(parsed);
    this.syncKittyStats();
    if (res === null) return; // non-final chunk: no item
    if (res.kind === 'drop') return; // counted only; APC already removed from stream
    if (res.kind === 'reset') {
      // GfxReset.gen is a stream counter (count of a=d APCs), not the per-image gen.
      this.streamGen++;
      flushOut();
      items.push({ kind: 'gfxreset', gen: this.streamGen });
      return;
    }
    if (res.kind === 'transmit') {
      const before = this.store.get(res.id);
      this.store.putPng(res.sha256, res.png);
      this.store.bind(res.id, { sha256: res.sha256, w: res.w, h: res.h });
      const pend = this.pendingPlace.get(res.id);
      if (pend !== undefined) {
        this.store.place(res.id, pend.cols, pend.rows);
        this.pendingPlace.delete(res.id);
      }
      const after = this.store.get(res.id)!;
      const changed = before === null
        || before.sha256 !== after.sha256
        || before.w !== after.w
        || before.h !== after.h
        || before.gen !== after.gen;
      if (changed && after.cols !== null && after.rows !== null) {
        const def: ImageDef = {
          id: after.id, gen: after.gen, sha256: after.sha256,
          mime: after.mime, w: after.w, h: after.h, cols: after.cols, rows: after.rows,
        };
        this.lastEmitted.set(after.id, { gen: after.gen, sha256: after.sha256, cols: after.cols, rows: after.rows });
        flushOut();
        items.push({ kind: 'imagedef', def });
      }
      return;
    }
    // place
    const rec = this.store.get(res.id);
    if (rec === null) {
      this.pendingPlace.set(res.id, { cols: res.cols, rows: res.rows });
      return;
    }
    this.store.place(res.id, res.cols, res.rows);
    const after = this.store.get(res.id)!;
    const last = this.lastEmitted.get(after.id);
    const differs = last === undefined
      || last.gen !== after.gen
      || last.sha256 !== after.sha256
      || last.cols !== after.cols
      || last.rows !== after.rows;
    if (differs && after.cols !== null && after.rows !== null) {
      const def: ImageDef = {
        id: after.id, gen: after.gen, sha256: after.sha256,
        mime: after.mime, w: after.w, h: after.h, cols: after.cols, rows: after.rows,
      };
      this.lastEmitted.set(after.id, { gen: after.gen, sha256: after.sha256, cols: after.cols, rows: after.rows });
      flushOut();
      items.push({ kind: 'imagedef', def });
    }
  }

  // -- IIP / SIXEL capture --

  private feedMedia(b: number, out: GrowBuf, items: GfxItem[]): void {
    if (this.mediaPendEsc) {
      this.mediaPendEsc = false;
      if (b === BS) {
        this.mediaBuf.push(ESC);
        this.mediaBuf.push(BS);
        this.completeMedia(out, items);
        return;
      }
      // Aborting ESC: drop, re-process.
      this.stats.abortedMedia++;
      this.state = 'sniff';
      this.sniffBuf = [ESC];
      this.sniffPhase = 'afterEsc';
      this.mediaBuf.reset();
      this.feedSniff(b, out, items);
      return;
    }
    if (b === ESC) {
      this.mediaPendEsc = true;
      return;
    }
    if (b === CAN || b === SUB) {
      this.stats.abortedMedia++;
      this.state = 'ground';
      this.mediaBuf.reset();
      return;
    }
    if (b === BEL && this.mediaKind === 'iip') {
      this.mediaBuf.push(b);
      this.completeMedia(out, items);
      return;
    }
    this.mediaBuf.push(b);
    if (this.mediaBuf.length > this.maxMediaBytes) {
      this.stats.oversizeMedia++;
      this.skipMediaKind = this.mediaKind;
      this.state = 'skipMedia';
      this.skipPendEsc = false;
      this.mediaBuf.reset();
    }
  }

  private completeMedia(out: GrowBuf, items: GfxItem[]): void {
    const orig = this.mediaBuf.copy();
    this.state = 'ground';
    this.mediaBuf.reset();
    const sha256 = createHash('sha256').update(orig).digest('hex');
    this.store.putSeq(sha256, orig);
    const len = orig.length;
    if (this.mediaKind === 'iip') this.stats.iip++;
    else this.stats.sixel++;
    if (out.length > 0) {
      items.push({ kind: 'bytes', data: out.copy() });
      out.reset();
    }
    items.push({ kind: 'mediadef', mediaKind: this.mediaKind, sha256, len });
    // Stub in the viewer stream (xterm.js ignores it; the SPA expands it).
    // It starts the next bytes segment so adjacent bytes coalesce.
    out.pushBytes(buildMediaStub(this.mediaKind, sha256, len));
  }

  private syncKittyStats(): void {
    const k = this.kitty.stats;
    const s = this.stats;
    s.apcs = k.apcs;
    s.transmits = k.transmits;
    s.places = k.places;
    s.resets = k.resets;
    s.chunks = k.chunks;
    s.abortedTransfer = k.abortedTransfer;
    s.unsupportedFormat = k.unsupportedFormat;
    s.unsupportedMedium = k.unsupportedMedium;
    s.compressed = k.compressed;
    s.directPlacement = k.directPlacement;
    s.nonVirtualPlacement = k.nonVirtualPlacement;
    s.queryAction = k.queryAction;
    s.imageNumberUnsupported = k.imageNumberUnsupported;
    s.orphanChunk = k.orphanChunk;
    s.badBase64 = k.badBase64;
    s.badPng = k.badPng;
    s.oversize = k.oversize;
    s.otherAction = k.otherAction;
  }
}

/** Build the stub that replaces an IIP/SIXEL sequence in the viewer stream. */
export function buildMediaStub(kind: 'iip' | 'sixel', sha256: string, len: number): Uint8Array {
  return new TextEncoder().encode(`\x1b]7746 ; kb-media ; ${kind} ; ${sha256} ; ${len} \x1b\\`);
}

/** Parse a stub built by `buildMediaStub`. Returns null on mismatch. */
export function parseMediaStub(bytes: Uint8Array): { mediaKind: 'iip' | 'sixel'; sha256: string; len: number } | null {
  let s: string;
  try {
    s = new TextDecoder().decode(bytes);
  } catch {
    return null;
  }
  const m = /^\x1b\]7746 ; kb-media ; (iip|sixel) ; ([0-9a-f]{64}) ; ([0-9]+) \x1b\\$/.exec(s);
  if (m === null) return null;
  return { mediaKind: m[1] as 'iip' | 'sixel', sha256: m[2]!, len: Number(m[3]) };
}
