// kb-term tap: the reseed (A/B capture) recipe with mode restore. Pure
// orchestration over a ControlLike and a Twin.
//
// Why: whenever the tap lost bytes (pause, `%exit too far behind`, restart,
// new control client), the twin is rebuilt from tmux itself. The recipe
// (spike T6: 12/12 identical reconstructions, 10/10 concurrent, and after a
// >300 s disconnect) is: wait for a quiescent point, then in ONE stdin
// write capture the pane twice around a `capture-pane`, retry if output
// interleaves, feed the capture to the twin, restore modes, then feed every
// %output that arrived after B, in order, with no gap. Live %output during
// a reseed is queued here; the caller must not feed the twin meanwhile.
//
// The capture is joined with CRLF and NOT sliced by history_size+cursor_y:
// with `-J` wrapped rows are joined, so the capture has fewer lines than
// grid rows whenever any line wrapped. The twin re-wraps at the same width;
// the twin's total line count is compared to B.history_size + rows, blank
// tail rows are padded with CRLFs, and an overflow counts a discard and
// retries from statement 2.

import type { Twin } from './twin.ts';
import { PANE_ID_RE } from './control.ts';

/** The narrow surface reseed needs from the control client. */
export interface ControlLike {
  cmd(line: string): Promise<string[]>;
  write(lines: string[]): Promise<string[]>[];
  onOutput(cb: (pane: string, bytes: Uint8Array, ageMs?: number) => void): void;
}

/** Quiet window before a capture is safe. STARTING value, unmeasured. */
export const QUIESCENT_MS = 50;
/** Absolute bound on the quiescent wait; then proceed with quiescent:false. */
export const QUIESCENT_MAX_MS = 2000;
export const MAX_RESEED_ATTEMPTS = 5;

/**
 * Mode format names queried in the same write as the capture. Confirmed on
 * tmux 3.7c by term_int_mode_formats_exist_on_pinned_tmux (display-message
 * -a); a name missing there would be dropped from this list and the restore.
 */
export const MODE_FORMATS = [
  'alternate_on',
  'scroll_region_upper',
  'scroll_region_lower',
  'wrap_flag',
  'cursor_flag',
  'insert_flag',
  'keypad_cursor_flag',
  'keypad_flag',
  'origin_flag',
] as const;

const ESC = '\x1b';
/** Synchronized-frame end: the parser is back in ground state after this. */
const FRAME_END = `${ESC}[?2026l`;
const FRAME_END_BYTES = new TextEncoder().encode(FRAME_END);

export interface ModeState {
  alternateOn: boolean;
  scrollTop: number | null;
  scrollBottom: number | null;
  wrap: boolean | null;
  cursor: boolean | null;
  insert: boolean | null;
  keypadCursor: boolean | null;
  keypad: boolean | null;
  origin: boolean | null;
}

export interface ReseededResult {
  attempts: number;
  quiescent: boolean;
  bytesFed: number;
  historySize: number;
  mode: ModeState | null;
  a: ReseedMarker;
  b: ReseedMarker;
  /** The -a (alternate-screen) capture rejected, but the reseed proceeded
   * (non-alt pane). Observable so a broken -a capture is not invisible. */
  altUnsupported: boolean;
}

export interface ReseedStats {
  totalAttempts: number;
  reseedCalls: number;
  preABytes: number;
  interleavedRetries: number;
  abDisagreeRetries: number;
  badMarkerRetries: number;
  badModeRetries: number;
  overflowRetries: number;
  givesUp: number;
  queuedDroppedBytes: number;
  altUnsupported: number;
  quiescentFalse: number;
}

export interface ReseedOpts {
  now?: () => number;
  sleep?: (ms: number) => Promise<void>;
  quiescentMs?: number;
  quiescentMaxMs?: number;
  maxAttempts?: number;
  onReseeded?: (r: ReseededResult) => void;
  /**
   * Feed the post-B queued bytes to the twin after the mode restore
   * (default true: the standalone T5 behaviour). createTapStack sets
   * false: the TapServer's own twin chain is the single feeder and feeds
   * the same bytes (post-withhold, through the ring) once the pause ends,
   * so feeding the queued raw bytes as well would double-feed the twin
   * and reintroduce withheld bytes.
   */
  feedQueued?: boolean;
}

