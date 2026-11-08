// kb-term tap: quiescent-join rule + state-restore seed builder (T6).
//
// Header, pinned by the T1 matrix: links and extended underline styles
// present at join time are lost for rows already in the buffer; new output
// after the join renders them normally. (COSMETIC_RESIDUALS in
// test/matrix/lib.ts names the three classes; stage A may report them and
// the derived 'fg colour'/'attributes' on link cells, and nothing else.)
//
// JOIN RULE. Late joiners are seeded only at quiescent points: the twin has
// parsed through the candidate chunk-end, synchronizedOutputMode is off, and
// the GroundTracker is at ground. ED2+ED3 and its reprint travel inside one
// ?2026 frame, so a join never falls inside one. Candidate join offsets are
// chunk ends of what was fed -- a mid-chunk ?2026l is not a candidate (T7
// chunks at frame ends if it needs finer points). Never join non-quiescent;
// the caller waits and retries (nextQuiescent resolves quiescent:false on
// timeout, it never joins anyway).
//
// RACE RULE (load-bearing): ground.push runs inside the twin's write
// callback, in order, never when bytes are merely queued. Otherwise the
// tracker runs ahead of the twin and the check describes a different offset.
// feed() is the ONLY way bytes enter the twin+tracker pair.
//
// buildSeed composes: serialize() body (50k scrollback desktop / 20k phone,
// never excludeModes/excludeAltBuffer -- unverified for 0.14.0) + a
// state-restore tail (DECSTBM, DECSC slot, live cursor/pending wrap, modes,
// live pen last). The body is expected to contain exactly one
// ESC[?1049h (alt content follows it when the alt buffer is active); the
// alt-active composition switches with ESC[?47h -- never ?1049h, which
// would save the cursor and overwrite the normal DECSC slot just set.
// DERIVED BY REASONING, NOT RUN when written: the T1 matrix is the arbiter.
//
// Post-filter: the assembled seed is stripped of main-screen mouse enables,
// kitty-keyboard push/set, ?2031 and never contains queries; mouse removals
// are counted in withheld.mouse (0 in every T6 test stream -- non-zero
// means a T3/T7 bug is being papered over and the tests fail on it).
// ?2026 must be absent (asserted).
//
// Adapters vs the T6 brief (merged code wins): the T5 factory returns
// {term, serialize, feed, ...} and its SerializeLike types serialize() as
// no-arg; the runtime SerializeAddon 0.14.0 accepts {scrollback} (verified
// against its source and the spike's usage), so buildSeed calls it through
// a cast. DECKPAM/DECCKM are emitted as CSI ?66 / CSI ?1 (the same values
// SerializeAddon itself emits), not ESC= (the tmux-side convention in
// reseed.ts).
import type { Twin } from './twin.ts';
import { GroundTracker } from './ground.ts';
import { captureTwinState, type BufferState, type TwinState } from './seed-state.ts';
import { sgrFromAttr } from './sgr.ts';

export interface JoinPoint {
  offset: number;
  quiescent: boolean;
  reason?: 'sync-frame' | 'mid-sequence' | 'twin-behind';
}

export type TailItem = 'decstbm' | 'decsc' | 'pen' | 'pendingWrap' | 'modes' | 'alt';

export interface Seed {
  bytes: Uint8Array;
  offset: number;
  scrollbackLines: number;
  tailBytes: number;
  withheld: { mouse: number };
}

const ESC = '\x1b';
const encoder = new TextEncoder();

/** One-off boundary read: modes.synchronizedOutputMode, fail-closed. */
function syncOn(twin: Twin): boolean {
  const term = twin.term as unknown;
  if (term === null || typeof term !== 'object' || !('modes' in term)) {
    throw new Error('seed: xterm internals changed: twin.term.modes');
  }
  const modes = (term as { modes: unknown }).modes;
  if (modes === null || typeof modes !== 'object' || !('synchronizedOutputMode' in modes)) {
    throw new Error('seed: xterm internals changed: twin.term.modes.synchronizedOutputMode');
  }
  return (modes as { synchronizedOutputMode: boolean }).synchronizedOutputMode === true;
}

/**
 * SerializeAddon 0.14.0 accepts {scrollback} at runtime but T5's Twin type
 * declares serialize() as no-arg; call through one checked boundary.
 */
