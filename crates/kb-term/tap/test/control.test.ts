// Control-mode client tests (T5 tests 6-8). Unit level: a fake spawn whose
// stdout is pushed by hand, so no tmux runs.
import { test, expect } from 'bun:test';
import { Control, ControlError, attachArgv, continueCommand, exitNeedsReseed, pauseCommand, type SpawnedProcess } from '../src/control.ts';
import { PAUSE_AFTER_SECS } from '../src/control.ts';

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

  push(chunk: string): void {
    const bytes = new TextEncoder().encode(chunk);
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
  failWrites = false;
  fn = (argv: string[]): SpawnedProcess => {
    this.argv = argv;
    return {
      stdin: {
        write: (d: string) => {
          if (this.failWrites) throw new Error('EPIPE');
          this.writes.push(d);
          return d.length;
        },
      },
      stdout: this.stdout,
      kill: () => {
        this.killed = true;
      },
    };
  };
}

const enc = (s: string): string => s;

async function start(fake: FakeSpawn): Promise<Control> {
  const c = new Control({ tmuxBin: '/fake/tmux-3.7c', socketPath: '/tmp/s.sock', sessionName: 's1', spawn: fake.fn });
  const starting = c.start();
  // pause-after is written at spawn (control mode emits %begin/%end only for
  // client-issued commands; there is no attach ack). Mirror real tmux
  // numbering: the attach block (num 0) and the pause-after block (num 1).
  fake.stdout.push(enc('%begin 1000 0 0\n%end 1000 0 0\n'));
  fake.stdout.push(enc('%begin 1000 1 0\n%end 1000 1 0\n'));
  await starting;
  return c;
}

/** Let the read loop consume pushed lines. */
/** Poll until cond() holds (or the deadline passes). */
async function waitFor(cond: () => boolean): Promise<void> {
  const deadline = Date.now() + 2000;
  while (!cond()) {
    if (Date.now() > deadline) throw new Error('waitFor timed out');
    await new Promise<void>((r) => setImmediate(r));
  }
}

test('attach_uses_ignore_size_and_the_configured_tmux_bin', async () => {
  const fake = new FakeSpawn();
  const c = await start(fake);
  expect(fake.argv).toEqual(['/fake/tmux-3.7c', '-S', '/tmp/s.sock', '-C', 'attach', '-f', 'ignore-size', '-t', 's1']);
  expect(fake.argv).toEqual(attachArgv('/fake/tmux-3.7c', '/tmp/s.sock', 's1'));
  c.stop();
});

test('attach_sets_pause_after_20', async () => {
  const fake = new FakeSpawn();
  const c = await start(fake);
  expect(PAUSE_AFTER_SECS).toBe(20);
  const joined = fake.writes.join('');
  expect(joined).toContain('refresh-client -f pause-after=20');
  c.stop();
});

test('control_parses_begin_end_blocks_and_interleaved_output', async () => {
  const fake = new FakeSpawn();
  const c = await start(fake);
  const outputs: { pane: string; bytes: Uint8Array }[] = [];
  c.onOutput((pane, bytes) => outputs.push({ pane, bytes }));
  const done = c.cmd('capture-pane -p -t %0');
  fake.stdout.push(enc('%begin 2000 2 0\n'));
  fake.stdout.push(enc('line-one\n'));
  fake.stdout.push(enc('line-two\n'));
  fake.stdout.push(enc('%end 2000 2 0\n'));
  const lines = await done;
  expect(lines).toEqual(['line-one', 'line-two']);
  // %output is a NOTIFICATION line: it belongs outside reply blocks, so it
  // arrives after the block closes and flows to onOutput with octal decoded.
  // The wire value carries the newline as octal `\012` (tmux octal-escapes
  // control bytes; the decoder's `\n` is literal backslash-n, not an escape)
  // and the line itself ends with a separate real newline terminator.
  fake.stdout.push(enc('%output %0 hello\\033[31mworld\\012\n'));
  await waitFor(() => outputs.length === 1);
  expect(outputs.length).toBe(1);
  expect(outputs[0].pane).toBe('%0');
  expect(outputs[0].bytes).toEqual(new TextEncoder().encode('hello\x1b[31mworld\n'));
  c.stop();
});

test('control_error_block_rejects_the_command', async () => {
  const fake = new FakeSpawn();
  const c = await start(fake);
  const done = c.cmd('refresh-client -A %0:continue');
  fake.stdout.push(enc('%begin 3000 2 0\n'));
  fake.stdout.push(enc('parse failed\n'));
  fake.stdout.push(enc('%error 3000 2 0\n'));
  await expect(done).rejects.toBeInstanceOf(ControlError);
  try {
    await done;
  } catch (err) {
    expect((err as Error).message).toContain('parse failed');
  }
  c.stop();
});

