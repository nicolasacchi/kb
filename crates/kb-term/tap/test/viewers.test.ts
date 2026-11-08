// Viewer fan-out, atomic join, ResumeFrom (T7 test 3). A controllable mock
// transport plus a fake clock; only the tap.sock mode test touches the fs.
// Async completion is awaited as an observable condition (hello/error
// frames), never as a fixed sleep: the only wall-clock time in this file is
// the polling interval inside waitUntil, which bounds how long a genuinely
// failing condition can hide before it throws.
import { test, expect } from 'bun:test';
import { createHash } from 'node:crypto';
import { mkdtempSync, statSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { connect } from 'node:net';
import { RefViewer } from '../src/client_ref.ts';
import { makeTwin, type Twin } from '../src/twin.ts';
import { TapServer, type Transport } from '../src/viewers.ts';
import { EnvelopeDecoder, decodeBinaryFrame, decodeTextFrame, encodeBinaryFrame, encodeControlMessage, encodeEnvelope, ENV_CONTROL, ENV_TEXT, parseS2C, type ServerFrame } from '../src/wire.ts';

const E = '\x1b';
const enc = (s: string) => new TextEncoder().encode(s);
const sha = (buf: Uint8Array) => createHash('sha256').update(Buffer.from(buf)).digest('hex');

class MockTransport implements Transport {
  written: Uint8Array[] = [];
  closed = false;
  private drainCbs: (() => void)[] = [];
  private closeCbs: (() => void)[] = [];
  alwaysBlock = false;

  write(bytes: Uint8Array): boolean {
    this.written.push(bytes);
    return !this.alwaysBlock;
  }
  close(): void {
    this.closed = true;
  }
  onDrain(cb: () => void): void {
    this.drainCbs.push(cb);
  }
  onClose(cb: () => void): void {
    this.closeCbs.push(cb);
  }
  drain(): void {
    for (const cb of this.drainCbs) cb();
  }
  clear(): void {
    this.written = [];
  }
}

interface S2CFrame {
  id: string;
  kind: number;
  text?: ServerFrame;
  binary?: { offset: number; payload: Uint8Array };
}

function collect(t: MockTransport): S2CFrame[] {
  const dec = new EnvelopeDecoder();
  const all = new Uint8Array(t.written.reduce((a, b) => a + b.length, 0));
  let o = 0;
  for (const part of t.written) {
    all.set(part, o);
    o += part.length;
  }
  const out: S2CFrame[] = [];
  for (const env of dec.push(all)) {
    const idLen = env.body[0];
    const id = Buffer.from(env.body.subarray(1, 1 + idLen)).toString('utf-8');
    const frame = env.body.subarray(1 + idLen);
    if (env.kind === ENV_TEXT) {
      const text = Buffer.from(frame).toString('utf-8');
      const decoded = decodeTextFrame(text);
      out.push({ id, kind: env.kind, text: decoded.kind === 'known' ? (decoded.frame as ServerFrame) : undefined });
    } else {
      out.push({ id, kind: env.kind, binary: decodeBinaryFrame(frame) });
    }
  }
  return out;
}

function openViewer(server: TapServer, t: MockTransport, id: string, profile: 'desktop' | 'phone' = 'desktop', resume: { offset: number; mseq: number; epoch: string } | null = null) {
  const conn = server.attachTransport(t);
  server.feedTransport(conn, encodeEnvelope(ENV_CONTROL, enc(encodeControlMessage({ t: 'viewer-open', viewer: id, profile, resume }))));
  return conn;
}

function fakeClock(start = 1_000_000) {
  let now = start;
  return {
    now: () => now,
    advance: (ms: number) => {
      now += ms;
    },
  };
}

/** Await an observable condition (frames landed, joins counted), not a guess. */
async function waitUntil(cond: () => boolean, what: string, timeoutMs = 10_000): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  while (!cond()) {
    if (Date.now() > deadline) throw new Error(`waitUntil timeout: ${what}`);
    await Bun.sleep(5);
  }
}

