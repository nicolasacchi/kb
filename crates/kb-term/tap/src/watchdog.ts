// kb-term tap: stall watchdogs. Pure, injectable clock/reader, callbacks
// only, no logging framework. Process wiring lives in T7.
//
//  * AGE_WARN_MS: warn when a %extended-output age exceeds it;
//  * LOOP_LAG_WARN_MS: the setInterval drift probe. The loop measures the
//    clock drift between expected and actual ticks; over the threshold it
//    warns (event-loop starvation shows up here before it shows up as a
//    stalled control read);
//  * TMUX_RSS_LIMIT_BYTES: tmux server RSS backstop (pid from
//    `tmux -S sock display-message -p '#{pid}'`, VmRSS from
//    /proc/<pid>/status). Over the limit it fires onRetap ONCE until the
//    RSS recovers -- a backstop above the pause-bounded worst case
//    (~20 s x 10.5 MB/s = 210 MB), never the stall policy itself.

import { readFileSync } from 'node:fs';

export const AGE_WARN_MS = 3000;
export const LOOP_LAG_WARN_MS = 3000;
export const TMUX_RSS_LIMIT_BYTES = 512 * 1024 * 1024;

export interface WatchdogHooks {
  onWarn?(what: string, valueMs: number): void;
  onRetap?(): void;
  /** Current tmux server pid (from `display-message -p '#{pid}'`). */
  getPid?(): number | undefined;
  /** RSS in bytes for that pid (default: VmRSS from /proc/<pid>/status). */
  readRssBytes?(pid: number): number | undefined;
  now?(): number;
}

export function readProcRssBytes(pid: number): number | undefined {
  try {
    const status = readFileSync(`/proc/${pid}/status`, 'utf-8');
    const line = status.split('\n').find((l) => l.startsWith('VmRSS:'));
    if (!line) return undefined;
    const kb = Number(line.split(/\s+/)[1]);
    return Number.isFinite(kb) ? kb * 1024 : undefined;
  } catch {
    return undefined;
  }
}

export class Watchdog {
  private readonly hooks: WatchdogHooks;
  private ageWarned = false;
  private lagWarned = false;
  private retapFired = false;
  private timer: ReturnType<typeof setInterval> | null = null;
  private tickCount = 0;
  private lastTickAt = 0;

  constructor(hooks: WatchdogHooks) {
    this.hooks = hooks;
  }

  private now(): number {
    return this.hooks.now ? this.hooks.now() : Date.now();
  }

  /** Feed the age of one %extended-output (milliseconds). Warns on the
   * crossing only; a fresh crossing needs a recovery first. */
  noteAge(ageMs: number): void {
    if (ageMs > AGE_WARN_MS) {
      if (!this.ageWarned) {
        this.ageWarned = true;
        this.hooks.onWarn?.('extended-output-age', ageMs);
      }
    } else {
      this.ageWarned = false;
    }
  }

  /** Start the drift probe: fires onWarn('loop-lag', driftMs) when the
   * actual tick lands more than LOOP_LAG_WARN_MS behind schedule. Ticks
   * every `intervalMs` (default 1000). */
  start(intervalMs = 1000): void {
    this.stop();
    this.tickCount = 0;
    this.lastTickAt = this.now();
    this.timer = setInterval(() => this.tick(intervalMs), intervalMs);
  }

  stop(): void {
    if (this.timer !== null) {
      clearInterval(this.timer);
      this.timer = null;
    }
  }

  /** One probe step. Public so tests can drive it with an injected clock. */
  tick(intervalMs = 1000): void {
    const t = this.now();
    this.tickCount++;
    const expected = this.lastTickAt + intervalMs;
    this.lastTickAt = t;
    const drift = t - expected;
    if (drift > LOOP_LAG_WARN_MS) {
      if (!this.lagWarned) {
        this.lagWarned = true;
        this.hooks.onWarn?.('loop-lag', drift);
      }
    } else if (drift <= 0) {
      this.lagWarned = false;
    }
  }

  /** Check tmux RSS once. Over the limit fires onRetap exactly ONCE until
   * the RSS recovers below the limit. Returns the last RSS seen. */
  checkRss(): number | undefined {
    const getPid = this.hooks.getPid;
    if (!getPid) return undefined;
    const pid = getPid();
    if (pid === undefined) return undefined;
    const read = this.hooks.readRssBytes ?? readProcRssBytes;
    const rss = read(pid);
    if (rss === undefined) return undefined;
    if (rss > TMUX_RSS_LIMIT_BYTES) {
      if (!this.retapFired) {
        this.retapFired = true;
        this.hooks.onRetap?.();
      }
    } else {
      this.retapFired = false;
    }
    return rss;
  }

  /** Whether a retap has fired and not yet recovered. */
  retapActive(): boolean {
    return this.retapFired;
  }
}
