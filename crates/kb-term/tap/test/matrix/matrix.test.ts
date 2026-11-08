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
  COSMETIC_RESIDUALS,
  type StateDiff,
} from './lib.ts';

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
    let pos = 0;
    for (const j of qjoins) {
      await twin.feed(bytes.subarray(pos, j.off));
      pos = j.off;
      if (refDirty) {
        ref.dispose();
        ref = await buildRef();
        refDirty = false;
      }
      const seed = buildSeed(twin, { scrollback: 50000, atOffset: j.off });
      if (seed.withheld.mouse !== 0) throw new Error(`withheld.mouse=${seed.withheld.mouse}`);
      const viewer = mk();
      await viewer.feed(seed.bytes);
      // stage A
      const aState = diffState(stateOf(twin.term as never), stateOf(viewer.term as never));
      const cellTables: Record<string, number> = {};
      for (const which of ['normal', 'alt'] as const) {
        const c = compareBuffers(twin.term as never, viewer.term as never, which);
        Object.assign(cellTables, classifyBufferDiff(c.cells, which, which === 'alt' ? stateOf(twin.term as never).activeBuffer === 'alternate' : true));
      }
      const aClasses = new Set<string>([...aState.map((d) => d.cls), ...Object.keys(cellTables)]);
      const aBad = nonCosmetic(aClasses, cellTables);
      if (aBad.length > 0) {
        throw new Error(
          `stage A ${variant}/${spec.name}@${j.off}: ${aBad.join(', ')}\n` +
          `state=${JSON.stringify(aState)}\ncells=${JSON.stringify(cellTables)}\n` +
          `--- twin last rows ---\n${dumpRows(twin.term as never, 'normal', 12).join('\n')}\n` +
          `--- viewer last rows ---\n${dumpRows(viewer.term as never, 'normal', 12).join('\n')}`,
        );
      }
      // stage B
      await viewer.feed(bytes.subarray(j.off));
      const bState = diffState(refState, stateOf(viewer.term as never));
      const bCells: Record<string, number> = {};
      for (const which of ['normal', 'alt'] as const) {
        const c = compareBuffers(ref.term as never, viewer.term as never, which);
        Object.assign(bCells, classifyBufferDiff(c.cells, which, which === 'alt' ? refState.activeBuffer === 'alternate' : true));
      }
      const bBad: string[] = [];
      for (const d of bState) if (!isStageBAllowed(variant, spec.name, d.cls)) bBad.push(`state:${d.cls}:${d.key}`);
      for (const cls of Object.keys(bCells)) if (!isStageBAllowed(variant, spec.name, cls)) bBad.push(`cell:${cls}`);
      if (bBad.length > 0) {
        throw new Error(
          `stage B ${variant}/${spec.name}@${j.off}: ${bBad.join(', ')}\n` +
          `bState=${JSON.stringify(bState)}\nbCells=${JSON.stringify(bCells)}\n` +
          `--- ref last rows ---\n${dumpRows(ref.term as never, 'normal', 12).join('\n')}\n` +
          `--- viewer last rows ---\n${dumpRows(viewer.term as never, 'normal', 12).join('\n')}`,
        );
      }
      // ESC 8 probe (only when stage B shows a DECSC class).
      let probe: string | undefined;
      if (bState.some((d) => d.cls === 'DECSC saved cursor')) {
        const pr = await probeDecrc(ref.term as never);
        const pv = await probeDecrc(viewer.term as never);
        const same = JSON.stringify([pr.x, pr.y, pr.pen, pr.charset]) === JSON.stringify([pv.x, pv.y, pv.pen, pv.charset]);
        probe = same ? 'same' : 'DIFFERENT';
        if (!same) throw new Error(`probeDecrc DIFFERENT at ${variant}/${spec.name}@${j.off}: ref=${JSON.stringify(pr)} viewer=${JSON.stringify(pv)}`);
        refDirty = true;
      }
      rec.joins.push({
        off: j.off, seedBytes: seed.bytes.length, probe,
        aClasses: [...aClasses], bClasses: [...bState.map((d) => d.cls), ...Object.keys(bCells)],
      });
      viewer.dispose();
    }
    twin.dispose();
    ref.dispose();
  } catch (e) {
    rec.error = String(e);
    throw e;
  }
}

/** Stage-A allow rule: cosmetic classes (and derived link-cell colours). */
function nonCosmetic(classes: Set<string>, cells: Record<string, number>): string[] {
  const bad: string[] = [];
  const hasLink = (prefix: string) =>
    Object.keys(cells).some((k) => k === `${prefix}OSC 8 hyperlink` || (!prefix && k === 'OSC 8 hyperlink'));
  const linkful = (raw: string) => {
    const p = raw.startsWith('alt-buffer content')
      ? raw.slice(0, raw.lastIndexOf(': ') + 2)
      : '';
    return p === '' ? hasLink('') : hasLink(p);
  };
  for (const c of classes) {
    const raw = c.replace(/^(alt-buffer content \((?:active|inactive, stale)\)): /, '');
    if ((COSMETIC_RESIDUALS as readonly string[]).includes(raw)) continue;
    // derived 'fg colour'/'attributes'/'bg colour' on cells that carry a link
    if ((raw === 'fg colour' || raw === 'attributes' || raw === 'bg colour') && linkful(c)) continue;
    bad.push(c);
  }
  return bad;
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
        await gate.feed(bytes.subarray(inside, after));
        const jpAfter = await gate.nextQuiescent(500);
        if (jpAfter.quiescent) expect(jpAfter.offset).toBe(after);
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

test('writes matrix.json to the witness dir (schema kbt-twin-matrix/1)', async () => {
  const dir = process.env.KB_WITNESS_DIR ?? process.env.RUNNER_TEMP ?? tmpdir();
  mkdirSync(dir, { recursive: true });
  const report: MatrixReport = { schema: 'kbt-twin-matrix/1', joinsRequested: JOINS, results: reports };
  writeFileSync(join(dir, 'matrix.json'), JSON.stringify(report, null, 1));
  expect(existsSync(join(dir, 'matrix.json'))).toBe(true);
  expect(reports.length).toBeGreaterThan(0);
}, 900_000);
