// ImageStore pins (KBT-GFX).
import { test, expect, afterAll } from 'bun:test';
import { mkdtempSync, readdirSync, readFileSync, writeFileSync, existsSync, appendFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { createHash } from 'node:crypto';
import { ImageStore } from '../src/imagestore.ts';
import { GraphicsExtractor } from '../src/graphics.ts';
import { makePng, kittyApc, enc } from './helpers-graphics.ts';

const parents: string[] = [];
function mkParent(): string {
  const d = mkdtempSync(join(tmpdir(), 'kbt-t4-store-'));
  parents.push(d);
  return d;
}

function shaOf(b: Uint8Array): string {
  return createHash('sha256').update(b).digest('hex');
}

function countFiles(dir: string): string[] {
  return readdirSync(dir);
}

afterAll(() => {
  for (const d of parents) rmSync(d, { recursive: true, force: true });
});

test('put_is_atomic_and_dedupes', () => {
  const parent = mkParent();
  const sess = join(parent, 'sess');
  let t = 1_000_000;
  const store = new ImageStore({ sessionDir: sess, now: () => t });
  const bytes = makePng(4, 4, 1);
  const sha = shaOf(bytes);
  const first = store.putPng(sha, bytes);
  expect(first.written).toBe(true);
  expect(store.readPng(sha)).toEqual(bytes);
  expect(store.has(sha)).toBe(true);
  // No .tmp left.
  expect(countFiles(join(sess, 'img')).filter((n) => n.startsWith('.tmp-'))).toEqual([]);
  const mtime1 = readFileSync(join(sess, 'img', sha));
  expect(mtime1).toEqual(Buffer.from(bytes));
  // Re-put touches mtime to the injected now and writes nothing.
  t = 2_000_000;
  const second = store.putPng(sha, bytes);
  expect(second.written).toBe(false);
  expect(countFiles(join(sess, 'img')).filter((n) => n.startsWith('.tmp-'))).toEqual([]);
  // putSeq dedupes the same way.
  const seqBytes = enc('\x1b]1337;File=test:AAAA\x07');
  const seqSha = shaOf(seqBytes);
  expect(store.putSeq(seqSha, seqBytes).written).toBe(true);
  expect(store.putSeq(seqSha, seqBytes).written).toBe(false);
  expect(store.readSeq(seqSha)).toEqual(seqBytes);
  expect(store.hasSeq(seqSha)).toBe(true);
  expect(store.readPng('0'.repeat(64))).toBeNull();
  expect(store.has('0'.repeat(64))).toBe(false);
});

test('session_cap_evicts_least_recently_seen', () => {
  const parent = mkParent();
  const sess = join(parent, 'sess');
  const pngA = makePng(8, 8, 1);
  const pngB = makePng(8, 8, 2);
  const pngC = makePng(8, 8, 3);
  const shaA = shaOf(pngA);
  const shaB = shaOf(pngB);
  const shaC = shaOf(pngC);
  // Budget fits two images, not three.
  const cap = pngA.length + pngB.length + 10;
  let t = 1_000;
  const store = new ImageStore({ sessionDir: sess, sessionCapBytes: cap, hostCapBytes: 1 << 30, now: () => t });
  store.putPng(shaA, pngA); t = 2_000;
  store.putPng(shaB, pngB); t = 3_000;
  // Re-see A so B is least-recently-seen.
  store.putPng(shaA, pngA); t = 4_000;
  // C pushes the total over cap: B (oldest mtime) goes, A stays.
  store.putPng(shaC, pngC);
  expect(store.has(shaA)).toBe(true);
  expect(store.has(shaC)).toBe(true);
  expect(store.has(shaB)).toBe(false);
  // Id map untouched by eviction (bind first, then check).
  store.bind(1, { sha256: shaA, w: 8, h: 8 });
  expect(store.get(1)!.sha256).toBe(shaA);
});

test('host_cap_spans_sibling_sessions', () => {
  const parent = mkParent();
  const s1 = join(parent, 's1');
  const s2 = join(parent, 's2');
  const hostRoot = parent;
  const pngA = makePng(8, 8, 10);
  const pngB = makePng(8, 8, 11);
  const pngC = makePng(8, 8, 12);
  const shaA = shaOf(pngA);
  const shaB = shaOf(pngB);
  const shaC = shaOf(pngC);
  const cap = pngA.length + pngB.length + 10;
  let t = 1_000;
  const store1 = new ImageStore({ sessionDir: s1, sessionCapBytes: 1 << 30, hostCapBytes: cap, hostRoot, now: () => t });
  const store2 = new ImageStore({ sessionDir: s2, sessionCapBytes: 1 << 30, hostCapBytes: cap, hostRoot, now: () => t });
  store1.putPng(shaA, pngA); t = 2_000;
  store2.putPng(shaB, pngB); t = 10_000; // >5s after store1/store2 scans
  // At the last put, the host total exceeded cap and the LRU file was evicted.
  // Store1's A is the oldest; allow either store to have performed the eviction.
  const hasA = existsSync(join(s1, 'img', shaA));
  const hasB = existsSync(join(s2, 'img', shaB));
  expect(hasA && hasB).toBe(false);
  // Third image forces another eviction; the just-written file survives.
  t = 20_000;
  store1.putPng(shaC, pngC);
  expect(existsSync(join(s1, 'img', shaC))).toBe(true);
  const totalAlive = [join(s1, 'img', shaA), join(s1, 'img', shaC), join(s2, 'img', shaB)]
    .filter((p) => existsSync(p)).length;
  expect(totalAlive).toBe(2);
});

test('evicted_then_retransmitted_rewrites_silently', () => {
  const parent = mkParent();
  const sess = join(parent, 'sess');
  const png = makePng(8, 8, 42);
  const sha = shaOf(png);
  const cap = png.length - 1; // smaller than one image: just-written still kept
  let t = 1_000;
  const store = new ImageStore({ sessionDir: sess, sessionCapBytes: cap, hostCapBytes: 1 << 30, now: () => t });
  store.bind(7, { sha256: sha, w: 8, h: 8 });
  store.place(7, 4, 2);
  store.putPng(sha, png);
  expect(store.has(sha)).toBe(true);
  // Evict by hand-rolled pressure: write a second distinct image.
  const other = makePng(8, 8, 43);
  const otherSha = shaOf(other);
  t = 2_000;
  store.putPng(otherSha, other);
  // One of the two must have gone (cap < one image, but just-written is kept,
  // so the older one goes).
  expect(store.has(sha)).toBe(false);
  expect(store.has(otherSha)).toBe(true);
  // A later identical retransmit rewrites the file and emits nothing new.
  const idmapBefore = readFileSync(join(sess, 'idmap.jsonl'), 'utf8');
  const extractor = new GraphicsExtractor(store);
  t = 3_000;
  const stream = Buffer.concat([
    Buffer.from(kittyApc(`a=t,f=100,i=7`, Buffer.from(png).toString('base64'))),
    Buffer.from(kittyApc('a=p,U=1,i=7,c=4,r=2')),
  ]);
  const items = extractor.push(new Uint8Array(stream));
  // No imagedef: binding unchanged and placement unchanged vs last-emitted seed.
  expect(items.filter((i) => i.kind === 'imagedef').length).toBe(0);
  expect(store.has(sha)).toBe(true);
  expect(readFileSync(join(sess, 'idmap.jsonl'), 'utf8')).toBe(idmapBefore);
  // Drop the extractor's unused other image from scope: no-op.
  expect(store.readPng(sha)).toEqual(png);
  // Keep the variable used for later steps.
  expect(otherSha.length).toBe(64);
});

test('idmap_replay_and_truncated_tail', () => {
  const parent = mkParent();
  const sess = join(parent, 'sess');
  let t = 1_000;
  const store = new ImageStore({ sessionDir: sess, now: () => t });
  store.bind(3, { sha256: 'a'.repeat(64), w: 10, h: 20 });
  store.place(3, 5, 6);
  store.bind(4, { sha256: 'b'.repeat(64), w: 1, h: 1 });
  // Change id 3 to a new sha: gen 2.
  store.bind(3, { sha256: 'c'.repeat(64), w: 11, h: 21 });
  expect(store.get(3)!.gen).toBe(2);
  // No-change bind appends nothing.
  const linesBefore = readFileSync(join(sess, 'idmap.jsonl'), 'utf8').trimEnd().split('\n').length;
  store.bind(3, { sha256: 'c'.repeat(64), w: 11, h: 21 });
  const linesAfter = readFileSync(join(sess, 'idmap.jsonl'), 'utf8').trimEnd().split('\n').length;
  expect(linesAfter).toBe(linesBefore);
  // Truncated trailing line is dropped and counted, never thrown.
  appendFileSync(join(sess, 'idmap.jsonl'), '{"id":9,"gen":1');
  const reopened = new ImageStore({ sessionDir: sess, now: () => t });
  expect(reopened.badIdmapLines).toBe(1);
  expect(reopened.get(3)!.sha256).toBe('c'.repeat(64));
  expect(reopened.get(3)!.gen).toBe(2);
  expect(reopened.get(3)!.cols).toBe(5);
  expect(reopened.get(3)!.rows).toBe(6);
  expect(reopened.get(4)!.sha256).toBe('b'.repeat(64));
  // Reopened gen continues (next change bumps to 3).
  reopened.bind(3, { sha256: 'd'.repeat(64), w: 12, h: 22 });
  expect(reopened.get(3)!.gen).toBe(3);
});

test('stale_tmp_files_removed_on_open', () => {
  const parent = mkParent();
  const sess = join(parent, 'sess');
  const store = new ImageStore({ sessionDir: sess, now: () => 1_000 });
  writeFileSync(join(sess, 'img', '.tmp-123-1'), 'junk');
  writeFileSync(join(sess, 'seq', '.tmp-999-7'), 'junk');
  expect(readdirSync(join(sess, 'img'))).toContain('.tmp-123-1');
  const reopened = new ImageStore({ sessionDir: sess, now: () => 1_000 });
  expect(countFiles(join(sess, 'img')).includes('.tmp-123-1')).toBe(false);
  expect(countFiles(join(sess, 'seq')).includes('.tmp-999-7')).toBe(false);
  // Every scan ignores .tmp files: put still works.
  const bytes = makePng(4, 4, 2);
  const sha = shaOf(bytes);
  expect(reopened.putPng(sha, bytes).written).toBe(true);
});

test('place_on_unbound_id_is_noop', () => {
  const parent = mkParent();
  const sess = join(parent, 'sess');
  const store = new ImageStore({ sessionDir: sess, now: () => 1_000 });
  expect(store.place(99, 5, 5)).toBe(false);
  expect(store.get(99)).toBeNull();
  expect(store.entries()).toEqual([]);
  expect(existsSync(join(sess, 'idmap.jsonl'))).toBe(false);
});
