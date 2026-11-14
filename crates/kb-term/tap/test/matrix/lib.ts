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
/** Named residuals beyond the three spec-6 classes (FXT6 amendment of spec
 * 6): each is allowed only when the visual-same proof holds over ALL
 * differing cells of the class (the matrix passes maxExamples high enough
 * to enumerate them; a capped sample cannot classify a class clean). */
export const VISUAL_SAME_P16 = 'SerializeAddon P16 downgrade for palette indices < 16' as const;
export const BLANK_FLAG_ATTR = 'serialize drops flags on blank cells' as const;

/** Spike (T1) stage-B residual budgets (spec 9.2): total cells/rows of each
 * named CELL/ROW class across ALL quiescent joins must stay at or below
 * these. A new allowlisted cell/row class with no budget entry fails the
 * bound test. STATE classes (DECSC) sit outside the cell/row spike budget
 * but are NOT silent: every allowed state diff is counted and enumerated by
 * the bound test under its class key (see DECSC_STAGEB_CLASS). */
export const SPIKE_BOUNDS: Readonly<Record<string, number>> = Object.freeze({
  'cell chars': 15,
  'wide-char boundary': 7,
  'rows only in viewer (extra)': 6,
  'rows only in expected (lost by seed/viewer)': 2,
});

/** Named stage-B state residual (FXT6 review round 2): DECRC (ESC 8)
 * restores from RAW saved coordinates shifted by ybase -- a REF that ran
 * the full stream and a seed viewer (seed + remainder) can share exactly
 * the viewport slot while their absolute savedY differs by the ybase
 * delta (different history lengths after ED3 trims): no CUP can reconcile
 * that from the seed, DECRC ignores it, and the viewport position is what
 * the user sees. Stage-B DECSC coordinate diffs whose DECRC-restored
 * positions agree (decscStageBAllowed) are allowed under THIS named
 * residual -- not a blanket -- and every such exemption is counted under
 * this key by the matrix and enumerated by the spike-bound test. The four
 * per-{variant,stream} STATE_ALLOWLIST DECSC entries remain for the
 * narrower case the named residual cannot cover (a restored position that
 * genuinely differs on those streams); their exemptions are counted under
 * the same key so no DECSC state diff is uncounted. probeDecrc remains
 * the ultimate arbiter at every join. Owned by this class, FLOORED nowhere
 * below: it is a state residual, deliberately outside the 15+7+6+2
 * cell/row spike budget (T6 spec 9.2) but never unenumerated. */
export const DECSC_STAGEB_CLASS = 'DECSC saved cursor (restored-position equivalence)' as const;

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

/** stateOf with correct pen/savedAttr underline signatures (see header).
 * savedX/savedY stay RAW (absolute buffer coordinates) so stage A fails an
 * in-viewport wrong slot; DECRC-restored equivalence is computed separately
 * (decscStageAAllowed) and used only for out-of-viewport slots and the
 * ESC 8 probe. */
export function stateOf(t: Term): Record<string, unknown> {
  const s = baseStateOf(t as never);
  const core = t._core as {
    _inputHandler: { _curAttrData: AttrLike | null; _eraseAttrData?: () => AttrLike };
    _bufferService: { buffers: Record<string, { savedCurAttrData: AttrLike | null }> };
  };
  s['pen'] = attrSig(core._inputHandler._curAttrData);
  try {
    if (core._inputHandler._eraseAttrData) s['eraseAttr'] = attrSig(core._inputHandler._eraseAttrData());
  } catch {
    // keep the base value
  }
  s.__rows = t.rows;
  for (const name of ['normal', 'alt']) {
    const b = core._bufferService.buffers[name];
    s[`${name}.savedAttr`] = attrSig(b ? b.savedCurAttrData : null);
  }
  return s;
}

/** The position DECRC (ESC 8) restores from a saved slot: y is
 * max(savedY - ybase, 0) then _restrictCursor clamps to [0, rows-1] and x to
 * [0, cols-1] (origin mode clamps to the scroll margin, identical on both
 * sides since the scroll region is compared separately). */
function decrcRestored(bs: { savedX: number; savedY: number; ybase: number }, cols: number, rows: number): [number, number] {
  const x = Math.min(Math.max(bs.savedX, 0), Math.max(0, cols - 1));
  const y = Math.min(Math.max(bs.savedY - bs.ybase, 0), Math.max(0, rows - 1));
  return [x, y];
}

function inViewport(bs: { savedX: number; savedY: number; ybase: number }, cols: number, rows: number): boolean {
  const vy = bs.savedY - bs.ybase;
  return bs.savedX >= 0 && bs.savedX < cols && vy >= 0 && vy < rows;
}

/**
 * Stage-A DECSC rule (FXT6): raw savedX/savedY equality is required whenever
 * the slot is in-viewport on either side (a wrong in-viewport CUP always
 * fails, however the restored position compares). A raw mismatch is allowed
 * only when neither side can express it: at least one slot sits outside its
 * viewport (scrollback-above / past-end / pending-wrap clamp) AND the
 * DECRC-restored positions still agree (the ESC 8 probe's claim).
 */
