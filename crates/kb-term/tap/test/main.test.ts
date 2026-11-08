// main.ts production wiring seam tests (integration audit F1/F7/F11): the
// whole chain control-%output -> QueryStripper -> GraphicsExtractor ->
// withhold -> ring -> twin -> viewer, driven through the real createTapStack
// with a fake control spawn (no tmux) and a real unix-socket viewer. Plus the
// --pins/--version/validation surface and the compiled-binary smoke.
import { test, expect } from 'bun:test';
import { spawnSync } from 'node:child_process';
import { PINS_JSON, SESSION_RE, createTapStack, parseArgs } from '../src/main.ts';
import { connect, type Socket } from 'node:net';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { makePng } from './helpers-graphics.ts';
import type { SpawnedProcess } from '../src/control.ts';
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
    const image = `q=1,f=32,i=7;${b64}`;
    const lines =
      octal('%0', '\x1b[6nhello') +
      octal('%0', `\x1b_G${image}\x1b\\`) +
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
test('compiled_binary_smoke_pins_version_placeholder_and_listen', () => {
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
    // will fail without tmux, so poll for the socket then kill.
    const runDir = join(dir, 'run');
    const proc = Bun.spawn([bin, '--session', 'smoke0001', '--run-dir', runDir, '--harness', 'omp'], {
      env: { ...process.env, KB_TMUX_BIN: '/bin/true' },
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
      Bun.sleepSync(50);
    }
    proc.kill();
    expect(sawSock).toBe(true);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});
