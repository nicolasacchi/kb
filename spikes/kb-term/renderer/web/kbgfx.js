// PROTOTYPE kb placeholder renderer (spike T2). Not production code.
// Parts: ImageStore (ImageDef map), ApcReassembler (kitty APC -> store, rest -> terminal),
// PlaceholderRenderer (scan visible rows for U+10EEEE, group, draw cropped pictures in an overlay).
import { DIAC } from './diacritics.js';

export const PH = 0x10eeee;
const DIAC_INDEX = new Map(DIAC.map((c, i) => [c, i]));

function pngSize(b64) {
  // IHDR width/height live at bytes 16..23 -> first 32 base64 chars are enough.
  const bin = atob(b64.slice(0, 32));
  const u32 = (o) => ((bin.charCodeAt(o) << 24) | (bin.charCodeAt(o + 1) << 16) | (bin.charCodeAt(o + 2) << 8) | bin.charCodeAt(o + 3)) >>> 0;
  return { w: u32(16), h: u32(20) };
}

export class ImageStore {
  constructor() {
    this.defs = new Map(); // id -> {id, url, w, h, version, b64len}
    this.place = new Map(); // id -> {c, r}   (virtual placement a=p,U=1)
    this.resets = 0; // GfxReset markers seen (a=d); the store itself is NOT freed
    this.listeners = new Set();
    this.transmits = 0;
  }
  onChange(fn) { this.listeners.add(fn); }
  _emit() { for (const fn of this.listeners) fn(); }
  setDef(id, b64) {
    this.transmits++;
    const url = 'data:image/png;base64,' + b64;
    const old = this.defs.get(id);
    if (old && old.url === url) return; // identical retransmit: no change, no version bump
    const { w, h } = pngSize(b64);
    const def = { id, url, w, h, version: (old ? old.version : 0) + 1 };
    // preload+decode now (when the ImageDef arrives), so the first paint of a placeholder box is not blank
    def.img = new Image(); def.img.src = url; def.ready = def.img.decode().then(() => true, () => false);
    this.defs.set(id, def);
    this._emit();
  }
  setPlace(id, c, r) {
    const o = this.place.get(id);
    if (o && o.c === c && o.r === r) return;
    this.place.set(id, { c, r });
    this._emit();
  }
  gfxReset() { this.resets++; }
  exportJSON() {
    return JSON.stringify({ defs: [...this.defs.values()].map((d) => ({ id: d.id, url: d.url })), place: [...this.place] });
  }
  importJSON(s) {
    const o = JSON.parse(s);
    for (const d of o.defs) this.setDef(d.id, d.url.slice('data:image/png;base64,'.length));
    for (const [id, p] of o.place) this.setPlace(id, p.c, p.r);
  }
}

