// main.ts production wiring seam tests (integration audit F1/F7/F11): the
// whole chain control-%output -> QueryStripper -> GraphicsExtractor ->
// withhold -> ring -> twin -> viewer, driven through the real createTapStack
// with a fake control spawn (no tmux) and a real unix-socket viewer. Plus the
// --pins/--version/validation surface and the compiled-binary smoke.
import { test, expect } from 'bun:test';
import { createHash } from 'node:crypto';
import { chmodSync, mkdtempSync, rmSync, existsSync, writeFileSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { PINS_JSON, SESSION_RE, createTapStack, parseArgs } from '../src/main.ts';
import { connect, type Socket } from 'node:net';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { makePng } from './helpers-graphics.ts';
import type { SpawnedProcess } from '../src/control.ts';
import { makeTwin, type Twin } from '../src/twin.ts';
import { EnvelopeDecoder, decodeTextFrame, encodeControlMessage, encodeEnvelope, ENV_CONTROL, ENV_TEXT, ENV_BINARY, parseS2C } from '../src/wire.ts';

const enc = (s: string) => new TextEncoder().encode(s);
const sha = (buf: Uint8Array) => createHash('sha256').update(Buffer.from(buf)).digest('hex');

// ---------------------------------------------------------------- fake spawn
class FakeStdout implements AsyncIterable<Uint8Array> {
  private queue: Uint8Array[] = [];
  private waiter: ((v: IteratorResult<Uint8Array>) => void) | null = null;

  [Symbol.asyncIterator](): AsyncIterator<Uint8Array> {
    const self = this;
    return {
      next(): Promise<IteratorResult<Uint8Array>> {
        if (self.queue.length > 0) return Promise.resolve({ value: self.queue.shift()!, done: false });
        return new Promise((resolve) => {
          self.waiter = resolve;
        });
      },
    };
  }

  push(s: string): void {
    const bytes = enc(s);
    if (this.waiter) {
      const w = this.waiter;
      this.waiter = null;
      w({ value: bytes, done: false });
    } else {
      this.queue.push(bytes);
    }
  }
}

class FakeSpawn {
  argv: string[] = [];
  writes: string[] = [];
  stdout = new FakeStdout();
  killed = false;

  fn = (argv: string[]): SpawnedProcess => {
    this.argv = argv;
    return {
      stdin: { write: (d: string) => (this.writes.push(d), d.length) },
      stdout: this.stdout,
      kill: () => {
        this.killed = true;
      },
    };
  };
}

// ----------------------------------------------------------------- helpers
async function waitUntil(cond: () => boolean, what: string, timeoutMs = 10_000): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  while (!cond()) {
    if (Date.now() > deadline) throw new Error(`waitUntil timeout: ${what}`);
    await Bun.sleep(5);
  }
}

/** Feed a decoded %output payload: ESC -> \033 octal like real tmux. */
/** Feed a decoded %output payload: ESC -> \033, backslash -> \134, like real tmux. */
function octal(pane: string, data: string): string {
  return `%output ${pane} ${data.replaceAll('\\', '\\134').replaceAll('\x1b', '\\033')}\n`;
}

interface Captured {
  texts: string[];
  binaries: Uint8Array[];
  closed: boolean;
}

function capture(): Captured {
  return { texts: [], binaries: [], closed: false };
}

/** Open a real viewer connection; viewer-open carries no resume (full join). */
function openViewer(sockPath: string, cap: Captured): Promise<Socket> {
  return new Promise((resolve, reject) => {
    const socket = connect(sockPath);
    const dec = new EnvelopeDecoder();
    socket.on('error', reject);
    socket.on('connect', () => {
      resolve(socket);
      socket.write(encodeEnvelope(ENV_CONTROL, enc(encodeControlMessage({ t: 'viewer-open', viewer: 'v1', profile: 'desktop', resume: null }))));
    });
    socket.on('data', (chunk: Buffer) => {
      for (const env of dec.push(new Uint8Array(chunk))) {
        if (env.kind === ENV_TEXT) {
          cap.texts.push(Buffer.from(parseS2C(env.body).frame).toString('utf-8'));
        } else if (env.kind === ENV_BINARY) {
          cap.binaries.push(parseS2C(env.body).frame.slice(8)); // strip 8-byte offset
        }
      }
    });
    socket.on('close', () => {
      cap.closed = true;
    });
  });
}

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

