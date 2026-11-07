import { QueryStripper } from '../src/strip.ts';
import { QueryStripper as RefQueryStripper } from './reference-strip.ts';
import { spawnSync } from 'node:child_process';
import { mkdirSync, writeFileSync } from 'node:fs';

export const E = '\x1b';
export const enc = (s: string) => new TextEncoder().encode(s);
export const cat = (a: Uint8Array[]) => { const n = a.reduce((x, y) => x + y.length, 0); const r = new Uint8Array(n); let o = 0; for (const p of a) { r.set(p, o); o += p.length; } return r; };
export const hexs = (b: Uint8Array) => Buffer.from(b).toString('latin1').replace(/[^\x20-\x7e]/g, c => '\\x' + c.charCodeAt(0).toString(16).padStart(2, '0'));
export const eq = (a: Uint8Array, b: Uint8Array) => a.length === b.length && a.every((v, i) => v === b[i]);

/** Minimum stripper surface the tests drive (production and frozen reference share it). */
export interface StripperBox {
  push(chunk: Uint8Array): Uint8Array;
  flush(): Uint8Array;
  pending(): number;
  stripped: number;
  aborted: number;
  capPasses: number;
  kinds: Record<string, number>;
  onStrip?: (bytes: Uint8Array, label: string) => void;
}

export const makeProd = (): StripperBox => new QueryStripper();
export const makeRef = (): StripperBox => new RefQueryStripper();

/** run data through a fresh stripper, cut at the given sorted offsets, flush at the end */
export function run(data: Uint8Array, cuts: number[] = [], make: () => StripperBox = makeProd) {
  const s = make(); const parts: Uint8Array[] = []; let p = 0;
  for (const c of [...cuts, data.length]) { parts.push(s.push(data.subarray(p, c))); p = c; }
  parts.push(s.flush());
  return { out: cat(parts), s };
}

export interface ChunkStep { out: Uint8Array; pending: number; stripped: number; aborted: number; capPasses: number; kinds: Record<string, number> }

/** Drive one stripper chunk-by-chunk, reporting output and counters after every chunk. */
export function runChunks(data: Uint8Array, cuts: number[] = [], make: () => StripperBox = makeProd): { steps: ChunkStep[]; s: StripperBox; out: Uint8Array } {
  const s = make(); const parts: Uint8Array[] = []; const steps: ChunkStep[] = []; let p = 0;
  const snapshot = (out: Uint8Array): ChunkStep => ({ out, pending: s.pending(), stripped: s.stripped, aborted: s.aborted, capPasses: s.capPasses, kinds: { ...s.kinds } });
  for (const c of [...cuts, data.length]) {
    const part = s.push(data.subarray(p, c));
    parts.push(part); steps.push(snapshot(part)); p = c;
  }
  const tail = s.flush();
  parts.push(tail); steps.push(snapshot(tail));
  return { steps, s, out: cat(parts) };
}

/** DCS-tmux envelope: every ESC of the payload doubled */
export const wrap = (inner: string) => `${E}Ptmux;${inner.replaceAll(E, E + E)}${E}\\`;


// ---------------------------------------------------------------- fuzz generators (seeded, synthetic)
// Shared corpus so fuzz/differential/perf tests never import test files.
export type R = () => number;
const pick = <T>(r: R, a: readonly T[]): T => a[Math.floor(r() * a.length)];

const BEL = '\x07', ST = `${E}\\`;

export const FRAGMENTS = [E, `${E}[`, `${E}[6`, `${E}[?20`, `${E}[>`, `${E}[?2026$`, `${E}[1;3`, `${E}[ `, `${E}[?`, `${E}[0`, `${E}[=`, `${E}[>0`,
  `${E}]`, `${E}]11;`, `${E}]4;0;`, `${E}]52;c;`, `${E}]7501;`, `${E}]8;;http://x`, `${E}]1337;Cap`, `${E}]99;i=1:p=`, `${E}]5522;type=`,
  `${E}P`, `${E}P+q4d`, `${E}P$q`, `${E}Pq#0;1`, `${E}P1;2`, `${E}_`, `${E}_G`, `${E}_Gi=1,a=`, `${E}_Ga=q,i=1`, `${E}_25a`,
  `${E}Ptmux;`, `${E}Ptmux;${E}${E}[`, `${E}Ptmu`, `${E}Ptmux;${E}${E}]11;`, `${E}(`, `${E}#`, `${E}X`, `${E}^pm`, `${E}]0;ti`, `${E}[22;2`];

