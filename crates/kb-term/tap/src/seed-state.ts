// kb-term tap: capture a twin's full parser state for seed building (T6).
//
// captureTwinState returns a plain serialisable TwinState (copies only, no
// live references). It FAILS CLOSED: every internal path read is asserted,
// and any missing path throws Error('seed: xterm internals changed: <path>').
// The matrix uses the spike's safe(..., default) diagnostics instead; the
// seed builder never returns defaults. INTERNALS_PATHS lists every static
// path this module reads; test/matrix/pins.test.ts asserts each one
// exists on a fresh terminal, so an xterm upgrade that renames an internal
// breaks the pinned build loudly instead of silently producing wrong
// seeds. The wrap-cell line fields (_data, _combined, _extendedAttrs) are
// instance fields read via lines.get(i) (readWrapCell, which throws
// fail-closed when they go missing) rather than static paths: the pin
// test asserts them on a content line instead.
//
// Charset note: designation characters are not retained by xterm's
// CharsetService (only the built character maps), so designations cannot
// be re-emitted from captured state. glevel is captured for state
// comparison only; not re-emitted (xterm 6.0.0 buffers carry no
// savedGlevel; the synthetic streams never set a nonzero glevel). The
// matrix is the arbiter. buildSeed fails closed on a non-default charset
// or nonzero glevel instead of silently dropping them.
import type { Twin } from './twin.ts';
import type { AttrCopy } from './sgr.ts';

export interface BufferState {
  x: number;
  y: number;
  ybase: number;
  scrollTop: number;
  scrollBottom: number;
  savedX: number;
  savedY: number;
  savedAttr: AttrCopy | null;
  savedCharset: string | null;
  pendingWrap: boolean;
  tabs: number[];
  length: number;
  /** Cell under a pending wrap, for the rewrite that restores wrapping. */
  wrap?: { char: string; attr: AttrCopy | null; width: number };
}

export interface TwinState {
  cols: number;
  rows: number;
  active: 'normal' | 'alternate';
  normal: BufferState;
  alt: BufferState;
  pen: AttrCopy;
  modes: Record<string, boolean | string | number>;
  cursorStyle: string | undefined;
  cursorBlink: boolean | undefined;
  isCursorHidden: boolean;
  glevel: number;
  charsetSets: number[];
  charsetActiveLen: number | null;
  charsetJson: string | null;
}

/** Every internal path captureTwinState reads (pinned by pins.test.ts). */
export const INTERNALS_PATHS: readonly string[] = [
  '_core',
  '_core._bufferService',
  '_core._bufferService.buffers',
  '_core._bufferService.buffers.normal',
  '_core._bufferService.buffers.alt',
  '_core._bufferService.buffers.normal.x',
  '_core._bufferService.buffers.normal.y',
  '_core._bufferService.buffers.normal.ybase',
  '_core._bufferService.buffers.normal.scrollTop',
  '_core._bufferService.buffers.normal.scrollBottom',
  '_core._bufferService.buffers.normal.savedX',
  '_core._bufferService.buffers.normal.savedY',
  '_core._bufferService.buffers.normal.savedCurAttrData',
  '_core._bufferService.buffers.normal.savedCharset',
  '_core._bufferService.buffers.normal.tabs',
  '_core._bufferService.buffers.normal.lines',
  '_core._bufferService.buffers.normal.lines.get',
  '_core._bufferService.buffers.alt.x',
  '_core._bufferService.buffers.alt.y',
  '_core._bufferService.buffers.alt.ybase',
  '_core._bufferService.buffers.alt.scrollTop',
  '_core._bufferService.buffers.alt.scrollBottom',
  '_core._bufferService.buffers.alt.savedX',
  '_core._bufferService.buffers.alt.savedY',
  '_core._bufferService.buffers.alt.savedCurAttrData',
  '_core._bufferService.buffers.alt.savedCharset',
  '_core._bufferService.buffers.alt.tabs',
  '_core._bufferService.buffers.alt.lines',
  '_core._bufferService.buffers.alt.lines.get',
  '_core._inputHandler',
  '_core._inputHandler._curAttrData',
  '_core.coreService',
  '_core.coreService.decPrivateModes',
  '_core.coreService.decPrivateModes.cursorStyle',
  '_core.coreService.decPrivateModes.cursorBlink',
  '_core.coreService.isCursorHidden',
  '_core._charsetService',
  '_core._charsetService.glevel',
  '_core._charsetService._charsets',
  'modes',
  'buffer',
  'buffer.active',
  'buffer.active.type',
] as const;