// ------------------------------------------------------------------- tests
test('seam_control_to_viewer_end_to_end', async () => {
  const runDir = mkdtempSync(join(tmpdir(), 'kbt7-main-'));
  const fake = new FakeSpawn();
  const stack = createTapStack({
    session: 'seam00001',
    runDir,
    harness: 'omp',
    cols: 80,
    rows: 24,
    epoch: 'a'.repeat(32),
    spawn: fake.fn,
    quiesceMs: 0,
    joinWaitMs: 2000,
  });
  const starting = stack.start();
  fake.stdout.push('%begin 1000 0 0\n%end 1000 0 0\n');
  fake.stdout.push('%begin 1000 1 0\n%end 1000 1 0\n');
  await starting;

  try {
    // Viewer joins BEFORE any pane output: seed is empty, then live bytes.
    const cap = capture();
    const socket = await openViewer(stack.server.sockPath, cap);
    await waitUntil(() => cap.texts.some((t) => t.includes('"t":"hello"')), 'hello');

    // One control line carrying: a stripped query (ESC[6n DSR), plain-text
    // bytes, and a kitty image transmission (must become an imagedef).
    const png = makePng(8, 8, 33);
    const b64 = Buffer.from(png).toString('base64');
    const image = `a=t,f=100,i=7;${b64}`;
    const lines =
      octal('%0', '\x1b[6nhello') +
      octal('%0', `\x1b_G${image}\x1b\\`) +
      octal('%0', '\x1b_Ga=p,U=1,q=2,i=7,p=7,c=8,r=8\x1b\\') +
      octal('%0', '\x1b[1;31mworld\x1b[0m');
    fake.stdout.push(lines);

    // The imagedef marker must reach the viewer; the DSR query and the APC
    // body must not.
    await waitUntil(() => cap.texts.some((t) => t.includes('"t":"imagedef"')), 'imagedef');
    await waitUntil(() => stack.server.head > 0, 'ring head advanced');
    const imagedef = cap.texts.find((t) => t.includes('"t":"imagedef"'))!;
    const parsed = JSON.parse(imagedef) as Record<string, unknown>;
    expect(parsed['seq']).toBeGreaterThan(0);
    expect(parsed['sha256']).toBe(sha(png));

    // Live binary frames after the seed are exactly the stripped, de-imaged
    // stream: the DSR query and the APC body never reach the viewer.
    await waitUntil(() => cap.binaries.length > 0, 'live bytes');
    const seedText = cap.texts.find((t) => t.includes('"t":"seed"'));
    const seedObj = (seedText === undefined ? {} : JSON.parse(seedText)) as Record<string, unknown>;
    const seedLenRaw = seedObj['len'];
    let skip = typeof seedLenRaw === 'number' ? seedLenRaw : 0;
    const liveParts: Uint8Array[] = [];
    for (const b of cap.binaries) {
      if (skip >= b.length) {
        skip -= b.length;
        continue;
      }
      liveParts.push(b.slice(skip));
      skip = 0;
    }
    const live = concat(liveParts);
    const expected = enc('hello\x1b[1;31mworld\x1b[0m');
    expect(sha(live)).toBe(sha(expected));
    expect(stack.server.head).toBe(expected.length);
    socket.destroy();
  } finally {
    stack.stop();
    rmSync(runDir, { recursive: true, force: true });
  }
});

