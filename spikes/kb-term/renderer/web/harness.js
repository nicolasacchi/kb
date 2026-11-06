import { Terminal } from '/nm/@xterm/xterm/lib/xterm.mjs';
import { WebglAddon } from '/nm/@xterm/addon-webgl/lib/addon-webgl.mjs';
import { ImageAddon } from '/nm/@xterm/addon-image/lib/addon-image.mjs';
import { Unicode11Addon } from '/nm/@xterm/addon-unicode11/lib/addon-unicode11.mjs';
import { UnicodeGraphemesAddon } from '/nm/@xterm/addon-unicode-graphemes/lib/addon-unicode-graphemes.mjs';
import { ImageStore, ApcReassembler, PlaceholderRenderer, PH } from './kbgfx.js';
import { DIAC } from './diacritics.js';

const q = new URLSearchParams(location.search);
const opt = {
  renderer: q.get('renderer') || 'dom',
  unicode: q.get('unicode') || '11',
  cols: Number(q.get('cols') || 100),
  rows: Number(q.get('rows') || 30),
  scrollback: Number(q.get('scrollback') || 5000),
  iip: q.get('iip') === '1',
  iipMB: Number(q.get('iipmb') || 128),
  gfx: q.get('gfx') !== '0',
};

function mkTerm(extra = {}) {
  return new Terminal({
    cols: opt.cols, rows: opt.rows, scrollback: opt.scrollback, allowProposedApi: true,
    fontFamily: 'monospace', fontSize: 14, lineHeight: 1, cursorBlink: false, cursorInactiveStyle: 'none',
    theme: { background: '#000000', foreground: '#dddddd' }, ...extra,
  });
}

const term = mkTerm();
const store = new ImageStore();
const pending = [];
const sink = (bytes) => { term.write(bytes); };
const reasm = new ApcReassembler(store, sink);

function useUnicode(t, kind) {
  if (kind === '11') { t.loadAddon(new Unicode11Addon()); t.unicode.activeVersion = '11'; }
  else if (kind === 'graphemes') { t.loadAddon(new UnicodeGraphemesAddon()); }
}

const info = { opt, webgl: 'n/a', webglError: null, contextLost: false, iip: opt.iip };
let imageAddon = null;
term.open(document.getElementById('term'));
useUnicode(term, opt.unicode);
if (opt.renderer === 'webgl') {
  try {
    const w = new WebglAddon();
    w.onContextLoss(() => { info.contextLost = true; });
    term.loadAddon(w);
    info.webgl = 'loaded';
  } catch (e) { info.webgl = 'failed'; info.webglError = String(e && e.message || e); }
}
if (opt.iip) {
  imageAddon = new ImageAddon({ storageLimit: opt.iipMB, showPlaceholder: true });
  term.loadAddon(imageAddon);
}
const renderer = opt.gfx ? new PlaceholderRenderer(term, store) : null;

const b64ToBytes = (s) => Uint8Array.from(atob(s), (c) => c.charCodeAt(0));
const raf = () => new Promise((r) => requestAnimationFrame(() => r()));
const frames = async (n = 2) => { for (let i = 0; i < n; i++) await raf(); };
const flush = () => new Promise((r) => term.write('', r));