export class ReseedError extends Error {
  readonly attempts: number;
  readonly reason: string;
  constructor(reason: string, attempts: number) {
    super(`reseed gave up after ${attempts} attempts: ${reason}`);
    this.name = 'ReseedError';
    this.attempts = attempts;
    this.reason = reason;
  }
}

/** A/B capture markers: tmux's own `#{history_size} #{cursor_x} #{cursor_y}`. */
export interface ReseedMarker {
  history: number;
  cursorX: number;
  cursorY: number;
}

function parseMarker(lines: string[] | undefined, tag: string): ReseedMarker | null {
  if (!lines || lines.length === 0) return null;
  const parts = lines[0].split(/\s+/).filter((x) => x.length > 0);
  if (parts.length < 4 || parts[0] !== tag) return null;
  const history = Number(parts[1]);
  const cursorX = Number(parts[2]);
  const cursorY = Number(parts[3]);
  if (![history, cursorX, cursorY].every(Number.isInteger) || history < 0) return null;
  return { history, cursorX, cursorY };
}

function parseMode(lines: string[] | undefined): ModeState | null {
  if (!lines || lines.length === 0) return null;
  const parts = lines[0].split(/\s+/);
  if (parts.length === 0 || parts[0] !== 'M') return null;
  const num = (i: number): number | null => {
    const v = Number(parts[i]);
    return Number.isInteger(v) ? v : null;
  };
  const flag = (i: number): boolean | null => {
    const t = parts[i];
    if (t === '1') return true;
    if (t === '0') return false;
    return null;
  };
  return {
    alternateOn: parts[1] === '1',
    scrollTop: num(2),
    scrollBottom: num(3),
    wrap: flag(4),
    cursor: flag(5),
    insert: flag(6),
    keypadCursor: flag(7),
    keypad: flag(8),
    origin: flag(9),
  };
}

/** Render a command rejection for a reason string: keep ControlError
 * `.lines` (the %error reply lines) alongside the message, which alone
 * discards tmux's diagnosis. */
function errorText(err: unknown): string {
  const base = err instanceof Error ? err.message : String(err);
  const lines = (err as { lines?: unknown } | null)?.lines;
  if (Array.isArray(lines) && lines.length > 0) return `${base}: ${lines.join('; ')}`;
  return base;
}

/** Offending-line diagnostics for a parse failure: 'empty', or the first
 * line truncated so a giant capture line cannot bloat the reason. */
function describeLines(lines: string[] | undefined): string {
  if (!lines || lines.length === 0) return 'empty';
  const first = lines[0];
  return JSON.stringify(first.length > 120 ? `${first.slice(0, 120)}…` : first);
}

/** A mode line is malformed when parseMode could not extract a complete
 * state from it (wrong tag, non-numeric fields, partial flags). An empty
 * reply is missing, not malformed. */
function modeMalformed(lines: string[] | undefined, mode: ModeState | null): boolean {
  if (mode === null) return lines !== undefined && lines.length > 0;
  return (
    [mode.scrollTop, mode.scrollBottom, mode.wrap, mode.cursor, mode.insert,
      mode.keypadCursor, mode.keypad, mode.origin].some((v) => v === null)
  );
}

