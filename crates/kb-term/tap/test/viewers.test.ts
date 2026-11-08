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
import { RefViewer } from '../src/client_ref.ts';
import { makeTwin, type Twin } from '../src/twin.ts';
import { TapServer, type Transport } from '../src/viewers.ts';
import { EnvelopeDecoder, decodeBinaryFrame, decodeTextFrame, encodeBinaryFrame, encodeControlMessage, encodeEnvelope, ENV_CONTROL, ENV_TEXT, type ServerFrame } from '../src/wire.ts';

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
  const t = new MockTransport();
  server.feed([{ kind: 'bytes', data: enc('AAAA') }]);
  openViewer(server, t, 'v1');
  await waitUntil(() => kinds(t).includes('hello'), 'join hello');
  // Push a chunk once the join has landed: the viewer sees it exactly once.
  // (The pre-join 'AAAA' is embodied in the seed -- here empty -- and is
  // never replayed; live bytes start at exactly the seed offset.)
  server.feed([{ kind: 'bytes', data: enc('BBBB') }]);
  const frames = collect(t);
  const payload = frames.filter((f) => f.binary).map((f) => Buffer.from(f.binary!.payload).toString('utf-8')).join('');
  expect(payload).toBe('BBBB');
  expect(frames.filter((f) => f.binary).map((f) => f.binary!.offset)).toEqual([4]);
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
