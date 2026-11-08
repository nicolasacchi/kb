// Cell/state comparison helpers for tap tests. Ported from
// spikes/kb-term/twin/lib.mjs (stateOf/diffState/cell compare, converted to
// TS). This is the ONLY place besides the clearly named internals section of
// src/twin.ts allowed to touch xterm internals (_core/_extendedAttrs/
// _oscLinkService).
//
// A = expected, B = got throughout. xterm has two encodings of the default
// underline colour (0 and the all-ones sentinel after SGR 59); the port
// treats them as equal.

type TermLike = {
  cols: number;
  buffer: { active: { type: string } };
  modes: Record<string, unknown>;
  _core: Record<string, unknown>;
};

const core = (t: TermLike): Record<string, unknown> => t._core;
const bufs = (t: TermLike): Record<string, BufferLike> =>
  ((core(t)._bufferService as { buffers: Record<string, BufferLike> }).buffers) as Record<string, BufferLike>;

interface BufferLike {
  x: number;
  y: number;
  ybase: number;
  lines: { length: number };
  scrollTop: number;
  scrollBottom: number;
  savedX: number;
  savedY: number;
  savedCurAttrData: AttrLike | null;
  savedCharset: unknown;
  savedGlevel: number;
  tabs: Record<string, unknown>;
}

interface AttrLike {
  fg: number;
  bg: number;
  extended: boolean | null;
  extendedAttrs?: { underlineStyle: number; underlineColor: number; urlId?: number; _urlId?: number };
}

type StateMap = Record<string, unknown>;

function safe<T>(f: () => T, d: T): T {
  try {
    const v = f();
    return v === undefined ? d : v;
  } catch {
    return d;
  }
}

const ulc = (v: number | undefined | null): number => ((v ?? 0) === 0x3ffffff ? 0 : (v ?? 0));

function attrSig(a: AttrLike | null): unknown {
  if (!a) return null;
  return safe(
    () => ({
      fg: a.fg >>> 0,
      bg: a.bg >>> 0,
      ul: a.extended
        ? [
            a.extendedAttrs?.underlineStyle ?? 0,
            ulc(a.extendedAttrs?.underlineColor),
            a.extendedAttrs?._urlId ?? a.extendedAttrs?.urlId ?? null,
          ]
        : null,
    }),
    String(a),
  );
}

export function stateOf(t: TermLike): StateMap {
  const c = core(t);
  const B = bufs(t);
  const s: StateMap = {};
  const cols = t.cols;
  s.__cols = t.cols;
  s.activeBuffer = t.buffer.active.type;
  for (const [name, b] of [['normal', B.normal], ['alt', B.alt]] as const) {
    s[`${name}.x`] = b.x;
    s[`${name}.y`] = b.y;
    s[`${name}.pendingWrap`] = b.x >= cols;
    s[`${name}.ybase`] = b.ybase;
    s[`${name}.length`] = b.lines.length;
    s[`${name}.scrollTop`] = b.scrollTop;
    s[`${name}.scrollBottom`] = b.scrollBottom;
    s[`${name}.savedX`] = b.savedX;
    s[`${name}.savedY`] = b.savedY;
    s[`${name}.savedAttr`] = attrSig(b.savedCurAttrData);
    s[`${name}.savedCharset`] = safe(() => JSON.stringify(b.savedCharset ?? null), null);
    s[`${name}.savedGlevel`] = b.savedGlevel;
    s[`${name}.tabs`] = safe(() => Object.keys(b.tabs).join(','), null);
  }
  for (const [k, v] of Object.entries(t.modes)) s[`modes.${k}`] = v;
  s['charset.glevel'] = safe(() => (c._charsetService as { glevel: unknown }).glevel, null);
  s['charset.sets'] = safe(
    () => JSON.stringify((c._charsetService as { charsets: (Record<string, unknown> | null)[] }).charsets.map((x) => (x ? Object.keys(x).length : 0))),
    null,
  );
  s['charset.active'] = safe(
    () => JSON.stringify((c._charsetService as { charset: Record<string, unknown> | null }).charset ? Object.keys((c._charsetService as { charset: Record<string, unknown> }).charset).length : 0),
    null,
  );
  s['cursorStyle'] = safe(() => (c.optionsService as { rawOptions: { cursorStyle: unknown } }).rawOptions.cursorStyle, null);
  s['cursorBlink'] = safe(() => (c.optionsService as { rawOptions: { cursorBlink: unknown } }).rawOptions.cursorBlink, null);
  s['decCursorStyle'] = safe(() => (c.coreService as { decPrivateModes: { cursorStyle: unknown } }).decPrivateModes.cursorStyle, null);
  s['decCursorBlink'] = safe(() => (c.coreService as { decPrivateModes: { cursorBlink: unknown } }).decPrivateModes.cursorBlink, null);
  s['pen'] = attrSig(safe(() => (c._inputHandler as { _curAttrData: AttrLike })._curAttrData, null));
  s['eraseAttr'] = safe(() => attrSig((c._inputHandler as { _eraseAttrData(): AttrLike })._eraseAttrData()), null);
  s['cursorHidden'] = safe(() => (c.coreService as { isCursorHidden: unknown }).isCursorHidden, null);
  s['title'] = safe(() => (c._inputHandler as { _title?: unknown })._title ?? null, null);
  return s;
}