/** Mode restore bytes: DECSTBM, origin, other modes, then an absolute CUP
 * LAST (CSI r homes the cursor; with origin mode the row is relative to the
 * region top). tmux's scroll_region_upper/lower are 0-BASED: for a full
 * 30-row pane tmux reports (0, 29), so DECSTBM -- which is 1-based -- needs
 * +1 on both fields. CI run 42433387483 proved both halves of that: the
 * reseeded twin's scrollBottom was 28 (the raw 29 written as a 1-based
 * bottom) while a byte-faithful reference twin had 29. A full-window region
 * (0..rows-1) is deliberately NOT emitted: it is exactly the twin's default
 * scroll region and the round-trip only corrupts state.
 *
 * Under origin mode (DECOM), tmux's cursor_y is still the viewport-absolute
 * 0-based row, but the emitted CUP row is region-relative, so the restore
 * subtracts the region top (scrollTop) and clamps to row 1 (a cursor whose
 * row was outside the region homes to the region top under CUP); with
 * origin off the emitted row stays the absolute cursorY+1. */
export function modeRestoreBytes(mode: ModeState | null, cursorX: number, cursorY: number, rows: number): string {
  let s = '';
  let cupRow = cursorY + 1; // 1-based absolute row (tmux cursor_y is 0-based)
  if (mode) {
    const top = mode.scrollTop;
    const bottom = mode.scrollBottom;
    if (
      top !== null &&
      bottom !== null &&
      top <= bottom &&
      !(top === 0 && bottom === rows - 1)
    ) {
      s += `${ESC}[${top + 1};${bottom + 1}r`;
      if (mode.origin === true) cupRow = Math.max(1, cursorY - top + 1);
    }
    if (mode.origin !== null) s += `${ESC}[?6${mode.origin ? 'h' : 'l'}`;
    if (mode.wrap !== null) s += `${ESC}[?7${mode.wrap ? 'h' : 'l'}`;
    if (mode.cursor !== null) s += `${ESC}[?25${mode.cursor ? 'h' : 'l'}`;
    if (mode.insert !== null) s += `${ESC}[4${mode.insert ? 'h' : 'l'}`;
    if (mode.keypadCursor !== null) s += `${ESC}[?1${mode.keypadCursor ? 'h' : 'l'}`;
    if (mode.keypad !== null) s += mode.keypad ? `${ESC}=` : `${ESC}>`;
  }
  s += `${ESC}[${cupRow};${cursorX + 1}H`;
  return s;
}

function joinLines(lines: string[]): string {
  return lines.map((l) => (l.endsWith('\r') ? l.slice(0, -1) : l)).join('\r\n');
}

function countIn(haystack: Uint8Array, needle: Uint8Array): boolean {
  outer: for (let i = 0; i + needle.length <= haystack.length; i++) {
    for (let j = 0; j < needle.length; j++) {
      if (haystack[i + j] !== needle[j]) continue outer;
    }
    return true;
  }
  return false;
}

interface QuiescentState {
  lastOutputAt: number;
  frameEnded: boolean;
  /** When `?2026l` was last seen: a frame end counts as quiescent only if
   * fresh (at/after the wait start), not a stale tail survivor. */
  frameEndedAt: number;
  tail: Uint8Array;
}

export class Reseeder {
  private readonly control: ControlLike;
  private readonly twin: Twin;
  private readonly now: () => number;

  private readonly feedQueued: boolean;
  private readonly sleep: (ms: number) => Promise<void>;
  private readonly qstates = new Map<string, QuiescentState>();
  private readonly st: ReseedStats = {
    totalAttempts: 0,
    reseedCalls: 0,
    preABytes: 0,
    interleavedRetries: 0,
    abDisagreeRetries: 0,
    badMarkerRetries: 0,
    badModeRetries: 0,
    overflowRetries: 0,
    givesUp: 0,
    queuedDroppedBytes: 0,
    altUnsupported: 0,
    quiescentFalse: 0,
  };
  private active: {
    pane: string;
    queue: Uint8Array[];
    interleaved: boolean;
    aResolved: boolean;
    bResolved: boolean;
    closed: boolean;
  } | null = null;

  constructor(control: ControlLike, twin: Twin, opts?: ReseedOpts) {
    this.control = control;
    this.twin = twin;
    this.feedQueued = opts?.feedQueued ?? true;
    // Monotonic clock: only elapsed time matters here, and a wall-clock
    // backward step must not outlive a deadline.
    this.now = opts?.now ?? (() => performance.now());
    this.sleep = opts?.sleep ?? ((ms: number) => new Promise<void>((r) => setTimeout(r, ms)));
    control.onOutput((pane, bytes) => this.noteOutput(bytes, pane));
  }

