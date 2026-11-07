// Test helpers for the graphics stage (KBT-GFX). Synthetic only.
// PNG builder, kitty APC builder, DCS-tmux wrapper, whole-buffer oracle
// (shares no code with src/graphics.ts / src/kitty.ts), synth-stream runner.

import { deflateSync } from 'node:zlib';
import { createHash } from 'node:crypto';
import { spawnSync } from 'node:child_process';
import { join } from 'node:path';

export const E = '\x1b';
export const ST = `${E}\\`;

/** Minimal 8-bit RGBA PNG with the given pixel size (filter 0 rows). */
export function makePng(w: number, h: number, fill = 128): Uint8Array {
  const raw = new Uint8Array((w * 4 + 1) * h);
  for (let y = 0; y < h; y++) {
    const base = y * (w * 4 + 1);
    raw[base] = 0;
    for (let x = 0; x < w; x++) {
      const o = base + 1 + x * 4;
      raw[o] = (x * 37 + fill) & 0xff;
      raw[o + 1] = (y * 53 + fill) & 0xff;
      raw[o + 2] = fill & 0xff;
      raw[o + 3] = 255;
    }
  }
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(w, 0);
  ihdr.writeUInt32BE(h, 4);
  ihdr[8] = 8; ihdr[9] = 6; ihdr[10] = 0; ihdr[11] = 0; ihdr[12] = 0;
  const chunk = (type: string, data: Buffer): Buffer => {
    const len = Buffer.alloc(4);
    len.writeUInt32BE(data.length, 0);
    const body = Buffer.concat([Buffer.from(type, 'ascii'), data]);
    const crc = Buffer.alloc(4);
    crc.writeUInt32BE(crc32(body), 0);
    return Buffer.concat([len, body, crc]);
  };
  const png = Buffer.concat([
    Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]),
    chunk('IHDR', ihdr),
    chunk('IDAT', Buffer.from(deflateSync(Buffer.from(raw)))),
    chunk('IEND', Buffer.alloc(0)),
  ]);
  return new Uint8Array(png);
}

const CRC_TABLE = (() => {
  const t = new Uint32Array(256);
  for (let n = 0; n < 256; n++) {
    let c = n;
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    t[n] = c >>> 0;
  }
  return t;
})();

function crc32(buf: Buffer): number {
  let c = 0xffffffff;
  for (let i = 0; i < buf.length; i++) c = CRC_TABLE[(c ^ buf[i]!) & 0xff]! ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
}

export const enc = (s: string) => new TextEncoder().encode(s);
export const cat = (a: Uint8Array[]) => {
  const n = a.reduce((x, y) => x + y.length, 0);
  const r = new Uint8Array(n);
  let o = 0;
  for (const p of a) { r.set(p, o); o += p.length; }
  return r;
};

const APG_START = Buffer.from('\x1b_G');
const ST_BYTES = Buffer.from('\x1b\\');

/** One kitty APC: ESC _ G <ctrl[;payload]> ESC \. */
export function kittyApc(ctrl: string, payload?: string | Uint8Array): Uint8Array {
  const head = enc(`\x1b_G${ctrl}`);
  const tail = enc(ST);
  if (payload === undefined) return cat([head, tail]);
  const mid = enc(';');
  const body = typeof payload === 'string' ? enc(payload) : payload;
  return cat([head, mid, body, tail]);
}

/** Transmit `png` as kitty `a=t` chunked with `chunkSize` base64 chars per APC. */
export function transmitApcs(id: number, png: Uint8Array, chunkSize = 4096): Uint8Array[] {
  const b64 = Buffer.from(png).toString('base64');
  const out: Uint8Array[] = [];
  if (b64.length === 0) {
    out.push(kittyApc(`a=t,f=100,q=2,i=${id},m=0`, ''));
    return out;
  }
  const chunks: string[] = [];
  for (let k = 0; k < b64.length; k += chunkSize) chunks.push(b64.slice(k, k + chunkSize));
  if (chunks.length === 1) {
    return [kittyApc(`a=t,f=100,q=2,i=${id}`, chunks[0]!)];
  }
  for (let n = 0; n < chunks.length; n++) {
    const more = n < chunks.length - 1 ? '1' : '0';
    const ctrl = n === 0 ? `a=t,f=100,q=2,i=${id},m=${more}` : `q=2,m=${more}`;
    out.push(kittyApc(ctrl, chunks[n]!));
  }
  return out;
}

export function placeApc(id: number, cols: number, rows: number): Uint8Array {
  return kittyApc(`a=p,U=1,q=2,i=${id},p=${id},c=${cols},r=${rows}`);
}

export function deleteApc(scope: 'A' | 'I', id?: number): Uint8Array {
  return scope === 'A' ? kittyApc('a=d,d=A,q=2') : kittyApc(`a=d,d=I,i=${id},q=2`);
}

