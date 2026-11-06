// T1 matrix: usage: node run-matrix.mjs STREAMS_DIR OUT_DIR [JOINS=100]
// STREAMS_DIR holds NAME.CxR.bin files (cols x rows in the name).
import fs from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';
import { dumpRows, probeDecrc, VARIANTS, makeTerm, write, writeChunked, stateOf, diffState, compareBuffers, classifyBufferDiff } from './lib.mjs';

const [streamsDir, outDir, joinsArg] = process.argv.slice(2);
const JOINS = Number(joinsArg ?? 100), INSIDE_TARGET = 15;
fs.mkdirSync(outDir, { recursive: true });

function mulberry(seed) { return () => { seed |= 0; seed = (seed + 0x6D2B79F5) | 0; let t = Math.imul(seed ^ (seed >>> 15), 1 | seed); t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t; return ((t ^ (t >>> 14)) >>> 0) / 4294967296; }; }

function frames(buf) {
  const s = buf.toString('latin1'), iv = [];
  let p = 0;
  while (true) {
    const a = s.indexOf('\x1b[?2026h', p); if (a < 0) break;
    let b = s.indexOf('\x1b[?2026l', a); if (b < 0) b = s.length;
    iv.push([a + 8, b]); p = b + 8;
  }
  return { s, iv };
}
function pickJoins(buf, name) {
  const { s, iv } = frames(buf), rnd = mulberry([...name].reduce((h, c) => (h * 31 + c.charCodeAt(0)) | 0, 7));
  const inFrame = (o) => iv.some(([a, b]) => o > a && o < b);
  const outside = [], inside = [];
  for (let i = s.indexOf('\n'); i >= 0; i = s.indexOf('\n', i + 1)) if (i + 1 < s.length && !inFrame(i + 1)) outside.push(i + 1);
  for (const [a, b] of iv) for (let i = s.indexOf('\x1b', a + 1); i >= 0 && i < b; i = s.indexOf('\x1b', i + 1)) inside.push(i);
  const strat = (arr, n) => {
    if (arr.length === 0) return [];
    const out = new Set();
    for (let k = 0; k < n; k++) {
      const lo = Math.floor((k * arr.length) / n), hi = Math.max(lo + 1, Math.floor(((k + 1) * arr.length) / n));
      out.add(arr[lo + Math.floor(rnd() * (hi - lo))]);
    }
    return [...out];
  };
  const nIn = inside.length ? Math.min(INSIDE_TARGET, JOINS) : 0;
  const ins = strat(inside, nIn).map((o) => ({ off: o, inside: true }));
  const outs = strat(outside, JOINS - ins.length).map((o) => ({ off: o, inside: false }));
  return [...outs, ...ins].sort((a, b) => a.off - b.off);
}