test('extended_output_data_is_after_first_colon_space', async () => {
  const fake = new FakeSpawn();
  const c = await start(fake);
  const got: { bytes: Uint8Array; ageMs?: number }[] = [];
  c.onOutput((_pane, bytes, ageMs) => got.push({ bytes, ageMs }));
  // The value is the data after the FIRST " : " in the line, so the "a" in
  // the header slot is not part of the value and the second " : " stays.
  fake.stdout.push(enc('%extended-output %0 42 a : 1 : tail\n'));
  await waitFor(() => got.length === 1);
  expect(got.length).toBe(1);
  expect(got[0].ageMs).toBe(42);
  expect(got[0].bytes).toEqual(new TextEncoder().encode('1 : tail'));
  c.stop();
});

test('octal_escapes_decode', async () => {
  const fake = new FakeSpawn();
  const c = await start(fake);
  const got: Uint8Array[] = [];
  c.onOutput((_pane, bytes) => got.push(bytes));
  fake.stdout.push(enc('%output %0 \\033[?2026h\\134x\\007\n'));
  await waitFor(() => got.length === 1);
  expect(got.length).toBe(1);
  expect(got[0]).toEqual(new TextEncoder().encode('\x1b[?2026h\\x\x07'));
  c.stop();
});

test('pause_sends_quoted_continue_then_reseeds', async () => {
  const fake = new FakeSpawn();
  const c = await start(fake);
  const writesBefore: number[] = [];
  c.onPause(() => {
    writesBefore.push(fake.writes.length);
    // mimic the reseed opening: A/capture/B batch written AFTER the continue.
    // Replies never arrive in this unit test, so consume the rejections that
    // stop() raises later (a connection-close rejection, not a test error).
    for (const p of c.write([`display-message -p -t %0 'A #{history_size}'`, `capture-pane -p -e -J -S - -t %0`, `display-message -p -t %0 'B #{history_size}'`])) p.catch(() => {});
  });
  fake.stdout.push(enc('%pause %0\n'));
  // Wait until the continue AND the handler's reseed batch have been written
  // (pause-after, continue, batch = 3 stdin writes).
  await waitFor(() => fake.writes.length >= 3);
  const all = fake.writes.join('');
  const continueLine = "refresh-client -A '%0:continue'";
  expect(all).toContain(continueLine);
  expect(continueCommand('%0')).toBe(continueLine);
  expect(all.indexOf(continueLine)).toBeLessThan(all.indexOf("display-message -p -t %0 'A"));
  // the unquoted form must never be emitted (it %errors with parse).
  expect(all).not.toContain('refresh-client -A %0:continue\n');
  // the pause handler ran after the continue was already written.
  expect(writesBefore[0]).toBeGreaterThanOrEqual(2);
  c.stop();
});

test('pause_sends_quoted_pause', () => {
  const line = "refresh-client -A '%0:pause'";
  expect(pauseCommand('%0')).toBe(line);
  // the unquoted form fails with a %error parse (same rule as continue).
  expect(line).not.toContain('refresh-client -A %0:pause');
});

test('exit_too_far_behind_needs_reseed', () => {
  expect(exitNeedsReseed('too far behind')).toBe(true);
  expect(exitNeedsReseed('normal exit')).toBe(false);
});

test('control_fire_and_forget_block_does_not_steal_tracked_pending', async () => {
  const fake = new FakeSpawn();
  const c = await start(fake);
  // start()'s pause-after (fire-and-forget) was already answered by the
  // helper's num-1 block. A %pause makes control write the (fire-and-forget)
  // continue, which owns a Pending exactly like any tracked command: its
  // %begin/%end block (num 2) must resolve only its own promise, never the
  // cmd issued after it (num 3).
  fake.stdout.push(enc('%pause %0\n'));
  await waitFor(() => fake.writes.length >= 2);
  const done = c.cmd("display-message -p -t %0 'A #{history_size}'");
  fake.stdout.push(enc('%begin 5000 2 0\n%end 5000 2 0\n'));
  fake.stdout.push(enc('%begin 5000 3 0\nA 3 5 0\n%end 5000 3 0\n'));
  const lines = await done;
  expect(lines).toEqual(['A 3 5 0']);
  c.stop();
});

test('control_pause_with_invalid_pane_sends_no_continue', async () => {
  const fake = new FakeSpawn();
  const c = await start(fake);
  await waitFor(() => fake.writes.length >= 1); // pause-after from start()
  fake.stdout.push(enc('%pause %0;send-keys\n'));
  // Let the read loop dispatch it; only the pause-after write exists.
  await new Promise<void>((r) => setImmediate(r));
  await new Promise<void>((r) => setImmediate(r));
  const all = fake.writes.join('');
  expect(all).not.toContain('continue');
  c.stop();
});

