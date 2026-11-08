// kb-tap entrypoint (T7): `--session <id> --run-dir <dir> --harness <h>`.
// Boot writes a fresh epoch (offsets and marker seqs start at 0 per epoch);
// a restarted tap is a new epoch with an empty ring and a new twin, so every
// reconnecting viewer is silently downgraded to a full join. Without
// --session it prints the placeholder line (the term-tap lane's compile
// smoke expects exactly that output from a bare run).
import { randomUUID } from 'node:crypto';
import { makeTwin } from './twin.ts';
import { TapServer } from './viewers.ts';

const HARNESSES = ['claude', 'codex', 'opencode', 'grok', 'kimi', 'omp'] as const;

function parseArgs(argv: string[]): { session?: string; runDir?: string; harness?: string } {
  const out: { session?: string; runDir?: string; harness?: string } = {};
  for (let i = 0; i < argv.length; i++) {
    if (argv[i] === '--session' && argv[i + 1] !== undefined) out.session = argv[++i];
    else if (argv[i] === '--run-dir' && argv[i + 1] !== undefined) out.runDir = argv[++i];
    else if (argv[i] === '--harness' && argv[i + 1] !== undefined) out.harness = argv[++i];
  }
  return out;
}

const args = parseArgs(process.argv.slice(2));
// Reject an unknown harness up front: it would otherwise be emitted in hello
// frames that both decoders refuse (a closed set in Rust and in wire.ts).
if (args.harness !== undefined && !(HARNESSES as readonly string[]).includes(args.harness)) {
  console.error(`usage: kb-tap --session <id> --run-dir <dir> --harness <${HARNESSES.join('|')}>`);
  process.exit(2);
}
if (args.session === undefined) {
  console.log('kb-tap placeholder');
} else {
  const epoch = randomUUID().replaceAll('-', '');
  const server = new TapServer({
    session: args.session,
    harness: args.harness ?? 'omp',
    epoch,
    runDir: args.runDir ?? process.env.KB_TERM_RUN_DIR,
    twin: makeTwin(120, 40),
  });
  await server.listen();
  const shutdown = () => {
    server.close().then(() => process.exit(0));
  };
  process.on('SIGTERM', shutdown);
  process.on('SIGINT', shutdown);
}
