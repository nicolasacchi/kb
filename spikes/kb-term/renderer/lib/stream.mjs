// Synthetic stream builders for the T2 renderer spike. Everything here is content-free and deterministic.
import zlib from 'node:zlib';
import { DIAC } from './diac.mjs';

export const ESC = '\x1b';
export const PH = String.fromCodePoint(0x10eeee);

// ---- PNG encoder (RGBA8, no deps) --------------------------------------------------------------
const crcTable = (() => { const t = new Uint32Array(256); for (let n = 0; n < 256; n++) { let c = n; for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1; t[n] = c >>> 0; } return t; })();
function crc32(buf) { let c = 0xffffffff; for (let i = 0; i < buf.length; i++) c = crcTable[(c ^ buf[i]) & 255] ^ (c >>> 8); return (c ^ 0xffffffff) >>> 0; }
function chunk(type, data) {
  const len = Buffer.alloc(4); len.writeUInt32BE(data.length);
  const td = Buffer.concat([Buffer.from(type, 'ascii'), data]);
  const crc = Buffer.alloc(4); crc.writeUInt32BE(crc32(td));
  return Buffer.concat([len, td, crc]);
}
export function png(w, h, f) {
  const raw = Buffer.alloc((w * 4 + 1) * h);
  for (let y = 0; y < h; y++) {
    const o = y * (w * 4 + 1); raw[o] = 0;
    for (let x = 0; x < w; x++) {
      const c = f(x, y); const p = o + 1 + x * 4;
      raw[p] = c[0]; raw[p + 1] = c[1]; raw[p + 2] = c[2]; raw[p + 3] = c.length > 3 ? c[3] : 255;
    }
  }
  const ihdr = Buffer.alloc(13); ihdr.writeUInt32BE(w, 0); ihdr.writeUInt32BE(h, 4); ihdr[8] = 8; ihdr[9] = 6;
  return Buffer.concat([Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]), chunk('IHDR', ihdr), chunk('IDAT', zlib.deflateSync(raw, { level: 3 })), chunk('IEND', Buffer.alloc(0))]);
}

// ---- known test pattern: four quadrants + a centre square -------------------------------------
export const PALETTE = [[230, 30, 30], [30, 200, 30], [30, 30, 230], [240, 220, 20]];
export const MAGENTA = [255, 0, 255];
export function quadColor(u, v, k = 0) {
  if (Math.abs(u - 0.5) < 0.1 && Math.abs(v - 0.5) < 0.1) return MAGENTA;
  const q = (v < 0.5 ? 0 : 2) + (u < 0.5 ? 0 : 1);
  return PALETTE[(q + k) % 4];
}
// expected colour at (u,v) or null when a pattern boundary is too close to call
export function expectedAt(u, v, k = 0) {
  const c = quadColor(u, v, k); const e = 0.015;
  for (const [du, dv] of [[e, 0], [-e, 0], [0, e], [0, -e], [e, e], [-e, -e]]) {
    const o = quadColor(u + du, v + dv, k); if (o[0] !== c[0] || o[1] !== c[1] || o[2] !== c[2]) return null;
  }
  return c;
}
export const quadPng = (w, h, k = 0) => png(w, h, (x, y) => quadColor((x + 0.5) / w, (y + 0.5) / h, k));
// fine checker (squares of `sq` px) for sharpness measurements
export const checkerPng = (w, h, sq) => png(w, h, (x, y) => (((Math.floor(x / sq) + Math.floor(y / sq)) & 1) ? [255, 255, 255] : [0, 0, 0]));
export const solidPng = (w, h, c) => png(w, h, () => c);

