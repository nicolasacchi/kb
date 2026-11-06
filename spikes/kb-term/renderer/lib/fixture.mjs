import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { expectedAt } from './stream.mjs';

const here = path.dirname(fileURLToPath(import.meta.url));
export const OUT = path.join(here, '..', 'out');
export const SHOTS = path.join(OUT, 'screenshots');
fs.mkdirSync(SHOTS, { recursive: true });
const RESULTS = path.join(OUT, 'results.jsonl');

export function record(section, key, value) {
  fs.appendFileSync(RESULTS, JSON.stringify({ section, key, value }) + '\n');
}

export const VIEW = { width: 900, height: 620 };
export const BASE = 'http://127.0.0.1:4173/';

export async function openPage(browser, o = {}) {
  const dpr = o.dpr || 1;
  const ctx = await browser.newContext({ viewport: VIEW, deviceScaleFactor: dpr });
  const page = await ctx.newPage();
  const errors = [];
  page.on('pageerror', (e) => errors.push('pageerror: ' + e.message));
  page.on('console', (m) => { if (m.type() === 'error' || m.type() === 'warning') errors.push(m.type() + ': ' + m.text()); });
  const qs = new URLSearchParams({
    renderer: o.renderer || 'dom', unicode: o.unicode || '11', cols: String(o.cols || 100), rows: String(o.rows || 30),
    scrollback: String(o.scrollback ?? 5000), halo: o.halo === false ? '0' : '1', iip: o.iip ? '1' : '0', iipmb: String(o.iipmb || 128), gfx: o.gfx === false ? '0' : '1',
  });
  await page.goto(BASE + '?' + qs);
  await page.waitForFunction(() => window.__ready === true, null, { timeout: 30000 });
  const m = await page.evaluate(() => window.T.metrics());
  // device pixel size of one cell (xterm rounds it to whole device pixels)
  const devCW = m.device ? m.device.cw : Math.round(m.cw * dpr);
  const devCH = m.device ? m.device.ch : Math.round(m.ch * dpr);
  return { ctx, page, m, dpr, devCW, devCH, renderer: o.renderer || 'dom', errors, close: () => ctx.close() };
}

export const feed = (page, buf) => page.evaluate((b) => window.T.feed(b), buf.toString('base64'));
export const feedChunked = (page, buf) => page.evaluate((b) => window.T.feedChunked(b), buf.toString('base64'));

export async function shot(page, name, { save = true } = {}) {
  const buf = await page.screenshot({ type: 'png' });
  if (save && name) fs.writeFileSync(path.join(SHOTS, name + '.png'), buf);
  return buf;
}

export const probe = (page, png, pts) => page.evaluate(([b, p]) => window.T.probe(b, p), [png.toString('base64'), pts]);

const near = (a, b, tol) => a && b && Math.abs(a[0] - b[0]) <= tol && Math.abs(a[1] - b[1]) <= tol && Math.abs(a[2] - b[2]) <= tol;
export const isBg = (c) => c && c[0] < 24 && c[1] < 24 && c[2] < 24;