function serializeBody(twin: Twin, scrollback: number): string {
  const ser = twin.serialize as unknown;
  if (ser === null || typeof ser !== 'object' || !('serialize' in ser)) {
    throw new Error('seed: xterm internals changed: twin.serialize');
  }
  const fn = (ser as { serialize: unknown }).serialize;
  if (typeof fn !== 'function') throw new Error('seed: xterm internals changed: twin.serialize.serialize');
  return (fn as (this: unknown, o: { scrollback?: number }) => string).call(ser, { scrollback });
}

/** Point-in-time quiescence: twin parsed (caller guarantees), frame off, ground. */
export function isQuiescent(twin: Twin, ground: GroundTracker): boolean {
  if (syncOn(twin)) return false;
  return ground.atGround();
}

export class QuiescentJoinGate {
  private twin: Twin;
  private ground: GroundTracker;
  private parsedBytes = 0;
  private inFlight = 0;
  private waiters: Array<(jp: JoinPoint) => void> = [];

  constructor(twin: Twin, ground: GroundTracker) {
    this.twin = twin;
    this.ground = ground;
  }

  /** Bytes parsed through (write callbacks fired, in order). */
  get parsed(): number {
    return this.parsedBytes;
  }

  /** Feeds currently in flight (twin write callbacks not yet fired). */
  get pending(): number {
    return this.inFlight;
  }

  /** The ONLY way bytes enter the twin+tracker pair (RACE RULE above). */
  feed(bytes: Uint8Array | string): Promise<void> {
    const all = typeof bytes === 'string' ? encoder.encode(bytes) : bytes;
    if (all.length === 0) return Promise.resolve();
    this.inFlight++;
    const { promise, resolve } = Promise.withResolvers<void>();
    this.twin.term.write(all, () => {
      this.ground.push(all);
      this.parsedBytes += all.length;
      this.inFlight--;
      this.checkWaiters();
      resolve();
    });
    return promise;
  }

  /** Current join point: false (twin-behind) while any feed is in flight. */
  currentJoinPoint(): JoinPoint {
    const offset = this.parsedBytes;
    if (this.inFlight > 0) return { offset, quiescent: false, reason: 'twin-behind' };
    if (syncOn(this.twin)) return { offset, quiescent: false, reason: 'sync-frame' };
    if (!this.ground.atGround()) return { offset, quiescent: false, reason: 'mid-sequence' };
    return { offset, quiescent: true };
  }

  /** Resolves with the first chunk-end at which twin+frame+ground all hold. */
  nextQuiescent(maxWaitMs = 2000): Promise<JoinPoint> {
    const now = this.currentJoinPoint();
    if (now.quiescent) return Promise.resolve(now);
    const { promise, resolve } = Promise.withResolvers<JoinPoint>();
    const waiter = (jp: JoinPoint) => resolve(jp);
    this.waiters.push(waiter);
    setTimeout(() => {
      const i = this.waiters.indexOf(waiter);
      if (i >= 0) {
        this.waiters.splice(i, 1);
        resolve(this.currentJoinPoint());
      }
    }, maxWaitMs);
    return promise;
  }

  private checkWaiters(): void {
    const jp = this.currentJoinPoint();
    if (!jp.quiescent) return;
    const ws = this.waiters;
    this.waiters = [];
    for (const w of ws) w(jp);
  }
}