const kinds = (t: MockTransport): string[] => collect(t).map((f) => (f.text?.t ?? 'binary'));

test('viewers_join_order_hello_profile_imagedef_seed_stream', async () => {
  const twin = makeTwin(120, 40);
  const clock = fakeClock();
  const server = new TapServer({ session: 'join00001', harness: 'omp', epoch: 'e'.repeat(32), twin, now: clock.now, quiesceMs: 0, joinWaitMs: 5000, makeSeed: () => enc('SEED-BYTES') });
  const t = new MockTransport();
  server.feed([{ kind: 'bytes', data: enc('hello world') }]);
  openViewer(server, t, 'v1');
  await waitUntil(() => kinds(t).includes('seed'), 'join frames');
  const frames = collect(t);
  expect(kinds(t).slice(0, 4)).toEqual(['hello', 'profile', 'seed', 'binary']);
  expect(frames[0].text).toMatchObject({ t: 'hello', proto: 'kb-term/1', session: 'join00001', epoch: 'e'.repeat(32), harness: 'omp' });
  expect(frames[1].text).toMatchObject({ t: 'profile', scrollback: 50000 });
  const seed = frames[2].text as Extract<ServerFrame, { t: 'seed' }>;
  expect(seed.offset).toBe(11);
  expect(seed.len).toBe(10);
  expect(frames[3].binary?.offset).toBe(11);
  expect(frames[3].binary?.payload).toEqual(enc('SEED-BYTES'));
  await server.close();
  twin.dispose();
});

test('viewers_atomic_join_no_loss_no_duplicate', async () => {
  const twin = makeTwin(120, 40);
  const clock = fakeClock();
  const server = new TapServer({ session: 'atom00001', harness: 'omp', epoch: 'e'.repeat(32), twin, now: clock.now, quiesceMs: 0, joinWaitMs: 5000, makeSeed: () => new Uint8Array(0) });
  const t1 = new MockTransport();
  const t2 = new MockTransport();
  server.feed([{ kind: 'bytes', data: enc('AAAA') }]);
  // Synchronized output on: both joins queue concurrently behind quiescence.
  server.feed([{ kind: 'bytes', data: enc(`${E}[?2026h`) }]);
  openViewer(server, t1, 'v1');
  openViewer(server, t2, 'v2');
  await waitUntil(() => server.withholder.syncOn, 'sync on');
  expect(kinds(t1).includes('hello')).toBe(false);
  expect(kinds(t2).includes('hello')).toBe(false);
  const preX = server.head;
  server.feed([{ kind: 'bytes', data: enc(`${E}[?2026l`) }]);
  await waitUntil(() => kinds(t1).includes('seed') && kinds(t2).includes('seed'), 'both joins land');
  const X = preX + enc(`${E}[?2026l`).length;
  for (const t of [t1, t2]) {
    const seed = collect(t).find((f) => f.text?.t === 'seed')?.text as Extract<ServerFrame, { t: 'seed' }>;
    expect(seed.offset).toBe(X);
    expect(seed.len).toBe(0);
  }
  // Push immediately after the joins return: both viewers see BBBB exactly
  // once at X, and no pre-X byte is replayed as live data.
  server.feed([{ kind: 'bytes', data: enc('BBBB') }]);
  for (const t of [t1, t2]) {
    const binaries = collect(t).filter((f) => f.binary);
    expect(binaries.length).toBe(1);
    expect(binaries[0].binary?.offset).toBe(X);
    expect(Buffer.from(binaries[0].binary!.payload).toString('utf-8')).toBe('BBBB');
  }
  await server.close();
  twin.dispose();
});

