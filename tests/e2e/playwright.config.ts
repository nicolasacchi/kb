import { defineConfig, devices } from "@playwright/test";

// V72-C3 — GitHub-hosted CI runners are load, not logic: with 282 specs
// (chromium+firefox) on a shared hosted runner, one random `expect` a run
// crosses the 5s default budget while the other 281 pass — a DIFFERENT spec
// each time (evidence: run 34032948454 lost spa-comments.spec.ts:1238 under
// firefox on a `toBeVisible` timeout; run 34044510928 lost
// spa-newtab.spec.ts:39 under chromium on a `toHaveURL` timeout; a
// from-scratch local investigation of the first found no product defect and
// 8/8 local reruns passed). Neither spec has EVER failed twice, so this is
// resource contention, not a real bug — widening the budget and absorbing
// one retry is honest here in a way it would not be if a spec recurred.
const isCI = !!process.env.CI;

/**
 * Playwright config for kb's iframe + SSE smoke. The test boots the kb-server
 * binary via globalSetup (tests/e2e/global-setup.ts) and tears it down in
 * globalTeardown.
 */
export default defineConfig({
  testDir: ".",
  testMatch: /.*\.spec\.ts/,
  timeout: 60_000,
  // CI-only widening of the PER-ASSERTION budget (load, not a logic change):
  // doubles the 5s default so a slow-under-contention `expect` on a hosted
  // runner has room to resolve. Local runs keep Playwright's 5s default.
  expect: { timeout: isCI ? 10_000 : 5_000 },
  fullyParallel: false, // single daemon per run
  workers: 1,
  // CI gets exactly one retry to absorb a hosted-runner-load stall rather
  // than blocking a merge on it — but the retry must stay VISIBLE, never a
  // silent green: `list` still prints every spec's result, and `github`
  // turns a retried-then-passed spec into a job-summary annotation (Playwright
  // reports it as "flaky", not "passed"). Local runs get zero retries, so a
  // real local failure still fails loud on the first try.
  retries: isCI ? 1 : 0,
  reporter: isCI ? [["list"], ["github"]] : [["list"]],
  globalSetup: require.resolve("./global-setup.ts"),
  globalTeardown: require.resolve("./global-teardown.ts"),
  use: {
    actionTimeout: 10_000,
    navigationTimeout: 15_000,
    trace: "retain-on-failure",
    headless: true,
    // Map a synthetic DNS-name host onto the loopback daemon. The
    // identity-mismatch specs need a non-IP-literal parent to exercise
    // verifyAgainstIdentity's cross-check (it skips IP-literal parents,
    // where the window.location heuristic can't recover a DNS suffix).
    launchOptions: {
      args: ["--host-resolver-rules=MAP kb-host.test 127.0.0.1"],
    },
  },
  projects: [
    {
      name: "chromium",
      use: { ...devices["Desktop Chrome"] },
    },
    // A GECKO slice, not a second full suite. It exists because the
    // selection→comment path is the one place where an engine's own event
    // ORDERING is the product: Gecko collapses a text selection before the
    // `click` that caused it, where Blink collapses after — a W3C-documented
    // divergence that no amount of Chromium coverage can surface. Running
    // those specs (and only those) under Firefox is what turns "we think this
    // works on Gecko" into a check.
    //
    // Scoped by `grep` on a `@selection` marker in the describe titles rather
    // than by file: spa-comments.spec.ts also holds identity / origin-check
    // specs that depend on the Chromium-only `--host-resolver-rules` flag
    // below, and those must not run here.
    {
      name: "firefox",
      grep: /@selection/,
      use: {
        ...devices["Desktop Firefox"],
        // Touch emulation so the mobile-shaped specs exercise the same
        // coarse-pointer branches they do under chromium.
        hasTouch: true,
        // OVERRIDE, not extend: the shared `use.launchOptions` above carries
        // `--host-resolver-rules`, a Chromium switch. Firefox rejects unknown
        // command-line arguments outright, so the whole browser fails to
        // launch if this inherits. The specs in this project's grep all talk
        // to 127.0.0.1 directly, so they need no host mapping.
        launchOptions: {},
      },
    },
  ],
});