  /** Track the post-transform stream for the quiescent wait; queue output
   * arriving during a reseed. Called by the control subscription (and
   * directly by tests). */
  noteOutput(bytes: Uint8Array, pane = ''): void {
    const q = this.qstate(pane);
    q.lastOutputAt = this.now();
    const grown = new Uint8Array(Math.min(q.tail.length + bytes.length, 128));
    grown.set(q.tail.subarray(0, Math.max(0, grown.length - bytes.length)));
    grown.set(bytes.subarray(Math.max(0, bytes.length - grown.length)), Math.max(0, grown.length - bytes.length));
    q.tail = grown;
    const frameNow = countIn(q.tail, FRAME_END_BYTES);
    if (frameNow && !q.frameEnded) {
      // Rising edge only: fresh bytes that merely follow a frame end must
      // not re-stamp it, or every later wait reads as instantly quiescent.
      q.frameEndedAt = this.now();
    }
    q.frameEnded = frameNow;
    const a = this.active;
    if (a && (pane === '' || pane === a.pane)) {
      if (a.closed) {
        // The reseed slot is closed: the normal pipeline feeds the twin
        // now; nothing is queued (and nothing is silently counted away).
      } else if (!a.aResolved) {
        // already inside the capture: drop silently, but count it.
        this.st.preABytes += bytes.length;
      } else if (!a.bResolved) {
        // between A's %begin and B's %end: this attempt cannot stand.
        a.interleaved = true;
      } else {
        // after B's %end: feed after the mode restore, with no gap.
        const copy = new Uint8Array(bytes.length);
        copy.set(bytes);
        a.queue.push(copy);
      }
    }
  }

  stats(): ReseedStats {
    return { ...this.st };
  }

  private qstate(pane: string): QuiescentState {
    let q = this.qstates.get(pane);
    if (!q) {
      q = { lastOutputAt: -Infinity, frameEnded: false, frameEndedAt: -Infinity, tail: new Uint8Array(0) };
      this.qstates.set(pane, q);
    }
    return q;
  }

  async waitQuiescent(pane: string, opts?: ReseedOpts): Promise<boolean> {
    // Per-call now/sleep win over the constructor's (tests inject a fake
    // clock here); ignoring them froze the wait forever.
    const now = opts?.now ?? this.now;
    const sleepFn = opts?.sleep ?? this.sleep;
    const qms = opts?.quiescentMs ?? QUIESCENT_MS;
    if (qms <= 0) return true;
    const max = opts?.quiescentMaxMs ?? QUIESCENT_MAX_MS;
    const q = this.qstate(pane);
    const start = now();
    for (;;) {
      const t = now();
      // A `?2026l` seen at/after this wait started that is ALSO the most
      // recent output proves the parser is back in ground state; a stale
      // one (in particular a frame end that landed BEFORE this wait
      // started, even within the previous qms) or one superseded by
      // fresh output falls through to the silence rule instead. There is
      // deliberately no start-qms grace window: a producer that ended
      // its burst milliseconds before reseed() must earn quiescence by
      // the silence rule, not by a frame end it already had.
      if (q.frameEndedAt >= start && q.frameEndedAt >= q.lastOutputAt) return true;
      if (t - q.lastOutputAt >= qms) return true;
      if (t - start >= max) return false;
      await sleepFn(Math.max(1, Math.min(5, qms)));
    }
  }

