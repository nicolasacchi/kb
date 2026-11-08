// T1 twin-seed matrix library (T6): dual vocabulary for stage A/B equality.
//
// Ported from the CI-spike twin/lib.mjs (branch origin/spike/kb-term-ci).
// Adapters vs that spike (merged code wins where the brief differs):
// - stateOf/diffState/compareBuffers/dumpRows are re-exported from T5's
//   test/support/compare.ts (ported from the same spike) with ONE wrapper:
//   stateOf recomputes pen/eraseAttr/savedAttr signatures from the real
//   IAttributeData shape ({fg, bg, extended:{underlineStyle,
//   underlineColor}}). The compare.ts attrSig reads a nonexistent
//   `extendedAttrs` field, which would hide pen underline diffs; our wrapper
//   drops `_urlId`/`urlId` (link ids are per-terminal; links are in
//   COSMETIC_RESIDUALS) and reads the real `extended`. That is the spike's
//   attrSig behaviour minus the link id.
// - stateOf also compares tabs/title/eraseAttr/charset.* (kept, non-cosmetic).
// - Term construction comes from T5's makeTwin, not a copy.
// - The stage-B allowlist below starts EMPTY (spec 9.2); entries may only be
//   added with a one-line root cause, totals staying within the spike's
//   residuals (15 cell-chars + 7 wide + 6 extra + 2 lost).

import {
  stateOf as baseStateOf,
  diffState as baseDiffState,
  compareBuffers as baseCompareBuffers,
  dumpRows as baseDumpRows,
  type StateDiff,
} from '../support/compare.ts';

export { diffState as baseDiffState, compareBuffers as baseCompareBuffers, dumpRows as baseDumpRows };
export type { StateDiff };
export type { BufferCompare } from '../support/compare.ts';

/** Documented cosmetic losses (spec 6): serialize() cannot express them. */
export const COSMETIC_RESIDUALS = ['OSC 8 hyperlink', 'underline style', 'SGR 58 underline colour'] as const;

type Term = {
  cols: number;
  buffer: { active: { type: string; cursorX: number; cursorY: number; baseY: number } };
  modes: Record<string, unknown>;
  _core: Record<string, unknown>;
  write(d: Uint8Array | string, cb?: () => void): void;
};

const RGB_PEEK = 0x3ffffff;
const ulc = (v: number | undefined | null): number => ((v ?? 0) === RGB_PEEK ? 0 : (v ?? 0));

type AttrLike = {
  fg: number;
  bg: number;
  extended?: { underlineStyle: number; underlineColor: number; urlId?: number; _urlId?: number };
};

/** Spike attrSig MINUS the link id (deliberate deviation, spec 7). */
function attrSig(a: AttrLike | null): unknown {
  if (!a) return null;
  return {
    fg: a.fg >>> 0,
    bg: a.bg >>> 0,
    ul: a.extended
      ? [a.extended.underlineStyle ?? 0, ulc(a.extended.underlineColor ?? 0), null]
      : null,
  };
}

/** stateOf with correct pen/savedAttr underline signatures (see header),
 * and DECRC-equivalence for the DECSC slot: the raw savedY is an absolute
 * buffer coordinate that CUP cannot name once ybase has moved (scroll or
 * ED3 trim), so raw comparison can never hold for out-of-viewport slots.
 * xterm's DECRC restores y = max(savedY - ybase, 0) and _restrictCursor
 * clamps to [0, rows-1] (x to [0, cols-1]); we compare that restored
 * position instead of the raw absolute value. See README. */