// Byte-level state machine: extracts `ESC _ G ... ESC \` (kitty APC) from the stream. Everything else is passed on.
// Input is expected to be DCS-tmux-unwrapped already (the tap does that in the design).
export class ApcReassembler {
  constructor(store, sink) {
    this.store = store;
    this.sink = sink; // (Uint8Array) => void
    this.state = 0; // 0 normal, 1 ESC, 2 ESC _, 3 in APC-G body, 4 body+ESC
    this.body = [];
    this.pendingId = null;
    this.chunks = [];
    this.stats = { apc: 0, transmits: 0, placements: 0, deletes: 0, unsupportedFormat: 0 };
  }
  feed(bytes) {
    const out = [];
    const flush = () => { if (out.length) { this.sink(Uint8Array.from(out)); out.length = 0; } };
    for (let i = 0; i < bytes.length; i++) {
      const b = bytes[i];
      switch (this.state) {
        case 0:
          if (b === 0x1b) this.state = 1; else out.push(b);
          break;
        case 1:
          if (b === 0x5f) this.state = 2; else { out.push(0x1b); this.state = 0; i--; }
          break;
        case 2:
          if (b === 0x47) { this.state = 3; this.body = []; } else { out.push(0x1b, 0x5f); this.state = 0; i--; }
          break;
        case 3:
          if (b === 0x1b) this.state = 4; else this.body.push(b);
          break;
        case 4:
          if (b === 0x5c) { this._apc(String.fromCharCode(...this.body)); this.state = 0; }
          else { this.body.push(0x1b); this.state = 3; i--; }
          break;
      }
    }
    flush();
  }
  _apc(text) {
    this.stats.apc++;
    const semi = text.indexOf(';');
    const ctrl = semi < 0 ? text : text.slice(0, semi);
    const payload = semi < 0 ? '' : text.slice(semi + 1);
    const kv = {};
    for (const p of ctrl.split(',')) { const e = p.indexOf('='); if (e > 0) kv[p.slice(0, e)] = p.slice(e + 1); }
    const a = kv.a || 't';
    if (a === 't' || a === 'T') {
      if (kv.i !== undefined) { this.pendingId = Number(kv.i); this.chunks = []; this.fmt = kv.f || '32'; }
      if (this.pendingId === null) return;
      this.chunks.push(payload);
      if (kv.m !== '1') {
        const id = this.pendingId; this.pendingId = null;
        if (this.fmt !== '100') { this.stats.unsupportedFormat++; return; }
        this.stats.transmits++;
        this.store.setDef(id, this.chunks.join(''));
      }
    } else if (a === 'p') {
      this.stats.placements++;
      if (kv.U === '1' && kv.i !== undefined) this.store.setPlace(Number(kv.i), Number(kv.c || 0), Number(kv.r || 0));
    } else if (a === 'd') {
      this.stats.deletes++;
      this.store.gfxReset();
    }
  }
}

export function decodePlaceholder(chars, fgIsRGB, fgIsPal, fg, prev) {
  const cps = [...chars].map((c) => c.codePointAt(0));
  if (cps[0] !== PH) return null;
  let id = fgIsRGB ? fg & 0xffffff : fgIsPal ? fg & 0xff : 0;
  let row = DIAC_INDEX.has(cps[1]) ? DIAC_INDEX.get(cps[1]) : -1;
  let col = DIAC_INDEX.has(cps[2]) ? DIAC_INDEX.get(cps[2]) : -1;
  if (DIAC_INDEX.has(cps[3])) id |= DIAC_INDEX.get(cps[3]) << 24;
  if (row < 0) {
    // kitty spec: no diacritics -> continue previous cell (same row, next column) when same colour
    if (prev && prev.id === id) { row = prev.row; col = prev.col + 1; } else { row = 0; col = 0; }
  } else if (col < 0) {
    if (prev && prev.id === id && prev.row === row) col = prev.col + 1; else col = 0;
  }
  return { id, row, col };
}