test('seam_sync_stuck_yields_busy_and_close', async () => {
  const runDir = mkdtempSync(join(tmpdir(), 'kbt7-main-'));
  const fake = new FakeSpawn();
  const stack = createTapStack({
    session: 'syncbusy1',
    runDir,
    harness: 'omp',
    cols: 80,
    rows: 24,
    epoch: 'a'.repeat(32),
    spawn: fake.fn,
    quiesceMs: 0,
    joinWaitMs: 300,
  });
  const starting = stack.start();
  fake.stdout.push('%begin 1000 0 0\n%end 1000 0 0\n');
  fake.stdout.push('%begin 1000 1 0\n%end 1000 1 0\n');
  await starting;

  try {
    // Open a synchronized frame that never closes: joins must go busy.
    fake.stdout.push(octal('%0', '\x1b[?2026h'));
    await waitUntil(() => stack.server.head > 0, 'frame head');
    const cap = capture();
    const socket = await openViewer(stack.server.sockPath, cap);
    await waitUntil(() => cap.texts.some((t) => t.includes('"busy"')), 'busy frame');
    await waitUntil(() => cap.closed, 'viewer closed');
    socket.destroy();
  } finally {
    stack.stop();
    rmSync(runDir, { recursive: true, force: true });
  }
});

test('seam_mid_sequence_waits_for_strip_pending_to_clear', async () => {
  const runDir = mkdtempSync(join(tmpdir(), 'kbt7-main-'));
  const fake = new FakeSpawn();
  const stack = createTapStack({
    session: 'midseq001',
    runDir,
    harness: 'omp',
    cols: 80,
    rows: 24,
    epoch: 'a'.repeat(32),
    spawn: fake.fn,
    quiesceMs: 0,
    joinWaitMs: 2000,
  });
  const starting = stack.start();
  fake.stdout.push('%begin 1000 0 0\n%end 1000 0 0\n');
  fake.stdout.push('%begin 1000 1 0\n%end 1000 1 0\n');
  await starting;

  try {
    // Half a CSI: the stripper holds it (pending > 0), so the join must wait
    // rather than seed mid-escape.
    fake.stdout.push(octal('%0', 'hi\x1b['));
    await waitUntil(() => stack.server.head > 0, 'first bytes in ring');
    const cap = capture();
    const socket = await openViewer(stack.server.sockPath, cap);
    await Bun.sleep(100);
    expect(cap.texts.some((t) => t.includes('"t":"seed"'))).toBe(false);
    // Complete the sequence: the join may now proceed.
    fake.stdout.push(octal('%0', '1;31mbye'));
    await waitUntil(() => cap.texts.some((t) => t.includes('"t":"seed"')), 'seed after completion');
    socket.destroy();
  } finally {
    stack.stop();
    rmSync(runDir, { recursive: true, force: true });
  }
});

test('seam_media_stub_advances_offsets_and_counts', async () => {
  const runDir = mkdtempSync(join(tmpdir(), 'kbt7-main-'));
  const fake = new FakeSpawn();
  const stack = createTapStack({
    session: 'media0001',
    runDir,
    harness: 'omp',
    cols: 80,
    rows: 24,
    epoch: 'a'.repeat(32),
    spawn: fake.fn,
    quiesceMs: 0,
    joinWaitMs: 2000,
  });
  const starting = stack.start();
  fake.stdout.push('%begin 1000 0 0\n%end 1000 0 0\n');
  fake.stdout.push('%begin 1000 1 0\n%end 1000 1 0\n');
  await starting;

  try {
    const cap = capture();
    const socket = await openViewer(stack.server.sockPath, cap);
    await waitUntil(() => cap.texts.some((t) => t.includes('"t":"hello"')), 'hello');
    // IIP sequence: ESC]1337;File=inline=1:<b64>BEL.
    const payload = Buffer.from('fake-iip-bytes').toString('base64');
    fake.stdout.push(octal('%0', '\x1b]1337;File=inline=1:' + payload + '\x07'));
    await waitUntil(() => stack.gfx.stats.iip + stack.gfx.stats.sixel > 0, 'media extracted');
    // The stub is stream bytes with real offsets (cursor-rule acceptance):
    // head advanced by exactly the stub length, not a silent drop.
    const origBytes = enc(`\x1b]1337;File=inline=1:${payload}`);
    const stubLen = enc(`\x1b]7746 ; kb-media ; iip ; ${sha(origBytes)} ; ${origBytes.length} \x1b\\`).length;
    expect(stack.server.head).toBe(stubLen);
    socket.destroy();
  } finally {
    stack.stop();
    rmSync(runDir, { recursive: true, force: true });
  }
});

