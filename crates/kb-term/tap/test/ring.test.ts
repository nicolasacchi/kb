// Ring numbering, wrap, markers (T7 test 1). Seeded PRNG property test
// against a naive Buffer.concat reference; 200 iterations.
import { test, expect } from 'bun:test';
import { Ring } from '../src/ring.ts';

/** mulberry32: deterministic synthetic PRNG. */
function mulberry32(seed: number): () => number {
  let a = seed >>> 0;
  return () => {
    a |= 0;
    a = (a + 0x6d2b79f5) | 0;
    let t = Math.imul(a ^ (a >>> 15), 1 | a);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

function fill(rnd: () => number, n: number): Uint8Array {
  const out = new Uint8Array(n);
  for (let i = 0; i < n; i++) out[i] = Math.floor(rnd() * 256);
  return out;
}

const concat = (parts: Uint8Array[]): Uint8Array => {
  const total = parts.reduce((a, b) => a + b.length, 0);
  const out = new Uint8Array(total);
  let o = 0;
  for (const p of parts) {
    out.set(p, o);
    o += p.length;
  }
  return out;
};

test('ring_push_returns_start_and_advances_head_tail', () => {
  const ring = new Ring(64);
  expect(ring.head).toBe(0);
  expect(ring.tail).toBe(0);
  expect(ring.push(new Uint8Array([1, 2, 3]))).toBe(0);
  expect(ring.head).toBe(3);
  expect(ring.tail).toBe(0);
  expect(ring.push(new Uint8Array(100))).toBe(3);
  expect(ring.head).toBe(103);
  expect(ring.tail).toBe(39);
});

test('ring_read_matches_naive_reference_across_wrap', () => {
  for (const seed of [1, 7, 42, 2026]) {
    const rnd = mulberry32(seed);
    const cap = 64;
    const ring = new Ring(cap);
    const parts: Uint8Array[] = [];
    let head = 0;
    for (let i = 0; i < 200; i++) {
      if (rnd() < 0.3 && head > 0) {
        const tail = ring.tail;
        // A read strictly below the tail ages out to null.
        if (tail > 0) expect(ring.read(tail - 1, 8)).toBeNull();
        // Any in-window read matches the naive reference byte-for-byte.
        const choice = Math.floor(rnd() * 3);
        const span = Math.min(head, 80);
        const from = Math.max(tail, head - Math.floor(rnd() * span));
        const maxLen = Math.floor(rnd() * 40);
        if (choice === 0) {
          expect(ring.read(from, maxLen)).toEqual(concat(parts).subarray(from, Math.min(head, from + maxLen)));
        } else if (choice === 1) {
          expect(ring.read(from, 0)).toEqual(new Uint8Array(0));
        } else {
          const got = ring.read(from, maxLen);
          expect(got).not.toBeNull();
          expect(Array.from(got!)).toEqual(Array.from(concat(parts).subarray(from, Math.min(head, from + maxLen))));
        }
        continue;
      }
      const chunk = fill(rnd, Math.floor(rnd() * 96));
      expect(ring.push(chunk)).toBe(head);
      parts.push(chunk);
      head += chunk.length;
      expect(ring.head).toBe(head);
      expect(ring.tail).toBe(Math.max(0, head - cap));
    }
  }
});

test('ring_chunk_larger_than_cap_keeps_only_its_tail', () => {
  const ring = new Ring(16);
  const data = Uint8Array.from({ length: 40 }, (_, i) => i);
  expect(ring.push(data)).toBe(0);
  expect(ring.head).toBe(40);
  expect(ring.tail).toBe(24);
  expect(ring.read(24, 16)).toEqual(data.subarray(24));
  expect(ring.read(23, 16)).toBeNull();
});

test('ring_wrap_boundary_reads_are_exact', () => {
  const ring = new Ring(10);
  ring.push(new Uint8Array([1, 2, 3, 4, 5, 6, 7, 8]));
  ring.push(new Uint8Array([9, 10, 11, 12]));
  // head 12, tail 2: offsets 0-1 are aged out.
  expect(ring.read(0, 12)).toBeNull();
  expect(ring.read(1, 12)).toBeNull();
  expect(ring.read(2, 10)).toEqual(Uint8Array.from([3, 4, 5, 6, 7, 8, 9, 10, 11, 12]));
  expect(ring.read(2, 4)).toEqual(Uint8Array.from([3, 4, 5, 6]));
  expect(ring.read(12, 4)).toEqual(new Uint8Array(0));
  expect(ring.read(99, 4)).toEqual(new Uint8Array(0));
});

test('ring_64mib_in_4kib_chunks_single_allocation', () => {
  const ring = new Ring(32 * 1024 * 1024);
  const rnd = mulberry32(99);
  const chunk = fill(rnd, 4096);
  const parts: Uint8Array[] = [];
  for (let i = 0; i < 16384; i++) {
    ring.push(chunk);
    parts.push(chunk);
  }
  expect(ring.allocCount).toBe(1);
  expect(ring.head).toBe(64 * 1024 * 1024);
  expect(ring.tail).toBe(32 * 1024 * 1024);
  const retained = concat(parts).subarray(ring.tail);
  const first = ring.read(ring.tail, 64 * 1024);
  expect(Array.from(first!.subarray(0, 4096))).toEqual(Array.from(retained.subarray(0, 4096)));
  const lastFrom = ring.tail + (ring.cap - 4096);
  const last = ring.read(lastFrom, 4096);
  expect(Array.from(last!)).toEqual(Array.from(retained.subarray(ring.cap - 4096)));
  expect(ring.read(ring.tail - 1, 8)).toBeNull();
});

test('ring_markers_assign_seqs_and_prune_below_tail', () => {
  const ring = new Ring(32);
  expect(ring.addMarker((seq, offset) => `m${seq}@${offset}`)).toBe(1);
  ring.push(new Uint8Array(8));
  expect(ring.addMarker((seq, offset) => `m${seq}@${offset}`)).toBe(2);
  expect(ring.markersAfter(0).map((m) => m.frame)).toEqual(['m1@0', 'm2@8']);
  expect(ring.markersAfter(1).map((m) => m.seq)).toEqual([2]);
  expect(ring.markersAfter(2)).toEqual([]);
  ring.push(new Uint8Array(24));
  expect(ring.head).toBe(32);
  expect(ring.tail).toBe(0);
  ring.push(new Uint8Array(8));
  // head 40, tail 8: the m1 marker at offset 0 is below the tail -> pruned.
  expect(ring.markersAfter(0).map((m) => m.seq)).toEqual([2]);
});

test('ring_bad_cap_throws', () => {
  expect(() => new Ring(0)).toThrow(RangeError);
  expect(() => new Ring(-4)).toThrow(RangeError);
  expect(() => new Ring(1.5)).toThrow(RangeError);
});
