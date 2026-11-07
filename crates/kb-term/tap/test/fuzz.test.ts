// Semantic fuzz (KBT-STRIP): an INDEPENDENT VT action parser (oracle.ts, no shared code) must find
//   actions(output) == actions(input) minus stripped queries, and no query shape left in the output,
// for every chunking. Streams: fragment+query+tail generators, a random-token soup, and
// fixtures/fake_harness.py output with queries injected at random byte offsets.
// Oracle comparison policy (test/oracle.ts shares NO code with src/strip.ts): a sequence cut
// by ESC/CAN/SUB/C1 yields no action; DCS-tmux is unwrapped recursively. CAN actions are
// ignored on both sides (the stripper appends a CAN after aborting a forwarded string).
import { test, expect, afterAll } from 'bun:test';
import { QueryStripper } from '../src/strip.ts';
import { expected, observed, parse } from './oracle.ts';
import { E, enc, cat, run, eq, hexs, wrap, mulberry32, metric, writeMetrics, FRAGMENTS, TAILS, genQuery, genStream, soup, randomCuts, harness, inject, type R } from './helpers.ts';
const BEL = '\x07', ST = `${E}\\`;

interface Counters { streams: number; bytesIn: number; bytesOut: number; chunkings: number; queriesInInput: number; mismatches: string[] }
function checkStream(data: Uint8Array, cuts: number[][], c: Counters, tag: string) {
  const exp = expected(data);
  const inActs = parse(data);
  c.queriesInInput += inActs.filter(a => a.q && a.q !== 'c1').length;
  let ref: Uint8Array | null = null;
  for (const cs of cuts) {
    const { out } = run(data, cs);
    c.chunkings++; c.bytesIn += data.length; c.bytesOut += out.length;
    if (ref === null) ref = out;
    else if (!eq(ref, out)) { c.mismatches.push(`${tag} CHUNK-DEPENDENT cuts=${cs} in=${hexs(data)} a=${hexs(ref)} b=${hexs(out)}`); return; }
    const ob = observed(out);
    if (ob.leftovers.length) { c.mismatches.push(`${tag} QUERY LEFT ${ob.leftovers.map(a => a.s)} in=${hexs(data)} out=${hexs(out)}`); return; }
    if (ob.acts.join('\n') !== exp.join('\n')) {
      let k = 0; while (k < exp.length && exp[k] === ob.acts[k]) k++;
      const ctx = (a: string[]) => a.slice(Math.max(0, k - 3), k + 4).map(x => x.slice(0, 60)).join(' ');
      const bi = Buffer.from(data).toString('latin1');
      c.mismatches.push(`${tag} ACTIONS DIFFER at action ${k}/${exp.length} cuts=${cs.length}\n  want ${ctx(exp)}\n  got  ${ctx(ob.acts)}\n  in-len=${data.length} small-in=${data.length < 400 ? hexs(data) : ''}`); void bi; return;
    }
  }
  c.streams++;
}
const fresh = (): Counters => ({ streams: 0, bytesIn: 0, bytesOut: 0, chunkings: 0, queriesInInput: 0, mismatches: [] });

