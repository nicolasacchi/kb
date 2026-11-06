// Semantic fuzz (KBT-STRIP): an INDEPENDENT VT action parser (oracle.ts, no shared code) must find
//   actions(output) == actions(input) minus stripped queries, and no query shape left in the output,
// for every chunking. Streams: fragment+query+tail generators, a random-token soup, and
// gen/fake_harness.py output with queries injected at random byte offsets.
import { test, expect, afterAll } from 'bun:test';
import { QueryStripper } from './strip.ts';
import { expected, observed, parse } from './oracle.ts';
import { E, enc, cat, run, eq, hexs, wrap, mulberry32, metric, writeMetrics } from './helpers.ts';
import { spawnSync } from 'node:child_process';

const BEL = '\x07', ST = `${E}\\`;
type R = () => number;
const pick = <T>(r: R, a: readonly T[]): T => a[Math.floor(r() * a.length)];

const FRAGMENTS = [E, `${E}[`, `${E}[6`, `${E}[?20`, `${E}[>`, `${E}[?2026$`, `${E}[1;3`, `${E}[ `, `${E}[?`, `${E}[0`, `${E}[=`, `${E}[>0`,
  `${E}]`, `${E}]11;`, `${E}]4;0;`, `${E}]52;c;`, `${E}]7501;`, `${E}]8;;http://x`, `${E}]1337;Cap`, `${E}]99;i=1:p=`, `${E}]5522;type=`,
  `${E}P`, `${E}P+q4d`, `${E}P$q`, `${E}Pq#0;1`, `${E}P1;2`, `${E}_`, `${E}_G`, `${E}_Gi=1,a=`, `${E}_Ga=q,i=1`, `${E}_25a`,
  `${E}Ptmux;`, `${E}Ptmux;${E}${E}[`, `${E}Ptmu`, `${E}Ptmux;${E}${E}]11;`, `${E}(`, `${E}#`, `${E}X`, `${E}^pm`, `${E}]0;ti`, `${E}[22;2`];

const QUERIES = [`${E}[c`, `${E}[0c`, `${E}[>c`, `${E}[>0c`, `${E}[=c`, `${E}[>q`, `${E}[>0q`, `${E}[5n`, `${E}[6n`, `${E}[?6n`, `${E}[?996n`, `${E}[?25n`,
  `${E}[?2026$p`, `${E}[4$p`, `${E}[?1004$p`, `${E}[0x`, `${E}[1x`, `${E}Z`, `${E}[?u`, `${E}[?1;1;0S`, `${E}[1;1;1;1;9;9*y`, `${E}[?4m`, `${E}[1$u`, `${E}[2$w`,
  ...[11, 13, 14, 15, 16, 18, 19, 20, 21].map(n => `${E}[${n}t`), `${E}[14;2t`,
  `${E}]4;0;?${BEL}`, `${E}]4;1;?;2;?${ST}`, `${E}]5;0;?${BEL}`, `${E}]10;?${BEL}`, `${E}]11;?${ST}`, `${E}]12;?${BEL}`, `${E}]15;?${ST}`, `${E}]52;c;?${BEL}`, `${E}]7501;?${BEL}`,
  `${E}]99;i=x:p=?;${ST}`, `${E}]1337;Capabilities${ST}`, `${E}]1337;ReportCellSize${BEL}`, `${E}]1337;ReportVariable=Zm9v${BEL}`, `${E}]5522;type=read${ST}`, `${E}]21;foreground=?${BEL}`,
  `${E}P+q4d73${ST}`, `${E}P+q544e${ST}`, `${E}P$qm${ST}`, `${E}_Gi=31337,s=1,v=1,a=q,t=d,f=24;AAAA${ST}`, `${E}_Ga=q,i=1${ST}`, `${E}_25a1;x${ST}`];

const BENIGN = [`${E}[1;31m`, `${E}[0m`, `${E}[2J`, `${E}[H`, `${E}[?2004h`, `${E}[?2026h`, `${E}[?2026l`, `${E}[?25l`, `${E}[>4;2m`, `${E}[>7u`, `${E}[<u`, `${E}[22;2t`, `${E}[3b`, `${E}[0 q`,
  `${E}]0;title${BEL}`, `${E}]8;;https://x/?q=1${ST}`, `${E}]11;rgb:00/00/00${BEL}`, `${E}]52;c;aGk=${BEL}`, `${E}]1337;File=inline=1:AAAA${BEL}`, `${E}]7501;state=idle${ST}`,
  `${E}_Ga=T,f=100,i=7;iVBOR${ST}`, `${E}_Ga=d,d=A,q=2${ST}`, `${E}_Gi=1,qa=q;x${ST}`, `${E}Pq#0;2;0;0;0~${ST}`, `${E}(B`, `${E}7`, `${E}8`, `${E}=`, `${E}>`, `${E}\\`, `${E}[1c`, `${E}[2x`, `${E}[7n`, `${E}[3S`];

