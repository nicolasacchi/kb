// Shared helpers for the T1 twin spike: terminal factory, raw-buffer access, comparison.
import { createRequire } from 'node:module';
const require = createRequire(import.meta.url);
const { Terminal } = require('@xterm/headless');
const { SerializeAddon } = require('@xterm/addon-serialize');
const { Unicode11Addon } = require('@xterm/addon-unicode11');
const { UnicodeGraphemesAddon } = require('@xterm/addon-unicode-graphemes');

export const VARIANTS = ['unicode11', 'graphemes'];
export const OPTS = { scrollback: 50000, scrollOnEraseInDisplay: true, allowProposedApi: true };

export function makeTerm(cols, rows, variant) {
  const t = new Terminal({ cols, rows, ...OPTS });
  let ser = null;
  if (variant === 'unicode11') {
    t.loadAddon(new Unicode11Addon());
    t.unicode.activeVersion = '11';
  } else if (variant === 'graphemes') {
    t.loadAddon(new UnicodeGraphemesAddon());
    const v = [...(t.unicode.versions ?? [])].find((k) => /grapheme/i.test(String(k)));
    if (!v) throw new Error('graphemes addon registered no version; versions=' + JSON.stringify(t.unicode.versions));
    t.unicode.activeVersion = v;
  } else if (variant !== 'default') throw new Error('bad variant ' + variant);
  ser = new SerializeAddon();
  t.loadAddon(ser);
  return { t, ser };
}

export function write(t, data) {
  return new Promise((res) => t.write(data, res));
}
export async function writeChunked(t, bytes, from, to, chunk = 1 << 16) {
  for (let i = from; i < to; i += chunk) await write(t, bytes.subarray(i, Math.min(to, i + chunk)));
}

// ---- raw access (xterm internals; all guarded) ----
const core = (t) => t._core;
const bufs = (t) => core(t)._bufferService.buffers;
function safe(f, d) { try { const v = f(); return v === undefined ? d : v; } catch { return d; } }

function attrSig(a) {
  if (!a) return null;
  return safe(() => ({ fg: a.fg >>> 0, bg: a.bg >>> 0, ul: a.extended ? [a.extended.underlineStyle, a.extended.underlineColor, a.extended._urlId ?? a.extended.urlId] : null }), String(a));
}

export function stateOf(t) {
  const c = core(t), B = bufs(t), s = {};
  const cols = t.cols;
  s.__cols = t.cols;
  s.activeBuffer = t.buffer.active.type;
  for (const [name, b] of [['normal', B.normal], ['alt', B.alt]]) {
    s[`${name}.x`] = b.x; s[`${name}.y`] = b.y;
    s[`${name}.pendingWrap`] = b.x >= cols;
    s[`${name}.ybase`] = b.ybase; s[`${name}.length`] = b.lines.length;
    s[`${name}.scrollTop`] = b.scrollTop; s[`${name}.scrollBottom`] = b.scrollBottom;
    s[`${name}.savedX`] = b.savedX; s[`${name}.savedY`] = b.savedY;
    s[`${name}.savedAttr`] = attrSig(b.savedCurAttrData);
    s[`${name}.savedCharset`] = safe(() => JSON.stringify(b.savedCharset ?? null), null);
    s[`${name}.savedGlevel`] = b.savedGlevel;
    s[`${name}.tabs`] = safe(() => Object.keys(b.tabs).join(','), null);
  }
  for (const [k, v] of Object.entries(t.modes)) s[`modes.${k}`] = v;
  s['charset.glevel'] = safe(() => c._charsetService.glevel, null);
  s['charset.sets'] = safe(() => JSON.stringify(c._charsetService.charsets.map((x) => (x ? Object.keys(x).length : 0))), null);
  s['charset.active'] = safe(() => JSON.stringify(c._charsetService.charset ? Object.keys(c._charsetService.charset).length : 0), null);
  s['cursorStyle'] = safe(() => c.optionsService.rawOptions.cursorStyle, null);
  s['cursorBlink'] = safe(() => c.optionsService.rawOptions.cursorBlink, null);
  s['decCursorStyle'] = safe(() => c.coreService.decPrivateModes.cursorStyle, null);
  s['decCursorBlink'] = safe(() => c.coreService.decPrivateModes.cursorBlink, null);
  s['pen'] = attrSig(safe(() => c._inputHandler._curAttrData, null));
  s['eraseAttr'] = safe(() => { const e = c._inputHandler._eraseAttrData(); return attrSig(e); }, null);
  s['cursorHidden'] = safe(() => c.coreService.isCursorHidden, null);
  s['title'] = safe(() => c._inputHandler._title ?? null, null);
  return s;
}