// ---- kitty APC ----------------------------------------------------------------------------------
export const apc = (ctrl, payload = '') => `${ESC}_G${ctrl}${payload ? ';' + payload : ''}${ESC}\\`;
export const wrapTmux = (s) => `${ESC}Ptmux;${s.replaceAll(ESC, ESC + ESC)}${ESC}\\`;
export function transmit(id, pngBuf, { wrap = false } = {}) {
  const b64 = pngBuf.toString('base64'); const parts = [];
  for (let i = 0; i < b64.length; i += 4096) parts.push(b64.slice(i, i + 4096));
  return parts.map((c, n) => {
    const more = n < parts.length - 1 ? 1 : 0;
    const s = apc(n === 0 ? `a=t,f=100,q=2,i=${id},m=${more}` : `q=2,m=${more}`, c);
    return wrap ? wrapTmux(s) : s;
  }).join('');
}
export const place = (id, c, r) => apc(`a=p,U=1,q=2,i=${id},p=${id},c=${c},r=${r}`);
export const gfxReset = () => apc('a=d,d=A,q=2');

export function fgFor(id, mode = 'rgb') {
  if (mode === 'pal') return `${ESC}[38;5;${id}m`;
  return `${ESC}[38;2;${(id >> 16) & 255};${(id >> 8) & 255};${id & 255}m`;
}
export const diac = (i) => String.fromCodePoint(DIAC[i]);
// r rows of placeholder cells; every row starts at (1-based) column `indent`. Ends with CRLF.
export function placeholderRows(id, c, r, indent, mode = 'rgb', { implicit = false } = {}) {
  const rows = [];
  for (let y = 0; y < r; y++) {
    let line = fgFor(id, mode);
    for (let x = 0; x < c; x++) {
      if (implicit && x > 0) line += PH; // kitty: no diacritics -> inherit row, col+1
      else line += PH + diac(y) + diac(x);
    }
    rows.push(`${ESC}[${indent}G${line}${ESC}[39m`);
  }
  return rows.join('\r\n') + '\r\n';
}

export const text = (n, tag = 'line') => `${tag} ${String(n).padStart(6, '0')} the quick brown fox jumps over the lazy dog ${'#'.repeat(n % 17)}`;

// Line-tracking scenario builder: abs line numbers for truth, no scroll regions, no wrapping.
export class Scenario {
  constructor({ devCW, devCH }) { this.devCW = devCW; this.devCH = devCH; this.parts = []; this.line = 0; this.truth = []; }
  raw(s) { this.parts.push(s); return this; }
  head() { return this.raw(`${ESC}[?25l${ESC}[2J${ESC}[H`); }
  textLines(n, tag = 'line', start = 0) { for (let i = 0; i < n; i++) { this.raw(text(start + i, tag) + '\r\n'); this.line++; } return this; }
  blank(n = 1) { for (let i = 0; i < n; i++) { this.raw('\r\n'); this.line++; } return this; }
  image({ id, c, r, indent = 11, variant = 0, mode = 'rgb', scale = 1, implicit = false, transmitIt = true, pngBuf = null, wrap = false }) {
    if (transmitIt) this.raw(transmit(id, pngBuf || quadPng(c * this.devCW * scale, r * this.devCH * scale, variant), { wrap }));
    this.raw(place(id, c, r));
    this.truth.push({ id, line0: this.line, col0: indent - 1, c, r, variant });
    this.raw(placeholderRows(id, c, r, indent, mode, { implicit }));
    this.line += r;
    return this;
  }
  build() { return Buffer.from(this.parts.join(''), 'utf8'); }
}

// Shared scenarios -------------------------------------------------------------------------------
export function basicScenario(dev) {
  const s = new Scenario(dev).head().textLines(2, 'header').blank();
  s.image({ id: 0x3a7f21, c: 40, r: 10, indent: 11, variant: 0 });
  s.blank().textLines(1, 'middle').blank();
  s.image({ id: 0x1b4c9d, c: 20, r: 6, indent: 51, variant: 1 });
  return s;
}
export const BIG_IDS = [0x3a7f21, 0x1b4c9d, 0x6e2f80, 0x9c5a13];
export function bigScenario(dev, { textLinesTotal = 2000, at = [300, 800, 1300, 1800], c = 30, r = 8 } = {}) {
  const s = new Scenario(dev).head();
  let k = 0;
  for (let i = 0; i < textLinesTotal; i++) {
    if (k < at.length && i === at[k]) { s.blank(); s.image({ id: BIG_IDS[k], c, r, indent: 11, variant: k }); s.blank(); k++; }
    s.textLines(1, 'line', i);
  }
  return s;
}