test('viewers_join_waits_inside_synchronized_frame_then_gets_post_frame_head', async () => {
  const twin = makeTwin(120, 40);
  const clock = fakeClock();
  const server = new TapServer({ session: 'sync00001', harness: 'omp', epoch: 'e'.repeat(32), twin, now: clock.now, quiesceMs: 0, joinWaitMs: 5000, makeSeed: () => new Uint8Array(0) });
  const t = new MockTransport();
  server.feed([{ kind: 'bytes', data: enc(`${E}[?2026h`) }]);
  openViewer(server, t, 'v1');
  await waitUntil(() => server.withholder.syncOn, 'sync on');
  // While synchronized output is on, requestJoin cannot break its loop, so
  // no frame may have escaped (the only way a hello could exist is the
  // join loop exiting, which requires !syncOn).
  expect(kinds(t).includes('hello')).toBe(false);
  server.feed([{ kind: 'bytes', data: enc(`${E}[?2026lFRAME`) }]);
  await waitUntil(() => kinds(t).includes('seed'), 'post-frame join');
  const seed = collect(t).find((f) => f.text?.t === 'seed')?.text as Extract<ServerFrame, { t: 'seed' }>;
  expect(seed.offset).toBe(8 + 8 + 5);
  await server.close();
  twin.dispose();
});

test('viewers_degraded_join_when_never_quiet_and_busy_when_sync_stuck', async () => {
  const twin = makeTwin(120, 40);
  const clock = fakeClock();
  const noisy = new TapServer({ session: 'degr00001', harness: 'omp', epoch: 'e'.repeat(32), twin, now: clock.now, quiesceMs: 5000, joinWaitMs: 80, makeSeed: () => new Uint8Array(0) });
  const t1 = new MockTransport();
  noisy.feed([{ kind: 'bytes', data: enc('x') }]);
  openViewer(noisy, t1, 'v1');
  // The fake clock never moves on its own: advance past joinWaitMs so the
  // join loop's deadline fires and the join lands degraded (100 ms of
  // quiet still fails the 5000 ms quiesce window).
  clock.advance(100);
  await waitUntil(() => noisy.stats.joins >= 1, 'degraded join lands');
  expect(noisy.stats.degradedJoins).toBe(1);
  await noisy.close();

  const clock2 = fakeClock();
  const stuck = new TapServer({ session: 'busy00001', harness: 'omp', epoch: 'e'.repeat(32), twin, now: clock2.now, quiesceMs: 0, joinWaitMs: 80, makeSeed: () => new Uint8Array(0) });
  const t2 = new MockTransport();
  stuck.feed([{ kind: 'bytes', data: enc(`${E}[?2026h`) }]);
  openViewer(stuck, t2, 'v1');
  // Same fake-clock deadline discipline as the degraded case above.
  clock2.advance(100);
  await waitUntil(() => collect(t2).some((f) => f.text?.t === 'error' && f.text.code === 'busy'), 'busy error');
  expect(stuck.viewerCount).toBe(0);
  await stuck.close();
  twin.dispose();
});

test('viewers_over_8mib_behind_dropped_with_too_far_behind_then_rejoin', async () => {
  const clock = fakeClock();
  // Structural twin stub: feed is instant, serialize empty. joinNow needs a
  // non-null twin plus an injected makeSeed, which this supplies.
  const stubTwin = { feed: async () => {}, serialize: { serialize: () => '' } } as unknown as Twin;
  const server = new TapServer({ session: 'lag000002', harness: 'omp', epoch: 'e'.repeat(32), twin: stubTwin, now: clock.now, quiesceMs: 0, joinWaitMs: 5000, makeSeed: () => new Uint8Array(0) });
  const t = new MockTransport();
  t.alwaysBlock = true;
  openViewer(server, t, 'v1');
  await waitUntil(() => server.stats.joins >= 1, 'initial join');
  t.clear();
  server.feed([{ kind: 'bytes', data: new Uint8Array(9 * 1024 * 1024).fill(0x61) }]);
  expect(server.viewerCount).toBe(0);
  const frames = collect(t);
  expect(frames.some((f) => f.text?.t === 'error' && f.text.code === 'too-far-behind')).toBe(true);
  t.alwaysBlock = false;
  t.clear();
  openViewer(server, t, 'v2');
  await waitUntil(() => server.stats.joins >= 2, 'rejoin');
  expect(server.viewerCount).toBe(1);
  expect(kinds(t).includes('hello')).toBe(true);
  await server.close();
});