const ALT_INTRO = `${ESC}[?1049h`;
const MOUSE_RE = /\x1b\[\?(?:1000|1002|1003|1005|1006|1015|1016)[hl]/g;
const FORBIDDEN_RE = /\x1b\[\?2031[hl]/;

function sgrEscape(params: string): string {
  return `${ESC}[${params}m`;
}

/** DECSCUSR parameter from captured cursor style/blink (xterm 6.0.0
 * InputHandler.setCursorStyle: 0 resets to the option (undefined),
 * 1/2 block (blink/steady), 3/4 underline, 5/6 bar; blink == odd). */
function decscusr(st: TwinState): string {
  if (st.cursorStyle === undefined) return `${ESC}[0 q`;
  const idx = st.cursorStyle === 'block' ? 0 : st.cursorStyle === 'underline' ? 1 : 2;
  return `${ESC}[${idx * 2 + (st.cursorBlink ? 1 : 2)} q`;
}

/** Mode byte for `?N` set/reset from a captured mode value. */
function modeSeq(n: number, on: boolean | string | number): string {
  return `${ESC}[?${n}${on ? 'h' : 'l'}`;
}


export function buildSeed(
  twin: Twin,
  opts: { scrollback: 50000 | 20000; atOffset: number; skip?: TailItem[]; ground?: GroundTracker },
): Seed {
  const st = captureTwinState(twin);
  const quiescent = opts.ground ? isQuiescent(twin, opts.ground) : !syncOn(twin);
  if (!quiescent) {
    throw new Error(`seed: refusing not quiescent join at offset ${opts.atOffset}`);
  }
  const skip = new Set<TailItem>(opts.skip ?? []);

  const body = serializeBody(twin, opts.scrollback);
  const parts = body.split(ALT_INTRO);
  if (parts.length > 2) throw new Error('seed: multiple 1049h in serialize body');
  if (st.active === 'alternate' && parts.length < 2) {
    throw new Error('seed: alt active but serialize body has no 1049h');
  }
  const normalPart = parts[0];
  const altPart = parts[1] ?? '';

  // Charset/glevel fail-closed: designations cannot be re-emitted from
  // captured state (xterm keeps only the built maps), so a non-default
  // charset or nonzero live glevel is a hard error, never silently dropped.
  if (st.glevel !== 0 || st.normal.savedCharset !== null || st.alt.savedCharset !== null) {
    throw new Error('seed: non-default charset/glevel not supported');
  }

  const tailFor = (bs: BufferState): string => {
    let s = '';
    // (i) scroll region first: CSI r homes the cursor (origin off semantics).
    if (!skip.has('decstbm') && (bs.scrollTop !== 0 || bs.scrollBottom !== st.rows - 1)) {
      s += `${ESC}[${bs.scrollTop + 1};${bs.scrollBottom + 1}r`;
    }
    // (ii) origin off while positioning.
    s += `${ESC}[?6l`;
    // (iii) DECSC slot: savedY is absolute (ybase + viewport row); CUP is
    // viewport-relative, so convert via ybase. savedX may equal cols after
    // a pending wrap; CUP clamps to cols-1 (documented, restores the same
    // after _restrictCursor clamps DECRC). No glevel slot: xterm 6.0.0
    // buffers carry no savedGlevel, so ESC 7 records none.
    // A null savedAttr means never saved: leave the viewer default (no
    // ESC7) instead of clobbering it. Two out-of-viewport regimes:
    // - viewportY < 0 (slot scrolled into scrollback above): the live
    //   CUP below cannot address it after the body; the early prefix
    //   already saved it while ybase was 0, so skip the late save.
    // - viewportY >= rows (post-ED3 stale slot: ED3 trims lines and
    //   ybase drops, leaving savedY beyond the buffer): DECRC restores
    //   y = clamp(savedY - ybase, 0, rows - 1) via restoreCursor +
    //   _restrictCursor, so the viewer MUST re-save the clamped row
    //   here; the early prefix alone sits at min(savedY, rows-1), which
    //   after the body's ybase shift restores to a different row.
    //   `min(viewportY, rows - 1)` is exactly that clamp.
    // Equivalence is therefore in DECRC-restored coordinates, not in
    // raw savedY (see test/matrix/README.md). Abs comparison of the raw
    // value is unreachable for these regimes: no CUP can name them.
    if (!skip.has('decsc') && bs.savedAttr !== null) {
      const viewportY = bs.savedY - bs.ybase;
      if (viewportY >= 0) {
        const sy = Math.min(viewportY, st.rows - 1);
        const sx = Math.min(bs.savedX, st.cols - 1);
        s += `${ESC}[${sy + 1};${sx + 1}H`;
        s += sgrEscape(sgrFromAttr(bs.savedAttr));
        s += `${ESC}7`;
      }
    }
    // (iv) live cursor / pending wrap.
    if (bs.pendingWrap) {
      s += `${ESC}[${bs.y + 1};${st.cols}H`;
      if (!skip.has('pendingWrap') && bs.wrap) {
        s += sgrEscape(sgrFromAttr(bs.wrap.attr));
        s += bs.wrap.char;
      }
    } else {
      s += `${ESC}[${bs.y + 1};${bs.x + 1}H`;
    }
    // (v) restore origin mode value.
    if (st.modes.originMode) s += `${ESC}[?6h`;
    return s;
  };

  // Early normal DECSC prefix: savedY is absolute, so a slot in scrollback
  // (savedY < ybase) or the default slot (savedY=0 with ybase>0) can only
  // be saved while the fresh viewer still has ybase=0. Save it here, then
  // home + reset so the body replays from the fresh home/default pen. The
  // body contains no ESC7 (serialize emits none; the ?1049h is split out
  // and replaced by ?47h), so the early slot survives the body. If the slot
  // is in the viewport at join time, the late tail re-saves it correctly.
  let early = '';
  if (!skip.has('decsc') && st.normal.savedAttr !== null) {
    const ey = Math.min(st.normal.savedY, st.rows - 1);
    const ex = Math.min(st.normal.savedX, st.cols - 1);
    early += `${ESC}[?6l${ESC}[${ey + 1};${ex + 1}H`;
    early += sgrEscape(sgrFromAttr(st.normal.savedAttr));
    early += `${ESC}7${ESC}[1;1H${ESC}[0m`;
  }
  let appended = '';
  let out = early + normalPart;
  if (st.active === 'alternate') {
    appended += tailFor(st.normal);
    // ?47h, never ?1049h: 1049 saves the cursor and would overwrite the
    // normal DECSC slot just set.
    appended += skip.has('alt') ? ALT_INTRO : `${ESC}[?47h`;
    appended += altPart;
    appended += tailFor(st.alt);
  } else {
    appended += tailFor(st.normal);
  }

  // Mode tail (DECSCUSR, wrap, cursor visibility, bracketed paste, focus,
  // origin, insert, DECCKM/DECKPAM). ?2026 is never emitted here (asserted
  // below); mouse modes are never emitted (post-filtered from the body).
  if (!skip.has('modes')) {
    appended += decscusr(st);
    appended += modeSeq(7, st.modes.wraparoundMode);
    appended += modeSeq(25, !st.isCursorHidden);
    appended += modeSeq(2004, st.modes.bracketedPasteMode);
    appended += modeSeq(1004, st.modes.sendFocusMode);
    appended += modeSeq(6, st.modes.originMode);
    appended += modeSeq(4, st.modes.insertMode);
    appended += modeSeq(1, st.modes.applicationCursorKeysMode);
    appended += modeSeq(66, st.modes.applicationKeypadMode);
  }

  // Final trailer: absolute cursor placement (origin re-asserted after CUP
  // because CUP is region-relative under origin mode) and the live pen
  // LAST, so viewer cursor and pen equal the twin's.
  const ab = st.active === 'alternate' ? st.alt : st.normal;
  appended += `${ESC}[?6l`;
  if (ab.pendingWrap) {
    appended += `${ESC}[${ab.y + 1};${st.cols}H`;
    if (!skip.has('pendingWrap') && ab.wrap) {
      appended += sgrEscape(sgrFromAttr(ab.wrap.attr));
      appended += ab.wrap.char;
    }
  } else {
    appended += `${ESC}[${ab.y + 1};${ab.x + 1}H`;
  }
  if (st.modes.originMode) appended += `${ESC}[?6h`;
  if (!skip.has('pen')) appended += sgrEscape(sgrFromAttr(st.pen));

  out += appended;

  // Post-filter: strip mouse enables (counted), ?2031 and kitty-keyboard
  // push/set (throwing: they are never in a legal body -- presence would be
  // a T3-composition bug, not something to paper over).
  let withheld = 0;
  out = out.replace(MOUSE_RE, () => {
    withheld++;
    return '';
  });
  for (const re of [FORBIDDEN_RE, /\x1b\[>[\d;]*u/g, /\x1b\[=[\d;]*u/g]) {
    if (re.test(out)) throw new Error('seed: kitty-keyboard/?2031 sequence in serialize body');
  }
  if (out.includes(`${ESC}[?2026`)) throw new Error('seed: synchronized-output mode in seed');

  const bytes = encoder.encode(out);
  return {
    bytes,
    offset: opts.atOffset,
    scrollbackLines: Math.max(0, st.normal.length - st.rows),
    tailBytes: encoder.encode(early + appended).length,
    withheld: { mouse: withheld },
  };
}