interface AttrDataLike {
  fg: number;
  bg: number;
  extended: { underlineStyle: number; underlineColor: number };
}

function need<T>(obj: unknown, key: string, at: string): T {
  if (obj === null || typeof obj !== 'object' || !(key in obj)) {
    throw new Error(`seed: xterm internals changed: ${at}.${key}`);
  }
  const v = (obj as Record<string, unknown>)[key] as T;
  if (v === undefined) throw new Error(`seed: xterm internals changed: ${at}.${key}`);
  return v;
}
/** need() variant for fields that are legitimately undefined until set. */
function needMaybe<T>(obj: unknown, key: string, at: string): T {
  if (obj === null || typeof obj !== 'object' || !(key in obj)) {
    throw new Error(`seed: xterm internals changed: ${at}.${key}`);
  }
  return (obj as Record<string, unknown>)[key] as T;
}

/** Plain-number fields are read through need() too (undefined = changed). */
function num(obj: unknown, key: string, at: string): number {
  const v = need<unknown>(obj, key, at);
  if (typeof v !== 'number') throw new Error(`seed: xterm internals changed: ${at}.${key}`);
  return v;
}

function attrCopy(a: AttrDataLike | null, at: string): AttrCopy | null {
  if (a === null) return null;
  const ext = need(a, 'extended', at);
  return {
    fg: num(a, 'fg', at) >>> 0,
    bg: num(a, 'bg', at) >>> 0,
    underlineStyle: num(ext, 'underlineStyle', `${at}.extended`),
    underlineColor: num(ext, 'underlineColor', `${at}.extended`) >>> 0,
  };
}

const HAS_EXTENDED = 0x10000000;

/** The cell at absolute line/column, for pending-wrap rewrites. */
function readWrapCell(buf: unknown, bufAt: string, lineIdx: number, col: number): BufferState['wrap'] {
  const lines = need(buf, 'lines', bufAt);
  // CircularList.get must be called as a method: detaching it loses `this`
  // and crashes with "undefined is not an object (evaluating this._array)".
  const line = (lines as { get(i: number): unknown }).get(lineIdx);
  if (line === null || typeof line !== 'object') return undefined;
  const data = need(line, '_data', `${bufAt}.lines`);
  if (!(data instanceof Uint32Array)) return undefined;
  const lineRec = line as Record<string, unknown>;
  if (!('_combined' in lineRec)) {
    throw new Error(`seed: xterm internals changed: ${bufAt}.lines._combined`);
  }
  if (!('_extendedAttrs' in lineRec)) {
    throw new Error(`seed: xterm internals changed: ${bufAt}.lines._extendedAttrs`);
  }
  const combined = lineRec._combined as Record<number, string> | undefined;
  const exts = lineRec._extendedAttrs as
    | Record<number, { underlineStyle: number; underlineColor: number }>
    | undefined;
  const w0 = data[col * 3] >>> 0;
  const w1 = data[col * 3 + 1] >>> 0;
  const w2 = data[col * 3 + 2] >>> 0;
  const code = w0 & 0x1fffff;
  let char: string;
  if (w0 & 0x200000) char = combined?.[col] ?? ' ';
  else char = code === 0 ? ' ' : String.fromCodePoint(code);
  const ext = w2 & HAS_EXTENDED ? exts?.[col] : undefined;
  return {
    char,
    attr: ext ? { fg: w1, bg: w2, underlineStyle: ext.underlineStyle, underlineColor: ext.underlineColor >>> 0 } : { fg: w1, bg: w2, underlineStyle: 0, underlineColor: 0 },
    width: w0 >>> 22,
  };
}

