import { test, expect, type Page } from "@playwright/test";
import { spawn, ChildProcess } from "node:child_process";
import { mkdtempSync, copyFileSync, mkdirSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

// SW2 — the SharedWorker SSE transport. Spawns a DEDICATED daemon (port
// 4742) so the `sse_subscribers` gauge (SW1) is hermetic: nothing else in
// the suite connects to it, so the expected consumer counts are exact.
//
// What's asserted:
//   1. Two same-context pages both ride the "shared" transport, and the
//      daemon sees ONE worker connection between them (gauge = worker +
//      the Node-side probe = 2). Closing both pages kills the worker and
//      converges the gauge to the probe alone (1).
//   2. The kill-switch (`kb:sse:transport = "direct"`) forces the in-tab
//      fallback transport, which still connects and receives events —
//      the fallback is load-bearing (browsers without SharedWorker,
//      Playwright request interception) and must not rot.

const PORT = 4742;
const BASE = `http://127.0.0.1:${PORT}`;

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

let daemon: ChildProcess | null = null;

async function waitForDaemon(timeoutMs: number): Promise<boolean> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    try {
      const r = await fetch(`${BASE}/api/identity`);
      if (r.ok) return true;
    } catch {
      // not yet up
    }
    await new Promise((r) => setTimeout(r, 200));
  }
  return false;
}

test.beforeAll(async () => {
  const tmp = mkdtempSync(join(tmpdir(), "kb-sse-shared-"));
  const source = join(tmp, "corpus");
  mkdirSync(source, { recursive: true });
  for (const f of CANON_FILES) copyFileSync(join(CANON_DIR, f), join(source, f));
  const cfg = join(tmp, "kb.toml");
  writeFileSync(
    cfg,
    `[daemon]
name = "sse-shared"

[server]
addr = "127.0.0.1:${PORT}"

[defaults]
disable_embedder_fallback = true

[kb.canon]
path = "${source}"
`,
    "utf-8",
  );
  for (const sub of ["state", "config", "cache"]) mkdirSync(join(tmp, sub));
  daemon = spawn(KB_SERVER_BIN, ["--config", cfg], {
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
  // spawn/handshake), so 15s was needlessly tight and 30s is comfortable.
  const ok = await waitForDaemon(30_000);
  if (!ok) {
    daemon.kill("SIGTERM");
    throw new Error(`sse-shared daemon on :${PORT} never came up`);
  }
  await new Promise((r) => setTimeout(r, 1000)); // give the indexer a beat
});

test.afterAll(async () => {
  daemon?.kill("SIGTERM");
  daemon = null;
});

/// Read the daemon's metrics.tick stream until `sse_subscribers` reports
/// `target` (or the deadline passes — returns the last value seen). The
/// probe connection itself counts as one consumer; targets include it.
async function pollGauge(target: number, timeoutMs: number): Promise<number | null> {
  const ac = new AbortController();
  const deadline = setTimeout(() => ac.abort(), timeoutMs);
  let last: number | null = null;
  try {
    const res = await fetch(`${BASE}/api/events?types=metrics.tick`, {
      signal: ac.signal,
    });
    if (!res.ok || !res.body) return null;
    const reader = res.body.getReader();
    const dec = new TextDecoder();
    let buf = "";
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      buf += dec.decode(value, { stream: true });
      for (const line of buf.split("\n")) {
        const m = line.match(/"sse_subscribers":(\d+)/);
        if (m) last = Number(m[1]);
      }
      if (last === target) return last;
    }
  } catch {
    // aborted (deadline) or network error — fall through with `last`
  } finally {
    clearTimeout(deadline);
    ac.abort();
  }
  return last;
}

function transportOf(page: Page): Promise<string | undefined> {
  return page.evaluate(
    () =>
      (window as unknown as { __KB_SSE__?: { transport(): string } }).__KB_SSE__?.transport(),
  );
}

function phaseOf(page: Page): Promise<string | undefined> {
  return page.evaluate(
    () =>
      (
        window as unknown as {
          __KB_SSE__?: { status(): { daemons: { phase: string }[] } };
        }
      ).__KB_SSE__?.status().daemons[0]?.phase,
  );
}

test.describe("shared SSE transport", () => {
  // invariant:24
  test("two same-context pages share one worker connection; worker dies with its last page", async ({
    context,
  }) => {
    const page1 = await context.newPage();
    await page1.goto(`${BASE}/`);
    await expect.poll(() => transportOf(page1)).toBe("shared");
    await expect
      .poll(() => phaseOf(page1), { timeout: 10_000 })
      .not.toBe("disconnected");

    const page2 = await context.newPage();
    await page2.goto(`${BASE}/`);
    await expect.poll(() => transportOf(page2)).toBe("shared");
    await expect
      .poll(() => phaseOf(page2), { timeout: 10_000 })
      .not.toBe("disconnected");

    // Both pages live: ONE worker stream + the probe = 2 consumers.
    expect(await pollGauge(2, 15_000)).toBe(2);

    // Close both pages: the browser terminates the SharedWorker with its
    // last client, the daemon notices the dead socket on a tick write,
    // and the gauge converges to the probe alone.
    await page2.close();
    await page1.close();
    expect(await pollGauge(1, 20_000)).toBe(1);
  });

  // invariant:24
  test("kill-switch forces the direct in-tab transport, which still connects", async ({
    context,
  }) => {
    const page = await context.newPage();
    await page.addInitScript(() =>
      localStorage.setItem("kb:sse:transport", "direct"),
    );
    await page.goto(`${BASE}/`);
    await expect.poll(() => transportOf(page)).toBe("direct");
    await expect
      .poll(() => phaseOf(page), { timeout: 10_000 })
      .not.toBe("disconnected");
    await page.close();
  });
});
