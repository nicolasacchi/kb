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

test('valid_m1_start_interrupts_pending_and_b_completes', () => {
  // A valid m=1 start interrupting a pending transfer aborts it once
  // (abortedTransfer===1), the first transfer never completes, and the
  // interrupting transfer transmits with its own id/png.
  const pngA = makePng(8, 8, 7);
  const partsA = transmitApcs(9, pngA, 16);
  expect(partsA.length).toBeGreaterThan(2);
  const r = new KittyReassembler();
  expect(r.push(parseKittyControl(partsA[0]!.subarray(3, partsA[0]!.length - 2)))).toBeNull();
  const pngB = makePng(6, 6, 3);
  const partsB = transmitApcs(10, pngB, 16);
  expect(partsB.length).toBeGreaterThan(1);
  expect(r.push(parseKittyControl(partsB[0]!.subarray(3, partsB[0]!.length - 2)))).toBeNull();
  expect(r.stats.abortedTransfer).toBe(1);
  let result: KittyResult | null = null;
  for (let i = 1; i < partsB.length; i++) {
    result = r.push(parseKittyControl(partsB[i]!.subarray(3, partsB[i]!.length - 2)));
    if (i < partsB.length - 1) expect(result).toBeNull();
  }
  expect(result).not.toBeNull();
  expect(result!.kind).toBe('transmit');
  expect(r.stats.transmits).toBe(1);
  expect(r.stats.abortedTransfer).toBe(1);
  if (result!.kind === 'transmit') {
    expect(result!.id).toBe(10);
    expect(result!.png).toEqual(pngB);
  }
  // Transfer A never completes: its leftover continuations are orphans.
  expect(r.push(parseKittyControl(partsA[1]!.subarray(3, partsA[1]!.length - 2)))).toEqual({ kind: 'drop', reason: 'orphanChunk' });
  expect(r.stats.orphanChunk).toBe(1);
});