  async reseed(paneId: string, opts?: ReseedOpts): Promise<ReseededResult> {
    // paneId is spliced into tmux command lines verbatim: only tmux's own
    // `%N` form is accepted, anything else is rejected before any write.
    if (!PANE_ID_RE.test(paneId)) throw new ReseedError(`invalid pane id ${JSON.stringify(paneId)}`, 0);
    const maxAttempts = opts?.maxAttempts ?? MAX_RESEED_ATTEMPTS;
    this.st.reseedCalls++;
    const quiescent = await this.waitQuiescent(paneId, opts);
    if (!quiescent) this.st.quiescentFalse++;
    let lastReason = 'no attempt';
    for (let attempt = 1; attempt <= maxAttempts; attempt++) {
      this.st.totalAttempts++;
      const active = { pane: paneId, queue: [] as Uint8Array[], interleaved: false, aResolved: false, bResolved: false, closed: false };
      this.active = active;
      try {
        const result = await this.attempt(paneId, active);
        if (result.ok) {
          const out: ReseededResult = { attempts: attempt, quiescent, ...result.ok };
          opts?.onReseeded?.(out);
          return out;
        }
        lastReason = result.reason;
      } catch (err) {
        // Preserve ControlError.lines (%error reply lines): the message
        // alone discards tmux's diagnosis.
        lastReason = errorText(err);
      } finally {
        this.active = null;
        // Only count bytes as "dropped" when we owned feeding them
        // (feedQueued false: the caller's chain feeds them, so they are
        // not dropped at all).
        if (this.feedQueued) this.st.queuedDroppedBytes += active.queue.reduce((n, b) => n + b.length, 0);
      }
    }
    this.st.givesUp++;
    throw new ReseedError(lastReason, maxAttempts);
  }
  private async attempt(
    pane: string,
    active: { pane: string; queue: Uint8Array[]; interleaved: boolean; aResolved: boolean; bResolved: boolean; closed: boolean },
  ): Promise<{ ok?: Omit<ReseededResult, 'attempts' | 'quiescent'>; reason?: string }> {
    const aCmd = `display-message -p -t ${pane} 'A #{history_size} #{cursor_x} #{cursor_y}'`;
    const capture = `capture-pane -p -e -J -S - -t ${pane}`;
    const altCapture = `capture-pane -p -e -J -S - -a -t ${pane}`;
    const bCmd = `display-message -p -t ${pane} 'B #{history_size} #{cursor_x} #{cursor_y}'`;
    const modeCmd = `display-message -p -t ${pane} 'M ${MODE_FORMATS.map((f) => `#{${f}}`).join(' ')}'`;
    const ps = this.control.write([aCmd, capture, altCapture, bCmd, modeCmd]);
    ps[0].then(
      () => {
        active.aResolved = true;
      },
      () => {
        active.aResolved = true;
      },
    );
    ps[3].then(
      () => {
        active.bResolved = true;
      },
      () => {
        active.bResolved = true;
      },
    );
    // the -a capture must not abort a non-alt reseed if this tmux lacks it,
    // but its rejection is retained (with the %error lines) so an alt
    // reseed reports the real diagnosis, not an invented reason.
    let altError: unknown = null;
    const altPromise: Promise<string[] | null> = ps[2].then(
      (lines) => lines,
      (err: unknown) => {
        this.st.altUnsupported++;
        altError = err;
        return null;
      },
    );
    const [aLines, captureLines, altLines, bLines, modeLines] = await Promise.all([
      ps[0],
      ps[1],
      altPromise,
      ps[3],
      ps[4],
    ]);
    if (active.interleaved) {
      this.st.interleavedRetries++;
      return { reason: 'output interleaved between A and B' };
    }
    const a = parseMarker(aLines, 'A');
    const b = parseMarker(bLines, 'B');
    if (!a || !b) {
      this.st.badMarkerRetries++;
      // Carry the offending line (or 'empty') so a gives-up is diagnosable.
      const which = !a ? 'A' : 'B';
      return { reason: `unparseable ${which} marker: ${describeLines(!a ? aLines : bLines)}` };
    }
    if (a.history !== b.history || a.cursorX !== b.cursorX || a.cursorY !== b.cursorY) {
      this.st.abDisagreeRetries++;
      return { reason: 'A and B disagree' };
    }
    const mode = parseMode(modeLines);
    if (modeMalformed(modeLines, mode)) {
      // Malformed-but-present mode line: retry like a bad marker.
      this.st.badModeRetries++;
      return { reason: `unparseable mode line: ${describeLines(modeLines)}` };
    }
    if (mode === null) {
      // Empty reply: no evidence to retry on; counted (never thrown away
      // silently) and the restore proceeds CUP-only.
      this.st.badModeRetries++;
    }
    let bytesFed = 0;
    const feed = async (text: string): Promise<void> => {
      bytesFed += new TextEncoder().encode(text).length;
      await this.twin.feed(text);
    };
    const feedBytes = async (bytes: Uint8Array): Promise<void> => {
      bytesFed += bytes.length;
      await this.twin.feed(bytes);
    };
    const padTo = async (which: 'normal' | 'alt', expected: number): Promise<boolean> => {
      let n = this.twin.lineCount(which);
      if (n > expected) return false;
      if (n === expected) return true;
      // Blank-tail padding must append from the bottom row: CRLFs fed with
      // the cursor mid-screen only move the cursor down and never add a
      // line, so park it on the last row first. Each LF at the bottom then
      // scrolls exactly one blank row into the buffer.
      await feed(`${ESC}[${this.twin.rows};1H`);
      n = this.twin.lineCount(which);
      if (n > expected) return false;
      while (n < expected) {
        const before = n;
        await feed('\r\n'.repeat(Math.min(1024, expected - n)));
        n = this.twin.lineCount(which);
        if (n === before) return n === expected; // no progress (scrollback cap)
      }
      return n === expected;
    };
    this.twin.reset();
    if (mode?.alternateOn) {
      if (altLines === null) {
        return { reason: `alternate screen on but -a capture failed: ${errorText(altError)}` };
      }
      await feed(joinLines(altLines));
      if (!(await padTo('normal', b.history + this.twin.rows))) {
        this.st.overflowRetries++;
        return { reason: 'normal-buffer line count overflow' };
      }
      await feed(`${ESC}[?1049h`);
      await feed(joinLines(captureLines));
      if (!(await padTo('alt', this.twin.rows))) {
        this.st.overflowRetries++;
        return { reason: 'alt-buffer line count overflow' };
      }
    } else {
      await feed(joinLines(captureLines));
      if (!(await padTo('normal', b.history + this.twin.rows))) {
        this.st.overflowRetries++;
        return { reason: 'normal-buffer line count overflow' };
      }
    }
    await feed(modeRestoreBytes(mode, b.cursorX, b.cursorY, this.twin.rows));
    // Drain until the queue is empty, then close the slot in the same
    // synchronous turn: per spec §3 step 5, every %output after B's %end
    // is fed, in order, with no gap. A single for-of durably loses bytes
    // that land after its final length check (they were counted as
    // queuedDroppedBytes by the finally in reseed()), which makes the
    // emitted-reference comparison unwinnable under live flow. The
    // final empty check and the close have no await between them, so no
    // %output can land in the queue after the check: JS code runs
    // atomically between awaits. Anything arriving after the close is
    // post-reseed stream and feeds the twin via the normal pipeline.
    // With feedQueued false the caller (TapServer's twin chain) is the
    // single feeder: the queued raw bytes are left un-fed (the ring
    // already holds their post-withhold form), so the twin is fed exactly
    // once and never sees withheld bytes.
    while (this.feedQueued && active.queue.length > 0) {
      const batch = active.queue.splice(0, active.queue.length);
      for (const chunk of batch) await feedBytes(chunk);
    }
    active.closed = true;
    return { ok: { bytesFed, historySize: b.history, mode, a, b, altUnsupported: altLines === null } };
  }
}

/** One-shot convenience for tests that do not need a persistent reseeder. */
export async function reseed(control: ControlLike, twin: Twin, paneId: string, opts?: ReseedOpts): Promise<ReseededResult> {
  return new Reseeder(control, twin, opts).reseed(paneId, opts);
}
