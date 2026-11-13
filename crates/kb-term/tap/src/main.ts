// kb-tap entrypoint (T7): `--session <id> --run-dir <dir> --harness <h>
// --cols <n> --rows <n> --bound-sid <sid> --pane-id <%N>`.
//
// Production wiring (integration audit F1/F7/F8): the control-mode client
// feeds per-pane QueryStrippers, which feed the GraphicsExtractor, which
// feeds TapServer (ring + twin + fan-out). The runner's launch gate waits
// for exactly this control client (`wait_for_control_client`); without this
// wiring no client ever appears and every launch times out. A single
// ControlDecoder lives inside Control (decoded onOutput bytes) — the raw-line
// TapPipeline facade is NOT wired here, it would double-decode.
// Reseed: onPause (and onExit with a too-far-behind reason) pauses the
// TapServer's twin feed, runs the Reseeder against the same twin, then
// resumes (viewers.ts setTwinPaused — audit F4).
//
// `--version` prints `kb-tap <pkg version>`; `--pins` prints the tap's
// pinned @xterm dependency set as a JSON map for kb-term doctor's probe.
// Without --session (and without an info flag) it prints the placeholder
// line (the term-tap lane's compile smoke expects exactly that output from
// a bare run).
import { randomUUID } from 'node:crypto';
import { join as pathJoin } from 'node:path';
import { Control, exitNeedsReseed, PANE_ID_RE, type SpawnFn } from './control.ts';
import { GraphicsExtractor } from './graphics.ts';
import { makeTwin, type Twin } from './twin.ts';
import { TapServer } from './viewers.ts';
import { encodeServerFrame } from './wire.ts';
import { Reseeder } from './reseed.ts';
import { QueryStripper } from './strip.ts';
import { ImageStore } from './imagestore.ts';

export const HARNESSES = ['claude', 'codex', 'opencode', 'grok', 'kimi', 'omp'] as const;

/** tap/package.json "version"/"dependencies" pins, mirrored as constants so
 * the compiled binary needs no package.json at runtime. */
export const TAP_VERSION = '0.0.0';

/** The pinned xterm set the tap is built against (tap/package.json
 * `dependencies`). kb-term doctor's PINS additionally names viewer-side
 * packages (@xterm/xterm, addon-image, addon-webgl) this binary does not
 * link; reconciling that superset is the doctor owner's call (PR body). */
export const PINS: Record<string, string> = {
  '@xterm/headless': '6.0.0',
  '@xterm/addon-serialize': '0.14.0',
  '@xterm/addon-unicode-graphemes': '0.4.0',
  '@xterm/addon-unicode11': '0.9.0',
};

/** Full build pins as `--pins` prints them (doctor's parse_pins object map). */
export const PINS_JSON = JSON.stringify(PINS);

/** Session ids are spliced into a socket path; the runner mints 8..12
 * lowercase-alphanumeric ids (Rust `is_valid_term_id`). Reject anything
 * else before it can traverse out of s/<id>/ (audit F12). */
export const SESSION_RE = /^[a-z0-9]{8,12}$/;

export interface TapArgs {
  session?: string;
  runDir?: string;
  harness?: string;
  cols?: number;
  rows?: number;
  boundSid?: string;
  paneId?: string;
  socket?: string;
  sessionName?: string;
  storeDir?: string;
  version?: boolean;
  pins?: boolean;
}

