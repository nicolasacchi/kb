// Real-shape synthetic recording pins (KBT-GFX).
import { test, expect, afterAll } from 'bun:test';
import { mkdtempSync, readdirSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { ImageStore } from '../src/imagestore.ts';
import { GraphicsExtractor } from '../src/graphics.ts';
import type { GfxItem } from '../src/graphics.ts';
import { QueryStripper } from '../src/strip.ts';
import { cat, oracleScan, makePng, transmitApcs, placeApc } from './helpers-graphics.ts';
import { mulberry32 } from './helpers.ts';

const parents: string[] = [];

function mkParent(): string {
  const d = mkdtempSync(join(tmpdir(), 'kbt-t4-fix-'));
  parents.push(d);
  return d;
}

const FIXTURE = join(import.meta.dir, '../../fixtures/omp-synthetic-12img.bin');

function coalesce(items: GfxItem[]): GfxItem[] {
  const out: GfxItem[] = [];
  for (const it of items) {
    const last = out.length > 0 ? out[out.length - 1]! : null;
    if (it.kind === 'bytes' && last !== null && last.kind === 'bytes') {
      out[out.length - 1] = { kind: 'bytes', data: cat([last.data, it.data]) };
    } else out.push(it);
  }
  return out;
}

afterAll(() => {
  for (const d of parents) rmSync(d, { recursive: true, force: true });
});

test('twelve_img_fixture_matches_known_counts', () => {
  const input = new Uint8Array(readFileSync(FIXTURE));
  expect(input.length).toBe(610_240);
  const oracle = oracleScan(input);
  // Verified facts of the file: 119 APCs totalling 31,860 B, 42 transmits,
  // 14 distinct ids, 46 placements, 31 resets (3 d=A + 28 d=I).
  expect(input.length - oracle.bytes.length).toBe(31_860);
  expect(oracle.transmits.length).toBe(42);
  expect(new Set(oracle.transmits.map((t) => t.id)).size).toBe(14);
  expect(oracle.places.length).toBe(46);
  expect(oracle.resets).toBe(31);

  const run = (makeStripper: boolean, cuts: number[]): { imagedefs: number; resets: number[]; files: number; shaById: Map<number, string> } => {
    const parent = mkParent();
    const sess = join(parent, 'sess');
    const store = new ImageStore({ sessionDir: sess, now: () => 1_000 });
    const ex = new GraphicsExtractor(store);
    const stripper = makeStripper ? new QueryStripper() : null;
    const items: GfxItem[] = [];
    let p = 0;
    for (const c of [...cuts, input.length]) {
      const chunk = input.subarray(p, c);
      if (stripper) items.push(...ex.push(stripper.push(chunk)));
      else items.push(...ex.push(chunk));
      p = c;
    }
    if (stripper) items.push(...ex.push(stripper.flush()));
    items.push(...ex.flush());
    const co = coalesce(items);
    const imagedefs = co.filter((i) => i.kind === 'imagedef');
    const resets = co.filter((i) => i.kind === 'gfxreset').map((r) => r.kind === 'gfxreset' ? r.gen : -1);
    const files = readdirSync(join(sess, 'img')).filter((n) => !n.startsWith('.tmp-')).length;
    const shaById = new Map<number, string>();
    // The sha per id from the store (identical across the three renders).
    for (const e of store.entries()) shaById.set(e.id, e.sha256);
    return { imagedefs: imagedefs.length, resets, files, shaById };
  };

  const baseline = run(false, []);
  expect(baseline.imagedefs).toBe(14);
  expect(baseline.resets).toEqual(Array.from({ length: 31 }, (_, i) => i + 1));
  expect(baseline.files).toBe(14);
  // All gen 1, all c=64 r=20.
  {
    const parent = mkParent();
    const sess = join(parent, 'sess');
    const store = new ImageStore({ sessionDir: sess, now: () => 1_000 });
    const ex = new GraphicsExtractor(store);
    const items = coalesce([...ex.push(input), ...ex.flush()]);
    for (const it of items) {
      if (it.kind !== 'imagedef') continue;
      expect(it.def.gen).toBe(1);
      expect(it.def.cols).toBe(64);
      expect(it.def.rows).toBe(20);
    }
    const parts: Uint8Array[] = [];
    for (const it of items) {
      if (it.kind === 'bytes') parts.push(it.data);
    }
    const outBytes = cat(parts);
    expect(outBytes.length).toBe(input.length - 31_860);
    expect(Buffer.from(outBytes).includes(Buffer.from('\x1b_G'))).toBe(false);
    // All 60,416 placeholder cells survive.
    const cells = Buffer.from('\u{10EEEE}');
    let count = 0;
    let idx = 0;
    while (true) {
      const k = Buffer.from(outBytes).indexOf(cells, idx);
      if (k === -1) break;
      count++;
      idx = k + cells.length;
    }
    expect(count).toBe(60_416);
  }
  // Seeded random chunkings + through QueryStripper give the same counts.
  const rand = mulberry32(4242);
  for (let round = 0; round < 3; round++) {
    const cuts: number[] = [];
    let pos = 0;
    while (pos < input.length) {
      pos += 1 + Math.floor(rand() * 4096);
      if (pos < input.length) cuts.push(pos);
    }
    const r = run(round % 2 === 0, cuts);
    expect(r.imagedefs).toBe(14);
    expect(r.resets).toEqual(Array.from({ length: 31 }, (_, i) => i + 1));
    expect(r.files).toBe(14);
    expect(r.shaById.size).toBe(14);
    // Identical sha per id across renders (same map as baseline).
    for (const [id, sha] of r.shaById) expect(sha).toBe(baseline.shaById.get(id));
  }
});

test('store_survives_restart', () => {
  const input = new Uint8Array(readFileSync(FIXTURE));
  const parent = mkParent();
  const sess = join(parent, 'sess');
  const store = new ImageStore({ sessionDir: sess, now: () => 1_000 });
  const ex = new GraphicsExtractor(store);
  const first = [...ex.push(input), ...ex.flush()];
  expect(first.filter((i) => i.kind === 'imagedef').length).toBe(14);
  const firstParts: Uint8Array[] = [];
  for (const it of first) if (it.kind === 'bytes') firstParts.push(it.data);
  const firstBytes = cat(firstParts);
  expect(store.entries().length).toBe(14);
  // Fresh store on the same dir reports the same entries.
  const fresh = new ImageStore({ sessionDir: sess, now: () => 1_000 });
  expect(fresh.entries()).toEqual(store.entries());
  // Pushing again through a new extractor yields zero imagedef items.
  const ex2 = new GraphicsExtractor(fresh);
  const again = [...ex2.push(input), ...ex2.flush()];
  // Seeded last-emitted: a restart re-push emits zero imagedefs and identical
  // viewer bytes (fails if seeding broke and all 14 defs were re-emitted).
  expect(again.filter((i) => i.kind === 'imagedef').length).toBe(0);
  const againParts: Uint8Array[] = [];
  for (const it of again) if (it.kind === 'bytes') againParts.push(it.data);
  expect(cat(againParts)).toEqual(firstBytes);
  // Then one id with different bytes yields gen 2.
  const victim = fresh.entries()[0]!;
  const oldSha = victim.sha256;
  const freshPng = makePng(16, 16, 250);
  const freshStream = cat([...transmitApcs(victim.id, freshPng, 4096), placeApc(victim.id, victim.cols!, victim.rows!)]);
  const ex3 = new GraphicsExtractor(fresh);
  const out3 = ex3.push(freshStream);
  expect(out3.filter((i) => i.kind === 'imagedef').length).toBe(1);
  expect(fresh.get(victim.id)!.gen).toBe(2);
  expect(fresh.get(victim.id)!.sha256).not.toBe(oldSha);
});