const TAILS = ['c', 'n', '?\x07', '00h', '0c', '\\', 'x', 'abc', '\r\n', 'é', 'm', '; 5 H', '\x07', ST, 'hello', 'π', '\u0085', '\u009b', '\u009d', '\u009c', '\u0090', '\u009f', '😀', '\u{10EEEE}',
  '\t', '\x08', '\x7f', '\x01', ';', '?', '$p', '1', '2026$p', '+q', 'tmux;', ' ', ' ', '日'];

function genQuery(r: R): string {
  let q = pick(r, QUERIES);
  const v = r();
  if (v < 0.12) q = q.replace(/\[(\?|>|=)?(\d)/, (_m, p, d) => `[${p ?? ''}00${d}`);        // leading zeros
  else if (v < 0.2 && q.includes(BEL)) q = q.replace(BEL, ST);                                  // terminator swap
  else if (v < 0.28 && q.includes(ST)) q = q.replace(ST, BEL);
  else if (v < 0.34 && q.startsWith(`${E}[`)) q = q.replace(/^(\x1b\[[?>=]?[0-9;]*)/, '$1\x0a');   // C0 inside a CSI
  else if (v < 0.4 && q.startsWith(`${E}]`)) q = q.replace(/^(\x1b\][0-9]+;)/, '$1\x01');          // C0 inside an OSC
  const w = r();
  if (w < 0.18) q = wrap(q);
  else if (w < 0.22) q = wrap(wrap(q));
  else if (w < 0.25) q = wrap(wrap(wrap(q)));
  return q;
}
function genItem(r: R): string {
  const k = r();
  if (k < 0.34) return pick(r, FRAGMENTS) + genQuery(r) + pick(r, TAILS);       // fragment + query + tail
  if (k < 0.46) return genQuery(r);
  if (k < 0.58) return pick(r, FRAGMENTS) + pick(r, TAILS);                     // fragment + tail, no query
  if (k < 0.7) return pick(r, TAILS);
  if (k < 0.82) return pick(r, BENIGN);
  if (k < 0.88) return wrap(pick(r, BENIGN) + (r() < 0.5 ? genQuery(r).replaceAll(E + E, E) : ''));
  if (k < 0.94) return pick(r, FRAGMENTS) + pick(r, FRAGMENTS) + pick(r, TAILS);
  return pick(r, FRAGMENTS) + pick(r, BENIGN);
}
export function genStream(r: R): Uint8Array {
  let s = ''; const n = 1 + Math.floor(r() * 7);
  for (let i = 0; i < n; i++) s += genItem(r);
  return enc(s);
}
const SOUP = [E, E, `${E}[`, `${E}]`, `${E}P`, `${E}_G`, `${E}\\`, BEL, '\x18', '\x1a', '?', '>', '=', '$', 'p', 'c', 'q', 'n', 'u', 't', 'S', ';', '0', '1', '2', '4', '6', '9', 'a=q,', 'a=d,', 'tmux;', '+q', '$q', '11;?', '52;c;?', 'A', ' ', '\r\n', 'é', 'π', `${E}${E}`, '\u009b', '\u0085', ':', '*y', 'x'];
const soup = (r: R) => { let s = ''; const n = 1 + Math.floor(r() * 24); for (let i = 0; i < n; i++) s += pick(r, SOUP); return enc(s); };

function randomCuts(r: R, len: number): number[] {
  const n = Math.floor(r() * 5); const s = new Set<number>();
  for (let i = 0; i < n; i++) s.add(Math.floor(r() * (len + 1)));
  return [...s].sort((a, b) => a - b).filter(x => x > 0 && x < len);
}

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

// ---------------------------------------------------------------- fake_harness.py streams + injected queries
function harness(seed: number, length: number): Uint8Array {
  const py = new URL('../gen/fake_harness.py', import.meta.url).pathname;
  const p = spawnSync('python3', ['-I', py, '--seed', String(seed), '--length', String(length)], { maxBuffer: 1 << 28 });
  if (p.status !== 0) throw new Error(`fake_harness.py failed: ${p.stderr}`);
  return new Uint8Array(p.stdout);
}
function inject(r: R, base: Uint8Array, n: number): Uint8Array {
  const pos = Array.from({ length: n }, () => Math.floor(r() * (base.length + 1))).sort((a, b) => a - b);
  const parts: Uint8Array[] = []; let p = 0;
  for (const x of pos) { parts.push(base.subarray(p, x)); parts.push(enc(r() < 0.5 ? genQuery(r) : pick(r, FRAGMENTS) + genQuery(r) + pick(r, TAILS))); p = x; }
  parts.push(base.subarray(p)); return cat(parts);
}

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