window.T = {
  term, store, renderer, reasm, imageAddon, info, opt,
  async feed(b64) { reasm.feed(b64ToBytes(b64)); await flush(); await frames(3); },
  async feedChunked(b64, size = 65536) {
    const bytes = b64ToBytes(b64);
    for (let i = 0; i < bytes.length; i += size) { reasm.feed(bytes.subarray(i, i + size)); await flush(); }
    await frames(3);
  },
  async writeRaw(str) { term.write(str); await flush(); await frames(3); },
  async writeRawBytes(b64) { term.write(b64ToBytes(b64)); await flush(); await frames(3); },
  frames, flush,
  async scrollToLine(n) { term.scrollToLine(n); await frames(3); return term.buffer.active.viewportY; },
  viewportY: () => term.buffer.active.viewportY,
  bufLen: () => term.buffer.active.length,
  baseY: () => term.buffer.active.baseY,
  scan: () => renderer.scan(),
  scanAll() {
    const cells = renderer.scan(0, term.buffer.active.length);
    return cells.map((c) => [c.abs, c.x, c.id, c.row, c.col].join(','));
  },
  async sweepCheck(step, rounds) {
    let stale = 0, n = 0;
    for (let k = 0; k < rounds; k++) {
      term.scrollToTop(); await raf(); await raf();
      for (let y = 0; y < term.buffer.active.baseY; y += step) {
        term.scrollLines(step); await raf(); n++;
        if (renderer.renderedVy !== term.buffer.active.viewportY) stale++;
      }
    }
    return { frames: n, stale };
  },
  describe: () => renderer.describe(),
  exportDefs: () => store.exportJSON(),
  importDefs(s) { store.importJSON(s); },
  async bbox(b64, thr = 40) {
    const { x, w, h } = await window.T.loadPng(b64);
    const d = x.getImageData(0, 0, w, h).data; let x0 = w, y0 = h, x1 = -1, y1 = -1;
    for (let yy = 0; yy < h; yy++) for (let xx = 0; xx < w; xx++) { const i = (yy * w + xx) * 4; if (d[i] + d[i + 1] + d[i + 2] > thr) { if (xx < x0) x0 = xx; if (xx > x1) x1 = xx; if (yy < y0) y0 = yy; if (yy > y1) y1 = yy; } }
    return { x0, y0, x1, y1, w: x1 - x0 + 1, h: y1 - y0 + 1 };
  },
  async iipFill(n, w, h) {
    const lines = []; const oc = new OffscreenCanvas(w, h); const cx = oc.getContext('2d');
    for (let i = 0; i < n; i++) {
      cx.fillStyle = `rgb(${(i * 37) & 255},${(i * 91) & 255},${(i * 53) & 255})`; cx.fillRect(0, 0, w, h);
      cx.fillStyle = '#fff'; cx.fillRect(0, 0, w / 2, h / 2);
      const blob = await oc.convertToBlob({ type: 'image/png' }); const buf = new Uint8Array(await blob.arrayBuffer());
      let bin = ''; for (let k = 0; k < buf.length; k += 0x8000) bin += String.fromCharCode(...buf.subarray(k, k + 0x8000));
      const b = term.buffer.active; lines.push(b.baseY + b.cursorY);
      term.write(`\x1b]1337;File=inline=1;size=${buf.length}:${btoa(bin)}\x07\r\n`);
      await flush();
    }
    return lines;
  },
  iipSurvivors(lines) {
    const alive = []; lines.forEach((l, i) => { if (imageAddon.getImageAtBufferCell(0, l)) alive.push(i); });
    let internal = null;
    try { internal = imageAddon._storage._images.size; } catch (e) { internal = null; }
    return { alive: alive.length, first: alive[0] ?? null, last: alive[alive.length - 1] ?? null, usageMB: imageAddon.storageUsage, limitMB: imageAddon.storageLimit, internal };
  },
  metrics() {
    const el = term.element.querySelector('.xterm-screen');
    const r = el.getBoundingClientRect();
    let device = null;
    try { const d = term._core._renderService.dimensions; device = { cw: d.device.cell.width, ch: d.device.cell.height, cssCw: d.css.cell.width, cssCh: d.css.cell.height }; } catch (e) { device = null; }
    const canvases = [...el.querySelectorAll('canvas')].map((c) => ({ cls: c.className, w: c.width, h: c.height, cssW: c.style.width }));
    const rows = term.element.querySelector('.xterm-rows');
    return {
      dpr: window.devicePixelRatio, screen: { x: r.left, y: r.top, w: r.width, h: r.height },
      cw: r.width / term.cols, ch: r.height / term.rows, device, canvases,
      rowsDisplay: rows ? getComputedStyle(rows).display + '/' + getComputedStyle(rows).visibility : null,
      domRowSpans: rows ? rows.querySelectorAll('span').length : 0,
      webgl: info.webgl, webglError: info.webglError, contextLost: info.contextLost,
      cols: term.cols, rows: term.rows,
    };
  },
  async fastScroll(path) {
    // path: list of absolute target lines; jump without awaiting a paint between jumps (stress the sync)
    const t0 = performance.now();
    let maxMs = 0;
    for (const p of path) { term.scrollToLine(p); }
    await frames(3);
    return { ms: performance.now() - t0, maxMs };
  },
  async sweep(step, rounds) {
    // wheel-like scroll: scrollLines(step) once per animation frame through the whole buffer and back
    const t0 = performance.now(); let n = 0; const rr = renderer ? renderer.totalMs : 0; const f0 = renderer ? renderer.frames : 0;
    for (let k = 0; k < rounds; k++) {
      term.scrollToTop(); await raf();
      for (let y = 0; y < term.buffer.active.baseY; y += step) { term.scrollLines(step); await raf(); n++; }
      for (let y = 0; y < term.buffer.active.baseY; y += step) { term.scrollLines(-step); await raf(); n++; }
    }
    return { ms: performance.now() - t0, frames: n, rendererMs: renderer ? renderer.totalMs - rr : 0, rendererFrames: renderer ? renderer.frames - f0 : 0 };
  },
  overlayInfo() {
    const ov = term.element.querySelector('.kbgfx-overlay');
    return ov ? { children: ov.children.length, missing: ov.querySelectorAll('.kbgfx-missing').length, imgs: ov.querySelectorAll('img.kbgfx-img').length, texts: [...ov.querySelectorAll('.kbgfx-missing')].map((e) => e.textContent) } : null;
  },
  imgSrcVersions() { return renderer ? [...renderer.groups.values()].map((e) => e.ver) : []; },
  widthTable(kind) {
    const t = new Terminal({ cols: 20, rows: 3, allowProposedApi: true });
    useUnicode(t, kind);
    return new Promise((resolve) => {
      const bad = [];
      let i = 0;
      const next = () => {
        if (i >= DIAC.length) { resolve({ kind, total: DIAC.length, bad }); return; }
        const d = DIAC[i];
        const ph = String.fromCodePoint(PH);
        t.write('\x1b[2J\x1b[H' + ph + String.fromCodePoint(d) + String.fromCodePoint(DIAC[(i * 7 + 3) % DIAC.length]) + 'Z', () => {
          const line = t.buffer.active.getLine(0);
          const c0 = line.getCell(0).getChars();
          const c1 = line.getCell(1).getChars();
          const cps0 = [...c0].map((c) => c.codePointAt(0));
          if (!(c1 === 'Z' && cps0.length === 3)) bad.push({ cp: d, hex: d.toString(16), cell0Len: cps0.length, cell1: c1 });
          i++; next();
        });
      };
      next();
    });
  },
  // image-analysis helpers (operate on screenshots passed in as base64 PNG)
  async loadPng(b64) {
    const img = new Image(); img.src = 'data:image/png;base64,' + b64; await img.decode();
    const c = document.createElement('canvas'); c.width = img.naturalWidth; c.height = img.naturalHeight;
    const x = c.getContext('2d', { willReadFrequently: true }); x.drawImage(img, 0, 0);
    return { c, x, w: c.width, h: c.height };
  },
  async probe(b64, pts) {
    const { x, w, h } = await window.T.loadPng(b64);
    return pts.map((p) => { const px = Math.round(p.x), py = Math.round(p.y); if (px < 0 || py < 0 || px >= w || py >= h) return null; return Array.from(x.getImageData(px, py, 1, 1).data); });
  },
  lapEnergy(x, rect) {
    const d = x.getImageData(rect.x, rect.y, rect.w, rect.h).data; const w = rect.w, h = rect.h;
    const lum = new Float32Array(w * h);
    for (let i = 0; i < w * h; i++) lum[i] = 0.299 * d[i * 4] + 0.587 * d[i * 4 + 1] + 0.114 * d[i * 4 + 2];
    let s = 0, n = 0;
    for (let yy = 1; yy < h - 1; yy++) for (let xx = 1; xx < w - 1; xx++) {
      const i = yy * w + xx; const l = 4 * lum[i] - lum[i - 1] - lum[i + 1] - lum[i - w] - lum[i + w]; s += l * l; n++;
    }
    return n ? s / n : 0;
  },
  async energy(b64, rect) { const { x } = await window.T.loadPng(b64); return window.T.lapEnergy(x, rect); },
  async refEnergy(srcB64, dw, dh) {
    // reference: the source drawn once, high-quality downscale, into an exact device-size canvas
    const { c } = await window.T.loadPng(srcB64);
    const r = document.createElement('canvas'); r.width = dw; r.height = dh;
    const rx = r.getContext('2d', { willReadFrequently: true }); rx.imageSmoothingEnabled = true; rx.imageSmoothingQuality = 'high';
    rx.drawImage(c, 0, 0, dw, dh);
    return window.T.lapEnergy(rx, { x: 1, y: 1, w: dw - 2, h: dh - 2 });
  },
  async upscaleEnergy(b64, rect, factor) {
    // control: take a crop (rendered at 1x) and bilinear-upscale it by `factor`, measure energy of the result
    const { c } = await window.T.loadPng(b64);
    const r = document.createElement('canvas'); r.width = rect.w * factor; r.height = rect.h * factor;
    const rx = r.getContext('2d', { willReadFrequently: true }); rx.imageSmoothingEnabled = true; rx.imageSmoothingQuality = 'high';
    rx.drawImage(c, rect.x, rect.y, rect.w, rect.h, 0, 0, r.width, r.height);
    return window.T.lapEnergy(rx, { x: 1, y: 1, w: r.width - 2, h: r.height - 2 });
  },
};
await frames(2);
window.__ready = true;