test('oracle self-check: known actions (the oracle is itself tested before it judges)', () => {
  const a = (s: string) => parse(enc(s)).map(x => x.s + (x.q ? '!' : '')).join(' ');
  expect(a(`${E}[1;31mA`)).toBe('C|1;31||m P41');
  expect(a(`${E}[c`)).toBe('C||||c!'.replace('||||', '|||'));
  expect(a(`${E}[${E}[cc`)).toBe('C|||c! P63');
  expect(a(`${E}]11;?${BEL}`)).toBe('O31313b3f!');
  expect(a(`${E}]11;${E}[c`)).toBe('C|||c!');
  expect(a(`${E}]0;x${ST}`)).toBe('O303b78');
  expect(a(`${E}P+q4d${ST}`)).toBe('D|+|q|3464!');
  expect(a(`${E}_Ga=q;${ST}`)).toBe('A47613d713b!');
  expect(a(`a\u009bb`)).toBe('P61 C19b! P62');
  expect(a(`${wrap(`${E}[6n`)}x`)).toBe('C|6||n! P78');
  expect(a(`${wrap(`${E}[6`)}n`)).toBe('P6e');
  expect(a(`${E}Pq~~${E}[c`)).toBe('C|||c!');       // aborted DCS: no action
  expect(a('a\n\x18b')).toBe('P61 X0a X18 P62');
});

test('semantic fuzz A: fragment + query + tail streams, every chunking (10k random + all single cuts)', () => {
  const r = mulberry32(0xc0ffee); const c = fresh();
  const N = 12000;
  for (let i = 0; i < N; i++) {
    const data = genStream(r);
    const cuts: number[][] = [[], randomCuts(r, data.length), randomCuts(r, data.length)];
    if (i % 4 === 0) { for (let j = 1; j < data.length; j++) cuts.push([j]); }
    if (i % 25 === 0) cuts.push(Array.from({ length: data.length - 1 }, (_, k) => k + 1));
    checkStream(data, cuts, c, `A#${i}`);
  }
  metric('fuzzA', { streams: c.streams, chunkings: c.chunkings, bytes_in: c.bytesIn, bytes_out: c.bytesOut, queries_in_input: c.queriesInInput, mismatches: c.mismatches.length });
  expect(c.mismatches.slice(0, 8)).toEqual([]);
  expect(c.queriesInInput).toBeGreaterThan(5000);
}, 300000);

test('semantic fuzz B: random token soup (10k multi-chunk runs)', () => {
  const r = mulberry32(0xbadcafe); const c = fresh();
  for (let i = 0; i < 10000; i++) {
    const data = soup(r);
    checkStream(data, [[], randomCuts(r, data.length), randomCuts(r, data.length), randomCuts(r, data.length)], c, `B#${i}`);
  }
  metric('fuzzB', { streams: c.streams, chunkings: c.chunkings, queries_in_input: c.queriesInInput, mismatches: c.mismatches.length });
  expect(c.mismatches.slice(0, 8)).toEqual([]);
}, 300000);

test('semantic fuzz C: exhaustive over all 2-cut splits of fragment+query+tail streams', () => {
  const r = mulberry32(0x5eed); const c = fresh();
  for (let i = 0; i < 1500; i++) {
    const data = enc(pick(r, FRAGMENTS) + genQuery(r) + pick(r, TAILS));
    if (data.length > 70) continue;
    const cuts: number[][] = [[]];
    for (let a = 1; a < data.length; a++) for (let b = a + 1; b < data.length; b++) cuts.push([a, b]);
    for (let a = 1; a < data.length; a++) cuts.push([a]);
    checkStream(data, cuts, c, `C#${i}`);
  }
  metric('fuzzC', { streams: c.streams, chunkings: c.chunkings, mismatches: c.mismatches.length });
  expect(c.mismatches.slice(0, 8)).toEqual([]);
}, 300000);

test('semantic fuzz D: fake_harness.py output, clean and with queries injected at random byte offsets', () => {
  const r = mulberry32(0x4a52); const c = fresh();
  const harnessHasEnvelopes: number[] = [];
  let injectedStreams = 0, hbytes = 0;
  for (let seed = 1; seed <= 10; seed++) {
    const base = harness(seed, 25);
    hbytes += base.length;
    // clean harness output: nothing may change except envelope unwrapping, so compare actions too
    checkStream(base, [[], randomCuts(r, base.length), randomCuts(r, base.length)], c, `D-clean#${seed}`);
    harnessHasEnvelopes.push(Buffer.from(base).includes('\x1bPtmux;') ? 1 : 0);
    for (let k = 0; k < 6; k++) {
      const data = inject(r, base, 1 + Math.floor(r() * 12));
      checkStream(data, [[], randomCuts(r, data.length), Array.from({ length: 1 }, () => Math.floor(r() * data.length))], c, `D-inject#${seed}.${k}`);
      injectedStreams++;
    }
  }
  metric('fuzzD', { harness_bytes: hbytes, streams: c.streams, injected_streams: injectedStreams, harness_with_tmux_envelopes: harnessHasEnvelopes.reduce((a, b) => a + b, 0), queries_in_input: c.queriesInInput, mismatches: c.mismatches.length });
  expect(c.mismatches.slice(0, 5)).toEqual([]);
  expect(harnessHasEnvelopes.some(x => x === 1)).toBe(true);
}, 300000);

test('throughput on harness output (one 64 KiB chunk at a time)', () => {
  const base = cat([harness(7, 400), harness(8, 400)]);
  const s = new QueryStripper(); const t0 = performance.now(); let n = 0;
  const rounds = 4;
  for (let k = 0; k < rounds; k++) for (let o = 0; o < base.length; o += 65536) { s.push(base.subarray(o, o + 65536)); n += Math.min(65536, base.length - o); }
  const ms = performance.now() - t0;
  metric('throughput', { bytes: n, ms: Math.round(ms), MBps: +(n / 1048576 / (ms / 1000)).toFixed(2) });
  expect(n).toBeGreaterThan(0);
});

afterAll(() => writeMetrics('fuzz'));