export function decscStageAAllowed(a: Record<string, unknown>, b: Record<string, unknown>): boolean {
  const cols = Number(a.__cols ?? 0);
  const rows = Number(a.__rows ?? 0);
  if (!cols || !rows) return false;
  for (const name of ['normal', 'alt']) {
    const A = { savedX: Number(a[`${name}.savedX`]), savedY: Number(a[`${name}.savedY`]), ybase: Number(a[`${name}.ybase`]) };
    const B = { savedX: Number(b[`${name}.savedX`]), savedY: Number(b[`${name}.savedY`]), ybase: Number(b[`${name}.ybase`]) };
    if (A.savedX === B.savedX && A.savedY === B.savedY) continue;
    const ra = decrcRestored(A, cols, rows);
    const rb = decrcRestored(B, cols, rows);
    if (ra[0] !== rb[0] || ra[1] !== rb[1]) return false;
    if (inViewport(A, cols, rows) && inViewport(B, cols, rows)) return false;
  }
  return true;
}
/**
 * Stage-B DECSC rule (FXT6): compare DECRC-restored viewport positions, not
 * raw absolute savedY. This implements the named residual
 * DECSC_STAGEB_CLASS (see its root-cause comment): REF (full stream) and
 * the seed viewer (seed + remainder) can share exactly the viewport slot
 * while their absolute savedY differs by the ybase delta (different
 * history lengths after ED3 trims); no CUP can reconcile that from the
 * seed, DECRC ignores it, and the viewport position is what the user sees.
 * FXT6's raw-in-viewport strictness applies to stage A only, where the
 * seed must match its twin exactly wherever a CUP can express it. The
 * ESC 8 probe remains stage B's ultimate arbiter. Returns true only when
 * EVERY buffer's restored position agrees; the caller scopes this to
 * savedX/savedY keys so a savedAttr/savecharset diff still fails.
 */
export function decscStageBAllowed(a: Record<string, unknown>, b: Record<string, unknown>): boolean {
  const cols = Number(a.__cols ?? 0);
  const rows = Number(a.__rows ?? 0);
  if (!cols || !rows) return false;
  for (const name of ['normal', 'alt']) {
    const A = { savedX: Number(a[`${name}.savedX`]), savedY: Number(a[`${name}.savedY`]), ybase: Number(a[`${name}.ybase`]) };
    const B = { savedX: Number(b[`${name}.savedX`]), savedY: Number(b[`${name}.savedY`]), ybase: Number(b[`${name}.ybase`]) };
    if (A.savedX === B.savedX && A.savedY === B.savedY) continue;
    const ra = decrcRestored(A, cols, rows);
    const rb = decrcRestored(B, cols, rows);
    if (ra[0] !== rb[0] || ra[1] !== rb[1]) return false;
  }
  return true;
}

/** Stage-B STATE decision (FXT6 review round 2): every state diff is
 * either allowed -- an exact STATE_ALLOWLIST {variant,stream,class} entry,
 * or a DECSC coordinate diff under the named DECSC_STAGEB_CLASS residual
 * (restored positions agree; savedAttr/savecharset still fail) -- or bad.
 * There is no blanket class wildcard. The matrix counts each allowed diff
 * under its exact class key (DECSC under DECSC_STAGEB_CLASS) and fails
 * the join on any bad entry; the spike-bound test then enumerates every
 * counted key, so a new verb cannot pass silently. */
export function stageBStateDecision(
  variant: string,
  stream: string,
  refState: Record<string, unknown>,
  viewerStateB: Record<string, unknown>,
  bState: StateDiff[],
): { bad: string[]; allowed: StateDiff[] } {
  const bad: string[] = [];
  const allowed: StateDiff[] = [];
  for (const d of bState) {
    const restoredEq =
      d.cls === 'DECSC saved cursor' &&
      (d.key.endsWith('.savedX') || d.key.endsWith('.savedY')) &&
      decscStageBAllowed(refState, viewerStateB);
    if (isStageBAllowed(variant, stream, d.cls) || restoredEq) {
      allowed.push(d);
      continue;
    }
    bad.push(`state:${d.cls}:${d.key}`);
  }
  return { bad, allowed };
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

export const STATE_ALLOWLIST: ResidualEntry[] = [
  { variant: 'graphemes', stream: 'harness-s6', cls: 'DECSC saved cursor', why: 'stage-B: remainder drives the seed slot off via DECRC/ED3 after the join; buffers converge identically (bCells empty) and the DECRC probe stays same, but the final raw (and restored) slot differs after the remainder shifts ybase; every such exemption is counted under the named DECSC_STAGEB_CLASS residual (outside the cell/row spike budget, enumerated by the bound test).' },
  { variant: 'graphemes', stream: 'harness-s7', cls: 'DECSC saved cursor', why: 'stage-B: remainder drives the seed slot off via DECRC/ED3 after the join; buffers converge identically (bCells empty) and the DECRC probe stays same, but the final raw (and restored) slot differs after the remainder shifts ybase; every such exemption is counted under the named DECSC_STAGEB_CLASS residual (outside the cell/row spike budget, enumerated by the bound test).' },
  { variant: 'unicode11', stream: 'harness-s6', cls: 'DECSC saved cursor', why: 'stage-B: remainder drives the seed slot off via DECRC/ED3 after the join; buffers converge identically (bCells empty) and the DECRC probe stays same, but the final raw (and restored) slot differs after the remainder shifts ybase; every such exemption is counted under the named DECSC_STAGEB_CLASS residual (outside the cell/row spike budget, enumerated by the bound test).' },
  { variant: 'unicode11', stream: 'harness-s7', cls: 'DECSC saved cursor', why: 'stage-B: remainder drives the seed slot off via DECRC/ED3 after the join; buffers converge identically (bCells empty) and the DECRC probe stays same, but the final raw (and restored) slot differs after the remainder shifts ybase; every such exemption is counted under the named DECSC_STAGEB_CLASS residual (outside the cell/row spike budget, enumerated by the bound test).' },
];

export function isStageBAllowed(variant: string, stream: string, cls: string): boolean {
  return STATE_ALLOWLIST.some((e) => e.variant === variant && e.stream === stream && e.cls === cls);
}
