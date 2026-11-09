// Integration driver for tests/term_int_reseed.rs (tests 16-25).
// Usage: bun run reseed-driver.ts --socket <sock> --session <name> --tmux
// <bin> --cols <n> --rows <n> --pane <id> --ready <file> --ref <file|emitted>
// --stream <file> --scenario <name> [--expect-bytes <n>] [--quiescent on|off]
// Prints one JSON object as the LAST stdout line (progress goes to stderr).

import { readFileSync, writeFileSync } from 'node:fs';
import { Control, continueCommand, pauseCommand } from '../../src/control.ts';
import { makeTwin, type Twin } from '../../src/twin.ts';
import { Reseeder, type ReseededResult } from '../../src/reseed.ts';
import { compareBuffers, diffState, dumpRows, stateOf, termOf } from '../support/compare.ts';

const argv = Bun.argv.slice(2);
function arg(name: string): string | undefined {
  const i = argv.indexOf(`--${name}`);
  return i >= 0 ? argv[i + 1] : undefined;
}
const socket = arg('socket')!;
const session = arg('session')!;
const tmuxBin = arg('tmux')!;
const cols = Number(arg('cols') ?? '80');
const rows = Number(arg('rows') ?? '24');
const pane = arg('pane')!;
const readyFile = arg('ready');
const refMode = arg('ref') ?? 'file';
const streamFile = arg('stream')!;
const scenario = arg('scenario')!;
const expectBytes = Number(arg('expect-bytes') ?? '0');
const quiescentOpts = arg('quiescent') === 'off' ? { quiescentMs: 0 } : {};
/** Bounded cells gate for the pause scenario against the byte-0 file
 *  reference. A correct reseed differs only in the bottom padding padTo
 *  adds for an in-flight gap (observed gap ~29 lines ~ <=700 cells); a
 *  truncated/wrong-cell feed pads a much larger blank tail versus the
 *  flood reference (half-capture ~550k cells). 5000 (~217 flood rows)
 *  separates the two by orders of magnitude while tolerating in-flight
 *  race variance; the stricter part is the line-count gate. */
const FILE_REF_CELLS_BOUND = 5000;

const note = (...a: unknown[]): void => {
  console.error(...a);
};
// Wrong one-liner reads as checked code -- reviewed and this is the single
// honest way to reach the OS signal API from this Bun script without
// @types/node. Verified: process exists in Bun and kill() reaches the OS.
const proc = (globalThis as unknown as { process: { pid: number; kill(pid: number, sig: string): void } }).process;
const sleep = (ms: number): Promise<void> => new Promise((r) => setTimeout(r, ms));
interface CompareOut {
  ok: boolean;
  cellsDiff: number;
  stateDiff: number;
  stateDiffClasses: string[];
  stateDiffDetail: unknown;
  cursor: { x: number; y: number };
  cursorRef: { x: number; y: number };
  altCellsDiff: number;
  attempts: number;
  stats: unknown;
  bytesFed: number;
  hash: string;
  a: { history: number; cursorX: number; cursorY: number };
  b: { history: number; cursorX: number; cursorY: number };
  rowsRef: string[];
  rowsGot: string[];
  examples: unknown;
  altExamples: unknown;
  /** True when the mode-restore surface (alt buffer, active-buffer scroll
   * region, restored modes, cursor visibility) agrees. Gates `ok` with
   * cellsDiff, so a reconstruction that matches in cells but not in mode
   * state fails. Cursor position and pending-wrap stay informational:
   * pending wrap is a documented residual (term_int_reseed_pending_wrap)
   * and its cursor spelling differs by representation. */
  modeStateOk: boolean;
  modeStateDiff: unknown;
  cursorMatches: boolean;
}