test('viewers_imagedef_seq0_precedes_seed_with_refviewer_seed_framing', async () => {
  const twin = makeTwin(120, 40);
  const clock = fakeClock();
  const seedText = 'SEED!';
  const server = new TapServer({ session: 'img000001', harness: 'omp', epoch: 'e'.repeat(32), twin, now: clock.now, quiesceMs: 0, joinWaitMs: 5000, makeSeed: () => enc(seedText) });
  const t = new MockTransport();
  const digest = sha(enc('fakepng'));
  server.feed([
    { kind: 'bytes', data: enc('AA') },
    { kind: 'imagedef', def: { id: 3, gen: 1, sha256: digest, mime: 'image/png', w: 8, h: 8, cols: 1, rows: 1 } },
    { kind: 'bytes', data: enc('BB') },
    { kind: 'gfxreset', gen: 2 },
    { kind: 'bytes', data: enc('CC') },
  ]);
  openViewer(server, t, 'v1');
  await waitUntil(() => kinds(t).includes('seed'), 'join with imagedef');
  const frames = collect(t);
  const kindsList = frames.map((f) => (f.text?.t ?? 'binary'));
  const imgIdx = kindsList.indexOf('imagedef');
  const seedIdx = kindsList.indexOf('seed');
  expect(imgIdx).toBeGreaterThanOrEqual(0);
  expect(imgIdx).toBeLessThan(seedIdx);
  expect(frames[imgIdx].text).toMatchObject({ t: 'imagedef', seq: 0, id: 3, gen: 1 });
  const ref = new RefViewer({ epoch: 'e'.repeat(32) });
  for (const f of frames) {
    if (f.text !== undefined) ref.onText(JSON.stringify(f.text));
    if (f.binary !== undefined) ref.onBinary(encodeBinaryFrame(f.binary.offset, f.binary.payload));
  }
  expect(ref.closed).toBe(false);
  const ref2 = new RefViewer({ epoch: 'e'.repeat(32) });
  ref2.onText('{"t":"seed","offset":1000,"len":4}');
  ref2.onBinary(encodeBinaryFrame(1000, enc('ABCDE')));
  expect(ref2.closed).toBe(true);
  expect(ref2.badFrame).toContain('straddle');
  const ref3 = new RefViewer({ epoch: 'e'.repeat(32) });
  ref3.onText('{"t":"seed","offset":1000,"len":4}');
  ref3.onBinary(encodeBinaryFrame(1000, enc('ABCD')));
  expect(ref3.closed).toBe(false);
  expect(ref3.applied).toBe(1000);
  await server.close();
  twin.dispose();
});

test('viewers_resume_decision_table_rows', async () => {
  const clock = fakeClock();
  const twin = makeTwin(120, 40);
  const server = new TapServer({ session: 'res000001', harness: 'omp', epoch: 'e'.repeat(32), twin, now: clock.now, quiesceMs: 0, joinWaitMs: 5000, makeSeed: () => new Uint8Array(0) });
  server.feed([{ kind: 'bytes', data: enc('0123456789') }]);
  // Row: epoch mismatch -> silent downgrade to full join (seed, no error).
  {
    const t = new MockTransport();
    openViewer(server, t, 'mismatch', 'desktop', { offset: 0, mseq: 0, epoch: 'f'.repeat(32) });
    await waitUntil(() => kinds(t).includes('seed'), 'mismatch full join');
    expect(kinds(t)).not.toContain('error');
  }
  // Row: offset ahead -> error{offset-ahead} then full join.
  {
    const t = new MockTransport();
    openViewer(server, t, 'ahead', 'desktop', { offset: 999, mseq: 0, epoch: 'e'.repeat(32) });
    await waitUntil(() => kinds(t).includes('seed'), 'ahead full join');
    expect(kinds(t)).toContain('error');
    const err = collect(t).find((f) => f.text?.t === 'error')?.text as Extract<ServerFrame, { t: 'error' }>;
    expect(err.code).toBe('offset-ahead');
  }
  // Row: in range -> hello only (no profile/seed), replay from offset.
  {
    const t = new MockTransport();
    openViewer(server, t, 'inrange', 'desktop', { offset: 4, mseq: 0, epoch: 'e'.repeat(32) });
    await waitUntil(() => kinds(t).includes('binary'), 'in-range replay');
    const frames = collect(t);
    expect(frames.map((f) => (f.text?.t ?? 'binary'))).toEqual(['hello', 'binary']);
    expect(frames[1].binary?.offset).toBe(4);
    expect(frames[1].binary?.payload).toEqual(enc('456789'));
  }
  await server.close();
  twin.dispose();
});

