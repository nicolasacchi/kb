// ResumeFrom over a REAL unix socket, fake viewer (T7 test 5): deterministic
// multi-MB stream, 4 abrupt drops, server rewind; then the offline-longer-
// than-ring case, the backoff regression test, and the frame watchdog.
import { test, expect } from 'bun:test';
import { createHash } from 'node:crypto';
import { mkdtempSync } from 'node:fs';
import { connect, type Socket } from 'node:net';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { Backoff, RefViewer, backoffNominal } from '../src/client_ref.ts';
import { makeTwin, type Twin } from '../src/twin.ts';
import { TapServer } from '../src/viewers.ts';
import { EnvelopeDecoder, decodeTextFrame, encodeBinaryFrame, encodeControlMessage, encodeEnvelope, ENV_BINARY, ENV_CONTROL, ENV_TEXT, parseS2C } from '../src/wire.ts';

const enc = (s: string) => new TextEncoder().encode(s);
const sha = (buf: Uint8Array) => createHash('sha256').update(Buffer.from(buf)).digest('hex');

function concat(parts: Uint8Array[]): Uint8Array {
  const total = parts.reduce((a, b) => a + b.length, 0);
  const out = new Uint8Array(total);
  let o = 0;
  for (const p of parts) {
    out.set(p, o);
    o += p.length;
  }
  return out;
}

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

/** Printable synthetic stream (no ESC, so withhold passes it verbatim). */
function streamBytes(rnd: () => number, n: number): Uint8Array {
  const out = new Uint8Array(n);
  for (let i = 0; i < n; i++) out[i] = 0x20 + Math.floor(rnd() * 0x5f);
  return out;
}

async function waitUntil(cond: () => boolean, what: string, timeoutMs = 30_000): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  while (!cond()) {
    if (Date.now() > deadline) throw new Error(`waitUntil timeout: ${what}`);
    await Bun.sleep(5);
  }
}

/** Connect a RefViewer to the real tap.sock; viewer-open carries resume. */
function connectViewer(sockPath: string, ref: RefViewer, epoch: string): Promise<Socket> {
  return new Promise((resolve, reject) => {
    const socket = connect(sockPath);
    const dec = new EnvelopeDecoder();
    socket.on('error', reject);
    socket.on('data', (chunk: Buffer) => {
      for (const env of dec.push(new Uint8Array(chunk))) {
        if (env.kind === ENV_TEXT || env.kind === ENV_BINARY) {
          const { frame } = parseS2C(env.body);
          if (env.kind === ENV_TEXT) {
            const text = Buffer.from(frame).toString('utf-8');
            if (decodeTextFrame(text).kind === 'known') ref.onText(text);
          } else {
            ref.onBinary(frame);
          }
        }
      }
      resolve(socket);
    });
    socket.on('connect', () => {
      const resume = ref.applied > 0 || ref.mseq > 0 ? { offset: ref.applied, mseq: ref.mseq, epoch } : null;
      socket.write(encodeEnvelope(ENV_CONTROL, enc(encodeControlMessage({ t: 'viewer-open', viewer: 'v1', profile: 'desktop', resume }))));
    });
  });
}

