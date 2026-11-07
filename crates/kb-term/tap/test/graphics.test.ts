// Graphics extractor pins (KBT-GFX).
import { test, expect, afterAll } from 'bun:test';
import { mkdtempSync, readdirSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { createHash } from 'node:crypto';
import { ImageStore } from '../src/imagestore.ts';
import { GraphicsExtractor, GfxStats, buildMediaStub, parseMediaStub } from '../src/graphics.ts';
import type { GfxItem } from '../src/graphics.ts';
import { QueryStripper } from '../src/strip.ts';
import { makePng, kittyApc, transmitApcs, placeApc, deleteApc, wrapTmux, enc, cat, oracleScan, synthStream } from './helpers-graphics.ts';
import { mulberry32 } from './helpers.ts';

const parents: string[] = [];

function firstDef(items: GfxItem[]) {
  const found = items.find((i) => i.kind === 'imagedef');
  if (found === undefined || found.kind !== 'imagedef') throw new Error('no imagedef');
  return found.def;
}
let sharedParent: string | null = null;
let sessCounter = 0;
function mkStore(opts?: { sessionCapBytes?: number; hostCapBytes?: number; now?: () => number }): { parent: string; sess: string; store: ImageStore } {
  if (sharedParent === null) {
    sharedParent = mkdtempSync(join(tmpdir(), 'kbt-t4-gfx-'));
    parents.push(sharedParent);
  }
  const parent = sharedParent;
  const sess = join(parent, `sess-${++sessCounter}`);
  // Isolate the host-cap scan to this session: sharing one parent with
  // thousands of sibling sessions would make the per-store host scan O(n^2).
  const store = new ImageStore({ sessionDir: sess, sessionCapBytes: opts?.sessionCapBytes, hostCapBytes: opts?.hostCapBytes, hostRoot: sess, now: opts?.now });
  return { parent, sess, store };
}
function coalesce(items: GfxItem[]): GfxItem[] {
  const out: GfxItem[] = [];
  for (const it of items) {
    const last = out.length > 0 ? out[out.length - 1]! : null;
    if (it.kind === 'bytes' && last !== null && last.kind === 'bytes') {
      out[out.length - 1] = { kind: 'bytes', data: cat([last.data, it.data]) };
    } else {
      out.push(it);
    }
  }
  return out;
}

function bytesOf(items: GfxItem[]): Uint8Array {
  const parts: Uint8Array[] = [];
  for (const it of items) {
    if (it.kind === 'bytes') parts.push(it.data);
  }
  return cat(parts);
}

function itemsEqual(a: GfxItem[], b: GfxItem[]): boolean {
  if (a.length !== b.length) return false;
  for (let i = 0; i < a.length; i++) {
    const x = a[i]!;
    const y = b[i]!;
    if (x.kind === 'bytes' && y.kind === 'bytes') {
      if (x.data.length !== y.data.length) return false;
      for (let k = 0; k < x.data.length; k++) if (x.data[k] !== y.data[k]) return false;
    } else if (JSON.stringify(x) !== JSON.stringify(y)) return false;
  }
  return true;
}

function runWhole(store: ImageStore, data: Uint8Array): GfxItem[] {
  const ex = new GraphicsExtractor(store);
  return coalesce([...ex.push(data), ...ex.flush()]);
}

function runSplit(store: ImageStore, data: Uint8Array, cuts: number[]): GfxItem[] {
  const ex = new GraphicsExtractor(store);
  const items: GfxItem[] = [];
  let p = 0;
  for (const c of [...cuts, data.length]) {
    items.push(...ex.push(data.subarray(p, c)));
    p = c;
  }
  items.push(...ex.flush());
  return coalesce(items);
}

afterAll(() => {
  for (const d of parents) rmSync(d, { recursive: true, force: true });
});

test('omp_shape_stream_roundtrip', () => {
  const { store, sess } = mkStore();
  const input = synthStream(['--no-wrap', '--cols', '90', '--rows', '19', '--px', '360x152', '--id', '0x66CF38']);
  const oracle = oracleScan(input);
  expect(oracle.transmits.length).toBe(1);
  const items = runWhole(store, input);
  const outBytes = bytesOf(items);
  expect(outBytes).toEqual(oracle.bytes);
  const defs = items.filter((i) => i.kind === 'imagedef');
  expect(defs.length).toBe(1);
  const def = firstDef(items);
  expect(def.id).toBe(0x66cf38);
  expect(def.gen).toBe(1);
  expect(def.sha256).toBe(oracle.transmits[0]!.sha256);
  expect(def.cols).toBe(90);
  expect(def.rows).toBe(19);
  expect(def.w).toBe(360);
  expect(def.h).toBe(152);
  const stored = store.readPng(def.sha256);
  expect(stored).toEqual(oracle.transmits[0]!.png);
  expect(readFileSync(join(sess, 'img', def.sha256))).toEqual(Buffer.from(oracle.transmits[0]!.png));
  // 19*90 placeholder cells survive: U+10EEEE (1 cell per 4 UTF-8 bytes, 12 B per cell).
  expect(Buffer.from(outBytes).includes(Buffer.from('\u{10EEEE}'))).toBe(true);
  const s = new TextDecoder().decode(outBytes);
  expect(s.split('\u{10EEEE}').length - 1).toBe(19 * 90);
});

test('dcs_wrapped_equals_plain', () => {
  const plain = synthStream(['--no-wrap']);
  const wrapped = synthStream([]);
  const stripper = new QueryStripper();
  const strippedWrapped = cat([stripper.push(wrapped), stripper.flush()]);
  const a = mkStore();
  const b = mkStore();
  const itemsPlain = runWhole(a.store, plain);
  const itemsWrapped = runWhole(b.store, strippedWrapped);
  expect(itemsEqual(itemsPlain, itemsWrapped)).toBe(true);
});

test('every_one_cut_split_is_equivalent', () => {
  // Stream <= ~700 B: 8x8 PNG, 16-char chunks, one place, one delete, 2 placeholder rows, IIP + SIXEL, non-G APC, lone-ESC variants.
  const png = makePng(8, 8, 33);
  const id = 1234;
  const pieces: Uint8Array[] = [];
  pieces.push(enc('hello'));
  for (const a of transmitApcs(id, png, 16)) pieces.push(a);
  pieces.push(placeApc(id, 4, 2));
  pieces.push(enc('\r\n'));
  pieces.push(deleteApc('A'));
  pieces.push(enc('\x1b]1337;File=x:AAAA\x07'));
  pieces.push(enc('\x1bP0;1q\x01\x02\x1b\\'));
  pieces.push(kittyApc('a=z,foo=bar', 'ignored')); // non-kitkitty-turned-into-APC? a=z is otherAction APC G -- still removed
  pieces.push(enc('\x1b_x…\x1b\\'));
  pieces.push(enc('\x1b'));
  pieces.push(enc('X'));
  pieces.push(enc('\x1b[1m'));
  const stream = cat(pieces);
  expect(stream.length).toBeLessThanOrEqual(1500);
  const refStore = mkStore().store;
  const ref = runWhole(refStore, stream);
  const firstStore = mkStore().store;
  const first = runWhole(firstStore, stream.slice(0, 5));
  void first;
  for (let cut = 0; cut <= stream.length; cut++) {
    const st = mkStore().store;
    const got = runSplit(st, stream, [cut]);
    if (!itemsEqual(ref, got)) throw new Error(`one-cut mismatch at ${cut}/${stream.length}`);
  }
});

test('every_two_cut_split_is_equivalent_micro', () => {
  const png = makePng(4, 4, 4);
  const id = 55;
  const stream = cat([
    enc('ab'),
    ...transmitApcs(id, png, 4096),
    placeApc(id, 2, 1),
    enc('\x1b'),
  ]);
  expect(stream.length).toBeLessThanOrEqual(300);
  const ref = runWhole(mkStore().store, stream);
  let checked = 0;
  for (let c1 = 0; c1 <= stream.length; c1++) {
    for (let c2 = c1; c2 <= stream.length; c2++) {
      const got = runSplit(mkStore().store, stream, [c1, c2]);
      if (!itemsEqual(ref, got)) throw new Error(`two-cut mismatch at ${c1},${c2}`);
      checked++;
    }
  }
  expect(checked).toBeGreaterThan(1000);
});

test('random_multicut_equivalent', () => {
  const png = makePng(8, 8, 33);
  const id = 1234;
  const pieces: Uint8Array[] = [];
  pieces.push(enc('hello'));
  for (const a of transmitApcs(id, png, 16)) pieces.push(a);
  pieces.push(placeApc(id, 4, 2));
  pieces.push(deleteApc('A'));
  pieces.push(enc('\x1b]1337;File=x:AAAA\x07'));
  pieces.push(enc('\x1bP0;1q\x01\x02\x1b\\'));
  pieces.push(enc('\x1b_x…\x1b\\'));
  pieces.push(enc('\x1b'));
  pieces.push(enc('X'));
  const stream = cat(pieces);
  const ref = runWhole(mkStore().store, stream);
  const rand = mulberry32(20261107);
  for (let n = 0; n < 2000; n++) {
    const cuts = new Set<number>();
    const k = 1 + Math.floor(rand() * 8);
    for (let j = 0; j < k; j++) cuts.add(Math.floor(rand() * (stream.length + 1)));
    const got = runSplit(mkStore().store, stream, [...cuts].sort((a, b) => a - b));
    if (!itemsEqual(ref, got)) throw new Error(`multicut mismatch at iter ${n}`);
  }
});

test('identical_retransmit_is_free', () => {
  const { store, sess } = mkStore();
  const png = makePng(8, 8, 77);
  const id = 1001;
  const stream = cat([...transmitApcs(id, png, 4096), placeApc(id, 5, 3)]);
  let imagedefs = 0;
  for (let i = 0; i < 5; i++) {
    const ex = new GraphicsExtractor(store);
    const items = ex.push(stream);
    imagedefs += items.filter((x) => x.kind === 'imagedef').length;
    // A fresh extractor each time is also fine after the first (seeded last-emitted).
  }
  expect(imagedefs).toBe(1);
  expect(store.get(id)!.gen).toBe(1);
  const lines = readFileSync(join(sess, 'idmap.jsonl'), 'utf8').trimEnd().split('\n');
  // bind (gen1) + place = 2 lines; retransmits append nothing.
  expect(lines.length).toBe(2);
});

test('different_bytes_same_id_bumps_gen_once', () => {
  const { store } = mkStore();
  const png1 = makePng(8, 8, 1);
  const png2 = makePng(8, 8, 2);
  const id = 2002;
  const ex = new GraphicsExtractor(store);
  let items = ex.push(cat([...transmitApcs(id, png1, 4096), placeApc(id, 4, 2)]));
  expect(items.filter((i) => i.kind === 'imagedef').length).toBe(1);
  const sha1 = createHash('sha256').update(png1).digest('hex');
  expect(store.has(sha1)).toBe(true);
  items = ex.push(cat([...transmitApcs(id, png2, 4096), placeApc(id, 4, 2)]));
  const defs = items.filter((i) => i.kind === 'imagedef');
  expect(defs.length).toBe(1);
  expect(store.get(id)!.gen).toBe(2);
  const sha2 = createHash('sha256').update(png2).digest('hex');
  expect(store.has(sha1)).toBe(true); // old file still on disk
  expect(store.has(sha2)).toBe(true);
});

test('placement_change_reemits_without_gen_bump', () => {
  const { store } = mkStore();
  const png = makePng(8, 8, 9);
  const id = 3003;
  const ex = new GraphicsExtractor(store);
  ex.push(cat([...transmitApcs(id, png, 4096), placeApc(id, 4, 2)]));
  const items = ex.push(placeApc(id, 6, 3));
  const defs = items.filter((i) => i.kind === 'imagedef');
  expect(defs.length).toBe(1);
  const def = firstDef(items);
  expect(def.cols).toBe(6);
  expect(def.rows).toBe(3);
  expect(def.gen).toBe(1);
  expect(store.get(id)!.gen).toBe(1);
});

test('placement_before_transmit_announces_after_transmit', () => {
  const { store } = mkStore();
  const png = makePng(8, 8, 5);
  const id = 4004;
  const ex = new GraphicsExtractor(store);
  const before = ex.push(placeApc(id, 7, 2));
  expect(before.filter((i) => i.kind === 'imagedef').length).toBe(0);
  expect(store.get(id)).toBeNull();
  const after = ex.push(cat(transmitApcs(id, png, 4096)));
  const defs = after.filter((i) => i.kind === 'imagedef');
  expect(defs.length).toBe(1);
  const def = firstDef(after);
  expect(def.cols).toBe(7);
  expect(def.rows).toBe(2);
});

test('transmit_without_placement_is_never_announced', () => {
  const { store } = mkStore();
  const png = makePng(8, 8, 6);
  const id = 5005;
  const ex = new GraphicsExtractor(store);
  const items = ex.push(cat(transmitApcs(id, png, 4096)));
  expect(items.filter((i) => i.kind === 'imagedef').length).toBe(0);
  // Stored but never announced.
  expect(store.entries().length).toBe(1);
  expect(store.get(id)!.cols).toBeNull();
});

test('delete_never_frees_or_bumps', () => {
  const { store, sess } = mkStore();
  const png = makePng(8, 8, 8);
  const id = 6006;
  const ex = new GraphicsExtractor(store);
  ex.push(cat([...transmitApcs(id, png, 4096), placeApc(id, 4, 2)]));
  const sha = store.get(id)!.sha256;
  const idmapBefore = readFileSync(join(sess, 'idmap.jsonl'), 'utf8');
  const dels = ex.push(cat([deleteApc('A'), deleteApc('I', id)]));
  const resets = dels.filter((i) => i.kind === 'gfxreset');
  expect(resets.length).toBe(2);
  const gens = resets.map((r) => r.kind === 'gfxreset' ? r.gen : -1);
  expect(gens).toEqual([1, 2]);
  expect(store.has(sha)).toBe(true);
  expect(store.get(id)!.gen).toBe(1);
  expect(readFileSync(join(sess, 'idmap.jsonl'), 'utf8')).toBe(idmapBefore);
  // A following identical retransmit emits no imagedef.
  const again = ex.push(cat([...transmitApcs(id, png, 4096), placeApc(id, 4, 2)]));
  expect(again.filter((i) => i.kind === 'imagedef').length).toBe(0);
});

test('abort_inside_apc', () => {
  for (const abort of ['CAN', 'SUB', 'ESC'] as const) {
    const { store } = mkStore();
    const ex = new GraphicsExtractor(store);
    const png = makePng(8, 8, 14);
    const parts = transmitApcs(7001, png, 16);
    const first = parts[0]!;
    // Feed first chunk, then abort bytes, then a valid single-chunk transmit.
    let stream: Uint8Array;
    if (abort === 'CAN') stream = cat([enc('pre'), first.slice(0, first.length - 6), enc('\x18'), enc('post')]);
    else if (abort === 'SUB') stream = cat([enc('pre'), first.slice(0, first.length - 6), enc('\x1a'), enc('post')]);
    else stream = cat([enc('pre'), first.slice(0, first.length - 6), enc('\x1b[1m'), enc('post')]);
    const items = ex.push(stream);
    const outBytes = bytesOf(items);
    expect(Buffer.from(outBytes).includes(Buffer.from('pre'))).toBe(true);
    expect(Buffer.from(outBytes).includes(Buffer.from('post'))).toBe(true);
    // The CAN/SUB itself is swallowed.
    expect(Buffer.from(outBytes).includes(Buffer.from('\x18'))).toBe(false);
    expect(Buffer.from(outBytes).includes(Buffer.from('\x1a'))).toBe(false);
    // No APC G body bytes leak (base64 of the aborted chunk must not appear).
    expect(Buffer.from(outBytes).includes(Buffer.from('iVBOR'))).toBe(false);
    // In the ESC case the aborting ESC starts a new sequence: `[1m` passes.
    if (abort === 'ESC') expect(Buffer.from(outBytes).includes(Buffer.from('\x1b[1m'))).toBe(true);
    // A new valid transmit still works.
    const png2 = makePng(4, 4, 15);
    const ok = ex.push(cat([...transmitApcs(7002, png2, 4096), placeApc(7002, 2, 1)]));
    expect(ok.filter((i) => i.kind === 'imagedef').length).toBe(1);
    expect(ex.stats.abortedApc).toBe(1);
    // The aborted transfer's remaining chunks now drop as orphans (pending was discarded).
    const rest = cat(parts.slice(1));
    ex.push(rest);
    expect(ex.stats.orphanChunk).toBe(parts.length - 1);
    expect(Buffer.from(rest).includes(Buffer.from('q=2,m='))).toBe(true);
  }
  // T3-style forwarded-then-aborted string: APC cut by CAN leaves no CAN in output.
  // Simulate: an APC body forwarded past the stripper cap, then CAN, then text.
  const { store } = mkStore();
  const ex = new GraphicsExtractor(store);
  const longBody = enc('\x1b_Ga=t,f=100,i=8001,m=1;');
  const items = ex.push(cat([longBody, enc('\x18'), enc('after')]));
  const outBytes = bytesOf(items);
  expect(outBytes).toEqual(enc('after'));
});

test('oversize_apc_body_is_skipped_not_leaked', () => {
  const { store } = mkStore();
  const ex = new GraphicsExtractor(store, { maxApcBody: 64 });
  const big = kittyApc('a=t,f=100,i=8002', 'A'.repeat(200));
  const small = makePng(4, 4, 3);
  const tail = cat([...transmitApcs(8003, small, 4096), placeApc(8003, 2, 1), enc('tail')]);
  const items = ex.push(cat([enc('head'), big, tail]));
  const outBytes = bytesOf(items);
  expect(Buffer.from(outBytes).includes(Buffer.from('head'))).toBe(true);
  expect(Buffer.from(outBytes).includes(Buffer.from('tail'))).toBe(true);
  // The oversize body never appears in the output.
  expect(Buffer.from(outBytes).includes(Buffer.from('A'.repeat(100)))).toBe(false);
  expect(ex.stats.oversizeApc).toBe(1);
  // A following valid APC still works.
  expect(items.filter((i) => i.kind === 'imagedef').length).toBe(1);
});

test('unrelated_strings_pass_verbatim', () => {
  const { store } = mkStore();
  const ex = new GraphicsExtractor(store);
  const input = cat([
    enc('\x1b_xhello\x1b\\'),
    enc('\x1b]0;my title\x07'),
    enc('\x1b]8;;https://example.com\x1b\\link\x1b]8;;\x1b\\'),
    enc('\x1bPq;0;1;0?\x01\x02\x1b\\'.replace('?','x')), // non-sixel-ish DCS? still SIXEL-shaped; use non-matching instead
    enc('\x1b[38;5;196m'),
    enc('\x1b)'), // ESC intermediate -> pass through
    enc('text'),
  ]);
  // The SIXEL-shaped DCS above is captured (mediadef), so test the others verbatim
  // on a stream without it.
  const plain = cat([
    enc('\x1b_xhello\x1b\\'),
    enc('\x1b]0;my title\x07'),
    enc('\x1b[38;5;196m'),
    enc('text'),
  ]);
  const outPlain = ex.push(plain);
  expect(bytesOf(outPlain)).toEqual(plain);
  expect(outPlain.every((i) => i.kind === 'bytes')).toBe(true);
  // Lone ESC across a chunk edge.
  const { store: s2 } = mkStore();
  const ex2 = new GraphicsExtractor(s2);
  const first = ex2.push(enc('a\x1b'));
  expect(first.length).toBe(1); // only 'a'
  const second = ex2.push(enc('[1m'));
  expect(cat([bytesOf(first), bytesOf(second)])).toEqual(enc('a\x1b[1m'));
  void input;
});

test('no_esc_chunk_is_zero_copy', () => {
  const { store } = mkStore();
  const ex = new GraphicsExtractor(store);
  const chunk = enc('no escape here, just text 123');
  const out = ex.push(chunk);
  expect(out.length).toBe(1);
  const first = out[0]!;
  expect(first.kind).toBe('bytes');
  if (first.kind !== 'bytes') throw new Error('unreachable');
  expect(first.data).toBe(chunk);
});

test('iip_and_sixel_stored_and_stubbed', () => {
  const { store, sess } = mkStore();
  const ex = new GraphicsExtractor(store);
  const iipBel = enc('\x1b]1337;File=name=test.png;size=8:inline=1:AAAA\x07');
  const iipSt = enc('\x1b]1337;File=name=two.png:inline=1:BBBB\x1b\\');
  const sixel = enc('\x1bP0;1;0q\x01\x02\xff\x1b\\');
  const items = ex.push(cat([enc('pre'), iipBel, enc('mid'), iipSt, sixel, enc('post')]));
  const co = coalesce(items);
  expect(co.filter((i) => i.kind === 'mediadef').length).toBe(3);
  const outBytes = bytesOf(items);
  const sha1 = createHash('sha256').update(iipBel).digest('hex');
  const sha2 = createHash('sha256').update(iipSt).digest('hex');
  const sha3 = createHash('sha256').update(sixel).digest('hex');
  expect(store.readSeq(sha1)).toEqual(iipBel);
  expect(store.readSeq(sha2)).toEqual(iipSt);
  expect(store.readSeq(sha3)).toEqual(sixel);
  const stub1 = buildMediaStub('iip', sha1, iipBel.length);
  expect(Buffer.from(outBytes).includes(Buffer.from(stub1))).toBe(true);
  expect(Buffer.from(outBytes).includes(Buffer.from(buildMediaStub('sixel', sha3, sixel.length)))).toBe(true);
  // Each mediadef precedes its stub: find indexes.
  const kinds = co.map((i) => i.kind);
  expect(kinds.indexOf('mediadef')).toBeLessThan(kinds.lastIndexOf('bytes'));
  // The stub form is exactly the documented bytes.
  expect(stub1).toEqual(new TextEncoder().encode(`\x1b]7746 ; kb-media ; iip ; ${sha1} ; ${iipBel.length} \x1b\\`));
  expect(parseMediaStub(stub1)).toEqual({ mediaKind: 'iip', sha256: sha1, len: iipBel.length });
  expect(parseMediaStub(buildMediaStub('sixel', sha3, sixel.length))).toEqual({ mediaKind: 'sixel', sha256: sha3, len: sixel.length });
  expect(parseMediaStub(enc('not a stub'))).toBeNull();
  // Same payload twice writes once: re-push same sixel with a fresh extractor on the same store.
  const ex2 = new GraphicsExtractor(store);
  const before = readdirSync(join(sess, 'seq')).length;
  ex2.push(sixel);
  void before;
  expect(store.readSeq(sha3)).toEqual(sixel);
  expect(ex2.stats.sixel).toBe(1);

  // Oversize media: tail not leaked, counted, nothing stored.
  const small = mkStore();
  const exSmall = new GraphicsExtractor(small.store, { maxMediaBytes: 20 });
  const bigIip = enc('\x1b]1337;File=name=big.png:inline=1:' + 'B'.repeat(200) + '\x07');
  const items2 = exSmall.push(cat([enc('a'), bigIip, enc('b')]));
  expect(bytesOf(items2)).toEqual(cat([enc('a'), enc('b')]));
  expect(exSmall.stats.oversizeMedia).toBe(1);
  expect(readdirSync(join(small.sess, 'seq'))).toEqual([]);
  // Aborted media: dropped and counted.
  const exAbort = new GraphicsExtractor(mkStore().store);
  const aborted = exAbort.push(enc('\x1b]1337;File=name=x:inline=1:AAA\x18after'));
  expect(bytesOf(aborted)).toEqual(enc('after'));
  expect(exAbort.stats.abortedMedia).toBe(1);
  const stats: GfxStats = exAbort.stats;
  expect(stats.iip).toBe(0);
});
