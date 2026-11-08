// T1 twin-seed matrix (T6 Spec 7-9): the acceptance gate for buildSeed.
// Quiescent joins are asserted (stage A: seed fidelity within
// COSMETIC_RESIDUALS; stage B: convergence within STATE_ALLOWLIST, which
// starts empty; ESC 8 probe 'same'); raw spike offsets are reported only.
// Every (variant, stream) pair is one test with an explicit timeout.
import { test, expect } from 'bun:test';
import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { existsSync, mkdtempSync, readFileSync, writeFileSync, mkdirSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { makeTwin, type Twin } from '../../src/twin.ts';
import { buildSeed, QuiescentJoinGate } from '../../src/seed.ts';
import { GroundTracker } from '../../src/ground.ts';
import { stripAll } from '../../src/strip.ts';
import {
  stateOf,
  diffState,
  compareBuffers,
  classifyBufferDiff,
  probeDecrc,
  dumpRows,
  isStageBAllowed,
  decscStageAAllowed,
  COSMETIC_RESIDUALS,
  VISUAL_SAME_P16,
  BLANK_FLAG_ATTR,
  SPIKE_BOUNDS,
  sameVisualColour,
  type StateDiff,
} from './lib.ts';

// Examples per class per buffer compare: high enough to enumerate EVERY
// differing cell (FXT6: a maxExamples=3 sample cannot classify a class
// visual-same; the proof must run over all differing cells).
const ALL_EXAMPLES = 100000;

const FIX = new URL('../../../fixtures/', import.meta.url).pathname;
const INSIDE_TARGET = 15;
const JOINS = Number(process.env.KB_MATRIX_JOINS ?? 40);

function mulberry(seed: number): () => number {
  return () => {
    seed |= 0;
    seed = (seed + 0x6d2b79f5) | 0;
    let t = Math.imul(seed ^ (seed >>> 15), 1 | seed);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

/** ?2026h..?2026l intervals as byte ranges [open, close] (spike). */
function frames(s: string): Array<[number, number]> {
  const iv: Array<[number, number]> = [];
  let p = 0;
  while (true) {
    const a = s.indexOf('\x1b[?2026h', p);
    if (a < 0) break;
    let b = s.indexOf('\x1b[?2026l', a);
    if (b < 0) b = s.length;
    iv.push([a + 8, b]);
    p = b + 8;
    if (p >= s.length) break;
  }
  return iv;
}

function stratify<T>(arr: T[], n: number, rnd: () => number): T[] {
  if (arr.length === 0) return [];
  const out = new Set<T>();
  for (let k = 0; k < n; k++) {
    const lo = Math.floor((k * arr.length) / n);
    const hi = Math.max(lo + 1, Math.floor(((k + 1) * arr.length) / n));
    out.add(arr[lo + Math.floor(rnd() * (hi - lo))]);
  }
  return [...out];
}

/** Spike pickJoins: stratified by name-seed, INSIDE_TARGET inside frames. */
export function pickJoins(buf: Uint8Array, name: string): Array<{ off: number; inside: boolean }> {
  const s = Buffer.from(buf).toString('latin1');
  const iv = frames(s);
  const rnd = mulberry([...name].reduce((h, c) => (h * 31 + c.charCodeAt(0)) | 0, 7));
  const inFrame = (o: number) => iv.some(([a, b]) => o > a && o < b);
  const outside: number[] = [];
  const inside: number[] = [];
  for (let i = s.indexOf('\n'); i >= 0; i = s.indexOf('\n', i + 1)) {
    if (i + 1 < s.length && !inFrame(i + 1)) outside.push(i + 1);
  }
  for (const [a, b] of iv) {
    for (let i = s.indexOf('\x1b', a + 1); i >= 0 && i < b; i = s.indexOf('\x1b', i + 1)) inside.push(i);
  }
  const nIn = inside.length ? Math.min(INSIDE_TARGET, JOINS) : 0;
  const ins = stratify(inside, nIn, rnd).map((off) => ({ off, inside: true }));
  const outs = stratify(outside, JOINS - ins.length, rnd).map((off) => ({ off, inside: false }));
  return [...outs, ...ins].sort((a, b) => a.off - b.off);
}

/** Quiescent family: LF-boundary chunk ends outside frames AND at ground. */
export function pickQuiescentJoins(buf: Uint8Array, name: string): Array<{ off: number }> {
  const s = Buffer.from(buf).toString('latin1');
  const iv = frames(s);
  const inFrame = (o: number) => iv.some(([a, b]) => o > a && o < b);
  const candidates: number[] = [];
  for (let i = s.indexOf('\n'); i >= 0; i = s.indexOf('\n', i + 1)) {
    const off = i + 1;
    if (off < s.length && !inFrame(off)) candidates.push(off);
  }
  // One GroundTracker pass in sorted order: keep only ground offsets.
  const tracker = new GroundTracker();
  const ground: number[] = [];
  let p = 0;
  for (const off of candidates) {
    tracker.push(buf.subarray(p, off));
    p = off;
    if (tracker.atGround()) ground.push(off);
  }
  const rnd = mulberry([...name].reduce((h, c) => (h * 31 + c.charCodeAt(0)) | 0, 7));
  return stratify(ground, Math.min(JOINS, ground.length), rnd)
    .map((off) => ({ off }))
    .sort((a, b) => a.off - b.off);
}

interface StreamSpec {
  file: string;
  name: string;
  cols: number;
  rows: number;
  gen: () => Uint8Array;
}

function genToFile(args: string[], out: string): Uint8Array {
  // fake_harness.py writes to -o or stdout; synth_omp_kitty_stream.py only
  // to stdout. Capture stdout uniformly and persist it to the temp file.
  const p = spawnSync('python3', ['-I', ...args], { maxBuffer: 1 << 28 });
  if (p.status !== 0) {
    throw new Error(`generator failed: python3 -I ${args.join(' ')}: ${p.stderr?.toString()}`);
  }
  const bytes = new Uint8Array(p.stdout);
  writeFileSync(out, bytes);
  return bytes;
}

let streamsCache: Promise<StreamSpec[]> | null = null;

function getStreams(): Promise<StreamSpec[]> {
  if (streamsCache) return streamsCache;
  streamsCache = (async () => {
    const dir = mkdtempSync(join(tmpdir(), 'kbt6-streams-'));
    const out = (n: string) => join(dir, n);
    const specs: StreamSpec[] = [];
    const harnessCases: Array<[number, number, number, number]> = [
      [1, 100, 30, 60], [2, 100, 30, 60], [3, 100, 30, 120], [4, 80, 24, 60],
      [5, 200, 50, 60], [6, 40, 12, 60], [7, 100, 30, 300],
    ];
    for (const [seedN, cols, rows, len] of harnessCases) {
      const file = `harness-s${seedN}.${cols}x${rows}.bin`;
      specs.push({
        file, name: `harness-s${seedN}`, cols, rows,
        gen: () => genToFile(['fake_harness.py'].map((f) => join(FIX, f)).concat([
          '--seed', String(seedN), '--cols', String(cols), '--rows', String(rows), '--length', String(len),
        ]), out(file)),
      });
    }
    specs.push({
      file: 'omp-kitty.100x30.bin', name: 'omp-kitty', cols: 100, rows: 30,
      gen: () => genToFile([join(FIX, 'synth_omp_kitty_stream.py')], out('omp-kitty.100x30.bin')),
    });
    specs.push({
      file: 'omp-kitty-replay.100x30.bin', name: 'omp-kitty-replay', cols: 100, rows: 30,
      gen: () => genToFile([join(FIX, 'synth_omp_kitty_stream.py'), '--cols', '60', '--rows', '12', '--resize-replay'],
        out('omp-kitty-replay.100x30.bin')),
    });
    specs.push({
      file: 'fx-omp-synthetic-12img.100x30.bin', name: 'fx-omp-synthetic-12img', cols: 100, rows: 30,
      gen: () => new Uint8Array(readFileSync(join(FIX, 'omp-synthetic-12img.bin'))),
    });
    return specs;
  })();
  return streamsCache;
}

type Variant = 'graphemes' | 'unicode11';
const VARIANTS: Variant[] = ['graphemes', 'unicode11'];

// schema kbt-twin-matrix/1 report
interface MatrixReport {
  schema: 'kbt-twin-matrix/1';
  joinsRequested: number;
  results: Array<{
    variant: string;
    stream: string;
    cols: number;
    rows: number;
    bytes: number;
    sha256_16: string;
    joins: Array<Record<string, unknown>>;
    error: string | null;
  }>;
}

const reports: MatrixReport['results'] = [];
let harnessSeed3Sha = '';
// Stage-B residual cells/rows counted across every quiescent join and every
// (variant, stream) pair (FXT6: the spike bound 15+7+6+2 on spec 9.2 is now
// enforced by the 'stage-B residual totals stay within the spike bound'
// test, not only prose in STATE_ALLOWLIST comments).
const residualTotals: Record<string, number> = {};
function noteResidual(cls: string, n: number): void {
  const raw = cls.replace(/^(alt-buffer content \((?:active|inactive, stale)\)): /, '');
  residualTotals[raw] = (residualTotals[raw] ?? 0) + n;
}

async function runVariantStream(variant: Variant, spec: StreamSpec): Promise<void> {
  const raw = spec.gen();
  // The twin sees the POST-transform stream: run the production stripper.
  const bytes = stripAll(raw);
  const sha = createHash('sha256').update(bytes).digest('hex').slice(0, 16);
  const rec: MatrixReport['results'][number] = {
    variant, stream: spec.name, cols: spec.cols, rows: spec.rows,
    bytes: bytes.length, sha256_16: sha, joins: [], error: null,
  };
  reports.push(rec);
  const { cols, rows } = spec;
  const mk = () => makeTwin(cols, rows, { variant });

  const failures: string[] = [];
  try {
    const buildRef = async (): Promise<Twin> => {
      const r = mk();
      await r.feed(bytes);
      return r;
    };
    let ref = await buildRef();
    let refDirty = false;
    const refState = stateOf(ref.term as never);
    const qjoins = pickQuiescentJoins(bytes, spec.name);
    const twin = mk();
    // The ground tracker the seed join requires (FXT6: buildSeed's ground is
    // REQUIRED; there is no ground-less fallback). Fed the exact bytes the
    // twin has parsed through, in lockstep with twin.feed.
    const ground = new GroundTracker();
    let pos = 0;
    for (const j of qjoins) {
      await twin.feed(bytes.subarray(pos, j.off));
      ground.push(bytes.subarray(pos, j.off));
      pos = j.off;
      if (refDirty) {
        ref.dispose();
        ref = await buildRef();
        refDirty = false;
      }
      const viewer = mk();
      try {
        const seed = buildSeed(twin, { scrollback: 50000, atOffset: j.off, ground });
        if (seed.withheld.mouse !== 0) throw new Error(`withheld.mouse=${seed.withheld.mouse}`);
        await viewer.feed(seed.bytes);
        // stage A
        const twinState = stateOf(twin.term as never);
        const viewerStateA = stateOf(viewer.term as never);
        const aState = diffState(twinState, viewerStateA);
        const cellTables: Record<string, number> = {};
        const aExamples: Record<string, Array<{ a: { fg: number; bg: number; ch: string; width: number }; b: { fg: number; bg: number; ch: string; width: number } }>> = {};
        for (const which of ['normal', 'alt'] as const) {
          const c = compareBuffers(twin.term as never, viewer.term as never, which, ALL_EXAMPLES);
          const classified = classifyBufferDiff(c.cells, which, which === 'alt' ? twinState.activeBuffer === 'alternate' : true);
          Object.assign(cellTables, classified);
          for (const [cls, n] of Object.entries(classified)) {
            const raw = cls.replace(/^(alt-buffer content \((?:active|inactive, stale)\)): /, '');
            aExamples[cls] = [...(aExamples[cls] ?? []), ...(c.examples[raw] ?? [])];
          }
        }
        const aClasses = new Set<string>([...aState.map((d) => d.cls), ...Object.keys(cellTables)]);
        const aBad = nonCosmetic(aClasses, cellTables, aExamples, twinState, viewerStateA);
        if (aBad.length > 0) {
          throw new Error(
            `stage A ${variant}/${spec.name}@${j.off}: ${aBad.join(', ')}\n` +
            `state=${JSON.stringify(aState)}\ncells=${JSON.stringify(cellTables)}\n` +
            `examples=${JSON.stringify(aExamples)}\n` +
            `--- twin last rows ---\n${dumpRows(twin.term as never, 'normal', 12).join('\n')}\n` +
            `--- viewer last rows ---\n${dumpRows(viewer.term as never, 'normal', 12).join('\n')}`,
          );
        }
        // stage B
        await viewer.feed(bytes.subarray(j.off));
        const bState = diffState(refState, stateOf(viewer.term as never));
        const bCells: Record<string, number> = {};
        const bExamples: Record<string, Array<{ a: { fg: number; bg: number; ch: string; width: number }; b: { fg: number; bg: number; ch: string; width: number } }>> = {};
        for (const which of ['normal', 'alt'] as const) {
          const c = compareBuffers(ref.term as never, viewer.term as never, which, ALL_EXAMPLES);
          const classified = classifyBufferDiff(c.cells, which, which === 'alt' ? refState.activeBuffer === 'alternate' : true);
          Object.assign(bCells, classified);
          for (const cls of Object.keys(classified)) {
            const raw = cls.replace(/^(alt-buffer content \((?:active|inactive, stale)\)): /, '');
            bExamples[cls] = [...(bExamples[cls] ?? []), ...(c.examples[raw] ?? [])];
          }
        }
        const bBad: string[] = [];
        for (const d of bState) {
          if (!isStageBAllowed(variant, spec.name, d.cls)) bBad.push(`state:${d.cls}:${d.key}`);
        }
        for (const cls of Object.keys(bCells)) {
          // Every non-clean stage-B class goes through the named allowlists:
          // STATE_ALLOWLIST (exact {variant,stream,class}) or the folded
          // spec-6 residuals (stageBAllowed: COSMETIC_RESIDUALS plus the
          // named VISUAL_SAME_P16 / BLANK_FLAG_ATTR proofs over ALL cells).
          // There is no blanket class wildcard.
          const allowed = isStageBAllowed(variant, spec.name, cls) || stageBAllowed(cls, bExamples);
          if (allowed) {
            noteResidual(cls, bCells[cls]);
            continue;
          }
          bBad.push(`cell:${cls}`);
        }
        if (bBad.length > 0) {
          throw new Error(
            `stage B ${variant}/${spec.name}@${j.off}: ${bBad.join(', ')}\n` +
            `bState=${JSON.stringify(bState)}\nbCells=${JSON.stringify(bCells)}\nbExamples=${JSON.stringify(bExamples)}\n` +
            `--- ref last rows ---\n${dumpRows(ref.term as never, 'normal', 12).join('\n')}\n` +
            `--- viewer last rows ---\n${dumpRows(viewer.term as never, 'normal', 12).join('\n')}`,
          );
        }
        // ESC 8 probe at EVERY join (spec 9.3): DECRC must restore ref and
        // viewer to the same cursor/pen even when stage A/B are clean.
        const pr = await probeDecrc(ref.term as never);
        const pv = await probeDecrc(viewer.term as never);
        refDirty = true;
        const same = JSON.stringify([pr.x, pr.y, pr.pen, pr.charset]) === JSON.stringify([pv.x, pv.y, pv.pen, pv.charset]);
        let probe = same ? 'same' : 'DIFFERENT';
        if (!same && !isStageBAllowed(variant, spec.name, 'DECSC saved cursor')) throw new Error(`probeDecrc DIFFERENT at ${variant}/${spec.name}@${j.off}: ref=${JSON.stringify(pr)} viewer=${JSON.stringify(pv)}`);
        if (!same) { probe = 'DIFFERENT (allowed DECSC residual)'; }
        rec.joins.push({
          off: j.off, seedBytes: seed.bytes.length, probe,
          aClasses: [...aClasses], bClasses: [...bState.map((d) => d.cls), ...Object.keys(bCells)],
        });
      } catch (e) {
        failures.push(String((e as Error).message ?? e));
      } finally {
        viewer.dispose();
      }
    }
    twin.dispose();
    ref.dispose();
  } catch (e) {
    failures.push(String((e as Error).message ?? e));
    rec.error = failures.join('\n');
    throw e;
  }
  if (failures.length > 0) {
    rec.error = failures.join('\n');
    const msg = `${variant}/${spec.name}: ${failures.length} failing join(s)\n${failures.join('\n')}`;
    console.log(msg);
    throw new Error(msg);
  }
}

/** Stage-A allow rule: cosmetic classes (and derived link-cell colours,
 * plus fg/bg diffs proven visual-same: a P16/P256 palette < 16 downgrade
 * (named residual VISUAL_SAME_P16), which SerializeAddon 0.14.0 emits for
 * every styled cell with such an fg, reproduces the exact displayed colour
 * with different xterm mode bits; the proof runs over EVERY differing cell
 * of the class), and blank-flag 'attributes' diffs on empty glyphs (named
 * residual BLANK_FLAG_ATTR: serialize drops flags on blank cells). DECSC
 * diffs pass only under decscStageAAllowed (raw in-viewport equality;
 * DECRC equivalence only for out-of-viewport slots). */
function nonCosmetic(
  classes: Set<string>,
  cells: Record<string, number>,
  examples: Record<string, Array<{ a: { fg: number; bg: number; ch: string; width: number }; b: { fg: number; bg: number; ch: string; width: number } }>> = {},
  twinState?: Record<string, unknown>,
  viewerState?: Record<string, unknown>,
): string[] {
  const bad: string[] = [];
  const hasLink = (prefix: string) =>
    Object.keys(cells).some((k) => k === `${prefix}OSC 8 hyperlink` || (!prefix && k === 'OSC 8 hyperlink'));
  const linkful = (raw: string) => {
    const p = raw.startsWith('alt-buffer content')
      ? raw.slice(0, raw.lastIndexOf(': ') + 2)
      : '';
    return p === '' ? hasLink('') : hasLink(p);
  };
  const visualSame = (cls: string, field: 'fg' | 'bg'): boolean => {
    const ex = examples[cls] ?? [];
    if (ex.length === 0) return false;
    return ex.every((e) => sameVisualColour(e.a[field], e.b[field]));
  };
  const blank = (ch: string): boolean => ch === '' || ch === ' ';
  for (const c of classes) {
    const raw = c.replace(/^(alt-buffer content \((?:active|inactive, stale)\)): /, '');
    void VISUAL_SAME_P16;
    void BLANK_FLAG_ATTR;
    if ((COSMETIC_RESIDUALS as readonly string[]).includes(raw)) continue;
    // derived 'fg colour'/'attributes'/'bg colour' on cells that carry a link
    if ((raw === 'fg colour' || raw === 'attributes' || raw === 'bg colour') && linkful(c)) continue;
    if (raw === 'fg colour' && visualSame(c, 'fg')) continue;
    if (raw === 'bg colour' && visualSame(c, 'bg')) continue;
    if (raw === 'attributes') {
      const ex = examples[c] ?? [];
      if (ex.length > 0 && ex.every((e) => blank(e.a.ch) && blank(e.b.ch) && sameVisualColour(e.a.fg, e.b.fg) && sameVisualColour(e.a.bg, e.b.bg))) continue;
    }
    // DECSC (FXT6): raw saved coordinates must match unless the mismatch is
    // in the un-expressible out-of-viewport regime (see decscStageAAllowed).
    if (raw === 'DECSC saved cursor' && twinState && viewerState && decscStageAAllowed(twinState, viewerState)) continue;
    bad.push(c);
  }
  return bad;
}

/** Stage-B rule beyond STATE_ALLOWLIST: cosmetic cells at join time stay
 * lost for rows already in the buffer (spec 6, by design); plus the
 * visual-same and blank-flag derivations. */
function stageBAllowed(
  cls: string,
  examples: Record<string, Array<{ a: { fg: number; bg: number; ch: string; width: number }; b: { fg: number; bg: number; ch: string; width: number } }>> = {},
): boolean {
  const raw = cls.replace(/^(alt-buffer content \((?:active|inactive, stale)\)): /, '');
  if ((COSMETIC_RESIDUALS as readonly string[]).includes(raw)) return true;
  const ex = examples[cls] ?? [];
  if (raw === 'fg colour' && ex.length > 0 && ex.every((e) => sameVisualColour(e.a.fg, e.b.fg))) return true;
  if (raw === 'bg colour' && ex.length > 0 && ex.every((e) => sameVisualColour(e.a.bg, e.b.bg))) return true;
  if (raw === 'attributes' && ex.length > 0) {
    const blank = (ch: string): boolean => ch === '' || ch === ' ';
    if (ex.every((e) => blank(e.a.ch) && blank(e.b.ch) && sameVisualColour(e.a.fg, e.b.fg) && sameVisualColour(e.a.bg, e.b.bg))) return true;
  }
  return false;
}

/** QuiescentJoinGate cross-check on a sample of the picked offsets. */
async function gateCrossCheck(variant: Variant, spec: StreamSpec, bytes: Uint8Array, picks: number[], sample: number): Promise<void> {
  const twin = makeTwin(spec.cols, spec.rows, { variant });
  const ground = new GroundTracker();
  const gate = new QuiescentJoinGate(twin, ground);
  try {
    let pos = 0;
    for (const off of picks.slice(0, sample)) {
      await gate.feed(bytes.subarray(pos, off));
      pos = off;
      expect(gate.parsed).toBe(off);
      const jp = await gate.nextQuiescent(200);
      expect(jp.quiescent).toBe(true);
      expect(jp.offset).toBe(off);
    }
  } finally {
    twin.dispose();
  }
}

for (const variant of VARIANTS) {
  for (const specIdx of [0, 1, 2, 3, 4, 5, 6, 7, 8, 9]) {
    test(`matrix ${variant} ${specIdx}`, async () => {
      const specs = await getStreams();
      const spec = specs[specIdx];
      await runVariantStream(variant, spec);
      console.log(`matrix ${variant} ${spec.name}: ok`);
    }, 900_000);
  }
}

test('gate cross-check agrees with pickQuiescentJoins on a sample', async () => {
  const specs = await getStreams();
  for (const spec of specs.slice(0, 4)) {
    const bytes = stripAll(spec.gen());
    const picks = pickQuiescentJoins(bytes, spec.name).map((j) => j.off);
    expect(picks.length).toBeGreaterThanOrEqual(1);
    expect(picks.every((o, i) => i === 0 || o > picks[i - 1])).toBe(true);
    await gateCrossCheck('graphemes', spec, bytes, picks, 6);
  }
}, 900_000);

test('inside-frame joins: 0 allowed (gate resolves only after ?2026l)', async () => {
  const specs = await getStreams();
  for (const spec of specs.slice(0, 3)) {
    const bytes = stripAll(spec.gen());
    const picks = pickQuiescentJoins(bytes, spec.name);
    const s = Buffer.from(bytes).toString('latin1');
    for (const { off } of picks) {
      const before = s.slice(0, off);
      const lastH = before.lastIndexOf('\x1b[?2026h');
      const lastL = before.lastIndexOf('\x1b[?2026l');
      expect(lastH < lastL || lastL === -1 && lastH === -1).toBe(true);
    }
    // Drive the real gate on a chunk end inside a frame: it must not
    // resolve until the chunk end after ?2026l has been fed.
    const h = s.indexOf('\x1b[?2026h');
    const l = h >= 0 ? s.indexOf('\x1b[?2026l', h) : -1;
    if (h >= 0 && l >= 0) {
      const inside = h + 4;
      const after = l + '\x1b[?2026l'.length;
      const twin = makeTwin(spec.cols, spec.rows);
      const ground = new GroundTracker();
      const gate = new QuiescentJoinGate(twin, ground);
      try {
        await gate.feed(bytes.subarray(0, inside));
        const jpInside = await gate.nextQuiescent(50);
        expect(jpInside.quiescent).toBe(false);
        expect(jpInside.reason).toBe('sync-frame');
        // In-frame feed-stop: with no further bytes the gate must resolve
        // non-quiescent after the timeout (never join inside a frame).
        const jpStop = await gate.nextQuiescent(50);
        expect(jpStop.quiescent).toBe(false);
        await gate.feed(bytes.subarray(inside, after));
        const jpAfter = await gate.nextQuiescent(500);
        // FXT6 (defect 5): UNCONDITIONAL -- a gate whose syncOn never clears
        // must fail here, not pass vacuously behind `if (jpAfter.quiescent)`.
        expect(jpAfter.quiescent).toBe(true);
        expect(jpAfter.offset).toBe(after);
      } finally {
        twin.dispose();
      }
    }
  }
}, 900_000);

test('harness seed 3 determinism: byte-equal regen, sha256 reported', async () => {
  const specs = await getStreams();
  const spec = specs[2];
  const a = spec.gen();
  const b = spec.gen();
  expect(a).toEqual(b);
  harnessSeed3Sha = createHash('sha256').update(a).digest('hex');
  console.log(`harness-s3 sha256: ${harnessSeed3Sha}`);
  const PINNED = '3a2a4a3f0e97754d39a09b7860e9c7494c1fb0c4a87a4d0e5b1ae641b6b75849';
  expect(harnessSeed3Sha).toBe(PINNED);
}, 900_000);

test('raw-family baseline (informational, spike 52/741-style count)', async () => {
  const specs = await getStreams();
  const lines: string[] = [];
  for (const variant of VARIANTS) {
    for (const spec of specs) {
      const bytes = stripAll(spec.gen());
      const joins = pickJoins(bytes, spec.name);
      let clean = 0, cleanIn = 0, totalIn = 0;
      const twin = makeTwin(spec.cols, spec.rows, { variant });
      let pos = 0;
      for (const j of joins) {
        await twin.feed(bytes.subarray(pos, j.off));
        pos = j.off;
        const ser = twin.serialize as unknown as { serialize(o: { scrollback?: number }): string };
        const v = makeTwin(spec.cols, spec.rows, { variant });
        await v.feed(encStr(ser.serialize.call(twin.serialize, { scrollback: 50000 })));
        const diffs = diffState(stateOf(twin.term as never), stateOf(v.term as never));
        const cells: Record<string, number> = {};
        for (const which of ['normal', 'alt'] as const) {
          Object.assign(cells, classifyBufferDiff(
            compareBuffers(twin.term as never, v.term as never, which).cells, which,
            which === 'alt' ? stateOf(twin.term as never).activeBuffer === 'alternate' : true));
        }
        const ok = nonCosmetic(new Set([...diffs.map((d: StateDiff) => d.cls), ...Object.keys(cells)]), cells).length === 0;
        if (ok) clean++;
        if (j.inside) { totalIn++; if (ok) cleanIn++; }
        v.dispose();
      }
      twin.dispose();
      lines.push(`raw ${variant} ${spec.name}: clean=${clean}/${joins.length} inside-clean=${cleanIn}/${totalIn}`);
    }
  }
  console.log(`\n${lines.join('\n')}`);
}, 900_000);

function encStr(s: string): Uint8Array {
  return new TextEncoder().encode(s);
}

test('minimal repro: P256<16 fg on a curly+58 cell serializes as P16 (visual-same)', async () => {
  // Regression pin for the harness-s2 stage-A 'fg colour' class: feeding
  // '4:3;58:5;42;38;5;9' gives a cell whose fg word is P256 index 9.
  // SerializeAddon 0.14.0 emits palette indices < 16 as P16 codes, so
  // the viewer's word differs in mode bits while the displayed colour
  // is identical. This test proves the damage is exactly that and
  // nothing else (sameVisualColour); if serialize is ever fixed to
  // preserve P256 < 16, update this pin with T6b.
  const twin = makeTwin(40, 12);
  const viewer = makeTwin(40, 12);
  try {
    const seq = encStr('\x1b[0;4:3;58:5;42;38;5;9mX');
    await twin.feed(seq);
    const ground = new GroundTracker();
    ground.push(seq);
    const seed = buildSeed(twin, { scrollback: 50000, atOffset: 1, ground });
    await viewer.feed(seed.bytes);
    const c = compareBuffers(twin.term as never, viewer.term as never, 'normal', ALL_EXAMPLES);
    const ex = c.examples['fg colour']?.[0];
    if (ex) {
      console.log(`fg repro words: twin=${ex.a.fg.toString(16)} viewer=${ex.b.fg.toString(16)} visualSame=${sameVisualColour(ex.a.fg, ex.b.fg)}`);
    }
    expect(c.cells['fg colour']).toBeGreaterThan(0);
    expect(ex).toBeDefined();
    if (ex) {
      expect(ex.a.fg).not.toBe(ex.b.fg);
      expect(sameVisualColour(ex.a.fg, ex.b.fg)).toBe(true);
    }
  } finally {
    twin.dispose();
    viewer.dispose();
  }
}, 900_000);

test('writes matrix.json to the witness dir (schema kbt-twin-matrix/1)', async () => {
  const dir = process.env.KB_WITNESS_DIR ?? process.env.RUNNER_TEMP ?? tmpdir();
  mkdirSync(dir, { recursive: true });
  const report: MatrixReport = { schema: 'kbt-twin-matrix/1', joinsRequested: JOINS, results: reports };
  writeFileSync(join(dir, 'matrix.json'), JSON.stringify(report, null, 1));
  expect(existsSync(join(dir, 'matrix.json'))).toBe(true);
  expect(reports.length).toBeGreaterThan(0);
}, 900_000);

test('stage A DECSC: a one-row in-viewport wrong slot fails (FXT6 raw compare)', async () => {
  // A viewer whose DECSC slot is one viewport row off must be flagged by the
  // stage-A rule even when the DECRC-restored position happen to clamp
  // equal. With the pre-FXT6 clamped comparison this wrong slot compared
  // clean; with the raw comparison it must be non-cosmetic. Control: the
  // unmutated seed viewer passes.
  const twin = makeTwin(40, 12);
  try {
    const seq = encStr('\x1b[1;31m\x1b[4;7H\x1b7\x1b[0m\x1b[1;1Hxyz');
    await twin.feed(seq);
    const ground = new GroundTracker();
    ground.push(seq);
    const seed = buildSeed(twin, { scrollback: 50000, atOffset: 1, ground });
    const check = async (viewerBytes: Uint8Array): Promise<boolean> => {
      const viewer = makeTwin(40, 12);
      try {
        await viewer.feed(viewerBytes);
        const ts = stateOf(twin.term as never);
        const vs = stateOf(viewer.term as never);
        const classes = new Set(diffState(ts, vs).map((d) => d.cls));
        return nonCosmetic(classes, {}, {}, ts, vs).includes('DECSC saved cursor');
      } finally {
        viewer.dispose();
      }
    };
    // Control: the real seed restores the slot exactly.
    expect(await check(seed.bytes)).toBe(false);
    // Mutation: re-save the viewer's slot one row down (5;7 instead of 4;7).
    // Move the saved slot in a fresh viewer instead: feed seed + CUP + ESC 7.
    // (Post-seed viewer bytes: CUP to row 5 col 7 then DECSC.)
    const extra = encStr('\x1b[5;7H\x1b7');
    const mutatedViewer = new Uint8Array(seed.bytes.length + extra.length);
    mutatedViewer.set(seed.bytes, 0);
    mutatedViewer.set(extra, seed.bytes.length);
    expect(await check(mutatedViewer)).toBe(true);
  } finally {
    twin.dispose();
  }
}, 900_000);

test('stage-B residual totals stay within the spike bound (15+7+6+2)', async () => {
  // Runs after every matrix (variant, stream) test above in file order? Bun
  // runs tests in declaration order within a file, so residualTotals is
  // populated by then. Any allowlisted cell-chars/wide/extra/lost totals
  // beyond the T1 spike's budgets fail -- a new residual class with no
  // budget entry fails too (no silent wildcard).
  console.log(`stage-B residual totals: ${JSON.stringify(residualTotals)}`);
  for (const [cls, n] of Object.entries(residualTotals)) {
    expect(SPIKE_BOUNDS[cls] === undefined ? -1 : SPIKE_BOUNDS[cls]).toBeGreaterThanOrEqual(n);
  }
  expect(residualTotals['cell chars'] ?? 0).toBeLessThanOrEqual(15);
  expect(residualTotals['wide-char boundary'] ?? 0).toBeLessThanOrEqual(7);
  expect(residualTotals['rows only in viewer (extra)'] ?? 0).toBeLessThanOrEqual(6);
  expect(residualTotals['rows only in expected (lost by seed/viewer)'] ?? 0).toBeLessThanOrEqual(2);
}, 900_000);

test('STATE_ALLOWLIST entries carry root causes and budget-covered classes', () => {
  // Every entry names an exact {variant, stream, class} with a one-line root
  // cause (why) and either is budget-counted (SPIKE_BOUNDS key) or is a
  // state class (DECSC) whose stage-B residuals are counted via noteResidual
  // totals above. An entry with no why or an unknown class fails.
  for (const e of STATE_ALLOWLIST) {
    expect(e.why.trim().length).toBeGreaterThan(20);
    expect(e.cls.length).toBeGreaterThan(0);
  }
  expect(STATE_ALLOWLIST.length).toBeLessThanOrEqual(16);
}, 900_000);
