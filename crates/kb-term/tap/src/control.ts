// kb-term tap: tmux control-mode client. Zero workspace dependencies.
//
// Spawns `tmux -C attach -f ignore-size` (never PATH tmux: the binary comes
// from config/KB_TMUX_BIN) and decodes tmux control mode with T3's
// ControlDecoder; this module adds connection management on top:
//
//  * right after the attach acknowledgement, `refresh-client -f pause-after=20`
//    (PAUSE_AFTER_SECS) bounds the backlog a stalled tap can make tmux hold;
//  * command correlation: each command line written gets one
//    `%begin <time> <num> <flags>` ... `%end|%error <time> <num> <flags>`
//    block; lines in the block are raw replies (split on \n only, by the
//    decoder); `cmd` rejects with the %error text; `write` sends several
//    commands in ONE stdin write and returns their promises (reseed needs
//    the A/capture/B batch to be written atomically);
//  * %pause: `refresh-client -A '%P:continue'` is written immediately (pane
//    argument QUOTED -- the unquoted form fails with a %error parse) BEFORE
//    the onPause callback runs, so a reseed triggered by onPause always
//    writes its A/B commands after the continue.
//
// The tap-process wiring (reseeding on %exit too far behind / restart / new
// client) lives in T7; this module only exposes the events.

import { ControlDecoder, type CtlEvent } from './decode.ts';

/** Stall policy: tmux drops output paused longer than this. */
export const PAUSE_AFTER_SECS = 20;

/** Exact wire form for the pause continue (pane QUOTED). Pinned by tests. */
export function continueCommand(pane: string): string {
  return `refresh-client -A '${pane}:continue'`;
}

/** Exact argv for the control attach. Production never influences size. */
export function attachArgv(tmuxBin: string, socketPath: string, sessionName: string): string[] {
  return [tmuxBin, '-S', socketPath, '-C', 'attach', '-f', 'ignore-size', '-t', sessionName];
}

/** What a spawned process must provide; Bun.Subprocess satisfies this. */
export interface SpawnedProcess {
  stdin: { write(data: string): unknown };
  stdout: AsyncIterable<Uint8Array>;
  kill(): void;
}

export type SpawnFn = (argv: string[]) => SpawnedProcess;

const textDecoder = new TextDecoder('utf-8');

interface BunProc {
  stdin: { write(d: string): unknown };
  stdout: AsyncIterable<Uint8Array>;
  kill(): void;
}

interface BunHost {
  Bun: { spawn(a: string[], o: unknown): BunProc };
}

function defaultSpawn(argv: string[]): SpawnedProcess {
  const host = globalThis as unknown as BunHost;
  const proc = host.Bun.spawn(argv, { stdin: 'pipe', stdout: 'pipe', stderr: 'ignore' });
  return {
    stdin: { write: (d: string) => proc.stdin.write(d) },
    stdout: proc.stdout,
    kill: () => proc.kill(),
  };
}

export type OutputHandler = (pane: string, bytes: Uint8Array, ageMs?: number) => void;
export type PaneHandler = (pane: string) => void;
export type ExitHandler = (reason: string) => void;

interface Pending {
  replies: string[];
  resolve: (lines: string[]) => void;
  reject: (err: Error) => void;
}

export class ControlError extends Error {
  readonly lines: string[];
  constructor(text: string, lines: string[]) {
    super(text.length > 0 ? text : 'tmux control command failed');
    this.name = 'ControlError';
    this.lines = lines;
  }
}

export interface ControlDeps {
  tmuxBin: string;
  socketPath: string;
  sessionName: string;
  spawn?: SpawnFn;
  /** Override for the pause-after seconds sent on attach (default 20). */
  pauseAfterSecs?: number;
}

export class Control {
  private readonly deps: ControlDeps;
  private readonly dec = new ControlDecoder();
  private proc: SpawnedProcess | null = null;
  private readonly pending: Pending[] = [];
  private readonly byNum = new Map<string, Pending>();
  private currentNum: string | null = null;
  private readonly outHandlers: OutputHandler[] = [];
  private readonly pauseHandlers: PaneHandler[] = [];
  private readonly continueHandlers: PaneHandler[] = [];
  private readonly exitHandlers: ExitHandler[] = [];
  private started = false;
  private startAck: (() => void) | null = null;
  private held = false;
  private resumeWait: (() => void) | null = null;

  constructor(deps: ControlDeps) {
    this.deps = deps;
  }

  onOutput(cb: OutputHandler): void {
    this.outHandlers.push(cb);
  }
  onPause(cb: PaneHandler): void {
    this.pauseHandlers.push(cb);
  }
  onContinue(cb: PaneHandler): void {
    this.continueHandlers.push(cb);
  }
  onExit(cb: ExitHandler): void {
    this.exitHandlers.push(cb);
  }