export function stateOf(t: Term): Record<string, unknown> {
  const s = baseStateOf(t as never);
  const core = t._core as {
    _inputHandler: { _curAttrData: AttrLike | null; _eraseAttrData?: () => AttrLike };
    _bufferService: {
      rows?: number;
      buffers: Record<string, { savedCurAttrData: AttrLike | null; savedX?: number; savedY?: number; ybase?: number }>;
    };
  };
  s['pen'] = attrSig(core._inputHandler._curAttrData);
  try {
    if (core._inputHandler._eraseAttrData) s['eraseAttr'] = attrSig(core._inputHandler._eraseAttrData());
  } catch {
    // keep the base value
  }
  const rows = core._bufferService.rows;
  for (const name of ['normal', 'alt']) {
    const b = core._bufferService.buffers[name];
    s[`${name}.savedAttr`] = attrSig(b ? b.savedCurAttrData : null);
    if (b && typeof b.savedX === 'number' && typeof b.savedY === 'number' && typeof b.ybase === 'number' && typeof rows === 'number') {
      s[`${name}.savedX`] = Math.min(Math.max(b.savedX, 0), Math.max(0, t.cols - 1));
      s[`${name}.savedY`] = Math.min(Math.max(b.savedY - b.ybase, 0), Math.max(0, rows - 1));
    }
  }
  return s;
}

/** Visual colour equality for masked fg/bg words (bits 0-25, mode bits
 * 24-25): P16 vs P256 palette indices < 16 carry the same colour, only
 * xterm's mode bits differ. SerializeAddon 0.14.0 emits P256 indices
 * 0-15 as P16 codes (30-37/90-97), so its replayed word differs while
 * the displayed colour is identical. */
export function sameVisualColour(a: number, b: number): boolean {
  const mode = (w: number): number => (w >>> 24) & 3;
  const val = (w: number): number => w & 0xffffff;
  if (val(a) !== val(b)) return false;
  if (mode(a) === mode(b)) return true;
  return (mode(a) === 1 || mode(a) === 2) && (mode(b) === 1 || mode(b) === 2) && val(a) < 16;
}

export function diffState(a: Record<string, unknown>, b: Record<string, unknown>): StateDiff[] {
  return baseDiffState(a as never, b as never);
}

export function compareBuffers(ta: Term, tb: Term, which: 'normal' | 'alt', maxExamples = 3) {
  return baseCompareBuffers(ta as never, tb as never, which, maxExamples);
}

export function dumpRows(t: Term, which: 'normal' | 'alt', n: number): string[] {
  return baseDumpRows(t as never, which, n);
}

/** Buffer-compare classes with the alt-buffer activation prefix (spike). */
export function classifyBufferDiff(
  cells: Record<string, number>,
  which: 'normal' | 'alt',
  altActive: boolean,
): Record<string, number> {
  const out: Record<string, number> = {};
  for (const [c, n] of Object.entries(cells)) {
    const key = which === 'alt'
      ? (altActive ? 'alt-buffer content (active)' : 'alt-buffer content (inactive, stale)') + ': ' + c
      : c;
    out[key] = n;
  }
  return out;
}

/** ESC 8 (DECRC) probe: do ref and viewer restore to the same cursor/pen? */
export async function probeDecrc(t: Term): Promise<{ x: number; y: number; pen: unknown; charset: unknown }> {
  await new Promise<void>((res) => t.write('\x1b8', res));
  const s = stateOf(t);
  const b = t.buffer.active;
  return { x: b.cursorX, y: b.baseY + b.cursorY, pen: s['pen'], charset: s['charset.glevel'] };
}

// --- stage-B residuals ------------------------------------------------------

/** Named stage-B allowlist entries: one root-cause line each (spec 9.2). */
export interface ResidualEntry {
  variant: string;
  stream: string;
  /** Exact classified class key (after alt prefixing) or state class. */
  cls: string;
  /** One-line root cause, verified against a CI run. */
  why: string;
}

export const STATE_ALLOWLIST: ResidualEntry[] = [];

export function isStageBAllowed(variant: string, stream: string, cls: string): boolean {
  return STATE_ALLOWLIST.some((e) => e.variant === variant && e.stream === stream && e.cls === cls);
}
