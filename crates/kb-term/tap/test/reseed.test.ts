// Reseed orchestration tests (T5 tests 7, 9-13, 15). Fake control whose
// command promises the test resolves by hand; a real xterm twin.
import { test, expect } from 'bun:test';
import { makeTwin, type Twin } from '../src/twin.ts';
import { MAX_RESEED_ATTEMPTS, MODE_FORMATS, QUIESCENT_MAX_MS, QUIESCENT_MS, Reseeder, ReseedError, modeRestoreBytes, type ReseedStats } from '../src/reseed.ts';
import { dumpRows, stateOf } from './support/compare.ts';

interface PendingCmd {
  line: string;
  resolve: (lines: string[]) => void;
  reject: (err: Error) => void;
}

class FakeControl {
  writes: string[][] = [];
  cmds: PendingCmd[] = [];
  private outCb: ((pane: string, bytes: Uint8Array, ageMs?: number) => void) | null = null;
  now = 0;

  cmd(line: string): Promise<string[]> {
    return this.write([line])[0];
  }

  write(lines: string[]): Promise<string[]>[] {
    this.writes.push([...lines]);
    return lines.map(
      (line) =>
        new Promise<string[]>((resolve, reject) => {
          this.cmds.push({ line, resolve, reject });
        }),
    );
  }

  onOutput(cb: (pane: string, bytes: Uint8Array, ageMs?: number) => void): void {
    this.outCb = cb;
  }

  emitOutput(pane: string, bytes: Uint8Array): void {
    this.outCb?.(pane, bytes);
  }

  /** resolve the oldest outstanding command with reply lines */
  resolveNext(lines: string[]): void {
    const c = this.cmds.shift();
    if (!c) throw new Error('no pending command to resolve');
    c.resolve(lines);
  }
}

function setup(cols = 80, rows = 24): { control: FakeControl; twin: Twin; reseeder: Reseeder } {
  const control = new FakeControl();
  const twin = makeTwin(cols, rows);
  const reseeder = new Reseeder(control, twin, { now: () => control.now });
  return { control, twin, reseeder };
}

async function tick(): Promise<void> {
  for (let i = 0; i < 3; i++) await new Promise<void>((r) => setImmediate(r));
}

/** drive one attempt's five commands to completion */
async function resolveAttempt(
  control: FakeControl,
  capture: string[],
  a: string[],
  b: string[],
  mode: string[] = [],
  altCapture: string[] | null = null,
): Promise<void> {
  control.resolveNext(a); // A
  control.resolveNext(capture); // capture
  control.resolveNext(altCapture ?? []); // -a capture
  control.resolveNext(b); // B
  control.resolveNext(mode); // mode query
  await tick();
}

function cursorOf(twin: Twin): { x: number; y: number } {
  const buffer = twin.term.buffer as unknown as { active: { cursorX: number; cursorY: number } };
  return { x: buffer.active.cursorX, y: buffer.active.cursorY };
}

test('reseed_sends_A_capture_B_in_one_write', async () => {
  const { control, twin, reseeder } = setup();
  try {
    const done = reseeder.reseed('%0', { quiescentMs: 0 });
    await tick();
    expect(control.writes.length).toBe(1);
    expect(control.writes[0]).toEqual([
      "display-message -p -t %0 'A #{history_size} #{cursor_x} #{cursor_y}'",
      'capture-pane -p -e -J -S - -t %0',
      'capture-pane -p -e -J -S - -a -t %0',
      "display-message -p -t %0 'B #{history_size} #{cursor_x} #{cursor_y}'",
      `display-message -p -t %0 'M ${MODE_FORMATS.map((f) => `#{${f}}`).join(' ')}'`,
    ]);
    await resolveAttempt(control, ['hello'], ['A 0 5 0'], ['B 0 5 0'], ['M 1 0 23 1 1 0 0 0 0'], ['normal-line']);
    const r = await done;
    expect(r.attempts).toBe(1);
    expect(r.historySize).toBe(0);
  } finally {
    twin.dispose();
  }
});