export function parseArgs(argv: string[]): TapArgs {
  const out: TapArgs = {};
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a === '--version') out.version = true;
    else if (a === '--pins') out.pins = true;
    else if (a === '--session' && argv[i + 1] !== undefined) out.session = argv[++i];
    else if (a === '--run-dir' && argv[i + 1] !== undefined) out.runDir = argv[++i];
    else if (a === '--harness' && argv[i + 1] !== undefined) out.harness = argv[++i];
    else if (a === '--cols' && argv[i + 1] !== undefined) out.cols = Number(argv[++i]);
    else if (a === '--rows' && argv[i + 1] !== undefined) out.rows = Number(argv[++i]);
    else if (a === '--bound-sid' && argv[i + 1] !== undefined) out.boundSid = argv[++i];
    else if (a === '--pane-id' && argv[i + 1] !== undefined) out.paneId = argv[++i];
    else if (a === '--socket' && argv[i + 1] !== undefined) out.socket = argv[++i];
    else if (a === '--session-name' && argv[i + 1] !== undefined) out.sessionName = argv[++i];
    else if (a === '--store-dir' && argv[i + 1] !== undefined) out.storeDir = argv[++i];
  }
  return out;
}

function usage(): string {
  return `usage: kb-tap --session <id> --run-dir <dir> --harness <${HARNESSES.join('|')}> [--cols N --rows N --bound-sid SID --pane-id %N]`;
}

/** Grid must be xterm-sized and bounded (the runner supplies the pane's
 * real grid; anything else would desync twin/viewer cols). */
export interface TapStackOpts {
  session: string;
  runDir: string;
  harness: string;
  cols: number;
  rows: number;
  boundSid?: string;
  paneId?: string;
  /** tmux server socket (default `<runDir>/s/<session>.sock`). */
  socket?: string;
  /** tmux session name to attach (default the session id). */
  sessionName?: string;
  /** ImageStore dir (default `<runDir>/s/<session>/store`). */
  storeDir?: string;
  epoch: string;
  /** Test seam: inject the control spawn (fake tmux). */
  spawn?: SpawnFn;
  twin?: Twin;
  /** Test seam: join timing forwarded to TapServer. */
  quiesceMs?: number;
  joinWaitMs?: number;
}

export interface TapStack {
  server: TapServer;
  control: Control;
  twin: Twin;
  reseeder: Reseeder;
  gfx: GraphicsExtractor;
  start(): Promise<void>;
  /** Apply a grid change: twin resize + resize marker (audit F6). */
  setGrid(cols: number, rows: number): void;
  stop(): void;
}

/**
 * Build and wire the whole production chain without starting it, so tests
 * can drive control-mode lines in and viewer bytes out.
 */
