// Integration driver for the launch-grid term-int test in
// tests/term_int_reseed.rs. Boots the production tap stack against a REAL
// tmux server exactly as main.ts does (start, then syncGrid), joins one
// viewer at once and prints one JSON object as the LAST stdout line:
// {ok, profileCols, profileRows, twinCols, twinRows}.
// Usage: bun run launchgrid-driver.ts --socket <sock> --session-name <name>
// --run-dir <dir> --tmux <bin> [--cols 120 --rows 40]

import { connect } from 'node:net';
import { createTapStack } from '../../src/main.ts';
import { EnvelopeDecoder, encodeControlMessage, encodeEnvelope, ENV_CONTROL, ENV_TEXT, parseS2C } from '../../src/wire.ts';

const argv = Bun.argv.slice(2);
function arg(name: string): string {
  const i = argv.indexOf(`--${name}`);
  const v = i >= 0 ? argv[i + 1] : undefined;
  if (v === undefined) throw new Error(`missing --${name}`);
  return v;
}
const proc = (globalThis as unknown as { process: { env: Record<string, string | undefined>; exit(c: number): never } }).process;
proc.env.KB_TMUX_BIN = arg('tmux');

const stack = createTapStack({
  session: 'lgrid0001',
  runDir: arg('run-dir'),
  harness: 'omp',
  // The tap's own default: the real size must come from the attach-time query.
  cols: Number(argv.includes('--cols') ? arg('cols') : '120'),
  rows: Number(argv.includes('--rows') ? arg('rows') : '40'),
  socket: arg('socket'),
  sessionName: arg('session-name'),
  epoch: 'b'.repeat(32),
});

async function profileOf(sockPath: string): Promise<{ cols: number; rows: number }> {
  return await new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error('no profile frame within 10s')), 10_000);
    const socket = connect(sockPath);
    const dec = new EnvelopeDecoder();
    socket.on('error', reject);
    socket.on('connect', () => {
      socket.write(encodeEnvelope(ENV_CONTROL, new TextEncoder().encode(encodeControlMessage({ t: 'viewer-open', viewer: 'v1', profile: 'desktop', resume: null }))));
    });
    socket.on('data', (chunk: Buffer) => {
      for (const env of dec.push(new Uint8Array(chunk))) {
        if (env.kind !== ENV_TEXT) continue;
        const text = Buffer.from(parseS2C(env.body).frame).toString('utf-8');
        if (!text.includes('"t":"profile"')) continue;
        const p = JSON.parse(text) as { cols: number; rows: number };
        clearTimeout(timer);
        socket.destroy();
        resolve({ cols: p.cols, rows: p.rows });
      }
    });
  });
}

try {
  await stack.start();
  await stack.syncGrid();
  const profile = await profileOf(stack.server.sockPath);
  console.log(JSON.stringify({ ok: true, profileCols: profile.cols, profileRows: profile.rows, twinCols: stack.twin.cols, twinRows: stack.twin.rows }));
  stack.stop();
  proc.exit(0);
} catch (err) {
  console.log(JSON.stringify({ ok: false, error: String(err) }));
  stack.stop();
  proc.exit(1);
}