test('reseed_retries_when_output_interleaves', async () => {
  const { control, twin, reseeder } = setup();
  try {
    const done = reseeder.reseed('%0', { quiescentMs: 0 });
    await tick();
    // A resolves; output arrives before B resolves -> interleave -> retry.
    control.resolveNext(['A 0 5 0']);
    await tick();
    control.emitOutput('%0', new TextEncoder().encode('MID'));
    control.resolveNext(['hello']);
    control.resolveNext([]);
    control.resolveNext(['B 0 5 0']);
    control.resolveNext([]);
    await tick();
    expect(control.writes.length).toBe(2);
    await resolveAttempt(control, ['hello', 'MID'], ['A 0 6 0'], ['B 0 6 0']);
    const r = await done;
    expect(r.attempts).toBe(2);
    expect(reseeder.stats().interleavedRetries).toBe(1);
  } finally {
    twin.dispose();
  }
});

test('reseed_drops_pre_A_output_silently', async () => {
  const { control, twin, reseeder } = setup();
  try {
    const done = reseeder.reseed('%0', { quiescentMs: 0 });
    await tick();
    // output before A resolves: inside the capture, dropped silently.
    control.emitOutput('%0', new TextEncoder().encode('EARLY'));
    await resolveAttempt(control, ['EARLY-captured'], ['A 0 5 0'], ['B 0 5 0']);
    const r = await done;
    expect(r.attempts).toBe(1);
    expect(reseeder.stats().preABytes).toBe(5);
  } finally {
    twin.dispose();
  }
});

test('reseed_retries_when_A_and_B_disagree', async () => {
  const { control, twin, reseeder } = setup();
  try {
    const done = reseeder.reseed('%0', { quiescentMs: 0 });
    await tick();
    await resolveAttempt(control, ['hello'], ['A 3 5 0'], ['B 4 5 0']);
    await tick();
    expect(control.writes.length).toBe(2);
    await resolveAttempt(control, ['hello'], ['A 4 5 0'], ['B 4 5 0']);
    const r = await done;
    expect(r.attempts).toBe(2);
    expect(reseeder.stats().abDisagreeRetries).toBe(1);
  } finally {
    twin.dispose();
  }
});

test('reseed_gives_up_after_max_attempts', async () => {
  const { control, twin, reseeder } = setup();
  try {
    const done = reseeder.reseed('%0', { quiescentMs: 0 });
    let caught: unknown = null;
    void done.catch((e) => {
      caught = e;
    });
    for (let i = 0; i < MAX_RESEED_ATTEMPTS; i++) {
      await tick();
      control.resolveNext([`A ${i} 5 0`]);
      await tick();
      control.emitOutput('%0', new TextEncoder().encode('X'));
      control.resolveNext(['hello']);
      control.resolveNext([]);
      control.resolveNext([`B ${i} 5 0`]);
      control.resolveNext([]);
      await tick();
    }
    await tick();
    expect(caught).toBeInstanceOf(ReseedError);
    expect(reseeder.stats().givesUp).toBe(1);
  } finally {
    twin.dispose();
  }
});

test('reseed_joins_with_crlf_without_trailing_crlf_and_places_cursor', async () => {
  const { control, twin, reseeder } = setup();
  try {
    const done = reseeder.reseed('%0', { quiescentMs: 0 });
    await tick();
    await resolveAttempt(control, ['alpha', 'beta', 'gamma'], ['A 0 4 1'], ['B 0 4 1']);
    const r = await done;
    expect(r.bytesFed).toBeGreaterThan(0);
    // cursor == B's (4,1), restored by the final CUP.
    expect(cursorOf(twin)).toEqual({ x: 4, y: 1 });
    const dump = twin.serialize.serialize();
    expect(dump).toContain('alpha');
    expect(dump).toContain('gamma');
  } finally {
    twin.dispose();
  }
});

