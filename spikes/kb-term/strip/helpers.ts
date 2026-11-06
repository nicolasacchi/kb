import { QueryStripper } from './strip.ts';
import { mkdirSync, writeFileSync } from 'node:fs';

export const E = '\x1b';
export const enc = (s: string) => new TextEncoder().encode(s);
export const cat = (a: Uint8Array[]) => { const n = a.reduce((x, y) => x + y.length, 0); const r = new Uint8Array(n); let o = 0; for (const p of a) { r.set(p, o); o += p.length; } return r; };
export const hexs = (b: Uint8Array) => Buffer.from(b).toString('latin1').replace(/[^\x20-\x7e]/g, c => '\\x' + c.charCodeAt(0).toString(16).padStart(2, '0'));
export const eq = (a: Uint8Array, b: Uint8Array) => a.length === b.length && a.every((v, i) => v === b[i]);

/** run data through a fresh pipeline, cut at the given sorted offsets, flush at the end */
export function run(data: Uint8Array, cuts: number[] = []) {
  const s = new QueryStripper(); const parts: Uint8Array[] = []; let p = 0;
  for (const c of [...cuts, data.length]) { parts.push(s.push(data.subarray(p, c))); p = c; }
  parts.push(s.flush());
  return { out: cat(parts), s };
}

/** DCS-tmux envelope: every ESC of the payload doubled */
export const wrap = (inner: string) => `${E}Ptmux;${inner.replaceAll(E, E + E)}${E}\\`;

export function mulberry32(a: number) { return () => { a |= 0; a = (a + 0x6d2b79f5) | 0; let t = Math.imul(a ^ (a >>> 15), 1 | a); t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t; return ((t ^ (t >>> 14)) >>> 0) / 4294967296; }; }

const metrics: Record<string, unknown> = {};
export function metric(k: string, v: unknown) { metrics[k] = v; }
export function writeMetrics(name: string) {
  const dir = new URL('./out/', import.meta.url).pathname;
  mkdirSync(dir, { recursive: true });
  writeFileSync(`${dir}metrics-${name}.json`, JSON.stringify(metrics, null, 1));
}