test('viewers_full_join_when_resume_beyond_8mib_lag', async () => {
  const clock = fakeClock();
  const stubTwin = { feed: async () => {}, serialize: { serialize: () => '' } } as unknown as Twin;
  const server = new TapServer({ session: 'res000002', harness: 'omp', epoch: 'e'.repeat(32), twin: stubTwin, now: clock.now, quiesceMs: 0, joinWaitMs: 5000, makeSeed: () => new Uint8Array(0) });
  server.feed([{ kind: 'bytes', data: new Uint8Array(40 * 1024 * 1024).fill(0x62) }]);
  const t = new MockTransport();
  openViewer(server, t, 'old', 'desktop', { offset: 0, mseq: 0, epoch: 'e'.repeat(32) });
  await waitUntil(() => kinds(t).includes('seed'), 'full join');
  expect(kinds(t)).not.toContain('error');
  await server.close();
});

test('viewers_clipboard_only_to_recently_typed_viewers', async () => {
  const clock = fakeClock(1_000_000);
  const twin = makeTwin(120, 40);
  const server = new TapServer({ session: 'clip00001', harness: 'omp', epoch: 'e'.repeat(32), twin, now: clock.now, quiesceMs: 0, joinWaitMs: 5000, makeSeed: () => new Uint8Array(0) });
  const t = new MockTransport();
  const conn = openViewer(server, t, 'v1');
  await waitUntil(() => kinds(t).includes('hello'), 'clipboard viewer join');
  server.feedTransport(conn, encodeEnvelope(ENV_CONTROL, enc(encodeControlMessage({ t: 'typed', viewer: 'v1' }))));
  t.clear();
  clock.advance(9900);
  server.feed([{ kind: 'bytes', data: enc(`${E}]52;c;aGk=\x07`) }]);
  const got1 = collect(t).filter((f) => f.text?.t === 'clipboard');
  expect(got1.length).toBe(1);
  expect(got1[0].text).toMatchObject({ t: 'clipboard', data_b64: 'aGk=' });
  t.clear();
  clock.advance(300);
  server.feed([{ kind: 'bytes', data: enc(`${E}]52;c;aGk=\x07`) }]);
  const got2 = collect(t).filter((f) => f.text?.t === 'clipboard');
  expect(got2.length).toBe(0);
  await server.close();
  twin.dispose();
});

test('viewers_tap_sock_mode_0600', async () => {
  const twin = makeTwin(120, 40);
  const runDir = mkdtempSync(join(tmpdir(), 'kbt7-'));
  const server = new TapServer({ session: 'sock00001', harness: 'omp', epoch: 'e'.repeat(32), twin, runDir, testMode: true });
  await server.listen();
  try {
    expect(statSync(server.sockPath).mode & 0o777).toBe(0o600);
  } finally {
    await server.close();
    twin.dispose();
  }
});