export const QUERIES = [`${E}[c`, `${E}[0c`, `${E}[>c`, `${E}[>0c`, `${E}[=c`, `${E}[>q`, `${E}[>0q`, `${E}[5n`, `${E}[6n`, `${E}[?6n`, `${E}[?996n`, `${E}[?25n`,
  `${E}[?2026$p`, `${E}[4$p`, `${E}[?1004$p`, `${E}[0x`, `${E}[1x`, `${E}Z`, `${E}[?u`, `${E}[?1;1;0S`, `${E}[1;1;1;1;9;9*y`, `${E}[?4m`, `${E}[1$u`, `${E}[2$w`,
  ...[11, 13, 14, 15, 16, 18, 19, 20, 21].map(n => `${E}[${n}t`), `${E}[14;2t`,
  `${E}]4;0;?${BEL}`, `${E}]4;1;?;2;?${ST}`, `${E}]5;0;?${BEL}`, `${E}]10;?${BEL}`, `${E}]11;?${ST}`, `${E}]12;?${BEL}`, `${E}]15;?${ST}`, `${E}]52;c;?${BEL}`, `${E}]7501;?${BEL}`,
  `${E}]99;i=x:p=?;${ST}`, `${E}]1337;Capabilities${ST}`, `${E}]1337;ReportCellSize${BEL}`, `${E}]1337;ReportVariable=Zm9v${BEL}`, `${E}]5522;type=read${ST}`, `${E}]21;foreground=?${BEL}`,
  `${E}P+q4d73${ST}`, `${E}P+q544e${ST}`, `${E}P$qm${ST}`, `${E}_Gi=31337,s=1,v=1,a=q,t=d,f=24;AAAA${ST}`, `${E}_Ga=q,i=1${ST}`, `${E}_25a1;x${ST}`];

const BENIGN = [`${E}[1;31m`, `${E}[0m`, `${E}[2J`, `${E}[H`, `${E}[?2004h`, `${E}[?2026h`, `${E}[?2026l`, `${E}[?25l`, `${E}[>4;2m`, `${E}[>7u`, `${E}[<u`, `${E}[22;2t`, `${E}[3b`, `${E}[0 q`,
  `${E}]0;title${BEL}`, `${E}]8;;https://x/?q=1${ST}`, `${E}]11;rgb:00/00/00${BEL}`, `${E}]52;c;aGk=${BEL}`, `${E}]1337;File=inline=1:AAAA${BEL}`, `${E}]7501;state=idle${ST}`,
  `${E}_Ga=T,f=100,i=7;iVBOR${ST}`, `${E}_Ga=d,d=A,q=2${ST}`, `${E}_Gi=1,qa=q;x${ST}`, `${E}Pq#0;2;0;0;0~${ST}`, `${E}(B`, `${E}7`, `${E}8`, `${E}=`, `${E}>`, `${E}\\`, `${E}[1c`, `${E}[2x`, `${E}[7n`, `${E}[3S`];

export const TAILS = ['c', 'n', '?\x07', '00h', '0c', '\\', 'x', 'abc', '\r\n', 'é', 'm', '; 5 H', '\x07', ST, 'hello', 'π', '\u0085', '\u009b', '\u009d', '\u009c', '\u0090', '\u009f', '😀', '\u{10EEEE}',
  '\t', '\x08', '\x7f', '\x01', ';', '?', '$p', '1', '2026$p', '+q', 'tmux;', ' ', ' ', '日'];

export function genQuery(r: R): string {
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
export const SOUP = [E, E, `${E}[`, `${E}]`, `${E}P`, `${E}_G`, `${E}\\`, BEL, '\x18', '\x1a', '?', '>', '=', '$', 'p', 'c', 'q', 'n', 'u', 't', 'S', ';', '0', '1', '2', '4', '6', '9', 'a=q,', 'a=d,', 'tmux;', '+q', '$q', '11;?', '52;c;?', 'A', ' ', '\r\n', 'é', 'π', `${E}${E}`, '\u009b', '\u0085', ':', '*y', 'x'];
export const soup = (r: R) => { let s = ''; const n = 1 + Math.floor(r() * 24); for (let i = 0; i < n; i++) s += pick(r, SOUP); return enc(s); };

export function randomCuts(r: R, len: number): number[] {
  const n = Math.floor(r() * 5); const s = new Set<number>();
  for (let i = 0; i < n; i++) s.add(Math.floor(r() * (len + 1)));
  return [...s].sort((a, b) => a - b).filter(x => x > 0 && x < len);
}
// ---------------------------------------------------------------- fake_harness.py streams + injected queries
export function harness(seed: number, length: number): Uint8Array {
  const py = new URL('../../fixtures/fake_harness.py', import.meta.url).pathname;
  const p = spawnSync('python3', ['-I', py, '--seed', String(seed), '--length', String(length)], { maxBuffer: 1 << 28 });
  if (p.status !== 0) throw new Error(`fake_harness.py failed: ${p.stderr}`);
  return new Uint8Array(p.stdout);
}
export function inject(r: R, base: Uint8Array, n: number): Uint8Array {
  const pos = Array.from({ length: n }, () => Math.floor(r() * (base.length + 1))).sort((a, b) => a - b);
  const parts: Uint8Array[] = []; let p = 0;
  for (const x of pos) { parts.push(base.subarray(p, x)); parts.push(enc(r() < 0.5 ? genQuery(r) : pick(r, FRAGMENTS) + genQuery(r) + pick(r, TAILS))); p = x; }
  parts.push(base.subarray(p)); return cat(parts);
}

export function mulberry32(a: number) { return () => { a |= 0; a = (a + 0x6d2b79f5) | 0; let t = Math.imul(a ^ (a >>> 15), 1 | a); t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t; return ((t ^ (t >>> 14)) >>> 0) / 4294967296; }; }

const metrics: Record<string, unknown> = {};
export function metric(k: string, v: unknown) { metrics[k] = v; }
export function writeMetrics(name: string) {
  const dir = new URL('../out/', import.meta.url).pathname;
  mkdirSync(dir, { recursive: true });
  writeFileSync(`${dir}metrics-${name}.json`, JSON.stringify(metrics, null, 1));
}
