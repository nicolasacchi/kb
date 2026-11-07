// kitty control + reassembly pins (KBT-GFX).
import { test, expect } from 'bun:test';
import { KittyReassembler, parseKittyControl } from '../src/kitty.ts';
import type { KittyResult } from '../src/kitty.ts';
import { makePng, kittyApc, transmitApcs, enc, oracleScan } from './helpers-graphics.ts';

test('parse_control_defaults_and_last_key_wins', () => {
  // No control data: default a=t, empty payload.
  let p = parseKittyControl(new Uint8Array(0));
  expect(p.keys.size).toBe(0);
  expect(p.payload.length).toBe(0);
  // a defaults to t (absent in Map; reassembler applies the default).
  expect(p.keys.get('a')).toBeUndefined();
  // Split at the FIRST ';' only.
  p = parseKittyControl(enc('a=t,f=100,i=7;AAAA;BBBB'));
  expect(p.keys.get('a')).toBe('t');
  expect(p.keys.get('f')).toBe('100');
  expect(p.keys.get('i')).toBe('7');
  expect(new TextDecoder().decode(p.payload)).toBe('AAAA;BBBB');
  // Duplicate key: last wins; unknown keys kept but ignored downstream.
  p = parseKittyControl(enc('a=p,a=d,i=1,i=2,x=y'));
  expect(p.keys.get('a')).toBe('d');
  expect(p.keys.get('i')).toBe('2');
  expect(p.keys.get('x')).toBe('y');
  // No ';': all keys, empty payload.
  p = parseKittyControl(enc('a=q,i=3'));
  expect(p.keys.get('a')).toBe('q');
  expect(p.payload.length).toBe(0);
});

test('multi_chunk_transmit_reassembles_to_oracle_sha', () => {
  const png = makePng(32, 24, 200);
  const apcs = transmitApcs(0x66cf38, png, 16);
  expect(apcs.length).toBeGreaterThan(20); // 21+ chunks at 16 chars
  const r = new KittyReassembler();
  let result: KittyResult | null = null;
  for (let i = 0; i < apcs.length; i++) {
    const body = apcs[i]!.subarray(3, apcs[i]!.length - 2); // strip ESC _ G / ESC \
    result = r.push(parseKittyControl(body));
    if (i < apcs.length - 1) expect(result).toBeNull();
  }
  expect(result).not.toBeNull();
  expect(result!.kind).toBe('transmit');
  if (result!.kind !== 'transmit') throw new Error('unreachable');
  const oracle = oracleScan(new Uint8Array(Buffer.concat(apcs.map((a) => Buffer.from(a)))));
  expect(oracle.transmits.length).toBe(1);
  expect(result!.sha256).toBe(oracle.transmits[0]!.sha256);
  expect(result!.w).toBe(32);
  expect(result!.h).toBe(24);
  expect(r.stats.transmits).toBe(1);
  expect(r.stats.chunks).toBe(apcs.length - 1);
});

test('single_chunk_without_m_key_is_final', () => {
  const png = makePng(5, 5, 9);
  const apc = transmitApcs(42, png, 4096)[0]!;
  expect(apc.length).toBeLessThan(200);
  const r = new KittyReassembler();
  const res = r.push(parseKittyControl(apc.subarray(3, apc.length - 2)));
  expect(res).not.toBeNull();
  expect(res!.kind).toBe('transmit');
  expect(r.stats.chunks).toBe(0);
  expect(r.stats.transmits).toBe(1);
});

// Each row: build ONE APC, push it, expect exactly one drop with the given reason.
const pc = (ctrl: string, payload = '') => parseKittyControl(enc(`${ctrl};${payload}`));
const tinyPngB64 = () => Buffer.from(makePng(4, 4, 1)).toString('base64');

test.each([
  ['f24', pc('a=t,f=24,i=1', tinyPngB64()), 'unsupportedFormat'],
  ['f32', pc('a=t,f=32,i=1', tinyPngB64()), 'unsupportedFormat'],
  ['format_missing', pc('a=t,i=1', tinyPngB64()), 'unsupportedFormat'],
  ['medium_file', pc('a=t,f=100,t=f,i=1', tinyPngB64()), 'unsupportedMedium'],
  ['medium_tmp', pc('a=t,f=100,t=t,i=1', tinyPngB64()), 'unsupportedMedium'],
  ['medium_shm', pc('a=t,f=100,t=s,i=1', tinyPngB64()), 'unsupportedMedium'],
  ['compressed', pc('a=t,f=100,o=z,i=1', tinyPngB64()), 'compressed'],
  ['direct_placement', pc('a=T,f=100,i=1', tinyPngB64()), 'directPlacement'],
  ['nonvirtual_placement', pc('a=p,i=1,c=5,r=2'), 'nonVirtualPlacement'],
  ['query_action', pc('a=q,i=1'), 'queryAction'],
  ['image_number_without_i', pc('a=t,f=100,I=1', tinyPngB64()), 'imageNumberUnsupported'],
  ['place_without_i', pc('a=p,U=1,c=5,r=2'), 'imageNumberUnsupported'],
  ['transmit_without_i', pc('a=t,f=100', tinyPngB64()), 'imageNumberUnsupported'],
  ['bad_id_zero', pc('a=t,f=100,i=0', tinyPngB64()), 'imageNumberUnsupported'],
  ['bad_id_overflow', pc('a=t,f=100,i=4294967296', tinyPngB64()), 'imageNumberUnsupported'],
  ['bad_base64_charset', pc('a=t,f=100,i=1', 'AA*A'), 'badBase64'],
  ['bad_base64_length', pc('a=t,f=100,i=1', 'AAA'), 'badBase64'],
  ['non_png', pc('a=t,f=100,i=1', Buffer.from('hello world!').toString('base64')), 'badPng'],
] as const)('drop_reasons %s', (_name, parsed, reason) => {
  const r = new KittyReassembler();
  const res = r.push(parsed);
  expect(res).toEqual({ kind: 'drop', reason });
  const s = r.stats;
  expect(s.apcs).toBe(1);
  expect(s.transmits).toBe(0);
  expect(s.places).toBe(0);
  expect(s.resets).toBe(0);
  expect(s.chunks).toBe(0);
  expect(s[reason]).toBe(1);
});