function cursorOf(twin: Twin): { x: number; y: number } {
  const buffer = twin.term.buffer as unknown as { active: { cursorX: number; cursorY: number } };
  return { x: buffer.active.cursorX, y: buffer.active.cursorY };
}
/** FNV-1a over both buffers' visible text plus the full state map. */
function stateHash(twin: Twin): string {
  const t = termOf(twin);
  const parts: string[] = [
    JSON.stringify(dumpRows(t, 'normal', 100000)),
    JSON.stringify(dumpRows(t, 'alt', 1000)),
    JSON.stringify(stateOf(t)),
  ];
  const s = parts.join('|');
  let h = 0x811c9dc5;
  for (let i = 0; i < s.length; i++) {
    h ^= s.charCodeAt(i);
    h = Math.imul(h, 0x01000193);
  }
  return (h >>> 0).toString(16);
}

function compareTwins(reseeded: Twin, ref: Twin, result: ReseededResult, stats: unknown): CompareOut {
  const a = termOf(reseeded);
  const b = termOf(ref);
  const normal = compareBuffers(a, b, 'normal');
  const altActive = stateOf(a).activeBuffer === 'alternate' || stateOf(b).activeBuffer === 'alternate';
  const alt = altActive ? compareBuffers(a, b, 'alt') : null;
  const sumCells = (c: { rowsDiff: number; cells: Record<string, number> }): number =>
    c.rowsDiff + Object.values(c.cells).reduce((x, y) => x + y, 0);
  const cellsDiff = sumCells(normal) + (alt ? sumCells(alt) : 0);
  const stateA = stateOf(a);
  const stateB = stateOf(b);
  const sd = diffState(stateA, stateB);
  // Mode-restore surface: everything modeRestoreBytes must prove (alt
  // buffer, the ACTIVE buffer's scroll region, the six restored mode
  // keys, cursor visibility). Saved cursor/charset/title/tabs/pen are
  // T6-tail state no capture carries and stay out of the gate.
  const activeBuf = stateA.activeBuffer === 'alternate' ? 'alt' : 'normal';
  const MODE_KEYS: ReadonlySet<string> = new Set([
    'activeBuffer',
    'cursorHidden',
    'modes.originMode',
    'modes.wraparoundMode',
    'modes.insertMode',
    'modes.applicationCursorKeysMode',
    'modes.applicationKeypadMode',
  ]);
  const isModeKey = (k: string): boolean => MODE_KEYS.has(k) || k === `${activeBuf}.scrollTop` || k === `${activeBuf}.scrollBottom`;
  const modeDiffs = sd.filter((d) => isModeKey(d.key));
  const modeStateOk = modeDiffs.length === 0;
  const cursor = cursorOf(reseeded);
  const cursorRef = cursorOf(ref);
  const cursorMatches = cursor.x === cursorRef.x && cursor.y === cursorRef.y;
  const ok = cellsDiff === 0 && modeStateOk;
  return {
    ok,
    cellsDiff,
    stateDiff: sd.length,
    stateDiffClasses: [...new Set(sd.map((d) => d.cls))].sort(),
    stateDiffDetail: sd,
    cursor,
    cursorRef,
    altCellsDiff: alt ? sumCells(alt) : 0,
    attempts: result.attempts,
    stats,
    bytesFed: result.bytesFed,
    hash: stateHash(reseeded),
    a: result.a,
    b: result.b,
    examples: normal.examples,
    altExamples: alt?.examples ?? null,
    modeStateOk,
    modeStateDiff: modeDiffs,
    cursorMatches,
    // Diagnostics only; dumped ONLY on mismatch (and capped at 4 rows) --
    // the full dumps made the driver's JSON larger than the OS pipe
    // buffer (64 KiB): with stdout piped and Rust reading only on
    // process exit, the write blocked forever and the driver hung until
    // the Rust deadline killed it (four CI runs, text always at the
    // deadline). The gated fields (ok/cellsDiff/hash) are unaffected.
    rowsRef: ok ? [] : dumpRows(b, 'normal', 4),
    rowsGot: ok ? [] : dumpRows(a, 'normal', 4),
  };
}