// --------------------------------------------------------- reseed seam

/** %begin/%end reply block: lines are the command's reply lines. */
function block(num: number, lines: string[]): string {
  return `%begin 1000 ${num} 0\n${lines.map((l) => `${l}\n`).join('')}%end 1000 ${num} 0\n`;
}

/**
 * A real twin behind a recording proxy: every feed call is recorded (so
 * exactly-once and withheld-byte assertions are direct), one feed payload
 * containing `gateOn` (substring) parks on a releasable deferred (so the
 * test can land mid-attempt output deterministically), and resets are
 * counted.
 */
function recordingTwin(inner: Twin, gateOn: string) {
  const feeds: string[] = [];
  let release: ((v: Promise<void>) => void) | null = null;
  const rec = {
    feeds,
    resets: 0,
    released: false,
    releaseGated(): void {
      rec.released = true;
      release?.(inner.feed(enc('')));
    },
  };
  const twin = {
    term: inner.term,
    serialize: inner.serialize,
    cols: inner.cols,
    rows: inner.rows,
    widthPin: inner.widthPin,
    feed: (b: Uint8Array | string): Promise<void> => {
      const s = typeof b === 'string' ? b : Buffer.from(b).toString('utf-8');
      feeds.push(s);
      if (!rec.released && s.includes(gateOn)) {
        return new Promise<void>((resolve) => {
          release = (p: Promise<void>) => {
            void p.then(() => inner.feed(b)).then(resolve);
          };
        });
      }
      return inner.feed(b);
    },
    reset: () => {
      rec.resets++;
      inner.reset();
    },
    resize: (c: number, r: number) => inner.resize(c, r),
    dispose: () => inner.dispose(),
    repliesDropped: () => inner.repliesDropped(),
    lineCount: (w: 'normal' | 'alt') => inner.lineCount(w),
  } as Twin;
  return { rec, twin };
}