function stateClass(k) {
  if (k.startsWith('modes.')) return 'modes';
  if (/\.saved/.test(k)) return 'DECSC saved cursor';
  if (/^charset|savedCharset|savedGlevel/.test(k)) return 'charsets';
  if (/cursorStyle|cursorBlink|decCursor/i.test(k)) return 'cursor style';
  if (/pendingWrap/.test(k)) return 'pending wrap';
  if (/scrollTop|scrollBottom/.test(k)) return 'scroll region';
  if (k === 'activeBuffer') return 'active buffer';
  if (/^pen|eraseAttr/.test(k)) return 'pen (current SGR)';
  if (/\.tabs/.test(k)) return 'tab stops';
  if (/\.(ybase|length)$/.test(k)) return 'scrollback geometry';
  if (/\.[xy]$/.test(k)) return 'cursor position';
  if (k === 'cursorHidden') return 'cursor visibility';
  return 'other state';
}

export function diffState(a, b) {
  const out = []; // {key, cls, a, b}
  const keys = new Set([...Object.keys(a), ...Object.keys(b)]);
  for (const k of keys) {
    const x = JSON.stringify(a[k]), y = JSON.stringify(b[k]);
    if (x === y) continue;
    let cls = stateClass(k);
    // a cursor x that differs because one side sits in the pending-wrap column is a pending-wrap loss
    if (/\.x$/.test(k) && (a[k] >= a.__cols || b[k] >= a.__cols)) cls = 'pending wrap';
    out.push({ key: k, cls, a: a[k], b: b[k] });
  }
  return out;
}

// ---- cell comparison on raw line data ----
const CM = 0x3000000, COLOR = 0x03FFFFFF;
function rawLine(buf, i) {
  const l = buf.lines.get(i);
  return l && l._data ? l : null;
}
function cellOf(line, x) {
  const w0 = line._data[x * 3], w1 = line._data[x * 3 + 1] >>> 0, w2 = line._data[x * 3 + 2] >>> 0;
  let code = w0 & 0x1FFFFF, width = w0 >>> 22;
  let ch;
  if (w0 & 0x200000) ch = line._combined[x] ?? '';
  else ch = code === 0 ? '' : String.fromCodePoint(code);
  let ext = null;
  if (w2 & 0x10000000) {
    const e = line._extendedAttrs[x];
    if (e) ext = { ul: e.underlineStyle, ulc: e.underlineColor, url: e.urlId ?? e._urlId, raw: e._ext };
  }
  return { ch, width, fg: w1 & COLOR, bg: w2 & COLOR, fgA: w1 & 0xFC000000, bgA: (w2 & 0xFC000000) & ~0x10000000, ext };
}
function linkOf(t, id) {
  if (!id) return null;
  return safe(() => { const d = core(t)._oscLinkService.getLinkData(id); return d ? `${d.id ?? ''}|${d.uri}` : null; }, null);
}
const nz = (c) => (c.ch === '' && c.width === 1 ? ' ' : c.ch);

