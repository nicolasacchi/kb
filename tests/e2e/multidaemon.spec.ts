import { test, expect } from "@playwright/test";
import { spawn, ChildProcess } from "node:child_process";
import { mkdtempSync, copyFileSync, mkdirSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { PORT } from "./helpers";

// Spawns 2 extra daemons (ports 4738 + 4739) alongside the global-setup
// daemon at PORT (4737). The SPA's localStorage is pre-seeded with all
// three URLs so the SSE manager opens 3 EventSources. The test asserts
// the status pill aggregates correctly: three rows in the drop-up
// detail panel, all "idle" once the initial walks settle.
//
// This is the v0.1 plan's required multi-daemon stress check before
// shipping. Future iterations can add overlapping-reindex assertions
// to verify in-flight summing under load.

const REPO_ROOT = resolve(__dirname, "..", "..");
const KB_SERVER_BIN = resolve(REPO_ROOT, "target", "fast", "kb-server");
const SPA_DIST = resolve(REPO_ROOT, "web", "dist");
const CANON_DIR = resolve(REPO_ROOT, "corpus", "canon");
const CANON_FILES = [
  "fullscreen-viz.html",
  "kitchen-sink.html",
  "multi-page.html",
  "cost-of-abstraction.html",
];

const EXTRA_PORTS = [4738, 4739];

type Daemon = { proc: ChildProcess; port: number; tmp: string };
const spawned: Daemon[] = [];

async function waitForDaemon(port: number, timeoutMs: number): Promise<boolean> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    try {
      const r = await fetch(`http://127.0.0.1:${port}/api/identity`);
      if (r.ok) return true;
    } catch {
      // not yet up
    }
    await new Promise((r) => setTimeout(r, 200));
  }
  return false;
}

async function spawnDaemon(port: number, name: string): Promise<Daemon> {
  const tmp = mkdtempSync(join(tmpdir(), `kb-mdaemon-${name}-`));
  const source = join(tmp, "corpus");
  mkdirSync(source, { recursive: true });
  for (const f of CANON_FILES) copyFileSync(join(CANON_DIR, f), join(source, f));

  const cfg = join(tmp, "kb.toml");
  writeFileSync(
    cfg,
    `[daemon]
name = "${name}"

[server]
addr = "127.0.0.1:${port}"

[defaults]
disable_embedder_fallback = true

[kb.${name}]
path = "${source}"
`,
    "utf-8",
  );

  for (const sub of ["state", "config", "cache"]) mkdirSync(join(tmp, sub));
  const proc = spawn(KB_SERVER_BIN, ["--config", cfg], {
    env: {
      ...process.env,
      XDG_STATE_HOME: join(tmp, "state"),
      XDG_CONFIG_HOME: join(tmp, "config"),
      XDG_CACHE_HOME: join(tmp, "cache"),
      KB_SPA_DIST: SPA_DIST,
      RUST_LOG: "warn",
    },
    stdio: "ignore",
  });

  // 30s matches global-setup's main daemon. With disable_embedder_fallback
  // above, cold boot is just migrations + the initial walk (no embedder
  // spawn/handshake), so this is comfortable.
  const ok = await waitForDaemon(port, 30_000);
  if (!ok) {
    proc.kill("SIGTERM");
    throw new Error(`extra daemon on :${port} never came up`);
  }
  await new Promise((r) => setTimeout(r, 1000)); // give indexer a beat
  return { proc, port, tmp };
}

test.beforeAll(async () => {
  for (let i = 0; i < EXTRA_PORTS.length; i++) {
    spawned.push(await spawnDaemon(EXTRA_PORTS[i], `extra-${i + 1}`));
  }
});

test.afterAll(async () => {
  for (const d of spawned) {
    d.proc.kill("SIGTERM");
  }
  spawned.length = 0;
});

test.describe("multi-daemon", () => {
  test("status pill aggregates all 3 daemons in the detail panel", async ({
    page,
  }) => {
    const all = [PORT, ...EXTRA_PORTS].map((p) => `http://127.0.0.1:${p}`);
    await page.addInitScript((urls) => {
      localStorage.setItem("kb:daemons", JSON.stringify(urls));
    }, all);

    await page.goto(`http://127.0.0.1:${PORT}/`);

    // Status pill renders. Click to expand the per-daemon panel.
    const pill = page.getByRole("button", { name: /daemon status/i });
    await expect(pill).toBeVisible();
    await pill.click();

    const panel = page.getByRole("dialog", { name: "daemon detail" });
    await expect(panel).toBeVisible();

    // The panel shows one row per daemon. We assert by URL substring;
    // each of the three configured daemons should appear once.
    for (const port of [PORT, ...EXTRA_PORTS]) {
      await expect(panel.getByText(`127.0.0.1:${port}`)).toBeVisible();
    }

    // SW3 — connectedness, not just row presence. The SPA's origin is
    // 127.0.0.1:4737, so the two extra daemons are CROSS-ORIGIN; their
    // /api/events streams are only readable because the daemon now
    // serves loopback-origin CORS on reads. Pre-SW3 those rows rendered
    // but sat "disconnected" forever — which this spec never caught.
    await expect
      .poll(
        () =>
          page.evaluate(() =>
            (
              window as unknown as {
                __KB_SSE__?: { status(): { daemons: { phase: string }[] } };
              }
            ).__KB_SSE__
              ?.status()
              .daemons.map((d) => d.phase)
              .sort(),
          ),
        { timeout: 20_000 },
      )
      .toEqual(["idle", "idle", "idle"]);
  });
});