export class PlaceholderRenderer {
  constructor(term, store, opts = {}) {
    this.term = term;
    this.store = store;
    this.opts = opts;
    this.groups = new Map(); // key -> {clip, img|box, ver}
    this.frames = 0;
    this.lastMs = 0;
    this.totalMs = 0;
    this.overlay = null;
    this._raf = 0;
    this.hideGlyphs = opts.hideGlyphs !== false;
    this.decos = new Map(); // marker -> Map(key -> decoration)
    this.stats = { imgCreated: 0, imgCompleteAtCreate: 0, decosCreated: 0, decosDisposed: 0 };
    this.attach();
  }
  attach() {
    const screen = this.term.element.querySelector('.xterm-screen');
    this.screen = screen;
    const ov = document.createElement('div');
    ov.className = 'kbgfx-overlay';
    ov.style.cssText = 'position:absolute;left:0;top:0;right:0;bottom:0;overflow:hidden;pointer-events:none;z-index:50;';
    screen.appendChild(ov);
    this.overlay = ov;
    // Render in the same frame xterm paints (onRender) so text and pictures never disagree by a frame.
    this.term.onRender(() => this.render());
    this.term.onScroll(() => this.render());
    this.term.onResize(() => this.schedule());
    this.store.onChange(() => this.schedule());
  }
  schedule() {
    if (this._raf) return;
    this._raf = requestAnimationFrame(() => { this._raf = 0; this.render(); });
  }
  scan(baseOverride, count) {
    const buf = this.term.buffer.active;
    const base = baseOverride === undefined ? buf.viewportY : baseOverride;
    const cells = [];
    const reuse = buf.getNullCell();
    const nrows = count === undefined ? this.term.rows : count;
    for (let y = 0; y < nrows; y++) {
      const line = buf.getLine(base + y);
      if (!line) continue;
      let prev = null;
      for (let x = 0; x < this.term.cols; x++) {
        const c = line.getCell(x, reuse);
        if (!c) break;
        const ch = c.getChars();
        if (ch.length >= 2 && ch.codePointAt(0) === PH) {
          const d = decodePlaceholder(ch, c.isFgRGB(), c.isFgPalette(), c.getFgColor(), prev);
          if (d) { cells.push({ ...d, y, x, abs: base + y }); prev = d; continue; }
        }
        prev = null;
      }
    }
    return cells;
  }
  render() {
    const t0 = performance.now();
    this.frames++;
    const rect = this.screen.getBoundingClientRect();
    const cw = rect.width / this.term.cols;
    const ch = rect.height / this.term.rows;
    const dpr = window.devicePixelRatio || 1;
    const snap = (v) => Math.round(v * dpr) / dpr;
    const cells = this.scan();
    const groups = new Map();
    for (const c of cells) {
      const key = `${c.id}:${c.abs - c.row}:${c.x - c.col}`;
      let g = groups.get(key);
      if (!g) { g = { key, id: c.id, originAbs: c.abs - c.row, originCol: c.x - c.col, minX: 1e9, maxX: -1, minY: 1e9, maxY: -1, maxRow: 0, maxCol: 0, n: 0 }; groups.set(key, g); }
      g.minX = Math.min(g.minX, c.x); g.maxX = Math.max(g.maxX, c.x);
      g.minY = Math.min(g.minY, c.y); g.maxY = Math.max(g.maxY, c.y);
      g.maxRow = Math.max(g.maxRow, c.row); g.maxCol = Math.max(g.maxCol, c.col); g.n++;
    }
    const base = this.term.buffer.active.viewportY;
    this.renderedVy = base;
    for (const [key, g] of groups) {
      let el = this.groups.get(key);
      const def = this.store.defs.get(g.id);
      const pl = this.store.place.get(g.id);
      const C = pl && pl.c ? pl.c : g.maxCol + 1;
      const R = pl && pl.r ? pl.r : g.maxRow + 1;
      const kind = def ? 'img' : 'missing';
      if (el && el.kind !== kind) { el.clip.remove(); this.groups.delete(key); el = null; }
      if (!el) {
        const clip = document.createElement('div');
        clip.className = 'kbgfx-clip';
        clip.style.cssText = 'position:absolute;overflow:hidden;background:#000;';
        clip.dataset.kbgfxId = String(g.id);
        let inner;
        if (def) {
          inner = document.createElement('img');
          inner.className = 'kbgfx-img';
          inner.draggable = false;
          inner.style.cssText = 'position:absolute;display:block;object-fit:contain;max-width:none;';
        } else {
          inner = document.createElement('div');
          inner.className = 'kbgfx-missing';
          inner.textContent = 'image unavailable';
          inner.style.cssText = 'position:absolute;box-sizing:border-box;display:flex;align-items:center;justify-content:center;background:#2b2b30;color:#9a9aa2;border:1px dashed #5a5a62;font:12px sans-serif;text-align:center;overflow:hidden;';
        }
        clip.appendChild(inner);
        this.overlay.appendChild(clip);
        el = { kind, clip, inner, ver: -1 };
        this.groups.set(key, el);
      }
      el.seen = this.frames;
      const left = snap(g.minX * cw), top = snap(g.minY * ch);
      const right = snap((g.maxX + 1) * cw), bottom = snap((g.maxY + 1) * ch);
      el.clip.style.left = left + 'px';
      el.clip.style.top = top + 'px';
      el.clip.style.width = (right - left) + 'px';
      el.clip.style.height = (bottom - top) + 'px';
      // The full (virtual) picture box in screen css px, may extend outside the visible clip.
      const boxL = snap(g.originCol * cw), boxT = snap((g.originAbs - base) * ch);
      const boxR = snap((g.originCol + C) * cw), boxB = snap((g.originAbs + R - base) * ch);
      el.inner.style.left = (boxL - left) + 'px';
      el.inner.style.top = (boxT - top) + 'px';
      el.inner.style.width = (boxR - boxL) + 'px';
      el.inner.style.height = (boxB - boxT) + 'px';
      if (kind === 'img' && el.ver !== def.version) {
        const first = el.ver === -1;
        el.inner.src = def.url; el.ver = def.version;
        if (first) { this.stats.imgCreated++; if (el.inner.complete) this.stats.imgCompleteAtCreate++; }
      }
      if (kind === 'missing') {
        // unavailable box spans the visible rectangle, not the whole virtual box
        el.inner.style.left = '0px'; el.inner.style.top = '0px';
        el.inner.style.width = (right - left) + 'px'; el.inner.style.height = (bottom - top) + 'px';
      }
    }
    for (const [key, el] of this.groups) {
      if (el.seen !== this.frames) { el.clip.remove(); this.groups.delete(key); }
    }
    if (this.hideGlyphs) this.syncDecorations(cells);
    this.lastMs = performance.now() - t0;
    this.totalMs += this.lastMs;
    this.lastGroups = groups.size;
    this.lastCells = cells.length;
  }
  // The U+10EEEE glyph (tofu) and its diacritics are painted by xterm in the id colour and can spill ~1px past the cell.
  // Recolour exactly those cells to the background via the decoration API so nothing is left to spill.
  syncDecorations(cells) {
    const buf = this.term.buffer.active;
    const base = buf.viewportY, rows = this.term.rows;
    const wanted = new Map(); // abs -> Set(key)
    const runs = [];
    for (const c of cells) {
      const last = runs[runs.length - 1];
      if (last && last.abs === c.abs && last.x0 + last.n === c.x) last.n++; else runs.push({ abs: c.abs, x0: c.x, n: 1 });
    }
    for (const r of runs) { const k = `${r.x0}:${r.n}`; if (!wanted.has(r.abs)) wanted.set(r.abs, new Set()); wanted.get(r.abs).add(k); }
    for (const [marker, m] of [...this.decos]) {
      if (marker.isDisposed) { this.decos.delete(marker); continue; }
      const abs = marker.line;
      if (abs < base || abs >= base + rows) continue;
      for (const [k, d] of [...m]) {
        if (!(wanted.get(abs) && wanted.get(abs).has(k))) { d.dispose(); m.delete(k); this.stats.decosDisposed++; }
      }
    }
    const byLine = new Map();
    for (const [marker, m] of this.decos) byLine.set(marker.line, m);
    const curAbs = buf.baseY + buf.cursorY;
    const bg = this.term.options.theme?.background || '#000000';
    for (const r of runs) {
      const k = `${r.x0}:${r.n}`;
      let m = byLine.get(r.abs);
      if (m && m.has(k)) continue;
      const marker = m ? [...this.decos.keys()].find((mk) => mk.line === r.abs) : this.term.registerMarker(r.abs - curAbs);
      if (!marker) continue;
      if (!m) { m = new Map(); this.decos.set(marker, m); byLine.set(r.abs, m); }
      const d = this.term.registerDecoration({ marker, x: r.x0, width: r.n, height: 1, foregroundColor: bg, backgroundColor: bg, layer: 'top' });
      if (d) { m.set(k, d); this.stats.decosCreated++; }
    }
  }
  describe() {
    const rect = this.screen.getBoundingClientRect();
    return [...this.groups.values()].map((e) => {
      const r = e.clip.getBoundingClientRect();
      const i = e.inner.getBoundingClientRect();
      return { kind: e.kind, id: Number(e.clip.dataset.kbgfxId), clip: { x: r.left, y: r.top, w: r.width, h: r.height }, inner: { x: i.left, y: i.top, w: i.width, h: i.height }, screen: { x: rect.left, y: rect.top } };
    });
  }
}