test('reseed_restores_alt_screen_region_and_wrap_modes', async () => {
  const { control, twin, reseeder } = setup();
  try {
    // M: alternate=1 top=3 lower=20 wrap=0 cursor=1 insert=0 keypadCursor=1 keypad=1 origin=1
    const mode = ['M 1 3 20 0 1 0 1 1 1'];
    const done = reseeder.reseed('%0', { quiescentMs: 0 });
    await tick();
    await resolveAttempt(control, ['alt-1', 'alt-2'], ['A 0 2 1'], ['B 0 2 1'], mode, ['normal-1', 'normal-2']);
    const r = await done;
    expect(r.mode?.alternateOn).toBe(true);
    expect(r.mode?.scrollTop).toBe(3);
    const state = stateOf(twin.term as never);
    expect(state.activeBuffer).toBe('alternate');
    expect(state['alt.scrollTop']).toBe(2);
    expect(state['alt.scrollBottom']).toBe(19);
    // normal buffer holds the -a capture. dumpRows returns the LAST n rows,
    // so dump the whole buffer; rows are space-padded to cols width, hence
    // the substring (includes) form rather than exact-element contains.
    const normal = dumpRows(twin.term as never, 'normal', 1000);
    expect(normal.some((r) => r.includes('normal-1'))).toBe(true);
    expect(normal.some((r) => r.includes('normal-2'))).toBe(true);
    // CUP is the last restore step: its row is relative to the region top.
    expect(cursorOf(twin).y).toBe(3);
    // restore byte order: region first, CUP last.
    const bytes = modeRestoreBytes(r.mode, 2, 1, 24);
    expect(bytes.startsWith('\x1b[3;20r')).toBe(true);
    expect(bytes.endsWith('\x1b[2;3H')).toBe(true);
    expect(bytes.indexOf('\x1b[3;20r')).toBeLessThan(bytes.indexOf('\x1b[?6h'));
    expect(bytes.indexOf('\x1b[?6h')).toBeLessThan(bytes.indexOf('\x1b[2;3H'));
  } finally {
    twin.dispose();
  }
});

test('reseed_waits_for_quiescence', async () => {
  const { control, twin, reseeder } = setup();
  try {
    expect(QUIESCENT_MS).toBe(50);
    expect(QUIESCENT_MAX_MS).toBe(2000);
    const sleepAdvances: number[] = [];
    const opts = {
      now: () => control.now,
      sleep: (ms: number) => {
        sleepAdvances.push(ms);
        control.now += ms;
        return Promise.resolve();
      },
    };
    // Seed state from construction-time now: emit at t=0 with no frame end.
    control.emitOutput('%0', new TextEncoder().encode('noise'));
    const done = reseeder.reseed('%0', opts);
    await tick();
    // The write must not happen until 50ms of silence have elapsed.
    expect(control.writes.length).toBe(1);
    expect(control.now).toBeGreaterThanOrEqual(50);
    await resolveAttempt(control, ['hello'], ['A 0 5 0'], ['B 0 5 0']);
    const r = await done;
    expect(r.quiescent).toBe(true);

    // A complete ?2026l (even split across two chunks) skips the wait.
    control.now = 0;
    const reseeder2 = new Reseeder(control, twin, opts);
    reseeder2.noteOutput(new TextEncoder().encode('\x1b[?20'), '%0');
    reseeder2.noteOutput(new TextEncoder().encode('26l'), '%0');
    sleepAdvances.length = 0;
    const done2 = reseeder2.reseed('%0', { ...opts, quiescentMs: 50, quiescentMaxMs: 2000 });
    await tick();
    expect(control.now).toBe(0);
    expect(sleepAdvances.length).toBe(0);
    await resolveAttempt(control, ['hello'], ['A 0 5 0'], ['B 0 5 0']);
    const r2 = await done2;
    expect(r2.quiescent).toBe(true);

    // Output keeps arriving: proceed after QUIESCENT_MAX_MS with false.
    control.now = 0;
    const reseeder3 = new Reseeder(control, twin, {
      now: () => control.now,
      sleep: (ms: number) => {
        control.now += ms;
        reseeder3.noteOutput(new TextEncoder().encode('x'), '%0');
        return Promise.resolve();
      },
    });
    reseeder3.noteOutput(new TextEncoder().encode('seed'), '%0');
    const done3 = reseeder3.reseed('%0', { now: () => control.now, quiescentMaxMs: 100 });
    await tick();
    expect(control.now).toBeGreaterThanOrEqual(100);
    await resolveAttempt(control, ['hello'], ['A 0 5 0'], ['B 0 5 0']);
    const r3 = await done3;
    expect(r3.quiescent).toBe(false);
    expect(reseeder3.stats().quiescentFalse).toBe(1);

    // A `?2026l` seen long ago must NOT satisfy a fresh wait: fresh output
    // after it re-dates the silence window, so the wait pays the 50ms rule
    // even though frameEnded bytes still sit in the tail.
    control.now = 100;
    reseeder2.noteOutput(new TextEncoder().encode('x'), '%0');
    const done4 = reseeder2.reseed('%0', opts);
    await tick();
    // The write must only land after 50ms past the fresh output.
    expect(control.writes.length).toBe(4);
    expect(control.now).toBeGreaterThanOrEqual(150);
    await resolveAttempt(control, ['hello'], ['A 0 5 0'], ['B 0 5 0']);
    const r4 = await done4;
    expect(r4.quiescent).toBe(true);

    // A frame end followed within the quiescent window by fresh output must
    // NOT take the frame-end fast path: the frame end is no longer the most
    // recent output, so the wait pays the 50ms silence rule.
    control.now = 200;
    const reseeder4 = new Reseeder(control, twin, opts);
    reseeder4.noteOutput(new TextEncoder().encode('\x1b[?2026l'), '%0');
    control.now = 230;
    reseeder4.noteOutput(new TextEncoder().encode('x'), '%0');
    const done5 = reseeder4.reseed('%0', opts);
    await tick();
    // The write must only land after 50ms past the fresh output (now=230).
    expect(control.now).toBeGreaterThanOrEqual(280);
    await resolveAttempt(control, ['hello'], ['A 0 5 0'], ['B 0 5 0']);
    const r5 = await done5;
    expect(r5.quiescent).toBe(true);
  } finally {
    twin.dispose();
  }
});