const files = fs.readdirSync(streamsDir).filter((f) => f.endsWith('.bin')).sort();
const t0 = Date.now();
const results = [];
const dumped = { unicode11: 0, graphemes: 0 };
for (const variant of VARIANTS) {
  for (const f of files) {
    const m = f.match(/^(.*)\.(\d+)x(\d+)\.bin$/);
    if (!m) { console.error('skip (name lacks CxR)', f); continue; }
    const [, name, C, R] = m; const cols = +C, rows = +R;
    const bytes = fs.readFileSync(path.join(streamsDir, f));
    const sha = crypto.createHash('sha256').update(bytes).digest('hex').slice(0, 16);
    const joins = pickJoins(bytes, name);
    const rec = { variant, stream: name, cols, rows, bytes: bytes.length, sha256_16: sha, joins: [], error: null };
    results.push(rec);
    try {
      const buildRef = async () => { const r0 = makeTerm(cols, rows, variant).t; await writeChunked(r0, bytes, 0, bytes.length); return r0; };
      let ref = await buildRef(), refDirty = false;
      const refState = stateOf(ref);
      const twin = makeTerm(cols, rows, variant);
      let pos = 0;
      for (const j of joins) {
        await writeChunked(twin.t, bytes, pos, j.off); pos = j.off;
        if (refDirty) { ref.dispose(); ref = await buildRef(); refDirty = false; }
        const twinState = stateOf(twin.t);
        const seed = twin.ser.serialize({ scrollback: 50000 });
        const v = makeTerm(cols, rows, variant).t;
        await write(v, seed);
        const afterSeedState = stateOf(v);
        // stage A: seed fidelity (viewer right after the seed vs the twin at the join)
        const A = { state: diffState(twinState, afterSeedState), cells: {}, examples: {} };
        for (const which of ['normal', 'alt']) {
          const c = compareBuffers(twin.t, v, which);
          const altActive = twinState.activeBuffer === 'alternate';
          Object.assign(A.cells, classifyBufferDiff(c, which, which === 'alt' ? altActive : true));
          if (Object.keys(c.examples).length) A.examples[which] = c.examples;
        }
        // stage B: convergence (viewer after the remaining stream vs the reference that saw everything)
        await writeChunked(v, bytes, j.off, bytes.length);
        const B = { state: diffState(refState, stateOf(v)), cells: {}, examples: {}, probe: null };
        for (const which of ['normal', 'alt']) {
          const c = compareBuffers(ref, v, which);
          Object.assign(B.cells, classifyBufferDiff(c, which, which === 'alt' ? refState.activeBuffer === 'alternate' : true));
          if (Object.keys(c.examples).length) B.examples[which] = c.examples;
        }
        // effective DECSC probe: do ref and viewer restore to the same cursor/pen? (probe mutates both, so run last on clones' end state)
        if (B.state.some((d) => d.cls === 'DECSC saved cursor')) {
          const pr = await probeDecrc(ref), pv = await probeDecrc(v);
          B.probe = JSON.stringify({ x: pr.x, y: pr.y }) === JSON.stringify({ x: pv.x, y: pv.y }) && JSON.stringify(pr.pen) === JSON.stringify(pv.pen) ? 'same' : 'DIFFERENT';
          B.probeDetail = B.probe === 'DIFFERENT' ? { ref: pr, viewer: pv } : undefined;
          // restore ref to its pre-probe state is not possible; rebuild by marking ref dirty so later joins re-create it
          refDirty = true;
        }
        if (dumped[variant] < 4 && (B.cells['cell chars'] || B.cells['rows only in viewer (extra)'] || B.cells['rows only in expected (lost by seed/viewer)'] || B.cells['wide-char boundary'])) {
          dumped[variant]++;
          const lines = [`variant=${variant} stream=${name} join=${j.off} inside=${j.inside} twinActive=${twinState.activeBuffer} refRows=${ref.buffer.normal.length} viewerRows=${v.buffer.normal.length}`, 'B.cells=' + JSON.stringify(B.cells), '--- reference (last 400 rows) ---', ...dumpRows(ref, 'normal', 400), '--- viewer after remainder (last 400 rows) ---', ...dumpRows(v, 'normal', 400), '--- seed (JSON escaped, first 3000 chars) ---', JSON.stringify(seed).slice(0, 3000)];
          fs.writeFileSync(path.join(outDir, `debug-${variant}-${name}-${j.off}.txt`), lines.join('\n') + '\n');
        }
        rec.joins.push({ off: j.off, inside: j.inside, seedBytes: Buffer.byteLength(seed), twinActive: twinState.activeBuffer, A, B });
        v.dispose();
      }
      twin.t.dispose(); ref.dispose();
    } catch (e) { rec.error = String(e?.stack ?? e); console.error('ERROR', variant, name, rec.error); }
    console.error(`[${((Date.now() - t0) / 1000).toFixed(0)}s] ${variant} ${name} ${cols}x${rows} ${bytes.length}B joins=${rec.joins.length}`);
  }
}
fs.writeFileSync(path.join(outDir, 'matrix.json'), JSON.stringify({ schema: 'kbt-twin-matrix/1', joinsRequested: JOINS, results }, null, 1));
console.error('matrix done in', ((Date.now() - t0) / 1000).toFixed(0), 's');