test('control_dispatch_error_is_recorded_and_loop_continues', async () => {
  const fake = new FakeSpawn();
  const c = await start(fake);
  c.onOutput(() => {
    throw new Error('handler boom');
  });
  fake.stdout.push(enc('%output %0 first\n'));
  await waitFor(() => c.lastError !== null);
  expect(c.lastError?.message).toBe('handler boom');
  // The loop did not die on the throwing handler: on the SAME client a later
  // command still dispatches and resolves with its own replies.
  const done = c.cmd('capture-pane -p -t %0');
  fake.stdout.push(enc('%begin 6000 2 0\nline-one\n%end 6000 2 0\n'));
  expect(await done).toEqual(['line-one']);
  c.stop();
});
test('control_start_acks_pause_after_with_no_initial_begin', async () => {
  const fake = new FakeSpawn();
  const c = new Control({ tmuxBin: '/fake/tmux-3.7c', socketPath: '/tmp/s.sock', sessionName: 's1', spawn: fake.fn });
  const starting = c.start();
  // control mode emits no %begin for the attach itself: pause-after must be
  // written WITHOUT waiting for any block first.
  await waitFor(() => fake.writes.length >= 1);
  expect(fake.writes.join('')).toContain('refresh-client -f pause-after=20');
  // pause-after's own block (num 0) then acks start().
  fake.stdout.push(enc('%begin 7000 0 0\n%end 7000 0 0\n'));
  await starting;
  c.stop();
});

test('control_cmd_before_start_rejects_without_orphaning', async () => {
  const fake = new FakeSpawn();
  const c = new Control({ tmuxBin: '/fake/tmux-3.7c', socketPath: '/tmp/s.sock', sessionName: 's1', spawn: fake.fn });
  // Not started: cmd/write must return a rejected PROMISE (so .catch()
  // callers see it -- a synchronous throw would escape .catch()), and no
  // Pending may be left orphaned.
  const err = await c.cmd('display-message -p -t %0 x').then(() => null, (e: unknown) => e);
  expect(err).toBeInstanceOf(Error);
  expect((err as Error).message).toBe('control client not started');
  const errs = await Promise.all(
    c.write(['one', 'two']).map((q) => q.then(() => null, (e: unknown) => e)),
  );
  expect(errs.every((e) => e instanceof Error && (e as Error).message === 'control client not started')).toBe(true);
  // The rejected calls never reached the fake's stdin.
  expect(fake.writes.length).toBe(0);
  // A subsequent real start is unaffected: an orphaned Pending would
  // consume the pause-after %begin block and start() would hang.
  const starting = c.start();
  fake.stdout.push(enc('%begin 1000 0 0\n%end 1000 0 0\n'));
  fake.stdout.push(enc('%begin 1000 1 0\n%end 1000 1 0\n'));
  await starting;
  expect(fake.writes.length).toBe(1);
  c.stop();
});

test('control_pause_continue_failure_still_runs_handlers', async () => {
  const fake = new FakeSpawn();
  const c = await start(fake);
  // Make stdin writes throw (e.g. a broken pipe): the %pause continue
  // must not silence onPause.
  fake.failWrites = true;
  let paused = 0;
  c.onPause(() => {
    paused++;
  });
  fake.stdout.push(enc('%pause %0\n'));
  await waitFor(() => paused === 1);
  expect(c.lastError).not.toBeNull();
  fake.failWrites = false;
  c.stop();
});

test('command_builders_reject_invalid_pane_ids', () => {
  expect(() => continueCommand('%0')).not.toThrow();
  expect(() => pauseCommand('%0')).not.toThrow();
  expect(() => continueCommand('bogus')).toThrow('invalid pane id');
  expect(() => pauseCommand('%')).toThrow('invalid pane id');
  expect(() => continueCommand('%0;send-keys')).toThrow('invalid pane id');
  expect(() => pauseCommand('')).toThrow('invalid pane id');
});

test('attach_argv_rejects_empty_and_nul_arguments', () => {
  expect(() => attachArgv('/t', '/s', 's1')).not.toThrow();
  expect(() => attachArgv('', '/s', 's1')).toThrow('invalid tmuxBin');
  expect(() => attachArgv('/t', '', 's1')).toThrow('invalid socketPath');
  expect(() => attachArgv('/t', '/s', '')).toThrow('invalid sessionName');
  expect(() => attachArgv('/t', '/s\0x', 's1')).toThrow('invalid socketPath');
});