test('dropped_transmit_starts_do_not_abort_pending', () => {
  // Each dropped start (bad format, bad medium, compressed, missing/bad i,
  // bad base64 charset, m=1 oversize) must leave a legitimate pending
  // multi-chunk transfer resumable, with abortedTransfer still 0.
  const badStarts = [
    ['bad_format', 'a=t,f=24,i=555', 'AAAA', 'unsupportedFormat'],
    ['bad_format32', 'a=t,f=32,i=555', 'AAAA', 'unsupportedFormat'],
    ['bad_medium', 'a=t,f=100,t=f,i=555', 'AAAA', 'unsupportedMedium'],
    ['bad_medium_tmp', 'a=t,f=100,t=t,i=555', 'AAAA', 'unsupportedMedium'],
    ['bad_medium_shm', 'a=t,f=100,t=s,i=555', 'AAAA', 'unsupportedMedium'],
    ['compressed', 'a=t,f=100,o=z,i=555', 'AAAA', 'compressed'],
    ['missing_i', 'a=t,f=100', 'AAAA', 'imageNumberUnsupported'],
    ['bad_i_zero', 'a=t,f=100,i=0', 'AAAA', 'imageNumberUnsupported'],
    ['bad_i_overflow', 'a=t,f=100,i=4294967296', 'AAAA', 'imageNumberUnsupported'],
    ['bad_base64', 'a=t,f=100,i=555', 'AA*A', 'badBase64'],
  ] as const;
  for (const [name, ctrl, payload, reason] of badStarts) {
    const png = makePng(8, 8, 7);
    const parts = transmitApcs(9, png, 16);
    const r = new KittyReassembler();
    expect(r.push(parseKittyControl(parts[0]!.subarray(3, parts[0]!.length - 2)))).toBeNull();
    expect(r.push(pc(ctrl, payload))).toEqual({ kind: 'drop', reason });
    expect(r.stats.abortedTransfer).toBe(0);
    // Pending survives: remaining continuations complete the original transfer.
    let result: KittyResult | null = null;
    for (let i = 1; i < parts.length; i++) {
      result = r.push(parseKittyControl(parts[i]!.subarray(3, parts[i]!.length - 2)));
    }
    expect(result).not.toBeNull();
    expect(result!.kind).toBe('transmit');
    expect(r.stats.abortedTransfer).toBe(0);
    expect(r.stats.transmits).toBe(1);
    if (result!.kind === 'transmit') {
      expect(result!.png).toEqual(png);
      expect(result!.id).toBe(9);
    }
    // Sanity: this row really exercised the named drop.
    if (r.stats[reason] !== 1) {
      throw new Error(`${name}: expected stats.${reason} === 1, got ${r.stats[reason]}`);
    }
  }
  // m=1 oversize start also does not abort the pending transfer.
  {
    const png = makePng(8, 8, 7);
    const parts = transmitApcs(9, png, 16);
    const r = new KittyReassembler({ maxImageBytes: 500 });
    expect(r.push(parseKittyControl(parts[0]!.subarray(3, parts[0]!.length - 2)))).toBeNull();
    // First chunk alone decodes to well under 500, so this is not the pending
    // transfer; craft an oversize m=1 start with a huge payload.
    const huge = Buffer.alloc(800).toString('base64'); // 800 decoded bytes > 500
    expect(r.push(pc('a=t,f=100,i=555,m=1', huge))).toEqual({ kind: 'drop', reason: 'oversize' });
    expect(r.stats.abortedTransfer).toBe(0);
    let result: KittyResult | null = null;
    for (let i = 1; i < parts.length; i++) {
      result = r.push(parseKittyControl(parts[i]!.subarray(3, parts[i]!.length - 2)));
    }
    expect(result!.kind).toBe('transmit');
    expect(r.stats.abortedTransfer).toBe(0);
    expect(r.stats.transmits).toBe(1);
    if (result!.kind === 'transmit') expect(result!.png).toEqual(png);
  }
  // Single-chunk dropped starts (bad PNG / oversize) also leave pending alone.
  {
    const png = makePng(8, 8, 7);
    const parts = transmitApcs(9, png, 16);
    const r = new KittyReassembler();
    expect(r.push(parseKittyControl(parts[0]!.subarray(3, parts[0]!.length - 2)))).toBeNull();
    expect(r.push(pc('a=t,f=100,i=555', Buffer.from('hello world!').toString('base64')))).toEqual({ kind: 'drop', reason: 'badPng' });
    expect(r.stats.abortedTransfer).toBe(0);
    let result: KittyResult | null = null;
    for (let i = 1; i < parts.length; i++) {
      result = r.push(parseKittyControl(parts[i]!.subarray(3, parts[i]!.length - 2)));
    }
    expect(result!.kind).toBe('transmit');
    expect(r.stats.abortedTransfer).toBe(0);
    expect(r.stats.transmits).toBe(1);
    if (result!.kind === 'transmit') expect(result!.png).toEqual(png);
  }
  // Single-chunk finish-path oversize drop also leaves pending alone.
  {
    const png = makePng(8, 8, 7);
    const parts = transmitApcs(9, png, 16);
    const r = new KittyReassembler({ maxImageBytes: 500 });
    expect(r.push(parseKittyControl(parts[0]!.subarray(3, parts[0]!.length - 2)))).toBeNull();
    const huge = Buffer.alloc(800).toString('base64'); // decodes over the 500 cap at finish
    expect(r.push(pc('a=t,f=100,i=555', huge))).toEqual({ kind: 'drop', reason: 'oversize' });
    expect(r.stats.abortedTransfer).toBe(0);
    let result: KittyResult | null = null;
    for (let i = 1; i < parts.length; i++) {
      result = r.push(parseKittyControl(parts[i]!.subarray(3, parts[i]!.length - 2)));
    }
    expect(result!.kind).toBe('transmit');
    expect(r.stats.abortedTransfer).toBe(0);
    expect(r.stats.transmits).toBe(1);
    if (result!.kind === 'transmit') expect(result!.png).toEqual(png);
  }
  // Single-chunk finish-path bad-base64 (length) drop also leaves
  // pending alone ('AAA' passes the charset gate, fails length % 4 at finish).
  {
    const png = makePng(8, 8, 7);
    const parts = transmitApcs(9, png, 16);
    const r = new KittyReassembler();
    expect(r.push(parseKittyControl(parts[0]!.subarray(3, parts[0]!.length - 2)))).toBeNull();
    expect(r.push(pc('a=t,f=100,i=555', 'AAA'))).toEqual({ kind: 'drop', reason: 'badBase64' });
    expect(r.stats.abortedTransfer).toBe(0);
    let result: KittyResult | null = null;
    for (let i = 1; i < parts.length; i++) {
      result = r.push(parseKittyControl(parts[i]!.subarray(3, parts[i]!.length - 2)));
    }
    expect(result!.kind).toBe('transmit');
    expect(r.stats.abortedTransfer).toBe(0);
    expect(r.stats.transmits).toBe(1);
    if (result!.kind === 'transmit') expect(result!.png).toEqual(png);
  }
});