/** DCS-tmux envelope: every ESC of the payload doubled. */
export function wrapTmux(inner: Uint8Array): Uint8Array {
  const innerStr = Buffer.from(inner).toString('latin1');
  const doubled = innerStr.replaceAll('\x1b', '\x1b\x1b');
  return new TextEncoder().encode(`\x1bPtmux;${doubled}\x1b\\`);
}

/** Placeholder rows for an image: SGR pair + 19/90-style U+10EEEE cells. */
export function placeholderRows(id: number, cols: number, rows: number, diac = 773): Uint8Array {
  const rgb = [((id >> 16) & 255), ((id >> 8) & 255), (id & 255)];
  const sgr = enc(`\x1b[38;2;${rgb[0]};${rgb[1]};${rgb[2]}m\x1b[58:2::${rgb[0]}:${rgb[1]}:${rgb[2]}m`);
  const cell = enc('\u{10EEEE}' + String.fromCharCode(diac) + String.fromCharCode(diac));
  const parts: Uint8Array[] = [];
  for (let r = 0; r < rows; r++) {
    const cells: Uint8Array[] = [];
    for (let c = 0; c < cols; c++) cells.push(cell);
    parts.push(cat([sgr, cat(cells), enc('\x1b[39;59m')]));
  }
  return cat([...parts, enc('\r\r\n')]);
}

// ------------------------------------------------------------------ oracle
// Whole-buffer independent scanner: removes every APC G, decodes base64,
// reassembles transfers naively. Shares no code with the state machine.

export interface OracleTransmit { id: number; png: Uint8Array; sha256: string; w: number; h: number }
export interface OracleScan {
  bytes: Uint8Array; // input with every APC G removed
  transmits: OracleTransmit[];
  places: { id: number; cols: number; rows: number }[];
  resets: number;
}

export function oracleScan(input: Uint8Array): OracleScan {
  const buf = Buffer.from(input);
  const kept: Buffer[] = [];
  const transmits: OracleTransmit[] = [];
  const places: { id: number; cols: number; rows: number }[] = [];
  let resets = 0;
  let pos = 0;
  let pendId: number | null = null;
  let pendParts: string[] = [];
  const finish = () => {
    const full = pendParts.join('');
    const png = new Uint8Array(Buffer.from(full, 'base64'));
    const sha256 = createHash('sha256').update(png).digest('hex');
    let w = 0;
    let h = 0;
    if (png.length >= 24) {
      const dv = new DataView(png.buffer, png.byteOffset, png.byteLength);
      w = dv.getUint32(16, false);
      h = dv.getUint32(20, false);
    }
    transmits.push({ id: pendId!, png, sha256, w, h });
    pendId = null;
    pendParts = [];
  };
  while (true) {
    const s = buf.indexOf(APG_START, pos);
    if (s === -1) { kept.push(buf.subarray(pos)); break; }
    const e = buf.indexOf(ST_BYTES, s + 3);
    if (e === -1) { kept.push(buf.subarray(pos)); break; }
    kept.push(buf.subarray(pos, s));
    const body = buf.subarray(s + 3, e).toString('latin1');
    const semi = body.indexOf(';');
    const ctrl = semi === -1 ? body : body.slice(0, semi);
    const payload = semi === -1 ? '' : body.slice(semi + 1);
    const keys = new Map<string, string>();
    for (const part of ctrl.split(',')) {
      const eq = part.indexOf('=');
      if (eq > 0) keys.set(part.slice(0, eq), part.slice(eq + 1));
    }
    const a = keys.get('a') ?? 't';
    if (!keys.has('a') && pendId !== null) {
      // continuation
      if (keys.get('m') === '1') pendParts.push(payload);
      else { pendParts.push(payload); finish(); }
    } else if (a === 't' && keys.get('f') === '100' && keys.has('i')) {
      const id = Number(keys.get('i'));
      if (keys.get('m') === '1') { pendId = id; pendParts = [payload]; }
      else { pendId = id; pendParts = [payload]; finish(); }
    } else if (a === 'p' && keys.get('U') === '1' && keys.has('i')) {
      places.push({ id: Number(keys.get('i')), cols: Number(keys.get('c') ?? 0), rows: Number(keys.get('r') ?? 0) });
    } else if (a === 'd') {
      resets++;
    }
    pos = e + 2;
  }
  return { bytes: cat(kept), transmits, places, resets };
}

// ------------------------------------------------------------ synth stream
/** Run the Python synthetic omp stream generator. Throws (never skips) on failure. */
export function synthStream(args: string[] = ['--no-wrap']): Uint8Array {
  const py = join(import.meta.dir, '../../fixtures/synth_omp_kitty_stream.py');
  const p = spawnSync('python3', ['-I', py, ...args], { maxBuffer: 1 << 28 });
  if (p.error !== undefined) throw new Error(`synth generator spawn failed: ${(p.error as Error).message}`);
  if (p.status !== 0) throw new Error(`synth generator failed: ${Buffer.from(p.stderr).toString()}`);
  return new Uint8Array(p.stdout);
}