function stateClass(k: string): string {
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

export interface StateDiff {
  key: string;
  cls: string;
  a: unknown;
  b: unknown;
}

export function diffState(a: StateMap, b: StateMap): StateDiff[] {
  const out: StateDiff[] = [];
  const keys = new Set([...Object.keys(a), ...Object.keys(b)]);
  for (const k of keys) {
    const x = JSON.stringify(a[k]);
    const y = JSON.stringify(b[k]);
    if (x === y) continue;
    let cls = stateClass(k);
    if (/\.x$/.test(k) && (Number(a[k]) >= Number(a.__cols) || Number(b[k]) >= Number(a.__cols))) cls = 'pending wrap';
    out.push({ key: k, cls, a: a[k], b: b[k] });
  }
  return out;
}

// ---- cell comparison on raw line data ----

const COLOR = 0x03ffffff;

interface RawLine {
  _data: Uint32Array;
  _combined: Record<string, string>;
  _extendedAttrs: Record<string, ExtAttr>;
  isWrapped: boolean;
}

interface ExtAttr {
  underlineStyle: number;
  underlineColor: number;
  urlId?: number;
  _urlId?: number;
  _ext?: number;
}

interface RawBuffer {
  lines: { length: number; get(i: number): RawLine | undefined };
}

interface Cell {
  ch: string;
  width: number;
  fg: number;
  bg: number;
  fgA: number;
  bgA: number;
  ext: { ul: number; ulc: number; url: number | null; raw: number } | null;
}

function rawLine(buf: RawBuffer, i: number): RawLine | null {
  const l = buf.lines.get(i) as RawLine & { _data?: Uint32Array };
  return l && l._data ? (l as RawLine) : null;
}

function cellOf(line: RawLine, x: number): Cell {
  const w0 = line._data[x * 3];
  const w1 = line._data[x * 3 + 1] >>> 0;
  const w2 = line._data[x * 3 + 2] >>> 0;
  const code = w0 & 0x1fffff;
  const width = w0 >>> 22;
  let ch: string;
  if (w0 & 0x200000) ch = line._combined[x] ?? '';
  else ch = code === 0 ? '' : String.fromCodePoint(code);
  let ext: Cell['ext'] = null;
  if (w2 & 0x10000000) {
    const e = line._extendedAttrs[x];
    if (e) ext = { ul: e.underlineStyle, ulc: ulc(e.underlineColor), url: e.urlId ?? e._urlId ?? null, raw: e._ext ?? 0 };
  }
  return { ch, width, fg: w1 & COLOR, bg: w2 & COLOR, fgA: w1 & 0xfc000000, bgA: (w2 & 0xfc000000) & ~0x10000000, ext };
}

function linkOf(t: TermLike, id: number | null | undefined): string | null {
  if (!id) return null;
  return safe(() => {
    const d = (core(t)._oscLinkService as { getLinkData(id: number): { id?: string; uri: string } | undefined }).getLinkData(id);
    return d ? `${d.id ?? ''}|${d.uri}` : null;
  }, null);
}

const nz = (c: Cell): string => (c.ch === '' && c.width === 1 ? ' ' : c.ch);

function cellDiff(ca: Cell, cb: Cell, ta: TermLike, tb: TermLike, wideNear: boolean): string[] {
  const reasons: string[] = [];
  const A = nz(ca);
  const B = nz(cb);
  if (A !== B || ca.width !== cb.width) {
    if (!(A === ' ' && cb.ch === ' ') && !(ca.ch === '' && cb.ch === ' ' && ca.width === cb.width)) {
      reasons.push(wideNear ? 'wide-char boundary' : ca.width !== cb.width ? 'cell width' : 'cell chars');
    }
  }
  if (ca.fg !== cb.fg) reasons.push('fg colour');
  if (ca.bg !== cb.bg) reasons.push('bg colour');
  const ea = ca.ext;
  const eb = cb.ext;
  const la0 = linkOf(ta, ea?.url);
  const lb0 = linkOf(tb, eb?.url);
  const linked = !!(la0 || lb0);
  const UL = 0x10000000;
  const fgAa = linked ? ca.fgA & ~UL : ca.fgA;
  const fgAb = linked ? cb.fgA & ~UL : cb.fgA;
  if (fgAa !== fgAb || ca.bgA !== cb.bgA) reasons.push('attributes');
  const ula = linked ? 0 : (ea ? ea.ul : 0);
  const ulb = linked ? 0 : (eb ? eb.ul : 0);
  const uca = ea ? ea.ulc : 0;
  const ucb = eb ? eb.ulc : 0;
  if ((ula ?? 0) !== (ulb ?? 0)) reasons.push('underline style');
  if ((uca ?? 0) !== (ucb ?? 0)) reasons.push('SGR 58 underline colour');
  if (la0 !== lb0) reasons.push('OSC 8 hyperlink');
  return reasons;
}

interface RowDiff {
  x: number;
  reason: string;
  a: Cell;
  b: Cell;
}

function rowReasons(ta: TermLike, tb: TermLike, la: RawLine, lb: RawLine, cols: number): RowDiff[] {
  const da = la._data;
  const db = lb._data;
  let fast = true;
  for (let i = 0; i < cols * 3; i++) {
    if (da[i] !== db[i]) {
      fast = false;
      break;
    }
  }
  if (fast) {
    for (const key in la._combined) {
      if (la._combined[key] !== lb._combined[key]) {
        fast = false;
        break;
      }
    }
    if (fast) {
      for (const key in la._extendedAttrs) {
        const x = la._extendedAttrs[key];
        const y = lb._extendedAttrs[key];
        if (!x || !y || x.underlineStyle !== y.underlineStyle || ulc(x.underlineColor) !== ulc(y.underlineColor) || linkOf(ta, x.urlId ?? x._urlId) !== linkOf(tb, y.urlId ?? y._urlId)) {
          fast = false;
          break;
        }
      }
    }
  }
  if (fast) return [];
  const out: RowDiff[] = [];
  for (let x = 0; x < cols; x++) {
    const ca = cellOf(la, x);
    const cb = cellOf(lb, x);
    const wideNear = [x - 1, x, x + 1].some((j) => {
      if (j < 0 || j >= cols) return false;
      return (la._data[j * 3] >>> 22) !== 1 || (lb._data[j * 3] >>> 22) !== 1;
    });
    for (const r of cellDiff(ca, cb, ta, tb, wideNear)) out.push({ x, reason: r, a: ca, b: cb });
  }
  return out;
}

export interface BufferCompare {
  lenA: number;
  lenB: number;
  rowsCompared: number;
  rowsDiff: number;
  cells: Record<string, number>;
  examples: Record<string, { rowFromBottom: number; col: number; a: Cell; b: Cell }[]>;
}

interface RawBufFull {
  lines: { length: number; get(i: number): RawLine | undefined };
}

/** Compare two raw buffers from the bottom with one-row lookahead resync. A = ta (expected), B = tb (got). */
export function compareBuffers(ta: TermLike, tb: TermLike, which: 'normal' | 'alt', maxExamples = 3): BufferCompare {
  const A = bufs(ta)[which] as unknown as RawBufFull;
  const B = bufs(tb)[which] as unknown as RawBufFull;
  const res: BufferCompare = { lenA: A.lines.length, lenB: B.lines.length, rowsCompared: 0, rowsDiff: 0, cells: {}, examples: {} };
  const cols = ta.cols;
  const bump = (cls: string, n = 1): void => {
    res.cells[cls] = (res.cells[cls] ?? 0) + n;
  };
  let i = res.lenA - 1;
  let j = res.lenB - 1;
  let extraA = 0;
  let extraB = 0;
  let wrapDiff = 0;
  while (i >= 0 && j >= 0) {
    const la = rawLine(A, i);
    const lb = rawLine(B, j);
    if (!la || !lb) {
      bump('unreadable line');
      i--;
      j--;
      continue;
    }
    res.rowsCompared++;
    const r = rowReasons(ta, tb, la, lb, cols);
    if (!r.length) {
      if (la.isWrapped !== lb.isWrapped) wrapDiff++;
      i--;
      j--;
      continue;
    }
    let skipped = false;
    for (let k = 1; k <= 4 && !skipped; k++) {
      const a2 = i - k >= 0 ? rawLine(A, i - k) : null;
      const b2 = j - k >= 0 ? rawLine(B, j - k) : null;
      if (a2 && !rowReasons(ta, tb, a2, lb, cols).length) {
        extraA += k;
        i -= k;
        skipped = true;
      } else if (b2 && !rowReasons(ta, tb, la, b2, cols).length) {
        extraB += k;
        j -= k;
        skipped = true;
      }
    }
    if (skipped) continue;
    res.rowsDiff++;
    for (const { x, reason, a, b } of r) {
      bump(reason);
      const ex = (res.examples[reason] ??= []);
      if (ex.length < maxExamples) ex.push({ rowFromBottom: res.lenA - 1 - i, col: x, a, b });
    }
    i--;
    j--;
  }
  if (extraA) bump('rows only in expected (lost by seed/viewer)', extraA);
  if (extraB) bump('rows only in viewer (extra)', extraB);
  if (i >= 0 && j < 0) bump('rows only in expected (lost by seed/viewer)', i + 1);
  if (j >= 0 && i < 0) bump('rows only in viewer (extra)', j + 1);
  if (wrapDiff) bump('wrapped-line flag', wrapDiff);
  return res;
}

/** Text of the last n rows of a buffer (top-down order), for debug dumps. */
export function dumpRows(t: TermLike, which: 'normal' | 'alt', n: number): string[] {
  const B = bufs(t)[which] as unknown as RawBufFull;
  const out: string[] = [];
  for (let i = Math.max(0, B.lines.length - n); i < B.lines.length; i++) {
    const l = rawLine(B, i);
    let str = '';
    if (l) {
      for (let x = 0; x < t.cols; x++) {
        const c = cellOf(l, x);
        str += c.ch === '' ? (c.width === 0 ? '' : ' ') : c.ch;
      }
    }
    out.push(str);
  }
  return out;
}

/** A twin's active buffer view as expected by the compare helpers. */
export function termOf(twin: { term: TermLike }): TermLike {
  return twin.term;
}