// Reseed through createTapStack: output after B must reach the twin
// EXACTLY ONCE (only via the resumed chain, post-withhold), the withhold
// mouse-enable must never be reintroduced, and the seed a late viewer
// gets afterwards carries the capture + post-B bytes.
test('seam_reseed_handover_feeds_post_B_bytes_exactly_once_post_withhold', async () => {
  const runDir = mkdtempSync(join(tmpdir(), 'kbt7-main-'));
  const fake = new FakeSpawn();
  const inner = makeTwin(80, 24);
  const { rec, twin } = recordingTwin(inner, 'reseeded');
  const stack = createTapStack({
    session: 'reseed001',
    runDir,
    harness: 'omp',
    cols: 80,
    rows: 24,
    epoch: 'a'.repeat(32),
    spawn: fake.fn,
    twin,
    quiesceMs: 0,
    joinWaitMs: 3000,
  });
  const starting = stack.start();
  fake.stdout.push(block(0, []));
  fake.stdout.push(block(1, []));
  await starting;
  try {
    const cap = capture();
    const socket = await openViewer(stack.server.sockPath, cap);
    await waitUntil(() => cap.texts.some((t) => t.includes('"t":"hello"')), 'pre-reseed hello');
    fake.stdout.push(octal('%0', 'PRE'));
    await waitUntil(() => rec.feeds.some((f) => f.includes('PRE')), 'PRE reached twin');

    fake.stdout.push('%pause %0\n');
    await waitUntil(() => fake.writes.length >= 3, 'pause continue + reseed batch written');
    fake.stdout.push(block(2, [])); // continue ack
    fake.stdout.push(block(3, ['A 0 5 0']));
    fake.stdout.push(block(4, ['reseeded-content']));
    fake.stdout.push(block(5, []));
    fake.stdout.push(block(6, ['B 0 5 0']));
    fake.stdout.push(block(7, []));
    await waitUntil(() => rec.feeds.some((f) => f.includes('reseeded-content')), 'capture feed parked');
    // Post-B output, while the attempt is parked on the capture feed:
    // a withheld main-screen mouse enable (must never reach the twin)
    // plus plain bytes (must reach it exactly once, after the capture).
    fake.stdout.push(octal('%0', 'AFTER-B\x1b[?1000htail'));
    await waitUntil(() => stack.server.head >= 3 + 11, 'post-B bytes in ring');
    rec.releaseGated();
    await waitUntil(() => rec.feeds.some((f) => f.includes('AFTER-Btail')), 'post-B fed via chain');

    // Exactly once: the reseed queue must not have fed them too, and no
    // feed may carry the withheld mouse enable.
    expect(rec.feeds.filter((f) => f.includes('AFTER-B')).length).toBe(1);
    expect(rec.feeds.filter((f) => f.includes('PRE')).length).toBe(1);
    expect(rec.feeds.filter((f) => f.includes('reseeded-content')).length).toBe(1);
    expect(rec.feeds.some((f) => f.includes('?1000h'))).toBe(false);
    const afterIdx = rec.feeds.findIndex((f) => f.includes('AFTER-Btail'));
    const capIdx = rec.feeds.findIndex((f) => f.includes('reseeded-content'));
    expect(afterIdx).toBeGreaterThan(capIdx);
    expect(rec.resets).toBe(1);
    // The live viewer receives the post-B bytes exactly once, in order.
    await waitUntil(() => Buffer.concat(cap.binaries.map((b) => Buffer.from(b))).toString('utf-8').includes('AFTER-Btail'), 'viewer live post-B');
    const liveText = Buffer.concat(cap.binaries.map((b) => Buffer.from(b))).toString('utf-8');
    expect(liveText.split('AFTER-Btail').length - 1).toBe(1);

    // A late viewer joins and its seed carries capture + post-B bytes.
    const cap2 = capture();
    const socket2 = await openViewer(stack.server.sockPath, cap2);
    await waitUntil(() => cap2.texts.some((t) => t.includes('"t":"seed"')), 'post-reseed seed');
    const seedText = cap2.texts.find((t) => t.includes('"t":"seed"'));
    const seedLen = seedText === undefined ? 0 : (JSON.parse(seedText) as Record<string, unknown>)['len'];
    expect(typeof seedLen).toBe('number');
    let skip = seedLen as number;
    const seedParts: Uint8Array[] = [];
    for (const b of cap2.binaries) {
      if (skip >= b.length) {
        skip -= b.length;
        continue;
      }
      seedParts.push(b.slice(skip));
      skip = 0;
    }
    const seedBytes = Buffer.concat(seedParts.map((b) => Buffer.from(b))).toString('utf-8');
    expect(seedBytes).toContain('reseeded-content');
    expect(seedBytes).toContain('AFTER-Btail');
    socket.destroy();
    socket2.destroy();
  } finally {
    stack.stop();
    inner.dispose();
    rmSync(runDir, { recursive: true, force: true });
  }
});