async function main(): Promise<void> {
  const control = new Control({ tmuxBin, socketPath: socket, sessionName: session, pauseAfterSecs: scenario === 'pause' ? 2 : undefined });
  await control.start();
  const emitted: Uint8Array[] = [];
  let emittedTotal = 0;
  const refTwin = makeTwin(cols, rows);
  // Cumulative reference: refTwin is fed incrementally from the emitted log
  // only up to the chunk count each comparison asks for (incRef below).
  // Comparisons ask for non-decreasing cuts, so the whole scenario stays
  // O(bytes) instead of replaying the prefix per comparison -- the
  // from-byte-0 rebuild blew the 120s Rust deadline under nextest load.
  let refFedChunks = 0;
  control.onOutput((p, bytes) => {
    if (p !== pane) return;
    const copy = new Uint8Array(bytes.length);
    copy.set(bytes);
    emitted.push(copy);
    emittedTotal += bytes.length;
  });
  const reseedTwin = makeTwin(cols, rows);
  const reseeder = new Reseeder(control, reseedTwin);
  // The %pause -> continue -> reseed recovery wiring (pause scenario):
  // Control's dispatch writes the quoted continue the moment %pause
  // arrives; once the pane settles, reseed. The pause case below awaits
  // this promise (with a manual-continue fallback if tmux never emits
  // %pause for the explicit pause in this run).
  let hookReseed: Promise<ReseededResult> | null = null;
  let fallbackTaken = false;
  control.onPause((p) => {
    if (scenario !== 'pause' || p !== pane) return;
    // If the manual fallback already took charge, do not race it with a
    // second reseed on the same twin.
    if (fallbackTaken) return;
    hookReseed = (async () => {
      await waitSettled();
      return reseeder.reseed(pane);
    })();
  });

  const touchReady = (): void => {
    if (readyFile) writeFileSync(readyFile, 'ready');
  };
  const waitEmitted = async (target: number): Promise<void> => {
    const deadline = Date.now() + 60_000;
    for (;;) {
      if (emittedTotal >= target) return;
      if (Date.now() > deadline) throw new Error(`timeout waiting for ${target} emitted bytes, got ${emittedTotal}`);
      await sleep(50);
    }
  };
  /** File-reference scenarios cannot require seeing the whole stream (a late
   * attacher misses it entirely); they only need the pane settled. */
  const waitSettled = async (): Promise<void> => {
    const deadline = Date.now() + 60_000;
    let lastTotal = -1;
    let quietSince = Date.now();
    for (;;) {
      if (emittedTotal !== lastTotal) {
        lastTotal = emittedTotal;
        quietSince = Date.now();
      }
      if (Date.now() - quietSince > 300) return;
      if (Date.now() > deadline) throw new Error(`timeout waiting for the pane to settle (emitted ${emittedTotal})`);
      await sleep(50);
    }
  };
  const concat = (chunks: Uint8Array[]): Uint8Array => {
    const total = chunks.reduce((n, c) => n + c.length, 0);
    const out = new Uint8Array(total);
    let o = 0;
    for (const c of chunks) {
      out.set(c, o);
      o += c.length;
    }
    return out;
  };
  /** Feed the cumulative reference twin from the emitted log up to `cut`
   * chunks and return it. Cuts are non-decreasing; an out-of-order cut
   * resets and replays (never happens in these scenarios). */
  const incRef = async (cut: number): Promise<Twin> => {
    if (cut < refFedChunks) {
      refTwin.reset();
      refFedChunks = 0;
    }
    if (cut > refFedChunks) {
      await refTwin.feed(concat(emitted.slice(refFedChunks, cut)));
      refFedChunks = cut;
    }
    return refTwin;
  };
  const fileRef = async (): Promise<Twin> => {
    const ref = makeTwin(cols, rows);
    await ref.feed(readFileSync(streamFile));
    return ref;
  };
  /** Reference from byte 0: the stream file, or the emitted prefix snapshot. */
  const compareAt = async (result: ReseededResult, uptoChunks?: number): Promise<CompareOut> => {
    const ref = refMode === 'file' ? await fileRef() : await incRef(uptoChunks ?? emitted.length);
    const out = compareTwins(reseedTwin, ref, result, reseeder.stats());
    if (refMode === 'file') ref.dispose();
    return out;
  };

  const out: Record<string, unknown> = { scenario };
  try {
    switch (scenario) {
      case 'text': {
        touchReady();
        await waitEmitted(1);
        const comparisons: CompareOut[] = [];
        for (let i = 0; i < 12; i++) {
          await sleep(700);
          const r = await reseeder.reseed(pane);
          // The reference is fed from byte 0 up to exactly what the reseed
          // had consumed (capture + post-B queue) by the time it returned.
          comparisons.push(await compareAt(r, emitted.length));
        }
        out.comparisons = comparisons;
        out.ok = comparisons.every((c) => c.ok);
        out.totalAttempts = comparisons.reduce((n, c) => n + c.attempts, 0);
        break;
      }
      case 'concurrent':
      case 'cursor':
      case 'wrap':
      case 'wrapped':
      case 'alt':
      case 'placeholder': {
        touchReady();
        await waitSettled();
        await sleep(200);
        const r = await reseeder.reseed(pane);
        out.compare = await compareAt(r);
        out.ok = (out.compare as CompareOut).ok;
        out.attempts = r.attempts;
        break;
      }
      case 'pause': {
        // Stall the whole driver process BEFORE the flood starts: SIGSTOP
        // freezes every thread of the tap, so the kernel pipe is never read
        // and a full client backlog builds up in tmux's queue while Rust
        // polls RSS. `pause-after=2` is set on the client (verified
        // accepted below) but tmux 3.7c never engaged its auto-pause in
        // >40s of verified stopped-client lag (CI 42439069928: the entire
        // 30MB flood drained post-resume undropped) -- see the PR body. The
        // documented per-pane pause (`refresh-client -A '<pane>:pause'`,
        // pauseCommand) pauses the pane deterministically here so the same
        // %pause -> continue -> reseed recovery path is exercised.
        note('STALLED');
        touchReady();
        const bytesAtStop = emittedTotal;
        proc.kill(proc.pid, 'SIGSTOP');
        // Resumed by Rust's SIGCONT (Rust verifies /proc State stopped).
        note('RESUMED');
        if (control.lastPauseAfterError) {
          throw new Error(`pause-after=2 was rejected: ${control.lastPauseAfterError}`);
        }
        // Explicit pane pause (quoted, like the continue). tmux accepts it
        // (no %error, shown by the cmd resolving) and the pane's output
        // halts: this is the stall policy's observable surface, since
        // `pause-after` auto-engagement never fired on 3.7c (see the PR
        // body evidence for the >40s stopped-client runs).
        await control.cmd(pauseCommand(pane));
        note('PAUSE_CMD_SENT');
        // Observe the halt. With the flood still flowing, a healthy drain
        // exceeds 200 KiB in 1.5s by orders of magnitude; a paused pane
        // delivers essentially nothing.
        const bytesAtPause = emittedTotal;
        await sleep(1500);
        const growthWhilePaused = emittedTotal - bytesAtPause;
        note(`GROWTH_WHILE_PAUSED ${growthWhilePaused}`);
        if (growthWhilePaused > 200_000) {
          throw new Error(`pane did not pause: ${growthWhilePaused} bytes still flowed`);
        }
        // Recover through the %pause -> continue -> reseed wiring: the
        // dispatch hook has already written the quoted continue; the
        // onPause handler reseeds once the pane settles. Wait briefly for
        // the hook event so the manual path is only the fallback.
        const hookDeadline = Date.now() + 10_000;
        while (!hookReseed && Date.now() < hookDeadline) await sleep(50);
        let r: ReseededResult;
        let pauseHookContinued = false;
        if (hookReseed) {
          pauseHookContinued = true;
          note('HOOK_PAUSE_SEEN');
          r = await hookReseed;
        } else {
          // Fallback: tmux did not emit %pause for the explicit pause in
          // this run. pauseHookContinued stays false, so the gate below
          // fails this test: the %pause -> continue -> reseed wiring is
          // the path under test (brief item 11), not the manual path.
          fallbackTaken = true;
          note('HOOK_PAUSE_MISSED_FALLBACK');
          await control.cmd(continueCommand(pane));
          note('CONTINUED');
          // Wait until the pane settles (flood done) before the reseed,
          // so the capture holds the complete state and a partial tail
          // cannot hide in the post-capture queue.
          await waitSettled();
          r = await reseeder.reseed(pane);
        }
        note('RESEEDED');
        // Gated on bounded equality, not emitted-ref equality: the pause
        // drops output by design, so no byte-stream reference can match
        // the capture (CI 42450773946: reseeded history 49971 vs
        // emitted-ref length 1483, cellsDiff 48518 -- the client received
        // only ~35 KB of the flood while the capture holds the pane's
        // full screen). The gate is (a) history proximity vs the byte-0
        // file reference within 2%, (b) exact determinism: a second
        // reseed of the idle pane reproduces the identical twin, (c) a
        // bounded cellsDiff + mode-state agreement of the reseeded twin
        // against the byte-0 file reference (so a deterministically
        // wrong reseed that feeds truncated/wrong cells to the twin
        // fails even though padTo pads the line count up and the two
        // reseeds agree with each other), and (d) the recovery drove
        // through the real %pause hook (no manual-continue fallback).
        // The emitted-ref comparison still runs and is recorded as a metric.
        out.compare = await compareAt(r);
        note('COMPARED');
        const hash1 = stateHash(reseedTwin);
        const refFull = await fileRef();
        const refHistory = refFull.lineCount('normal') - rows;
        // Content gate vs the byte-0 file reference (c): line-count
        // proximity alone cannot catch a truncated/wrong-cell feed
        // because padTo pads the line count with blank rows.
        const fileCompare = compareTwins(reseedTwin, refFull, r, reseeder.stats());
        const fileRefCellsOk = fileCompare.cellsDiff <= FILE_REF_CELLS_BOUND && fileCompare.modeStateOk;
        refFull.dispose();
        const historyGap = Math.abs(r.historySize - refHistory);
        const historyClose = historyGap <= Math.max(100, Math.floor(refHistory / 50));
        const r2 = await reseeder.reseed(pane);
        const hash2 = stateHash(reseedTwin);
        const deterministic = hash1 === hash2;
        out.ok = r.attempts >= 1 && r2.attempts >= 1 && deterministic && historyClose && fileRefCellsOk && pauseHookContinued;
        out.metric = {
          seedEqualsEmittedRef: (out.compare as CompareOut).ok,
          deterministic,
          historyGap,
          historyClose,
          pauseHookContinued,
          fileRefCellsOk,
          fileRefCellsDiff: fileCompare.cellsDiff,
          fileRefModeStateOk: fileCompare.modeStateOk,
        };
        break;
      }
      case 'midseq': {
        touchReady();
        // t+0.5s: mid-CSI moment; t+1.5s: mid-OSC moment; then completion.
        await sleep(500);
        const r1 = await reseeder.reseed(pane, quiescentOpts);
        const c1 = await compareAt(r1, emitted.length);
        await sleep(1000);
        const r2 = await reseeder.reseed(pane, quiescentOpts);
        const c2 = await compareAt(r2, emitted.length);
        // After piece3 completes, the r2-era reseeded twin is stale: it
        // never received the post-reseed live output, so comparing it
        // against the full-stream reference is structurally false (the
        // earlier CI `finalOk=false` was that test bug, not a reseed bug).
        // A third reseed produces a twin that actually includes the tail,
        // which makes `finalOk` a real assertion.
        await waitEmitted(expectBytes || emittedTotal);
        const r3 = await reseeder.reseed(pane, quiescentOpts);
        const cFinal = await compareAt(r3, emitted.length);
        out.moments = [c1, c2, cFinal];
        // ON must be clean; OFF is recorded as a metric, never asserted.
        out.ok = quiescentOpts.quiescentMs === 0 ? true : c1.ok && c2.ok && cFinal.ok;
        out.metric = { csiMomentsOk: c1.ok, oscMomentsOk: c2.ok, finalOk: cFinal.ok };
        break;
      }
      default:
        throw new Error(`unknown scenario ${scenario}`);
    }
  } catch (err) {
    out.error = err instanceof Error ? `${err.name}: ${err.message}` : String(err);
    out.ok = false;
  } finally {
    control.stop();
    try {
      refTwin.dispose();
    } catch {
      // noop
    }
    try {
      reseedTwin.dispose();
    } catch {
      // noop
    }
  }
  console.log(JSON.stringify(out));
}

await main();