function cellDiff(ca, cb, ta, tb, wideNear) {
  const reasons = [];
  const A = nz(ca), B = nz(cb);
  if (A !== B || ca.width !== cb.width) {
    if (A === ' ' && cb.ch === ' ' || (ca.ch === '' && cb.ch === ' ' && ca.width === cb.width)) { /* benign */ }
    else reasons.push(wideNear ? 'wide-char boundary' : (ca.width !== cb.width ? 'cell width' : 'cell chars'));
  }
  if (ca.fg !== cb.fg) reasons.push('fg colour');
  if (ca.bg !== cb.bg) reasons.push('bg colour');
  if (ca.fgA !== cb.fgA || ca.bgA !== cb.bgA) reasons.push('attributes');
  const ea = ca.ext, eb = cb.ext;
  const ula = ea ? ea.ul : 0, ulb = eb ? eb.ul : 0;
  const uca = ea ? ea.ulc : 0, ucb = eb ? eb.ulc : 0;
  if ((ula ?? 0) !== (ulb ?? 0)) reasons.push('underline style');
  // underline colour only matters where an underline is drawn (style non-zero or UNDERLINE flag); report always when differing
  if ((uca ?? 0) !== (ucb ?? 0)) reasons.push('SGR 58 underline colour');
  const la = linkOf(ta, ea?.url), lb = linkOf(tb, eb?.url);
  if (la !== lb) reasons.push('OSC 8 hyperlink');
  return reasons;
}

/** Compare two raw buffers, bottom-aligned. Returns {lenA,lenB,rowsCompared,rowsDiff,cells:{cls:n},examples:[...]} */
export function compareBuffers(ta, tb, which, maxExamples = 3) {
  const A = bufs(ta)[which], B = bufs(tb)[which];
  const res = { lenA: A.lines.length, lenB: B.lines.length, rowsCompared: 0, rowsDiff: 0, wrapFlagDiff: 0, cells: {}, examples: {} };
  const n = Math.min(res.lenA, res.lenB), cols = ta.cols;
  for (let k = 0; k < n; k++) {
    const la = rawLine(A, res.lenA - 1 - k), lb = rawLine(B, res.lenB - 1 - k);
    res.rowsCompared++;
    if (!la || !lb) { res.cells['unreadable line'] = (res.cells['unreadable line'] ?? 0) + 1; continue; }
    let same = la.isWrapped === lb.isWrapped;
    if (!same) res.wrapFlagDiff++;
    // fast path: identical raw words and combined/ext maps
    const da = la._data, db = lb._data;
    let fast = true;
    for (let i = 0; i < cols * 3; i++) if (da[i] !== db[i]) { fast = false; break; }
    if (fast) {
      for (const key in la._combined) if (la._combined[key] !== lb._combined[key]) { fast = false; break; }
      if (fast) for (const key in la._extendedAttrs) {
        const x = la._extendedAttrs[key], y = lb._extendedAttrs[key];
        if (!x || !y || x.underlineStyle !== y.underlineStyle || x.underlineColor !== y.underlineColor || linkOf(ta, x.urlId ?? x._urlId) !== linkOf(tb, y.urlId ?? y._urlId)) { fast = false; break; }
      }
    }
    if (fast) { if (!same) { /* wrap flag only */ } continue; }
    let rowBad = false;
    for (let x = 0; x < cols; x++) {
      const ca = cellOf(la, x), cb = cellOf(lb, x);
      const wideNear = [x - 1, x, x + 1].some((j) => {
        if (j < 0 || j >= cols) return false;
        const wa = la._data[j * 3] >>> 22, wb = lb._data[j * 3] >>> 22;
        return wa !== 1 || wb !== 1;
      });
      const r = cellDiff(ca, cb, ta, tb, wideNear);
      for (const cls of r) {
        rowBad = true;
        res.cells[cls] = (res.cells[cls] ?? 0) + 1;
        const ex = (res.examples[cls] ??= []);
        if (ex.length < maxExamples) ex.push({ rowFromBottom: k, col: x, a: ca, b: cb });
      }
    }
    if (rowBad) res.rowsDiff++;
  }
  if (res.lenA !== res.lenB) res.cells['scrollback length'] = Math.abs(res.lenA - res.lenB);
  if (res.wrapFlagDiff) res.cells['wrapped-line flag'] = res.wrapFlagDiff;
  return res;
}

export function classifyBufferDiff(cmp, which, altActive) {
  // Map cell classes; alt buffer diffs are reported as alt-buffer content (stale-inactive vs active split)
  const out = {};
  for (const [c, n] of Object.entries(cmp.cells)) {
    const key = which === 'alt' ? (altActive ? 'alt-buffer content (active)' : 'alt-buffer content (inactive, stale)') + ': ' + c : c;
    out[key] = n;
  }
  return out;
}