test('resume_real_socket_4drops_rewind_exact_recovery', async () => {
  const runDir = mkdtempSync(join(tmpdir(), 'kbt7-'));
  const epoch = 'e'.repeat(32);
  const twin = makeTwin(120, 40);
  const server = new TapServer({ session: 'resume001', harness: 'omp', epoch, twin, runDir, quiesceMs: 0, joinWaitMs: 10_000, rewind: 32 });
  await server.listen();
  try {
    const rnd = mulberry32(2026);
    const total = 3 * 1024 * 1024;
    const expected: Uint8Array[] = [];
    const imageSha = createHash('sha256').update(enc('fakepng')).digest('hex');
    // One ED2+ED3+reprint frame, generated once and used in both places.
    const edframe = new Uint8Array([...enc('\x1b[2J\x1b[3J'), ...streamBytes(rnd, 200)]);
    const quarter = Math.floor(total / 4);
    const segments: (Uint8Array | 'image' | 'edframe' | 'reset')[] = [
      streamBytes(rnd, quarter),
      'image',
      streamBytes(rnd, quarter),
      'edframe',
      streamBytes(rnd, quarter),
      'reset',
      streamBytes(rnd, total - 3 * quarter),
    ];
    for (const seg of segments) {
      if (typeof seg !== 'string') expected.push(seg);
    }
    expected.push(edframe); // order irrelevant for concat-after-sorting below
    // Recompute expected in stream order:
    const expectedOrdered: Uint8Array[] = [];
    for (const seg of segments) {
      if (typeof seg !== 'string') expectedOrdered.push(seg);
      else if (seg === 'edframe') expectedOrdered.push(edframe);
    }
    void expected;
    const streamTotal = expectedOrdered.reduce((a, b) => a + b.length, 0);
    const ref = new RefViewer({ epoch });
    let socket = await connectViewer(server.sockPath, ref, epoch);
    const feed = (async () => {
      for (const seg of segments) {
        if (typeof seg !== 'string') {
          for (let i = 0; i < seg.length; i += 65536) {
            server.feed([{ kind: 'bytes', data: seg.subarray(i, Math.min(seg.length, i + 65536)) }]);
            await Bun.sleep(1);
          }
        } else if (seg === 'image') {
          server.feed([{ kind: 'imagedef', def: { id: 7, gen: 1, sha256: imageSha, mime: 'image/png', w: 16, h: 16, cols: 2, rows: 2 } }]);
        } else if (seg === 'edframe') {
          server.feed([{ kind: 'bytes', data: edframe }]);
        } else {
          server.feed([{ kind: 'gfxreset', gen: 2 }]);
        }
      }
    })();
    const dropOffsets = [0.2, 0.4, 0.6, 0.8].map((f) => Math.floor(streamTotal * f));
    let dropIdx = 0;
    const driver = (async () => {
      for (;;) {
        if (ref.closed) return;
        if (dropIdx >= dropOffsets.length && ref.applied >= streamTotal) return;
        if (dropIdx < dropOffsets.length && ref.applied >= dropOffsets[dropIdx]) {
          socket.destroy();
          dropIdx++;
          await waitUntil(() => socket.destroyed, 'socket gone');
          socket = await connectViewer(server.sockPath, ref, epoch);
        }
        await Bun.sleep(5);
      }
    })();
    await Promise.all([feed, driver]);
    await waitUntil(() => ref.applied >= streamTotal, 'recovery to full stream');
    expect(ref.gaps).toBe(0);
    // Each of the 4 resumes rewinds 32 bytes (< from - tail since tail == 0).
    expect(ref.dupBytes).toBe(4 * 32);
    const seqs = ref.markers.map((m) => m.seq);
    expect(seqs.length).toBe(2);
    expect(new Set(seqs).size).toBe(seqs.length);
    expect([...seqs].sort((a, b) => a - b)).toEqual(seqs);
    expect(sha(concat(ref.bytes))).toBe(sha(concat(expectedOrdered)));
    expect(server.stats.joins).toBeGreaterThanOrEqual(1);
  } finally {
    await server.close();
    twin.dispose();
  }
});

test('resume_offline_longer_than_ring_full_join', async () => {
  const runDir = mkdtempSync(join(tmpdir(), 'kbt7-'));
  const epoch = 'e'.repeat(32);
  const stubTwin = { feed: async () => {}, serialize: { serialize: () => '' } } as unknown as Twin;
  const server = new TapServer({ session: 'resume002', harness: 'omp', epoch, twin: stubTwin, runDir, quiesceMs: 0, joinWaitMs: 10_000 });
  await server.listen();
  try {
    for (let i = 0; i < 40; i++) {
      server.feed([{ kind: 'bytes', data: new Uint8Array(1024 * 1024).fill(0x63) }]);
    }
    const ref = new RefViewer({ epoch });
    let sawSeed = false;
    const socket = connect(server.sockPath);
    const dec = new EnvelopeDecoder();
    socket.on('data', (chunk: Buffer) => {
      for (const env of dec.push(new Uint8Array(chunk))) {
        if (env.kind !== ENV_TEXT) continue;
        const { frame } = parseS2C(env.body);
        const text = Buffer.from(frame).toString('utf-8');
        const decoded = decodeTextFrame(text);
        if (decoded.kind === 'known') {
          ref.onText(text);
          if ((decoded.frame as { t?: string }).t === 'seed') sawSeed = true;
        }
      }
    });
    await new Promise<void>((resolve, reject) => {
      socket.on('error', reject);
      socket.on('connect', () => {
        socket.write(encodeEnvelope(ENV_CONTROL, enc(encodeControlMessage({ t: 'viewer-open', viewer: 'v1', profile: 'desktop', resume: { offset: 0, mseq: 0, epoch } }))));
      });
      waitUntil(() => sawSeed, 'full join seed').then(resolve, reject);
    });
    expect(ref.applied).toBe(40 * 1024 * 1024);
    socket.destroy();
  } finally {
    await server.close();
  }
});