// A second pause during a slow reseed must serialize behind it: the
// second batch is not written until the first reseed completes, each
// capture resets exactly once, and mid-flight output lands exactly once.
test('seam_overlapping_pause_serializes_reseeds', async () => {
  const runDir = mkdtempSync(join(tmpdir(), 'kbt7-main-'));
  const fake = new FakeSpawn();
  const inner = makeTwin(80, 24);
  const { rec, twin } = recordingTwin(inner, 'first-capture');
  const stack = createTapStack({
    session: 'reseed002',
    runDir,
    harness: 'omp',
    cols: 80,
    rows: 24,
    epoch: 'a'.repeat(32),
    spawn: fake.fn,
    twin,
    quiesceMs: 0,
    joinWaitMs: 3000,
  });
  const starting = stack.start();
  fake.stdout.push(block(0, []));
  fake.stdout.push(block(1, []));
  await starting;
  try {
    fake.stdout.push(octal('%0', 'PRE'));
    await waitUntil(() => rec.feeds.some((f) => f.includes('PRE')), 'PRE reached twin');

    fake.stdout.push('%pause %0\n');
    await waitUntil(() => fake.writes.length >= 3, 'first batch written');
    fake.stdout.push(block(2, [])); // continue 1 ack
    fake.stdout.push(block(3, ['A 0 5 0']));
    fake.stdout.push(block(4, ['first-capture']));
    fake.stdout.push(block(5, []));
    fake.stdout.push(block(6, ['B 0 5 0']));
    fake.stdout.push(block(7, []));
    await waitUntil(() => rec.feeds.some((f) => f.includes('first-capture')), 'first capture parked');

    // Second pause DURING the first (gated) reseed: its continue is
    // written immediately, but its capture batch must queue behind the
    // first reseed and NOT be written yet.
    fake.stdout.push('%pause %0\n');
    await waitUntil(() => fake.writes.length >= 4, 'second continue written');
    // A wall-clock settle is required here: this asserts the ABSENCE of
    // the second batch write, and the only way it could appear is via the
    // (gated) first reseed completing.
    await Bun.sleep(100);
    expect(fake.writes.length).toBe(4);
    // Mid-flight output during both reseeds.
    fake.stdout.push(octal('%0', 'MID'));
    await waitUntil(() => stack.server.head >= 6, 'MID in ring');

    rec.releaseGated();
    await waitUntil(() => fake.writes.length >= 5, 'second batch written after first reseed');
    // Order of pending resolution: continue-2 ack, then the second batch.
    fake.stdout.push(block(8, []));
    fake.stdout.push(block(9, ['A 0 7 0']));
    fake.stdout.push(block(10, ['second-capture']));
    fake.stdout.push(block(11, []));
    fake.stdout.push(block(12, ['B 0 7 0']));
    fake.stdout.push(block(13, []));
    await waitUntil(() => rec.feeds.some((f) => f.includes('second-capture')), 'second capture fed');
    await waitUntil(() => rec.feeds.some((f) => f === 'MID'), 'MID fed exactly once');

    expect(rec.feeds.filter((f) => f === 'MID').length).toBe(1);
    expect(rec.feeds.filter((f) => f.includes('first-capture')).length).toBe(1);
    expect(rec.feeds.filter((f) => f.includes('second-capture')).length).toBe(1);
    expect(rec.resets).toBe(2);
    const firstIdx = rec.feeds.findIndex((f) => f.includes('first-capture'));
    const secondIdx = rec.feeds.findIndex((f) => f.includes('second-capture'));
    expect(secondIdx).toBeGreaterThan(firstIdx);
    // MID (enqueued during reseed 1) must feed before reseed 2's reset:
    // reseed 2 drains the pre-pause chain, so MID resolves into the
    // pre-capture twin and the second capture re-supplies its effect.
    const midIdx = rec.feeds.findIndex((f) => f === 'MID');
    expect(midIdx).toBeLessThan(secondIdx);
  } finally {
    stack.stop();
    inner.dispose();
    rmSync(runDir, { recursive: true, force: true });
  }
});

// ------------------------------------------------------- CLI surface tests
test('parse_args_extracts_all_stack_flags', () => {
  const a = parseArgs([
    '--session', 'abcd1234', '--run-dir', '/run/kb-term', '--harness', 'omp',
    '--cols', '100', '--rows', '30', '--bound-sid', 'sid-x', '--pane-id', '%3',
    '--socket', '/run/kb-term/s/abcd1234.sock', '--session-name', 'abcd1234',
  ]);
  expect(a.session).toBe('abcd1234');
  expect(a.runDir).toBe('/run/kb-term');
  expect(a.harness).toBe('omp');
  expect(a.cols).toBe(100);
  expect(a.rows).toBe(30);
  expect(a.boundSid).toBe('sid-x');
  expect(a.paneId).toBe('%3');
  expect(a.socket).toBe('/run/kb-term/s/abcd1234.sock');
  expect(a.sessionName).toBe('abcd1234');
  expect(a.version).toBeUndefined();
  expect(a.pins).toBeUndefined();
});