test('reseed_pads_blank_tail_and_does_not_slice_joined_wrapped_lines', async () => {
  const { control, twin, reseeder } = setup(80, 24);
  try {
    // history 10 + 24 rows expected = 34 lines; capture has 30 (tail blank
    // viewport rows not printed) -> pad 4 CRLFs.
    const lines = Array.from({ length: 30 }, (_, i) => `row-${i}`);
    const done = reseeder.reseed('%0', { quiescentMs: 0 });
    await tick();
    await resolveAttempt(control, lines, ['A 10 5 23'], ['B 10 5 23']);
    const r = await done;
    expect(r.historySize).toBe(10);
    expect(twin.lineCount('normal')).toBe(34);
    expect(cursorOf(twin)).toEqual({ x: 5, y: 23 });

    // One captured line of 200 chars (three -J joined rows) re-wraps to three
    // twin rows; the joiner must not slice it by history math.
    const long = 'X'.repeat(200);
    const done2 = reseeder.reseed('%0', { quiescentMs: 0 });
    await tick();
    await resolveAttempt(control, [long], ['A 0 40 2'], ['B 0 40 2']);
    await done2;
    const rows = dumpRows(twin.term as never, 'normal', 24);
    // dumpRows pads every row to cols width with spaces: compare trimmed
    // content (the 3rd row is 40 X + 40 spaces, not length 40).
    expect(rows[0].trimEnd()).toBe('X'.repeat(80));
    expect(rows[1].trimEnd()).toBe('X'.repeat(80));
    expect(rows[2].trimEnd()).toBe('X'.repeat(40));
    const s: ReseedStats = reseeder.stats();
    expect(s.overflowRetries).toBe(0);
  } finally {
    twin.dispose();
  }
});

test('reseed_rejects_invalid_pane_id_before_any_write', async () => {
  const { control, twin, reseeder } = setup();
  try {
    // paneId is spliced into tmux command lines: a crafted id carrying a
    // newline/quote must be rejected before anything reaches the socket.
    await expect(reseeder.reseed('%0\ndisplay-message -p INJECTED', { quiescentMs: 0 })).rejects.toThrow(/invalid pane id/);
    await expect(reseeder.reseed('0', { quiescentMs: 0 })).rejects.toThrow(/invalid pane id/);
    await expect(reseeder.reseed('', { quiescentMs: 0 })).rejects.toThrow(/invalid pane id/);
    expect(control.writes.length).toBe(0);
  } finally {
    twin.dispose();
  }
});