test('viewers_stats_counters_answer_in_test_mode', async () => {
  const clock = fakeClock();
  const twin = makeTwin(120, 40);
  const server = new TapServer({ session: 'stat00001', harness: 'omp', epoch: 'e'.repeat(32), twin, now: clock.now, quiesceMs: 0, joinWaitMs: 5000, makeSeed: () => new Uint8Array(0), testMode: true });
  const t = new MockTransport();
  const conn = openViewer(server, t, 'v1');
  await waitUntil(() => kinds(t).includes('hello'), 'stats viewer join');
  t.clear();
  server.feedTransport(conn, encodeEnvelope(ENV_CONTROL, enc('{"t":"stats"}')));
  const dec = new EnvelopeDecoder();
  const all = new Uint8Array(t.written.reduce((a, b) => a + b.length, 0));
  let o = 0;
  for (const part of t.written) {
    all.set(part, o);
    o += part.length;
  }
  const envs = dec.push(all);
  expect(envs.length).toBe(1);
  expect(Buffer.from(envs[0].body).toString('utf-8')).toBe('{"t":"stats","joins":1,"degradedJoins":0,"viewers":1}');
  await server.close();
  twin.dispose();
});

test('viewers_seed_from_real_twin_omits_withheld_mouse_enable', async () => {
  const twin = makeTwin(120, 40);
  const clock = fakeClock();
  // Default seed maker (real serialize): the test pins that the twin is fed
  // the post-withhold stream (brief rule 7).
  const server = new TapServer({ session: 'seed00001', harness: 'omp', epoch: 'e'.repeat(32), twin, now: clock.now, quiesceMs: 0, joinWaitMs: 5000 });
  const t = new MockTransport();
  server.feed([{ kind: 'bytes', data: enc(`${E}[?1000hX${E}[?1000l`) }]);
  openViewer(server, t, 'v1');
  await waitUntil(() => kinds(t).includes('seed'), 'join with real twin');
  const frames = collect(t);
  const seedPayload = Buffer.concat(frames.filter((f) => f.binary).map((f) => Buffer.from(f.binary!.payload)));
  expect(seedPayload.includes(Buffer.from('?1000h'))).toBe(false);
  expect(seedPayload.includes(Buffer.from('X'))).toBe(true);
  const ringBack = server.ring.read(0, server.head);
  expect(ringBack).not.toBeNull();
  expect(Buffer.from(ringBack!).includes(Buffer.from('?1000h'))).toBe(false);
  await server.close();
  twin.dispose();
});

test('viewers_resume_replays_marker_before_resume_offset', async () => {
  const clock = fakeClock();
  const twin = makeTwin(120, 40);
  const server = new TapServer({ session: 'mrk000001', harness: 'omp', epoch: 'e'.repeat(32), twin, now: clock.now, quiesceMs: 0, joinWaitMs: 5000, makeSeed: () => new Uint8Array(0) });
  server.feed([{ kind: 'bytes', data: enc('0123456789') }]);
  const digest = sha(enc('fakepng'));
  server.feed([{ kind: 'imagedef', def: { id: 5, gen: 1, sha256: digest, mime: 'image/png', w: 8, h: 8, cols: 1, rows: 1 } }]);
  server.feed([{ kind: 'bytes', data: enc('abcdefghij') }]);
  // The marker's offset is 10; resume from 15 with mseq 0: the viewer never
  // applied the marker, so replay must still deliver it exactly once.
  const t = new MockTransport();
  openViewer(server, t, 'v1', 'desktop', { offset: 15, mseq: 0, epoch: 'e'.repeat(32) });
  await waitUntil(() => kinds(t).includes('binary'), 'resume replay');
  const frames = collect(t);
  expect(frames.map((f) => (f.text?.t ?? 'binary'))).toEqual(['hello', 'imagedef', 'binary']);
  const markers = frames.filter((f) => f.text?.t === 'imagedef');
  expect(markers.length).toBe(1);
  expect(markers[0].text).toMatchObject({ t: 'imagedef', seq: 1, id: 5 });
  expect(frames[2].binary?.offset).toBe(15);
  expect(Buffer.from(frames[2].binary!.payload).toString('utf-8')).toBe('fghij');
  await server.close();
  twin.dispose();
});

