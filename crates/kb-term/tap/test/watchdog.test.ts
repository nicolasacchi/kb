// Watchdog tests (T5 test 14). Injected clocks/readers, no timers.
import { test, expect } from 'bun:test';
import { AGE_WARN_MS, LOOP_LAG_WARN_MS, TMUX_RSS_LIMIT_BYTES, Watchdog, readProcRssBytes } from '../src/watchdog.ts';

test('watchdog_age_lag_rss_thresholds', () => {
  expect(AGE_WARN_MS).toBe(3000);
  expect(LOOP_LAG_WARN_MS).toBe(3000);
  expect(TMUX_RSS_LIMIT_BYTES).toBe(512 * 1024 * 1024);

  const warns: { what: string; value: number }[] = [];
  let now = 0;
  const wd = new Watchdog({
    now: () => now,
    onWarn: (what, value) => warns.push({ what, value }),
  });

  wd.noteAge(AGE_WARN_MS - 1);
  expect(warns.length).toBe(0);
  wd.noteAge(AGE_WARN_MS + 1);
  expect(warns).toEqual([{ what: 'extended-output-age', value: AGE_WARN_MS + 1 }]);
  // still over: no second warn until a recovery
  wd.noteAge(AGE_WARN_MS + 1000);
  expect(warns.length).toBe(1);
  wd.noteAge(100);
  expect(warns.length).toBe(1);
  wd.noteAge(AGE_WARN_MS + 5);
  expect(warns.length).toBe(2);

  // loop lag: an on-schedule tick does not warn; a late one warns once.
  const wd2 = new Watchdog({ now: () => now, onWarn: (what, value) => warns.push({ what, value }) });
  wd2.start(1000);
  now = 1000;
  wd2.tick(1000);
  now = 2000;
  wd2.tick(1000);
  expect(warns.filter((w) => w.what === 'loop-lag').length).toBe(0);
  now = 2000 + 1000 + 4000;
  wd2.tick(1000);
  const lagWarns = warns.filter((w) => w.what === 'loop-lag');
  expect(lagWarns.length).toBe(1);
  expect(lagWarns[0].value).toBe(4000);
  // an on-schedule tick ends the episode, so the next crossing warns again.
  now += 1000;
  wd2.tick(1000);
  now += 1000;
  wd2.tick(1000);
  expect(warns.filter((w) => w.what === 'loop-lag').length).toBe(1);
  now += 5000;
  wd2.tick(1000);
  expect(warns.filter((w) => w.what === 'loop-lag').length).toBe(2);
  now += 1000;
  wd2.tick(1000);
  wd2.stop();
});

test('rss_over_limit_triggers_retap_once', () => {
  const MB = 1024 * 1024;
  let rss = 400 * MB;
  let retaps = 0;
  const wd = new Watchdog({
    getPid: () => 4242,
    readRssBytes: () => rss,
    onRetap: () => retaps++,
  });
  wd.checkRss();
  expect(retaps).toBe(0);
  expect(wd.retapActive()).toBe(false);
  rss = 600 * MB;
  wd.checkRss();
  expect(retaps).toBe(1);
  expect(wd.retapActive()).toBe(true);
  wd.checkRss();
  wd.checkRss();
  expect(retaps).toBe(1);
  rss = 100 * MB;
  wd.checkRss();
  expect(retaps).toBe(1);
  expect(wd.retapActive()).toBe(false);
  rss = 700 * MB;
  wd.checkRss();
  expect(retaps).toBe(2);
  expect(wd.retapActive()).toBe(true);
});

test('rss_unreadable_checks_are_counted_not_silent', () => {
  // No pid source at all: not a check, and counted as one.
  const wd0 = new Watchdog({});
  expect(wd0.checkRss()).toBeUndefined();
  expect(wd0.stats()).toEqual({ rssChecks: 1, rssUnreadable: 1 });
  // Pid unavailable.
  const wd1 = new Watchdog({ getPid: () => undefined });
  expect(wd1.checkRss()).toBeUndefined();
  expect(wd1.stats().rssUnreadable).toBe(1);
  // Unreadable /proc entry (injected).
  const wd2 = new Watchdog({ getPid: () => 4242, readRssBytes: () => undefined });
  expect(wd2.checkRss()).toBeUndefined();
  expect(wd2.checkRss()).toBeUndefined();
  expect(wd2.stats()).toEqual({ rssChecks: 2, rssUnreadable: 2 });
  // A readable under-limit check is distinguishable from not-measuring.
  let rss: number | undefined = 100 * 1024 * 1024;
  const wd3 = new Watchdog({ getPid: () => 4242, readRssBytes: () => rss });
  expect(wd3.checkRss()).toBe(100 * 1024 * 1024);
  expect(wd3.stats()).toEqual({ rssChecks: 1, rssUnreadable: 0 });
  rss = undefined;
  expect(wd3.checkRss()).toBeUndefined();
  expect(wd3.stats()).toEqual({ rssChecks: 2, rssUnreadable: 1 });
});

test('read_proc_rss_bytes_reads_this_process', () => {
  // The real /proc path (never exercised before: the shipped watchdog
  // reads VmRSS from /proc/<pid>/status by default).
  const proc = (globalThis as unknown as { process?: { pid?: number } }).process;
  expect((proc?.pid ?? 0) > 0).toBe(true);
  const rss = readProcRssBytes(proc!.pid!);
  expect(typeof rss).toBe('number');
  expect((rss ?? 0) > 0).toBe(true);
  // A pid that cannot exist gives undefined (countable via stats() when
  // wired as readRssBytes).
  expect(readProcRssBytes(999999999)).toBeUndefined();
});