test('client_ref_backoff_series_and_reset', () => {
  const backoff = new Backoff();
  const nominals: number[] = [];
  for (let i = 0; i < 30; i++) {
    nominals.push(backoff.nominal());
    const d = backoff.delay(() => 0);
    expect(d).toBeGreaterThanOrEqual(75);
    backoff.onFailure();
  }
  for (let i = 1; i < nominals.length; i++) {
    expect(nominals[i]).toBeGreaterThanOrEqual(nominals[i - 1]);
  }
  expect(Math.max(...nominals)).toBe(5000);
  expect(nominals[0]).toBe(100);
  expect(backoffNominal(0)).toBe(100);
  expect(backoffNominal(100)).toBe(5000);
  expect(backoff.delay(() => 0.999)).toBeLessThanOrEqual(5000);
  for (let i = 0; i < 10; i++) backoff.onGoodFrame();
  expect(backoff.nominal()).toBe(100);
  const b2 = new Backoff();
  b2.onFailure();
  for (let i = 0; i < 9; i++) b2.onGoodFrame();
  expect(b2.nominal()).toBe(200);
});

test('client_ref_watchdog_fake_clock', () => {
  let now = 1000;
  const ref = new RefViewer({ epoch: 'e'.repeat(32), now: () => now, watchdogMs: 1500 });
  expect(ref.checkWatchdog()).toBe(false);
  now += 1500;
  expect(ref.checkWatchdog()).toBe(false);
  now += 1;
  expect(ref.checkWatchdog()).toBe(true);
  ref.onText('{"t":"ping"}');
  expect(ref.checkWatchdog()).toBe(false);
  now += 1501;
  expect(ref.checkWatchdog()).toBe(true);
});

test('client_ref_gap_and_dedupe_accounting', () => {
  const ref = new RefViewer({ epoch: 'e'.repeat(32) });
  ref.onText('{"t":"hello","proto":"kb-term/1","session":"s","epoch":"' + 'e'.repeat(32) + '","harness":"omp","bound_sid":null}');
  expect(ref.fullJoin).toBe(false);
  ref.onBinary(encodeBinaryFrame(0, enc('hello')));
  expect(ref.applied).toBe(5);
  ref.onBinary(encodeBinaryFrame(3, enc('lloWW')));
  expect(ref.dupBytes).toBe(2);
  expect(ref.applied).toBe(8);
  ref.onBinary(encodeBinaryFrame(99, enc('x')));
  expect(ref.gaps).toBe(1);
  expect(ref.closed).toBe(true);
  expect(ref.applied).toBe(8);
});

test('client_ref_new_epoch_downgrades_and_marker_dedupe', () => {
  const ref = new RefViewer({ epoch: 'a'.repeat(32) });
  ref.onBinary(encodeBinaryFrame(0, enc('data')));
  ref.onText('{"t":"imagedef","seq":1,"id":1,"gen":1,"sha256":"x","mime":"image/png","w":1,"h":1,"cols":1,"rows":1,"offset":4}');
  ref.onText('{"t":"imagedef","seq":1,"id":1,"gen":1,"sha256":"x","mime":"image/png","w":1,"h":1,"cols":1,"rows":1,"offset":4}');
  ref.onText('{"t":"gfxreset","seq":2,"gen":1,"offset":4}');
  expect(ref.markers.map((m) => m.seq)).toEqual([1, 2]);
  expect(ref.mseq).toBe(2);
  ref.onText('{"t":"hello","proto":"kb-term/1","session":"s","epoch":"' + 'b'.repeat(32) + '","harness":"omp","bound_sid":null}');
  expect(ref.fullJoin).toBe(true);
  expect(ref.applied).toBe(0);
  expect(ref.mseq).toBe(0);
});
