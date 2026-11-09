// kb-tap entrypoint (T7/T12): `--session <id> --run-dir <dir> --harness <h>`.
//
// Boot writes a fresh epoch (offsets and marker seqs start at 0 per epoch);
// a restarted tap is a new epoch with an empty ring and a new twin, so every
// reconnecting viewer is silently downgraded to a full join. Without
// --session it prints the placeholder line (the term-tap lane's compile
// smoke expects exactly that output from a bare run).
//
// The pane bytes arrive through ONE tmux control-mode attach
// (`tmux -C attach`): the spawn's stdout is tee'd through the query
// stripper into the graphics extractor and the multi-viewer server,
// while Control alone owns the attach protocol (pause-after, the
// per-pane continue, and the command blocks). The runner's
// `wait_for_control_client` only sees a control-mode client once this
// link exists, so it is the boot proof: no attach, no launch.
import { randomUUID } from 'node:crypto';
import { join as pathJoin } from 'node:path';
import { Control, type SpawnedProcess } from './control.ts';
import { GraphicsExtractor } from './graphics.ts';
import { ImageStore } from './imagestore.ts';
import { TapPipeline } from './pipeline.ts';
import { makeTwin } from './twin.ts';
import { TapServer } from './viewers.ts';

const HARNESSES = ['claude', 'codex', 'opencode', 'grok', 'kimi', 'omp'] as const;

// The xterm pins the tap is built against; `kb-tap --pins` reports them
// so `kb-termd doctor` can detect a build drift.
const PINS: Record<string, string> = {
  '@xterm/xterm': '6.0.0',
  '@xterm/headless': '6.0.0',
  '@xterm/addon-serialize': '0.14.0',
  '@xterm/addon-unicode-graphemes': '0.4.0',
  '@xterm/addon-image': '0.9.0',
  '@xterm/addon-webgl': '0.19.0',
};

function parseArgs(argv: string[]): { session?: string; runDir?: string; harness?: string } {
  const out: { session?: string; runDir?: string; harness?: string } = {};
  for (let i = 0; i < argv.length; i++) {
    if (argv[i] === '--session' && argv[i + 1] !== undefined) out.session = argv[++i];
    else if (argv[i] === '--run-dir' && argv[i + 1] !== undefined) out.runDir = argv[++i];
    else if (argv[i] === '--harness' && argv[i + 1] !== undefined) out.harness = argv[++i];
  }
  return out;
}

const argv = process.argv.slice(2);
const args = parseArgs(argv);
if (argv.includes('--version')) {
  console.log('kb-tap 0.0.0');
  process.exit(0);
}
if (argv.includes('--pins')) {
  console.log(JSON.stringify(PINS));
  process.exit(0);
}
// Reject an unknown harness up front: it would otherwise be emitted in hello
// frames that both decoders refuse (a closed set in Rust and in wire.ts).
if (args.harness !== undefined && !(HARNESSES as readonly string[]).includes(args.harness)) {
  console.error(`usage: kb-tap --session <id> --run-dir <dir> --harness <${HARNESSES.join('|')}>`);
  process.exit(2);
}
if (args.session === undefined) {
  console.log('kb-tap placeholder');
} else {
  const runDir = args.runDir ?? process.env.KB_TERM_RUN_DIR ?? '/run/kb-term';
  const sessionDir = pathJoin(runDir, 's', args.session);
  const epoch = randomUUID().replaceAll('-', '');
  const store = new ImageStore({ sessionDir });
  const gfx = new GraphicsExtractor(store);
  const pipeline = new TapPipeline();
  const server = new TapServer({
    session: args.session,
    harness: args.harness ?? 'omp',
    epoch,
    runDir,
    twin: makeTwin(120, 40),
  });
  await server.listen();

  // The tap's only input path: one control-mode attach. Its stdout is
  // tee'd through the query stripper into the graphics extractor and
  // the server; a bad chunk must not kill the feed that Control is
  // decoding on the same stream.
  const spawnAttach = (a: string[]): SpawnedProcess => {
    const host = globalThis as unknown as { Bun: { spawn(argv: string[], opts: unknown): unknown } };
    const proc = host.Bun.spawn(a, { stdin: 'pipe', stdout: 'pipe', stderr: 'ignore' }) as unknown as SpawnedProcess;
    const stdout: AsyncIterable<Uint8Array> = {
      [Symbol.asyncIterator]() {
        const it = (proc.stdout as AsyncIterable<Uint8Array>)[Symbol.asyncIterator]();
        return {
          async next(): Promise<IteratorResult<Uint8Array>> {
            const r = await it.next();
            if (!r.done && r.value !== undefined) {
              try {
                for (const o of pipeline.feed(r.value)) {
                  if ('pane' in o) server.feed(gfx.push(o.bytes));
                }
              } catch {
                // One bad chunk is not a stream error: Control still
                // owns the attach, so the tap stays attached.
              }
            }
            return r;
          },
          return(): Promise<IteratorResult<Uint8Array>> {
            return Promise.resolve({ value: undefined, done: true });
          },
        };
      },
    };
    return { stdin: proc.stdin, stdout, kill: () => proc.kill() };
  };

  const control = new Control({
    tmuxBin: process.env.KB_TMUX_BIN ?? 'tmux',
    socketPath: pathJoin(runDir, 's', `${args.session}.sock`),
    sessionName: 'kb',
    spawn: spawnAttach,
  });
  control.onExit((reason) => {
    console.error(`tap: pane exited: ${reason}`);
  });
  await control.start();

  const shutdown = () => {
    control.stop();
    server
      .close()
      .then(() => process.exit(0))
      .catch(() => process.exit(0));
  };
  process.on('SIGTERM', shutdown);
  process.on('SIGINT', shutdown);
}
