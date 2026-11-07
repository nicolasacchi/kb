// Integration driver for tests/term_int_reseed.rs (tests 16-25).
// Usage: bun run reseed-driver.ts --socket <sock> --session <name> --tmux
// <bin> --cols <n> --rows <n> --pane <id> --ready <file> --ref <file|emitted>
// --stream <file> --scenario <name> [--expect-bytes <n>] [--quiescent on|off]
// Prints one JSON object as the LAST stdout line (progress goes to stderr).

import { readFileSync, writeFileSync } from 'node:fs';
import { Control } from '../../src/control.ts';
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

const note = (...a: unknown[]): void => {
  console.error(...a);
};
const sleep = (ms: number): Promise<void> => new Promise((r) => setTimeout(r, ms));

interface CompareOut {
  ok: boolean;
  cellsDiff: number;
  stateDiff: number;
  stateDiffClasses: string[];
  cursor: { x: number; y: number };
  cursorRef: { x: number; y: number };
  altCellsDiff: number;
  attempts: number;
  stats: unknown;
  bytesFed: number;
  hash: string;
  a: { history: number; cursorX: number; cursorY: number };
  b: { history: number; cursorX: number; cursorY: number };
  examples: unknown;
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
  const sd = diffState(stateOf(a), stateOf(b));
  return {
    ok: cellsDiff === 0,
    cellsDiff,
    stateDiff: sd.length,
    stateDiffClasses: [...new Set(sd.map((d) => d.cls))].sort(),
    cursor: cursorOf(reseeded),
    cursorRef: cursorOf(ref),
    altCellsDiff: alt ? sumCells(alt) : 0,
    attempts: result.attempts,
    stats,
    bytesFed: result.bytesFed,
    hash: stateHash(reseeded),
    a: result.a,
    b: result.b,
    examples: normal.examples,
  };
}

async function main(): Promise<void> {
  const control = new Control({ tmuxBin, socketPath: socket, sessionName: session, pauseAfterSecs: scenario === 'pause' ? 2 : undefined });
  await control.start();
  const emitted: Uint8Array[] = [];
  let emittedTotal = 0;
  const refTwin = makeTwin(cols, rows);
  let refChain: Promise<void> = Promise.resolve();
  control.onOutput((p, bytes) => {
    if (p !== pane) return;
    const copy = new Uint8Array(bytes.length);
    copy.set(bytes);
    emitted.push(copy);
    emittedTotal += bytes.length;
    refChain = refChain.then(() => refTwin.feed(copy));
  });
  const reseedTwin = makeTwin(cols, rows);
  const reseeder = new Reseeder(control, reseedTwin);

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
  const freshRef = async (uptoChunks: number): Promise<Twin> => {
    const ref = makeTwin(cols, rows);
    await ref.feed(concat(emitted.slice(0, uptoChunks)));
    return ref;
  };
  const fileRef = async (): Promise<Twin> => {
    const ref = makeTwin(cols, rows);
    await ref.feed(readFileSync(streamFile));
    return ref;
  };
  /** Reference from byte 0: the stream file, or the emitted prefix snapshot. */
  const compareAt = async (result: ReseededResult, uptoChunks?: number): Promise<CompareOut> => {
    const ref = refMode === 'file' ? await fileRef() : await freshRef(uptoChunks ?? emitted.length);
    const out = compareTwins(reseedTwin, ref, result, reseeder.stats());
    ref.dispose();
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
        // Stall the tap BEFORE the flood starts: unread output fills tmux's
        // client buffer, pause-after=2 fires, tmux pauses and drops the
        // flooder. Rust polls tmux RSS in this window.
        control.setHeld(true);
        note('STALLED');
        touchReady();
        await sleep(3500);
        const { promise: pausedP, resolve: pausedResolve } = Promise.withResolvers<string>();
        control.onPause((p) => {
          pausedResolve(p);
        });
        control.setHeld(false);
        await pausedP;
        note('PAUSED');
        const r = await reseeder.reseed(pane);
        note('RESEEDED');
        out.compare = await compareAt(r);
        note('COMPARED');
        out.ok = (out.compare as CompareOut).ok;
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
        await waitEmitted(expectBytes || emittedTotal);
        await refChain;
        const cFinal = await compareAt(r2, emitted.length);
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