test('oversize_exact_decoded_boundary_true_length_equals_cap', () => {
  // True decoded length == cap must transmit, not oversize-drop early.
  // Split so the first chunk length is b64.length-2 (not a multiple of 4):
  // the ceil upper bound over-counts by the padding (1-2 bytes) and would
  // drop; the exact 3*floor(len/4)-pad length fits.
  let png: Uint8Array | null = null;
  outer: for (let w = 2; w <= 12; w++) {
    for (let fill = 1; fill <= 30; fill++) {
      const cand = makePng(w, w, fill);
      if (cand.length % 3 !== 0) { png = cand; break outer; }
    }
  }
  if (png === null) throw new Error('no PNG with padding found');
  const b64 = Buffer.from(png).toString('base64');
  expect(b64.length % 4).toBe(0);
  expect(png.length % 3).not.toBe(0); // padding > 0
  const first = b64.slice(0, -2);
  const last = b64.slice(-2);
  const cap = png.length;
  const r = new KittyReassembler({ maxImageBytes: cap });
  expect(r.push(pc(`a=t,f=100,i=31,m=1`, first))).toBeNull();
  expect(r.stats.oversize).toBe(0);
  const res = r.push(pc(`q=2,m=0`, last));
  expect(res).not.toBeNull();
  expect(res!.kind).toBe('transmit');
  expect(r.stats.transmits).toBe(1);
  expect(r.stats.oversize).toBe(0);
  if (res!.kind === 'transmit') expect(res!.png).toEqual(png);
  // One byte over the cap must oversize-drop (enforcement not weakened).
  // Deterministic path: the first chunk (4k-2 chars, pad_first=0) decodes
  // to 3k-3 <= cap-1, so it always fits and returns null; the final
  // chunk then oversize-drops at finishTransmit.
  const r2 = new KittyReassembler({ maxImageBytes: cap - 1 });
  expect(r2.push(pc('a=t,f=100,i=31,m=1', first))).toBeNull();
  const res2 = r2.push(pc('q=2,m=0', last));
  expect(res2).toEqual({ kind: 'drop', reason: 'oversize' });
  expect(r2.stats.oversize).toBe(1);
  expect(r2.stats.transmits).toBe(0);
});

test('oversize_exact_continuation_boundary_at_cap', () => {
  // The pushContinuation more=true exact check accepts an intermediate
  // total at the cap: after the second chunk the concatenated length is
  // b64.length-2, whose exact decoded length 3k-3 fits the cap (3k-1 or
  // 3k-2), while the old ceil upper bound 3k would oversize-drop here.
  let png: Uint8Array | null = null;
  outer: for (let w = 2; w <= 12; w++) {
    for (let fill = 1; fill <= 30; fill++) {
      const cand = makePng(w, w, fill);
      if (cand.length % 3 !== 0) { png = cand; break outer; }
    }
  }
  if (png === null) throw new Error('no PNG with padding found');
  const b64 = Buffer.from(png).toString('base64');
  expect(b64.length % 4).toBe(0);
  expect(png.length % 3).not.toBe(0); // padding > 0
  const first = b64.slice(0, 4);
  const second = b64.slice(4, -2);
  const third = b64.slice(-2);
  const cap = png.length;
  const r = new KittyReassembler({ maxImageBytes: cap });
  expect(r.push(pc('a=t,f=100,i=32,m=1', first))).toBeNull();
  // Middle (more=true) continuation: exact intermediate <= cap, so null;
  // with the ceil bound this push would oversize-drop.
  expect(r.push(pc('q=2,m=1', second))).toBeNull();
  expect(r.stats.oversize).toBe(0);
  const res = r.push(pc('q=2,m=0', third));
  expect(res).not.toBeNull();
  expect(res!.kind).toBe('transmit');
  expect(r.stats.transmits).toBe(1);
  expect(r.stats.oversize).toBe(0);
  if (res!.kind === 'transmit') expect(res!.png).toEqual(png);
  // A mid-stream continuation that pushes the total over the cap
  // still drops, and the discarded pending transfer orphans the tail.
  const r3 = new KittyReassembler({ maxImageBytes: cap - 4 });
  expect(r3.push(pc('a=t,f=100,i=32,m=1', first))).toBeNull();
  expect(r3.push(pc('q=2,m=1', second))).toEqual({ kind: 'drop', reason: 'oversize' });
  expect(r3.stats.oversize).toBe(1);
  expect(r3.push(pc('q=2,m=0', third))).toEqual({ kind: 'drop', reason: 'orphanChunk' });
  expect(r3.stats.orphanChunk).toBe(1);
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
  // A pending transfer aborted by a valid interrupting start, plus a
  // dropped start that leaves the new pending transfer alone: each
  // push still lands in exactly one bucket, so the identity below is
  // unconditional (exercised over the abort paths, not only happy ones).
  const multi2 = transmitApcs(7, png, 16);
  r.push(parseKittyControl(multi2[0]!.subarray(3, multi2[0]!.length - 2))); // pending, chunks++
  r.push(pc('a=t,f=24,i=9', 'AAAA')); // drop unsupportedFormat, no abort
  const single2 = transmitApcs(8, png, 4096)[0]!;
  r.push(parseKittyControl(single2.subarray(3, single2.length - 2))); // transmit, aborts pending
  const s = r.stats;
  const drops = s.unsupportedFormat + s.unsupportedMedium + s.compressed
    + s.directPlacement + s.nonVirtualPlacement + s.queryAction
    + s.imageNumberUnsupported + s.orphanChunk + s.badBase64 + s.badPng
    + s.oversize + s.otherAction;
  expect(s.apcs).toBe(1 + 1 + 1 + multi.length + 2 + 3);
  expect(s.apcs).toBe(s.transmits + s.places + s.resets + s.chunks + drops);
  expect(s.transmits).toBe(3);
  expect(s.abortedTransfer).toBe(1);
  expect(s.unsupportedFormat).toBe(1);
  expect(s.places).toBe(1);
  expect(s.resets).toBe(1);
});
