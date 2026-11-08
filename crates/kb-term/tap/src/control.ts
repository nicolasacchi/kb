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

/** Pane ids are tmux's own `%N` tokens; anything else is rejected before it
 * can be spliced into a command line (injection via paneId). */
export const PANE_ID_RE = /^%\d+$/;

/** Exact wire form for the pause continue (pane QUOTED). Pinned by tests. */
export function continueCommand(pane: string): string {
  return `refresh-client -A '${pane}:continue'`;
}
/** Exact wire form for pausing a pane explicitly (pane QUOTED, same
 * convention as the continue). Used by the term-int pause test to trigger
 * the documented per-pane pause deterministically. */
export function pauseCommand(pane: string): string {
  return `refresh-client -A '${pane}:pause'`;
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
  private held = false;
  private resumeWait: (() => void) | null = null;
  /** Most recent read/dispatch failure. Recorded and surfaced here instead
   * of silently ending handling (one bad event must not kill the client). */
  lastError: Error | null = null;
  /** The exact reason the attach-time pause-after command failed, if it
   * did (a %error or exit). Kept so stall failures are diagnosable; null
   * means the command was accepted. */
  lastPauseAfterError: string | null = null;
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

  /** Spawn the control attach and write `refresh-client -f pause-after=20`
   * immediately. Control mode emits `%begin`/`%end` only for client-issued
   * commands, NOT for the attach itself, so there is no attach ack to wait
   * for: start() resolves when the pause-after Pending resolves (its own
   * `%end`; if tmux does emit an attach block it is consumed positionally by
   * that Pending, which resolves start() no later). Resolves once the
   * pause-after block has closed, or throws after a 10s deadline. */
  async start(): Promise<void> {
    const spawn = this.deps.spawn ?? defaultSpawn;
    this.proc = spawn(attachArgv(this.deps.tmuxBin, this.deps.socketPath, this.deps.sessionName));
    this.started = true;
    void this.readLoop(this.proc);
    const { promise: ackPromise, resolve: ack } = Promise.withResolvers<void>();
    const [pauseAck] = this.write([`refresh-client -f pause-after=${this.deps.pauseAfterSecs ?? PAUSE_AFTER_SECS}`]);
    // Resolved or rejected (`%error`): the line is on the wire either way.
    // A rejection is recorded (not swallowed silently) so a stall after a
    // failed pause-after is diagnosable; it surfaces through the RSS lane
    // if at all.
    pauseAck.then(() => ack(), (err: unknown) => {
      this.lastPauseAfterError = err instanceof Error ? err.message : String(err);
      ack();
    });
    let timer: ReturnType<typeof setTimeout> | null = null;
    const deadline = new Promise<never>((_resolve, reject) => {
      timer = setTimeout(() => reject(new Error('control attach timed out after 10s')), 10_000);
    });
    try {
      await Promise.race([ackPromise, deadline]);
    } finally {
      clearTimeout(timer);
    }
  }

  /** Hold/resume the read loop. The integration driver stalls the tap with
   * SIGSTOP now (a userspace hold does not stop Bun's stream from
   * prefetching the pipe, so tmux never saw the stall -- CI run
   * 42430279772); this remains a shutdown helper: stop() clears any hold
   * before killing the process. */
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

  /** Send one command through write() (so it owns its Pending and its
   * %begin/%end block is consumed correctly) but drop the promise: used for
   * pause-after and the pause continue, whose replies no caller awaits. */
  private writeFireAndForget(text: string): void {
    const [p] = this.write([text]);
    p.catch(() => {});
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
        for (const ev of this.dec.push(next.value)) {
          try {
            this.dispatch(ev);
          } catch (err) {
            // One bad event must not kill the client: record it and keep
            // handling instead of silently ending the loop.
            this.lastError = err instanceof Error ? err : new Error(String(err));
          }
        }
      }
    } catch (err) {
      this.lastError = err instanceof Error ? err : new Error(String(err));
    } finally {
      // tmux died mid-reseed: every blocked cmd/write promise must reject
      // instead of hanging its awaiter forever.
      this.failPending(new ControlError('control process exited', []));
    }
  }

  private failPending(err: Error): void {
    for (const p of this.pending) p.reject(err);
    for (const p of this.byNum.values()) p.reject(err);
    this.pending.length = 0;
    this.byNum.clear();
    this.currentNum = null;
  }

  private dispatch(ev: CtlEvent): void {
    switch (ev.kind) {
      case 'output':
        for (const h of this.outHandlers) h(ev.pane, ev.data);
        return;
      case 'extended-output':
        for (const h of this.outHandlers) h(ev.pane, ev.data, ev.ageMs);
        return;
      case 'pause': {
        // continue FIRST (quoted), then notify: a reseed triggered by the
        // callback must write its commands after the continue. A pane token
        // that is not tmux's `%N` form is never spliced into a command line.
        if (PANE_ID_RE.test(ev.pane)) this.writeFireAndForget(continueCommand(ev.pane));
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