test('viewers_twin_feed_survives_chunk_larger_than_ring_cap', async () => {
  const twin = makeTwin(120, 40);
  const clock = fakeClock();
  const server = new TapServer({ session: 'big000001', harness: 'omp', epoch: 'e'.repeat(32), twin, now: clock.now, quiesceMs: 0, joinWaitMs: 5000, ringCap: 64 * 1024 });
  const chunk = new Uint8Array(200 * 1024).fill(0x61);
  server.feed([{ kind: 'bytes', data: chunk }]);
  expect(server.head).toBe(chunk.length);
  const t = new MockTransport();
  openViewer(server, t, 'v1');
  // The seed arriving at all proves twinOffset drained to head; its content
  // proves the twin parsed the whole chunk (a ring re-read of the aged-out
  // start would have fed it nothing).
  await waitUntil(() => kinds(t).includes('seed'), 'join after oversize chunk');
  const seedPayload = Buffer.concat(collect(t).filter((f) => f.binary).map((f) => Buffer.from(f.binary!.payload)));
  expect(seedPayload.includes(Buffer.from('a'))).toBe(true);
  await server.close();
  twin.dispose();
});

test('viewers_join_emits_only_latest_image_gen_per_id', async () => {
  const clock = fakeClock();
  const twin = makeTwin(120, 40);
  const server = new TapServer({ session: 'gen000001', harness: 'omp', epoch: 'e'.repeat(32), twin, now: clock.now, quiesceMs: 0, joinWaitMs: 5000, makeSeed: () => new Uint8Array(0) });
  const digest = sha(enc('fakepng'));
  server.feed([
    { kind: 'imagedef', def: { id: 3, gen: 1, sha256: digest, mime: 'image/png', w: 8, h: 8, cols: 1, rows: 1 } },
    { kind: 'imagedef', def: { id: 3, gen: 2, sha256: digest, mime: 'image/png', w: 8, h: 8, cols: 1, rows: 1 } },
    { kind: 'imagedef', def: { id: 4, gen: 1, sha256: digest, mime: 'image/png', w: 8, h: 8, cols: 1, rows: 1 } },
  ]);
  const t = new MockTransport();
  openViewer(server, t, 'v1');
  await waitUntil(() => kinds(t).includes('seed'), 'join with imagedefs');
  const imgDefs = collect(t).filter((f) => f.text?.t === 'imagedef').map((f) => f.text as Extract<ServerFrame, { t: 'imagedef' }>);
  expect(imgDefs.length).toBe(2);
  expect(imgDefs.map((d) => `${d.id}:${d.gen}`)).toEqual(['3:2', '4:1']);
  expect(imgDefs.every((d) => d.seq === 0)).toBe(true);
  await server.close();
  twin.dispose();
});

test('viewers_bad_frames_close_only_the_offending_connection', async () => {
  const clock = fakeClock();
  const twin = makeTwin(120, 40);
  const server = new TapServer({ session: 'bad000001', harness: 'omp', epoch: 'e'.repeat(32), twin, now: clock.now, quiesceMs: 0, joinWaitMs: 5000, makeSeed: () => new Uint8Array(0) });
  server.feed([{ kind: 'bytes', data: enc('0123456789') }]);
  const good = new MockTransport();
  openViewer(server, good, 'good');
  await waitUntil(() => kinds(good).includes('hello'), 'good viewer join');
  good.clear();
  // Oversize envelope header (claimed length 1 MiB + 1) closes its connection.
  const evil = new MockTransport();
  const evilConn = server.attachTransport(evil);
  const hdr = new Uint8Array(4);
  new DataView(hdr.buffer).setUint32(0, 1_048_577);
  server.feedTransport(evilConn, hdr);
  expect(evil.closed).toBe(true);
  // resumefrom with a string offset (never park a viewer at NaN).
  const evil2 = new MockTransport();
  const evil2Conn = server.attachTransport(evil2);
  server.feedTransport(evil2Conn, encodeEnvelope(ENV_CONTROL, enc(`{"t":"viewer-open","viewer":"evil","profile":"desktop","resume":{"offset":"abc","mseq":0,"epoch":"${'e'.repeat(32)}"}}`)));
  expect(evil2.closed).toBe(true);
  // Viewer id longer than 255 bytes (never leave a zombie joining viewer).
  const evil3 = new MockTransport();
  const evil3Conn = server.attachTransport(evil3);
  server.feedTransport(evil3Conn, encodeEnvelope(ENV_CONTROL, enc(encodeControlMessage({ t: 'viewer-open', viewer: 'x'.repeat(300), profile: 'desktop', resume: null }))));
  expect(evil3.closed).toBe(true);
  expect(server.viewerCount).toBe(1);
  // The healthy connection still receives live bytes.
  server.feed([{ kind: 'bytes', data: enc('Z') }]);
  const got = collect(good).filter((f) => f.binary);
  expect(got.length).toBe(1);
  expect(Buffer.from(got[0].binary!.payload).toString('utf-8')).toBe('Z');
  expect(good.closed).toBe(false);
  await server.close();
  twin.dispose();
});