function captureBuffer(buf: unknown, at: string, cols: number): BufferState {
  const x = num(buf, 'x', at);
  const y = num(buf, 'y', at);
  const ybase = num(buf, 'ybase', at);
  const lines = need<{ length: number }>(buf, 'lines', at);
  const savedAttrData = need<AttrDataLike | null>(buf, 'savedCurAttrData', at);
  const savedCharset = needMaybe<unknown>(buf, 'savedCharset', at);
  const tabsRec = need<Record<string, unknown>>(buf, 'tabs', at);
  const st: BufferState = {
    x,
    y,
    ybase,
    scrollTop: num(buf, 'scrollTop', at),
    scrollBottom: num(buf, 'scrollBottom', at),
    savedX: num(buf, 'savedX', at),
    savedY: num(buf, 'savedY', at),
    savedAttr: attrCopy(savedAttrData, `${at}.savedCurAttrData`),
    savedCharset: savedCharset === null || savedCharset === undefined ? null : JSON.stringify(savedCharset),
    pendingWrap: x >= cols,
    tabs: Object.keys(tabsRec).map(Number).sort((p, q) => p - q),
    length: lines.length,
  };
  if (st.pendingWrap) st.wrap = readWrapCell(buf, at, ybase + y, cols - 1);
  return st;
}

export function captureTwinState(twin: Twin): TwinState {
  const term = twin.term as unknown as Record<string, unknown>;
  const cols = twin.cols;
  const rows = twin.rows;
  const core = need(term, '_core', 'twin.term');
  const bs = need(core, '_bufferService', 'twin.term._core');
  const buffers = need(bs, 'buffers', 'twin.term._core._bufferService');
  const normalBuf = need(buffers, 'normal', 'twin.term._core._bufferService.buffers');
  const altBuf = need(buffers, 'alt', 'twin.term._core._bufferService.buffers');
  const inputHandler = need(core, '_inputHandler', 'twin.term._core');
  const curAttrData = need<AttrDataLike>(inputHandler, '_curAttrData', 'twin.term._core._inputHandler');
  const coreService = need(core, 'coreService', 'twin.term._core');
  const decPrivateModes = need(coreService, 'decPrivateModes', 'twin.term._core.coreService');
  const charsetService = need(core, '_charsetService', 'twin.term._core');
  // Charset is diagnostic-only: read the charset service's records
  // directly; never fail on a missing diagnostic `charset` property.
  const charset = (charsetService as Record<string, unknown>).charset as Record<string, unknown> | undefined;
  const charsets = need<unknown[]>(charsetService, '_charsets', 'twin.term._core._charsetService');
  const buffer = need(term, 'buffer', 'twin.term');
  const active = need(buffer, 'active', 'twin.term.buffer');
  const activeType = need<string>(active, 'type', 'twin.term.buffer.active');
  const modes = need<Record<string, boolean | string | number>>(term, 'modes', 'twin.term');
  return {
    cols,
    rows,
    active: activeType === 'alternate' ? 'alternate' : 'normal',
    normal: captureBuffer(normalBuf, '._core._bufferService.buffers.normal', cols),
    alt: captureBuffer(altBuf, '._core._bufferService.buffers.alt', cols),
    pen: attrCopy(curAttrData, '._core._inputHandler._curAttrData') as AttrCopy,
    modes: { ...modes },
    cursorStyle: needMaybe<string | undefined>(decPrivateModes, 'cursorStyle', '._core.coreService.decPrivateModes'),
    cursorBlink: needMaybe<boolean | undefined>(decPrivateModes, 'cursorBlink', '._core.coreService.decPrivateModes'),
    isCursorHidden: need<boolean>(coreService, 'isCursorHidden', '._core.coreService'),
    glevel: num(charsetService, 'glevel', '._core._charsetService'),
    charsetSets: charsets.map((c) => (c && typeof c === 'object' ? Object.keys(c).length : 0)),
    charsetActiveLen: charset && typeof charset === 'object' ? Object.keys(charset).length : null,
    charsetJson: charset === undefined ? null : JSON.stringify(charset),
  };
}