// Oracle: given scenario truth (abs line0/col0/c/r/variant), the viewport top line and metrics, compute where each
// picture must be on screen and probe the screenshot. Independent of the renderer's own placeholder decoding.
export async function checkPictures(page, png, o, truths, vy, { tol = 10, label = '' } = {}) {
  const { m, dpr } = o;
  const cw = m.cw, ch = m.ch; // css
  const pts = []; const meta = [];
  const edgeDev = o.renderer === 'dom' ? 2 : 1; // DOM renderer cells are fractional in device px (8.43 css wide): allow 2 device px
  const D = edgeDev / dpr;
  for (const t of truths) {
    const top = t.line0 - vy, bottom = t.line0 + t.r - vy; // rows (float) relative to viewport
    const vTop = Math.max(0, top), vBot = Math.min(m.rows, bottom);
    if (vBot <= vTop) continue;
    const L = t.col0, R = t.col0 + t.c;
    // visible rect in css px
    const x0 = m.screen.x + L * cw, x1 = m.screen.x + R * cw, y0 = m.screen.y + vTop * ch, y1 = m.screen.y + vBot * ch;
    const addIn = (px, py, kind) => {
      const u = (px - (m.screen.x + L * cw)) / (t.c * cw), v = (py - (m.screen.y + top * ch)) / (t.r * ch);
      const exp = expectedAt(u, v, t.variant);
      if (!exp) return;
      pts.push({ x: px * dpr, y: py * dpr }); meta.push({ kind, exp, id: t.id });
    };
    // corners, one device pixel inside (exact placement test) and a 4x4 grid inside
    addIn(x0 + D, y0 + D, 'corner-tl'); addIn(x1 - 2 * D, y0 + D, 'corner-tr'); addIn(x0 + D, y1 - 2 * D, 'corner-bl'); addIn(x1 - 2 * D, y1 - 2 * D, 'corner-br');
    for (let i = 1; i <= 5; i++) for (let j = 1; j <= 5; j++) addIn(x0 + (x1 - x0) * i / 6, y0 + (y1 - y0) * j / 6, 'grid');
    // one device pixel just OUTSIDE the left/right edges (must be terminal background) at mid height
    const my = (y0 + y1) / 2;
    pts.push({ x: (x0 - 2 * D) * dpr, y: my * dpr }); meta.push({ kind: 'outside-left', exp: null, id: t.id });
    pts.push({ x: (x1 + D) * dpr, y: my * dpr }); meta.push({ kind: 'outside-right', exp: null, id: t.id });
    if (top >= 0) { pts.push({ x: ((x0 + x1) / 2) * dpr, y: (y0 - 2 * D) * dpr }); meta.push({ kind: 'outside-above', exp: null, id: t.id }); }
    if (bottom <= m.rows) { pts.push({ x: ((x0 + x1) / 2) * dpr, y: (y1 + D) * dpr }); meta.push({ kind: 'outside-below', exp: null, id: t.id }); }
  }
  const got = pts.length ? await probe(page, png, pts) : [];
  let total = 0, ok = 0; const bad = [];
  got.forEach((c, i) => {
    const md = meta[i]; total++;
    const good = md.exp ? near(c, md.exp, tol) : isBg(c);
    if (good) ok++; else bad.push({ kind: md.kind, id: md.id, got: c, exp: md.exp, at: pts[i] });
  });
  return { label, total, ok, bad: bad.slice(0, 8), badCount: total - ok, byKind: summarize(meta, got, tol) };
}
function summarize(meta, got, tol) {
  const s = {};
  meta.forEach((md, i) => {
    const k = md.kind.startsWith('outside') ? 'outside' : md.kind.startsWith('corner') ? 'corner' : 'grid';
    s[k] ??= { n: 0, ok: 0 }; s[k].n++;
    if (md.exp ? near(got[i], md.exp, tol) : isBg(got[i])) s[k].ok++;
  });
  return s;
}

export const CONFIGS = [];
for (const renderer of ['dom', 'webgl']) for (const dpr of [1, 2, 3]) CONFIGS.push({ renderer, dpr });

export function guardWebgl(test, o, cfg) {
  record('env', `${cfg.renderer}-dpr${cfg.dpr}`, { webgl: o.m.webgl, webglError: o.m.webglError, contextLost: o.m.contextLost, canvases: o.m.canvases.length, rowsDisplay: o.m.rowsDisplay, domRowSpans: o.m.domRowSpans, cell: { cssW: o.m.cw, cssH: o.m.ch, devW: o.devCW, devH: o.devCH }, screen: o.m.screen });
  if (cfg.renderer === 'webgl' && o.m.webgl !== 'loaded') test.skip(true, 'webgl unavailable: ' + o.m.webglError);
}
export const tag = (cfg) => `${cfg.renderer}-dpr${cfg.dpr}`;
