import { defineConfig, devices } from "@playwright/test";

/**
 * Playwright config for the kb-code Search-Everywhere box smoke (W4.3).
 * Mirrors the shape of the root `tests/e2e/playwright.config.ts` (kb's own
 * suite) but is self-contained under `web-code/e2e/` — its own daemon
 * (kb-code-server, not kb-server), its own fixture repo, its own port. The
 * test boots the daemon via globalSetup (global-setup.ts) and tears it
 * down in globalTeardown.
 */
export default defineConfig({
  testDir: ".",
  testMatch: /.*\.spec\.ts/,
  timeout: 60_000,
  fullyParallel: false, // single daemon per run
  workers: 1,
  reporter: [["list"]],
  globalSetup: require.resolve("./global-setup.ts"),
  globalTeardown: require.resolve("./global-teardown.ts"),
  // V70-A0 — two independent snapshot homes, both explicit `pathTemplate`s
  // so neither depends on Playwright's own default `<file>-snapshots/`
  // sibling-directory convention:
  //  - `regions.spec.ts` (landmark ARIA snapshots) → `__snapshots__/…`, the
  //    deliverable's explicit ask.
  //  - `visual.spec.ts` (full-page screenshots, opt-in via KBC_VISUAL=1,
  //    see that file's own header doc) → `visual/…`, committed baselines.
  expect: {
    toMatchAriaSnapshot: {
      pathTemplate: "__snapshots__/{testFilePath}/{arg}{ext}",
    },
    toHaveScreenshot: {
      pathTemplate: "visual/{arg}{-projectName}{-snapshotSuffix}{ext}",
      maxDiffPixelRatio: 0.02,
      animations: "disabled",
    },
  },
  use: {
    actionTimeout: 10_000,
    navigationTimeout: 15_000,
    trace: "retain-on-failure",
    headless: true,
  },
  projects: [
    {
      name: "chromium",
      use: { ...devices["Desktop Chrome"] },
    },
    // V70-A0 — opt-in only (`KBC_E2E_FIREFOX=1`), never in CI. Browser
    // floor, stated by the Location Contract's two history adapters (v7.0
    // A6, `src/nav/history.ts` — Navigation API where available, History-API
    // fallback otherwise — see docs/research/kb-code-v7-continuum-2026-09.html
    // §P7): Chrome/Edge 102+, Firefox 145+ (Baseline "newly available",
    // January 2026). Chromium exercises the Navigation adapter; this project
    // is what proves the History-API fallback still reads code where the
    // Navigation API is absent. This project exists so a developer CAN
    // spot-check cross-browser drift locally, but the suite (and `just
    // ci-code-e2e`) stays chromium-only.
    ...(process.env.KBC_E2E_FIREFOX === "1"
      ? [{ name: "firefox", use: { ...devices["Desktop Firefox"] } }]
      : []),
  ],
});