  /** Spawn the control attach and send `refresh-client -f pause-after=20`
   * right after the first attach acknowledgement. Resolves once the
   * pause-after line has been written. */
  async start(): Promise<void> {
    const spawn = this.deps.spawn ?? defaultSpawn;
    this.proc = spawn(attachArgv(this.deps.tmuxBin, this.deps.socketPath, this.deps.sessionName));
    this.started = true;
    void this.readLoop(this.proc);
    // first decoder event of any kind implies the attach succeeded.
    const { promise: ackPromise, resolve: ack } = Promise.withResolvers<void>();
    this.startAck = ack;
    let timer: ReturnType<typeof setTimeout> | null = null;
    const deadline = new Promise<never>((_resolve, reject) => {
      timer = setTimeout(() => reject(new Error('control attach timed out after 10s')), 10_000);
    });
    try {
      await Promise.race([ackPromise, deadline]);
    } finally {
      clearTimeout(timer);
    }
    this.writeRaw(`refresh-client -f pause-after=${this.deps.pauseAfterSecs ?? PAUSE_AFTER_SECS}`);
  }

  /** Hold/resume the read loop. While held, unread output accumulates in
   * tmux's client buffer so tmux pauses it after pause-after seconds; this
   * is what term_int_reseed_after_long_pause uses to stall the tap. */
  setHeld(held: boolean): void {
    this.held = held;
    if (!held && this.resumeWait) {
      const r = this.resumeWait;
      this.resumeWait = null;
      r();
    }
  }

  cmd(line: string): Promise<string[]> {
    return this.write([line])[0];
  }

  /** Send several commands in ONE stdin write; returns one promise per line,
   * in order. Each promise resolves with the block's raw reply lines
   * (trailing \r stripped) or rejects with the %error text. */
  write(lines: string[]): Promise<string[]>[] {
    const promises: Promise<string[]>[] = [];
    for (const _line of lines) {
      const { promise, resolve, reject } = Promise.withResolvers<string[]>();
      this.pending.push({ replies: [], resolve, reject });
      promises.push(promise);
    }
    this.writeRaw(lines.join('\n'));
    return promises;
  }

  stop(): void {
    this.setHeld(false);
    this.proc?.kill();
    this.proc = null;
  }

  private writeRaw(text: string): void {
    if (!this.proc) throw new Error('control client not started');
    this.proc.stdin.write(text.endsWith('\n') ? text : `${text}\n`);
  }

  private async readLoop(proc: SpawnedProcess): Promise<void> {
    try {
      const it = proc.stdout[Symbol.asyncIterator]();
      for (;;) {
        if (this.held) {
          const { promise, resolve } = Promise.withResolvers<void>();
          this.resumeWait = resolve;
          await promise;
          if (!this.started) return;
        }
        const next = await it.next();
        if (next.done) break;
        for (const ev of this.dec.push(next.value)) this.dispatch(ev);
      }
    } catch {
      // stdout closed (tmux exit) -- stop handling silently.
    }
  }

  private dispatch(ev: CtlEvent): void {
    if (this.startAck) {
      const ack = this.startAck;
      this.startAck = null;
      ack();
    }
    switch (ev.kind) {
      case 'output':
        for (const h of this.outHandlers) h(ev.pane, ev.data);
        return;
      case 'extended-output':
        for (const h of this.outHandlers) h(ev.pane, ev.data, ev.ageMs);
        return;
      case 'pause': {
        // continue FIRST (quoted), then notify: a reseed triggered by the
        // callback must write its commands after the continue.
        this.writeRaw(continueCommand(ev.pane));
        for (const h of this.pauseHandlers) h(ev.pane);
        return;
      }
      case 'continue':
        for (const h of this.continueHandlers) h(ev.pane);
        return;
      case 'exit':
        for (const h of this.exitHandlers) h(ev.reason);
        return;
      case 'begin': {
        const pend = this.pending.shift();
        this.currentNum = ev.num;
        if (pend) this.byNum.set(ev.num, pend);
        return;
      }
      case 'end': {
        const pend = this.byNum.get(ev.num);
        this.byNum.delete(ev.num);
        if (this.currentNum === ev.num) this.currentNum = null;
        pend?.resolve(pend.replies);
        return;
      }
      case 'error': {
        const pend = this.byNum.get(ev.num);
        this.byNum.delete(ev.num);
        if (this.currentNum === ev.num) this.currentNum = null;
        if (pend) {
          const text = pend.replies.join('\n') || ev.flags;
          pend.reject(new ControlError(text, pend.replies));
        }
        return;
      }
      case 'reply': {
        // raw reply line for the currently open block (tmux serializes blocks).
        const pend = this.currentNum ? this.byNum.get(this.currentNum) : undefined;
        if (pend) pend.replies.push(textDecoder.decode(ev.line).replace(/\r$/, ''));
        return;
      }
      default:
        return;
    }
  }
}

/** Whether tmux's disconnect reason means output was lost and a reseed is
 * required. Reseed wiring (T7) calls this from onExit. */
export function exitNeedsReseed(reason: string): boolean {
  return reason.includes('too far behind');
}