test('drop_reasons bad_png_variants', () => {
  // Wrong IHDR tag.
  const good = Buffer.from(makePng(4, 4, 1));
  const bad = Buffer.from(good);
  bad[12] = 0x58; // 'X' instead of 'I'
  let r = new KittyReassembler();
  expect(r.push(pc('a=t,f=100,i=1', bad.toString('base64')))).toEqual({ kind: 'drop', reason: 'badPng' });
  // Zero width.
  const zero = Buffer.from(makePng(4, 4, 1));
  zero.writeUInt32BE(0, 16);
  r = new KittyReassembler();
  expect(r.push(pc('a=t,f=100,i=1', zero.toString('base64')))).toEqual({ kind: 'drop', reason: 'badPng' });
  // Width 20000 (> 16384).
  const wide = Buffer.from(makePng(4, 4, 1));
  wide.writeUInt32BE(20000, 16);
  r = new KittyReassembler();
  expect(r.push(pc('a=t,f=100,i=1', wide.toString('base64')))).toEqual({ kind: 'drop', reason: 'badPng' });
});

test('drop_reasons orphan_continuation', () => {
  const r = new KittyReassembler();
  const res = r.push(pc('q=2,m=0', tinyPngB64()));
  expect(res).toEqual({ kind: 'drop', reason: 'orphanChunk' });
  expect(r.stats.orphanChunk).toBe(1);
});

test('drop_reasons interrupted_transmit_aborts_first', () => {
  const png = makePng(8, 8, 7);
  const parts = transmitApcs(9, png, 16);
  expect(parts.length).toBeGreaterThan(2);
  const r = new KittyReassembler();
  // First chunk of transfer 9 (m=1), then a fresh transmit start.
  expect(r.push(parseKittyControl(parts[0]!.subarray(3, parts[0]!.length - 2)))).toBeNull();
  const png2 = makePng(6, 6, 3);
  const single = transmitApcs(10, png2, 4096)[0]!;
  const res = r.push(parseKittyControl(single.subarray(3, single.length - 2)));
  expect(res).not.toBeNull();
  expect(res!.kind).toBe('transmit');
  expect(r.stats.abortedTransfer).toBe(1);
});

test('drop_reasons oversize_discards_pending_then_orphans', () => {
  const png = makePng(24, 24, 5);
  const parts = transmitApcs(77, png, 16);
  expect(parts.length).toBeGreaterThan(3);
  const r = new KittyReassembler({ maxImageBytes: 100 });
  let dropped = false;
  let idx = -1;
  for (let i = 0; i < parts.length; i++) {
    const res = r.push(parseKittyControl(parts[i]!.subarray(3, parts[i]!.length - 2)));
    if (res !== null && res.kind === 'drop' && res.reason === 'oversize') { dropped = true; idx = i; break; }
    // Before oversize, non-final chunks return null.
    if (res !== null) throw new Error(`unexpected result before oversize: ${JSON.stringify(res)}`);
  }
  expect(dropped).toBe(true);
  expect(r.stats.oversize).toBe(1);
  // The following chunk is an orphan.
  const next = r.push(parseKittyControl(parts[idx + 1]!.subarray(3, parts[idx + 1]!.length - 2)));
  expect(next).toEqual({ kind: 'drop', reason: 'orphanChunk' });
  expect(r.stats.orphanChunk).toBe(1);
});

test('stats_identity', () => {
  const r = new KittyReassembler();
  const png = makePng(6, 6, 11);
  const single = transmitApcs(5, png, 4096)[0]!;
  r.push(parseKittyControl(single.subarray(3, single.length - 2))); // transmit
  const place = kittyApc('a=p,U=1,i=5,c=4,r=2');
  r.push(parseKittyControl(place.subarray(3, place.length - 2))); // place
  r.push(parseKittyControl(kittyApc('a=d,d=A').subarray(3, kittyApc('a=d,d=A').length - 2))); // reset
  const multi = transmitApcs(6, png, 16);
  for (const a of multi) r.push(parseKittyControl(a.subarray(3, a.length - 2))); // chunks + transmit
  r.push(pc('a=q,i=1')); // drop queryAction
  // orphan continuation
  r.push(pc('q=2,m=0', tinyPngB64()));
  const s = r.stats;
  const drops = s.unsupportedFormat + s.unsupportedMedium + s.compressed
    + s.directPlacement + s.nonVirtualPlacement + s.queryAction
    + s.imageNumberUnsupported + s.orphanChunk + s.badBase64 + s.badPng
    + s.oversize + s.otherAction;
  expect(s.apcs).toBe(1 + 1 + 1 + multi.length + 2);
  expect(s.apcs).toBe(s.transmits + s.places + s.resets + s.chunks + drops);
  expect(s.transmits).toBe(2);
  expect(s.places).toBe(1);
  expect(s.resets).toBe(1);
});