test('viewers_second_local_listen_on_live_socket_refuses', async () => {
  const runDir = mkdtempSync(join(tmpdir(), 'kbt7-'));
  const twin = makeTwin(120, 40);
  const first = new TapServer({ session: 'live00001', harness: 'omp', epoch: 'e'.repeat(32), twin, runDir, testMode: true });
  await first.listen();
  try {
    const twin2 = makeTwin(120, 40);
    const second = new TapServer({ session: 'live00001', harness: 'omp', epoch: 'f'.repeat(32), twin: twin2, runDir, testMode: true });
    let refused = false;
    try {
      await second.listen();
    } catch {
      refused = true;
    }
    expect(refused).toBe(true);
    await second.close();
    twin2.dispose();
    // The socket still belongs to the first server: a real viewer-open over
    // it gets the first server's epoch in its hello.
    const socket = connect(first.sockPath);
    const dec = new EnvelopeDecoder();
    const hellos: string[] = [];
    socket.on('error', () => socket.destroy());
    socket.on('connect', () => {
      socket.write(encodeEnvelope(ENV_CONTROL, enc(encodeControlMessage({ t: 'viewer-open', viewer: 'probe', profile: 'desktop', resume: null }))));
    });
    socket.on('data', (chunk: Buffer) => {
      for (const env of dec.push(new Uint8Array(chunk))) {
        if (env.kind !== ENV_TEXT) continue;
        const { frame } = parseS2C(env.body);
        const text = Buffer.from(frame).toString('utf-8');
        const decoded = decodeTextFrame(text);
        if (decoded.kind === 'known' && (decoded.frame as ServerFrame).t === 'hello') {
          hellos.push((decoded.frame as Extract<ServerFrame, { t: 'hello' }>).epoch);
        }
      }
    });
    await waitUntil(() => hellos.length > 0, 'hello from first server');
    socket.destroy();
    expect(hellos[0]).toBe('e'.repeat(32));
  } finally {
    await first.close();
    twin.dispose();
  }
});

test('viewers_clipboard_never_reaches_a_viewer_that_never_typed', async () => {
  const clock = fakeClock(0);
  const twin = makeTwin(120, 40);
  const server = new TapServer({ session: 'clip00002', harness: 'omp', epoch: 'e'.repeat(32), twin, now: clock.now, quiesceMs: 0, joinWaitMs: 5000, makeSeed: () => new Uint8Array(0) });
  const t = new MockTransport();
  openViewer(server, t, 'v1');
  await waitUntil(() => kinds(t).includes('hello'), 'clipboard viewer join');
  t.clear();
  server.feed([{ kind: 'bytes', data: enc(`${E}]52;c;aGk=\x07`) }]);
  expect(collect(t).filter((f) => f.text?.t === 'clipboard').length).toBe(0);
  await server.close();
  twin.dispose();
});