export function createTapStack(opts: TapStackOpts): TapStack {
  const twin = opts.twin ?? makeTwin(opts.cols, opts.rows);
  const store = new ImageStore({
    sessionDir: opts.storeDir ?? pathJoin(opts.runDir, 's', opts.session, 'store'),
  });
  const gfx = new GraphicsExtractor(store);
  const strips = new Map<string, QueryStripper>();
  const stripPending = (): number => {
    let n = 0;
    for (const s of strips.values()) n += s.pending();
    return n;
  };

  const server = new TapServer({
    runDir: opts.runDir,
    session: opts.session,
    harness: opts.harness,
    boundSid: opts.boundSid,
    epoch: opts.epoch,
    cols: opts.cols,
    rows: opts.rows,
    twin,
    stripPending,
    quiesceMs: opts.quiesceMs,
    joinWaitMs: opts.joinWaitMs,
  });

  const control = new Control({
    tmuxBin: process.env.KB_TMUX_BIN ?? 'tmux',
    socketPath: opts.socket ?? pathJoin(opts.runDir, 's', `${opts.session}.sock`),
    sessionName: opts.sessionName ?? opts.session,
    spawn: opts.spawn,
    paneId: opts.paneId,
  });
  const reseeder = new Reseeder(control, server.reseedTwin(), { feedQueued: false });
  // feedQueued false: the Reseeder's post-B queue exists only for the
  // quiescence/A/B bookkeeping. The TapServer chain is the single twin
  // feeder and feeds those bytes post-withhold after the pause ends, so
  // the Reseeder must not feed the queued raw bytes too (that would
  // double-feed the twin and reintroduce withheld bytes).

  const feedStrip = (pane: string, raw: Uint8Array): void => {
    let s = strips.get(pane);
    if (!s) {
      s = new QueryStripper();
      strips.set(pane, s);
    }
    const stripped = s.push(raw);
    if (stripped.length === 0) return;
    server.feed(gfx.push(stripped));
  };

  // Control already decoded %output lines (octal unescape + dedupe the raw
  // control stream); the pane's bytes go through that pane's stripper only.
  control.onOutput((pane, bytes) => feedStrip(pane, bytes));

  let lastPane: string | undefined = opts.paneId;
  control.onOutput((pane) => {
    lastPane = pane;
  });

  // Serialized: a pause/exit during a slow reseed queues behind it
  // instead of overwriting Reseeder.active or racing twin.reset().
  let reseedChain: Promise<void> = Promise.resolve();
  const reseed = (pane: string | undefined): void => {
    if (pane === undefined || !PANE_ID_RE.test(pane)) return;
    reseedChain = reseedChain.then(async () => {
      // Single twin-write owner: pause the TapServer's feed chain, drain
      // the pre-pause items before the reseed's twin.reset(), and feed
      // the capture through the ground-tracking reseedTwin facade.
      server.setTwinPaused(true);
      try {
        await server.drainTwinQueue();
        await reseeder.reseed(pane);
      } catch (err: unknown) {
        console.error(`kb-tap: reseed failed: ${err instanceof Error ? err.message : String(err)}`);
      } finally {
        server.setTwinPaused(false);
      }
    });
  };
  control.onPause((pane) => reseed(pane));
  control.onExit((reason) => {
    server.recordMarker((seq, offset) => encodeServerFrame({ t: 'exit', seq, code: null, offset }));
    if (exitNeedsReseed(reason)) reseed(lastPane);
  });

  return {
    server,
    control,
    twin,
    reseeder,
    gfx,
    setGrid(cols: number, rows: number): void {
      server.setGrid(cols, rows);
    },
    async start(): Promise<void> {
      await server.listen();
      try {
        await control.start();
      } catch (err) {
        await server.close();
        throw err;
      }
    },
    stop(): void {
      for (const [pane, s] of strips) {
        const tail = s.flush();
        if (tail.length > 0) feedStrip(pane, tail);
      }
      control.stop();
      void server.close();
    },
  };
}

const args = parseArgs(process.argv.slice(2));

if (args.version === true) {
  console.log(`kb-tap ${TAP_VERSION}`);
} else if (args.pins === true) {
  console.log(PINS_JSON);
} else if (args.harness !== undefined && !(HARNESSES as readonly string[]).includes(args.harness)) {
  // Reject an unknown harness up front: it would otherwise be emitted in
  // hello frames that both decoders refuse (a closed set in Rust and wire.ts).
  console.error(usage());
  process.exit(2);
} else if (args.session === undefined) {
  console.log('kb-tap placeholder');
} else if (!SESSION_RE.test(args.session)) {
  console.error(usage());
  process.exit(2);
} else if ((args.cols !== undefined && !(Number.isInteger(args.cols) && args.cols >= 1 && args.cols <= 1000)) ||
           (args.rows !== undefined && !(Number.isInteger(args.rows) && args.rows >= 1 && args.rows <= 1000))) {
  console.error(usage());
  process.exit(2);
} else if (args.paneId !== undefined && !PANE_ID_RE.test(args.paneId)) {
  console.error(usage());
  process.exit(2);
} else {
  const stack = createTapStack({
    session: args.session,
    runDir: args.runDir ?? process.env.KB_TERM_RUN_DIR ?? '/run/kb-term',
    harness: args.harness ?? 'omp',
    cols: args.cols ?? 120,
    rows: args.rows ?? 40,
    boundSid: args.boundSid,
    paneId: args.paneId,
    socket: args.socket,
    sessionName: args.sessionName,
    storeDir: args.storeDir,
    epoch: randomUUID().replaceAll('-', ''),
  });
  await stack.start();
  const shutdown = () => {
    stack.stop();
    process.exit(0);
  };
  process.on('SIGTERM', shutdown);
  process.on('SIGINT', shutdown);
}