test('pins_report_true_build_pins_as_json_map', () => {
  const pins = JSON.parse(PINS_JSON) as Record<string, string>;
  expect(pins['@xterm/headless']).toBe('6.0.0');
  expect(pins['@xterm/addon-serialize']).toBe('0.14.0');
  expect(pins['@xterm/addon-unicode-graphemes']).toBe('0.4.0');
  expect(pins['@xterm/addon-unicode11']).toBe('0.9.0');
  // Sanity: wire.ts parsing is available; --pins is a JSON map consumed by
  // kb-term doctor's probe_tap_pins. Pins match tap/package.json dependencies.
  expect(decodeTextFrame('{"t":"ping"}').kind).toBe('known');
});

test('session_validation_rejects_path_traversal', () => {
  expect(SESSION_RE.test('abcd1234')).toBe(true);
  expect(SESSION_RE.test('a1b2c3d4e5f6')).toBe(true);
  expect(SESSION_RE.test('../evil')).toBe(false);
  expect(SESSION_RE.test('abc')).toBe(false);
  expect(SESSION_RE.test('..%2f..')).toBe(false);
  expect(SESSION_RE.test('ABC12345')).toBe(false);
  expect(SESSION_RE.test('ab cd1234')).toBe(false);
});

// ------------------------------------------------ compiled-binary smoke
test('compiled_binary_smoke_pins_version_placeholder_and_listen', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'kbt7-smoke-'));
  const bin = join(dir, 'kb-tap');
  const build = spawnSync('bun', ['build', '--compile', 'src/main.ts', '--outfile', bin], {
    cwd: process.cwd(),
    encoding: 'utf-8',
    timeout: 300_000,
  });
  try {
    expect(build.status).toBe(0);
    expect(existsSync(bin)).toBe(true);

    const bare = spawnSync(bin, [], { encoding: 'utf-8', timeout: 30_000 });
    expect(bare.status).toBe(0);
    expect((bare.stdout as string).trim()).toBe('kb-tap placeholder');

    const version = spawnSync(bin, ['--version'], { encoding: 'utf-8', timeout: 30_000 });
    expect(version.status).toBe(0);
    expect((version.stdout as string).startsWith('kb-tap ')).toBe(true);

    const pins = spawnSync(bin, ['--pins'], { encoding: 'utf-8', timeout: 30_000 });
    expect(pins.status).toBe(0);
    const parsed = JSON.parse((pins.stdout as string).trim()) as Record<string, string>;
    expect(parsed['@xterm/headless']).toBe('6.0.0');
    expect(parsed['@xterm/addon-serialize']).toBe('0.14.0');

    const badSession = spawnSync(bin, ['--session', '../evil', '--run-dir', dir], {
      encoding: 'utf-8',
      timeout: 30_000,
    });
    expect(badSession.status).toBe(2);

    // A real --session run must boot the tap and create tap.sock (this is
    // what CI's old bare-placeholder smoke never exercised). Control attach
    // needs a live fake tmux that answers the pause-after write's
    // %begin/%end block; then poll for the socket and kill the binary.
    const fakeTmux = join(dir, 'fake-tmux');
    writeFileSync(fakeTmux, '#!/bin/sh\nwhile IFS= read -r _line; do\n  printf "%%begin 0 1 0\\n%%end 0 1 0\\n"\ndone\n');
    chmodSync(fakeTmux, 0o755);
    const runDir = join(dir, 'run');
    const proc = Bun.spawn([bin, '--session', 'smoke0001', '--run-dir', runDir, '--harness', 'omp'], {
      env: { ...process.env, KB_TMUX_BIN: fakeTmux },
      stdout: 'ignore',
      stderr: 'ignore',
    });
    const sock = join(runDir, 's', 'smoke0001', 'tap.sock');
    let sawSock = false;
    const deadline = Date.now() + 15_000;
    while (Date.now() < deadline) {
      if (existsSync(sock)) {
        sawSock = true;
        break;
      }
      if (proc.exitCode !== null) break;
      await Bun.sleep(100);
    }
    proc.kill();
    expect(sawSock).toBe(true);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});
