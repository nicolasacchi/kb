// T25 - the terminal stack, end to end: a REAL tmux 3.7c holder, the compiled
// tap, kb-termd, three kb-server processes and the real SPA, driven by
// synthetic fake harnesses. Runs only in the opt-in `terminal-stack` projects
// (KB_E2E_TERMINAL=1, the `term-e2e` CI job); see terminal-stack/README.md.
//
// Every test launches its own session through the SPA's own REST route and
// cleans it up, so a retry never depends on earlier state. The restart test
// is serial and last. All content is synthetic (numbered lines, generated
// PNGs); no real harness, session, image or credential is involved.

import { test, expect, devices, type Page, type TestInfo } from "@playwright/test";
import { spawn, spawnSync } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { decodePng, forceDom, pixel, type Rgb } from "./spa-terminal-gfx-lib";
import {
  Api, MAIN, OFF, TTL, alive, procStateOf, PORT_MAIN, PORT_TTL, PORT_OFF,
  beat, bindSession, collectPageErrors, decodeStream, holderDir, idleHarness, keepBound,
  firstTextAt, killLaunched, kittyHarness, launch, listTerms, mintTicket, newSid, numberedHarness, openPanel,
  openPanelWhen, panelAttrs, rawUpgrade, readExpected, readMark, recordSockets, repoRoot, root, scratch,
  runnerRpc, sha256, tmuxBin, touch, waitDone, waitFile, waitPhase, wideHarness,
  type Harness, type Launched, type Sock,
} from "./terminal-stack/stack";
import { checkNumbered, describeNumbered, frameTracker, scanForQueries } from "./terminal-stack/oracle";
import { setupTerminalStack } from "./terminal-stack/global-setup-terminal";

// ------------------------------------------------------------ shared bits

let artifactSocks: Sock[] = [];

test.afterEach(async ({ request }, testInfo: TestInfo) => {
  // On failure keep the raw bytes the browser was handed (CI uploads this dir).
  if (testInfo.status !== testInfo.expectedStatus && artifactSocks.length > 0) {
    try {
      const dir = join(repoRoot(), "tests", "e2e", "test-results", "term-e2e-artifacts");
      mkdirSync(dir, { recursive: true });
      const slug = testInfo.title.replace(/[^a-z0-9]+/gi, "-").slice(0, 60);
      artifactSocks.forEach((s, i) => {
        const d = decodeStream(s.rx);
        writeFileSync(join(dir, `${slug}.sock${i}.rx.bin`), d.all);
        writeFileSync(join(dir, `${slug}.sock${i}.texts.json`), JSON.stringify({ url: s.url.replace(/ticket=[0-9a-f]+/, "ticket=X"), texts: d.texts }, null, 1));
      });
    } catch {
      // artifacts are best effort
    }
  }
  artifactSocks = [];
  await killLaunched(new Api(request));
});

interface Ctx {
  api: Api;
  term: Launched;
  sid: string;
  h: Harness;
  socks: Sock[];
  errs: string[];
}

/** Launch the scenario, bind a synthetic captured session to it, start recording sockets. */
async function prepare(
  page: Page,
  request: import("@playwright/test").APIRequestContext,
  h: Harness,
  grid?: { cols: number; rows: number },
): Promise<Ctx> {
  const api = new Api(request);
  const term = await launch(api, h, grid);
  const sid = newSid();
  await bindSession(api, sid, term);
  const socks = recordSockets(page);
  artifactSocks = socks;
  const errs = collectPageErrors(page);
  return { api, term, sid, h, socks, errs };
}

const range = (a: number, b: number): number[] => Array.from({ length: b - a + 1 }, (_, i) => a + i);
const dump = (nums: number[]): string => nums.map((n) => `L${String(n).padStart(6, "0")}`).join("\n") + "\nDONE\n";

// ============================================================ the oracles

test.describe("T25 oracle self-tests", () => {
  test("oracle self-tests: a dump that stops at 2714 of 3300 FAILS (highest != total)", () => {
    // Every line of 1..2714 is present once and in order: only `highest != total` can fail it.
    const r = checkNumbered(dump(range(1, 2714)), { expectTotal: 3300 });
    expect(r.duplicates).toEqual([]);
    expect(r.inOrder).toBe(true);
    expect(r.highest).toBe(2714);
    expect(r.ok).toBe(false);
  });
  test("oracle self-tests: one duplicate, one missing and one out-of-order line each FAIL", () => {
    const dup = checkNumbered(dump([...range(1, 50), 50, ...range(51, 100)]), { expectTotal: 100 });
    expect(dup.duplicates).toEqual([50]);
    expect(dup.ok).toBe(false);
    const gap = checkNumbered(dump([...range(1, 49), ...range(51, 100)]), { expectTotal: 100 });
    expect(gap.missing).toEqual([50]);
    expect(gap.ok).toBe(false);
    const swapped = range(1, 100);
    [swapped[9], swapped[10]] = [swapped[10], swapped[9]];
    const ooo = checkNumbered(dump(swapped), { expectTotal: 100 });
    expect(ooo.inOrder).toBe(false);
    expect(ooo.ok).toBe(false);
  });
  test("oracle self-tests: a complete dump passes, ignoring CSI, markers and prompts", () => {
    const text = `$ run\n\x1b[1mL000001\x1b[0m\nREBUILD-SOON\n${range(2, 99).map((n) => `L${String(n).padStart(6, "0")}`).join("\n")}\nL000100\nDONE\n`;
    const r = checkNumbered(text, { expectTotal: 100 });
    expect(r.ok, describeNumbered(r)).toBe(true);
    expect(checkNumbered(text, { expectTotal: 101 }).ok).toBe(false);
  });
  test("oracle self-tests: scanForQueries finds every query shape and nothing in plain output", () => {
    const E = "\x1b";
    const shapes = [
      `${E}[c`, `${E}[>c`, `${E}[>0q`, `${E}[5n`, `${E}[6n`, `${E}[?2026$p`, `${E}[?u`, `${E}[16t`,
      `${E}]10;?${E}\\`, `${E}]11;?${E}\\`, `${E}Ptmux;${E}${E}[c${E}\\`,
    ];
    for (const s of shapes) {
      expect(scanForQueries(Buffer.from(`text ${s} text`)).length, JSON.stringify(s)).toBeGreaterThan(0);
    }
    expect(scanForQueries(Buffer.from(`${E}[1mhello${E}[0m\n${E}[?2026h${E}[H${E}[2J${E}]10;rgb:ff/ff/ff${E}\\`))).toEqual([]);
  });
  test("oracle self-tests: frameTracker sees a stream that starts inside a ?2026 frame", () => {
    const E = "\x1b";
    expect(frameTracker(Buffer.from(`${E}[?2026hx${E}[?2026l`))).toMatchObject({ startsInsideFrame: false, opens: 1, closes: 1, endDepth: 0 });
    expect(frameTracker(Buffer.from(`tail${E}[?2026l more ${E}[?2026hx${E}[?2026l`)).startsInsideFrame).toBe(true);
    expect(frameTracker(Buffer.from(`${E}[?2026hx`)).endDepth).toBe(1);
  });
});

test.describe("T25 setup robustness", () => {
  // The setup returns no teardown closure on failure, so without its own
  // cleanup whatever it had started would outlive the throw. One injection
  // per phase: the root laid out, kb-termd up, every server up.
  for (const point of ["after-root", "after-termd", "after-servers"] as const) {
    test(`a bring-up that fails ${point} tears down what it started`, async () => {
      test.setTimeout(150_000);
      const logs = mkdtempSync(join(tmpdir(), "kbte-logs-"));
      let err: (Error & { root?: string }) | undefined;
      try {
        // Ports of their own: the real stack is already listening on 4737-4739.
        await setupTerminalStack({ injectFailure: point, keepLogsIn: logs, ports: { main: 4747, ttl: 4748, off: 4749 } });
      } catch (e) {
        err = e as Error & { root?: string };
      }
      expect(err?.message, "the injected failure surfaced").toContain("injected bring-up failure");
      const root = err?.root;
      expect(root, "the failure carries the bring-up root").toBeTruthy();
      const strays = spawnSync("pgrep", ["-f", root!], { encoding: "utf-8" }).stdout.trim();
      expect(strays, "processes naming the failed bring-up's root").toBe("");
      expect(existsSync(root!), "the failed bring-up's root was removed").toBe(false);
      if (point !== "after-root") expect(readdirSync(logs), "the failed bring-up's logs were kept").toContain("kb-termd.log");
      if (point === "after-servers") {
        expect(readdirSync(logs), "every server's log was kept").toEqual(
          expect.arrayContaining(["kb-server-4747.log", "kb-server-4748.log", "kb-server-4749.log"]),
        );
      }
    });
  }

  test("a zombie counts as dead for the cleanup, a sleeping process does not", async () => {
    expect(procStateOf("123 (sh) Z 1 123 123 0 -1")).toBe("Z");
    expect(procStateOf("123 (we ird) name) S 1 123")).toBe("S");
    expect(procStateOf("garbage")).toBeNull();
    test.skip(process.platform !== "linux", "needs /proc");
    // A real zombie: the child exits and its parent never waits for it.
    const parent = spawn("python3", ["-I", "-c", "import os,time\npid=os.fork()\nif pid==0:\n    os._exit(0)\nprint(pid, flush=True)\ntime.sleep(60)"], { stdio: ["ignore", "pipe", "ignore"] });
    try {
      const pid = await new Promise<number>((resolve, reject) => {
        const timer = setTimeout(() => reject(new Error("no child pid from the zombie parent")), 15_000);
        parent.stdout!.once("data", (d) => {
          clearTimeout(timer);
          resolve(Number(String(d).trim()));
        });
      });
      await expect
        .poll(() => procStateOf(readFileSync(`/proc/${pid}/stat`, "latin1")), { timeout: 10_000 })
        .toBe("Z");
      expect(() => process.kill(pid, 0), "control: kill(0) alone still reports the zombie alive").not.toThrow();
      expect(alive(pid), "alive() treats the zombie as dead").toBe(false);
      expect(alive(parent.pid!), "alive() still sees a running process").toBe(true);
    } finally {
      parent.kill("SIGKILL");
    }
  });
});

// ================================================== scrollback (Spec 5)

for (const g of [
  { name: "default grid", grid: undefined as { cols: number; rows: number } | undefined },
  { name: "201x48", grid: { cols: 201, rows: 48 } },
]) {
  test(`scrollback complete once after 3 rebuilds, ${g.name}`, async ({ page, request }) => {
    test.setTimeout(240_000);
    const gate = join(scratch("gate"), "go");
    const h = numberedHarness({ gate, seed: 3000, live: 400, rebuildEvery: 100, rate: 150 }, g.grid ?? { cols: 120, rows: 40 });
    const c = await prepare(page, request, h, g.grid);
    await openPanel(page, c.sid);
    // The viewer is live: only now does the harness start its live phase, so
    // every rebuild below happens AFTER the join.
    touch(gate);
    await waitFile(join(h.out, "expected.json"), 120_000);
    const exp = readExpected(h.out);
    expect(exp.rebuilds, "framed rebuilds after the viewer joined").toBeGreaterThanOrEqual(3);
    const text = await waitDone(page);
    const r = checkNumbered(text, { expectTotal: exp.total });
    expect(r.ok, describeNumbered(r)).toBe(true);
    expect(c.errs).toEqual([]);
  });
}

test("late join between rebuilds sees every line once, and the join is proven to land between two rebuilds", async ({ page, request }) => {
  test.setTimeout(240_000);
  // 12 lines/s: ~8 s between rebuilds. The page is loaded first and the click
  // (the join) waits for the harness's own frame-end-1 mark, so the join lands
  // right after rebuild 1's frame and ~8 s before rebuild 2's marker.
  const h = numberedHarness({ seed: 3000, live: 400, rebuildEvery: 100, rate: 12, leadMs: 800 });
  const c = await prepare(page, request, h);
  await openPanelWhen(page, c.sid, () => waitFile(join(h.out, "frame-end-1"), 120_000));
  const joinAt = c.socks[0].openedAt;
  const end1 = readMark(h.out, "frame-end-1");
  expect(end1, "the harness stamped the end of rebuild 1").not.toBeNull();
  expect(joinAt, "the join was requested after rebuild 1's frame ended").toBeGreaterThanOrEqual(end1!);
  await waitFile(join(h.out, "expected.json"), 180_000);
  // Read after the run finished so rebuild 2's marker certainly exists: an
  // absent mark is a failure, never a vacuous pass of the upper bound.
  const soon2 = readMark(h.out, "soon-2");
  expect(soon2, "the harness stamped rebuild 2's soon-2 marker").not.toBeNull();
  expect(soon2!, `the join (${joinAt}) preceded rebuild 2's marker (${soon2})`).toBeGreaterThan(joinAt);
  const exp = readExpected(h.out);
  const text = await waitDone(page);
  const r = checkNumbered(text, { expectTotal: exp.total });
  expect(r.ok, describeNumbered(r)).toBe(true);
});

test("join waits for the frame to end: a join requested INSIDE a ?2026 frame is seeded after it, outside any frame", async ({ page, request }) => {
  test.setTimeout(240_000);
  // A slow rebuild (16 chunks x 250 ms) keeps the synchronized frame open ~4 s.
  // The page is loaded first; the click (the join) is released only by the
  // harness's own frame-start-1 mark, so the join request lands inside the frame.
  const gate = join(scratch("gate"), "go");
  const h = numberedHarness({ gate, seed: 3000, live: 300, rebuildEvery: 100, rate: 60, leadMs: 300, slowRebuildMs: 250 });
  const c = await prepare(page, request, h);
  await openPanelWhen(page, c.sid, async () => {
    touch(gate);
    await waitFile(join(h.out, "frame-start-1"), 120_000);
  });
  await waitFile(join(h.out, "frame-end-1"), 60_000);
  const start1 = readMark(h.out, "frame-start-1")!;
  const end1 = readMark(h.out, "frame-end-1")!;
  expect(end1 - start1, "the slow frame really stayed open").toBeGreaterThan(2000);
  // The join: the first socket that was answered with a seed.
  const joined = c.socks.find((s) => decodeStream(s.rx).texts.some((t) => t.t === "seed"));
  expect(joined, "a socket received a seed").toBeDefined();
  const joinAt = joined!.openedAt;
  expect(joinAt, "the join was requested after the frame opened").toBeGreaterThanOrEqual(start1);
  expect(joinAt, "the join was requested before the frame closed").toBeLessThan(end1);
  const seedAt = firstTextAt(joined!, "seed")!;
  // 250 ms of slack: the harness stamps frame-end just after it flushes.
  expect(seedAt, `the seed (${seedAt}) waited for the frame to end (${end1})`).toBeGreaterThanOrEqual(end1 - 250);
  await waitFile(join(h.out, "expected.json"), 180_000);
  const exp = readExpected(h.out);
  const text = await waitDone(page);
  const r = checkNumbered(text, { expectTotal: exp.total });
  expect(r.ok, describeNumbered(r)).toBe(true);
  for (const s of c.socks) {
    const d = decodeStream(s.rx);
    if (!d.texts.some((t) => t.t === "seed")) continue; // a resume has no seed to check
    const f = frameTracker(d.live);
    expect(f.startsInsideFrame, `post-seed data of ${s.url.replace(/ticket=\w+/, "")} began inside a frame`).toBe(false);
  }
});

// ==================================== queries and replies (Spec 6)

test("no query bytes reach the browser", async ({ page, request }) => {
  test.setTimeout(240_000);
  const gate = join(scratch("gate"), "go");
  const h = numberedHarness({ gate, seed: 3000, live: 400, rebuildEvery: 100, rate: 150, queries: true });
  const c = await prepare(page, request, h);
  await openPanel(page, c.sid);
  touch(gate);
  await waitFile(join(h.out, "expected.json"), 120_000);
  const exp = readExpected(h.out);
  // Vacuous-pass guard: the harness really did send queries.
  expect(exp.queries_sent, "queries the harness injected").toBeGreaterThan(0);
  await waitDone(page);
  let total = 0;
  for (const s of c.socks) {
    const d = decodeStream(s.rx);
    total += d.all.length;
    const left = scanForQueries(d.all);
    expect(left.map((a) => a.q), "queries left in the bytes the browser received").toEqual([]);
  }
  expect(total, "bytes the browser received").toBeGreaterThan(10_000);
});

test("the SPA sends no terminal replies", async ({ page, request }) => {
  test.setTimeout(240_000);
  const gate = join(scratch("gate"), "go");
  const h = numberedHarness({ gate, seed: 600, live: 200, rebuildEvery: 100, rate: 100, queries: true });
  const c = await prepare(page, request, h);
  await openPanel(page, c.sid);
  touch(gate);
  await waitFile(join(h.out, "expected.json"), 120_000);
  expect(readExpected(h.out).queries_sent).toBeGreaterThan(0);
  await waitDone(page);
  await page.locator("[data-kb-term]").click();
  await page.keyboard.type("echo hi");
  await page.keyboard.press("Enter");
  const replyShape = /\x1b\[[?>]?[\d;]*c|\x1b\[[\d;]*R|\x1bP>?\||\$y|\x1b\]1[01];|\x1b\[\?[\d;]*u/;
  const typed: string[] = [];
  await expect
    .poll(() => {
      typed.length = 0;
      for (const s of c.socks) {
        for (const f of s.tx) {
          const o = JSON.parse(typeof f === "string" ? f : f.toString("latin1")) as { t: string; data?: string };
          expect(o.t, "the only client frame this SPA sends here").toBe("input");
          expect(o.data ?? "", "a terminal reply in what the SPA sent").not.toMatch(replyShape);
          typed.push(o.data ?? "");
        }
      }
      return typed.join("");
    }, { timeout: 15_000 })
    .toContain("echo hi");
  // The pane DID receive terminal queries and answered them itself: the replies are in the harness's stdin.
  const stdin = readFileSync(join(h.out, "stdin.log"), "latin1");
  expect(stdin, "the pane-side responder answered the injected queries").toMatch(/tmux 3\.7c|\d+;\d+;\d+c/);
});

test("keys sent through the REST composer path reach the pane (control for the stdin observation)", async ({ request }) => {
  test.setTimeout(120_000);
  const api = new Api(request);
  const h = numberedHarness({ seed: 10, live: 10, rebuildEvery: 0, rate: 100 });
  const t = await launch(api, h);
  await waitFile(join(h.out, "expected.json"), 60_000);
  const r = await api.call("POST", `/api/terms/${t.id}/keys`, { data: { keys: ["y", "n", "7"] } });
  expect(r.status(), await r.text()).toBe(200);
  await expect
    .poll(() => (existsSync(join(h.out, "stdin.log")) ? readFileSync(join(h.out, "stdin.log"), "latin1") : ""), { timeout: 15_000 })
    .toContain("yn7");
});

test("typed keys sent over the WebSocket reach the pane", async ({ page, request }) => {
  // The panel sends client `input` frames over the WebSocket. The tap has no
  // input path (it ignores them), so the runner's stream splice takes them out
  // and writes them to this stream's hidden tmux client (FXINPUT): what the
  // operator types in the panel reaches the harness's stdin.
  test.setTimeout(180_000);
  const gate = join(scratch("gate"), "go");
  const h = numberedHarness({ gate, seed: 100, live: 50, rebuildEvery: 0, rate: 100 });
  const c = await prepare(page, request, h);
  await openPanel(page, c.sid);
  touch(gate);
  await waitDone(page);
  await page.locator("[data-kb-term]").click();
  await page.keyboard.type("echo hi");
  await page.keyboard.press("Enter");
  await expect
    .poll(() => (existsSync(join(h.out, "stdin.log")) ? readFileSync(join(h.out, "stdin.log"), "latin1") : ""), { timeout: 10_000 })
    .toContain("echo hi");
});

// =============================================== pictures (Spec 7)

const KITTY_ID = 0x66cf38;
// Per-channel tolerance for the centre-row probe: the spec's 2 (T25 shipped 3).
// The probed pixels are checker-square centres (flat colour). Every run
// annotates the worst delta it measured, so a tolerance that is too tight (or
// loose) shows up as data, not as a guess.
// Per-channel tolerance for the centre-row pixel check. The `pixel-delta`
// annotations of a green CI run (PR #301) measured worst deltas of 2 at DPR 1
// (two tests) and 1 at DPR 2: a tolerance of 2 sits exactly on the measured
// value, with no headroom for a runner's GPU/software-raster variation, so it
// stays at 3 (the value the lane was specified with).
const PIXEL_TOL = 3;

async function pictureChecks(page: Page, api: Api, c: Ctx, dpr: number, placements = 1): Promise<{ sha: string }> {
  expect(await page.evaluate(() => window.devicePixelRatio)).toBe(dpr);
  // Let the view box grow with its content so the whole picture can be probed.
  await page.addStyleTag({ content: ".kb-term__view{height:auto !important;overflow:visible !important;}" });
  await expect(page.locator(".xterm-screen .kbgfx-overlay")).toHaveCount(1, { timeout: 30_000 });
  await expect(page.locator(".kbgfx-overlay .kbgfx-img")).toHaveCount(placements, { timeout: 30_000 });
  await page.waitForFunction(
    () => {
      const i = document.querySelector<HTMLImageElement>(".kbgfx-img");
      return !!i && i.complete && i.naturalWidth > 0 && document.querySelectorAll(".kbgfx-pending,.kbgfx-missing").length === 0;
    },
    null,
    { timeout: 30_000 },
  );

  // imagedef frames: one distinct id, and the bytes the media route serves hash to the announced sha.
  const defs = c.socks.flatMap((s) => decodeStream(s.rx).texts.filter((t) => t.t === "imagedef"));
  expect(defs.length).toBeGreaterThan(0);
  expect(new Set(defs.map((d) => Number(d.id)))).toEqual(new Set([KITTY_ID]));
  const shas = new Set(defs.map((d) => String(d.sha256)));
  expect(shas.size).toBe(1);
  const sha = [...shas][0];
  const media = await api.call("GET", `/api/terms/media/${sha}`);
  expect(media.status()).toBe(200);
  expect(media.headers()["content-type"]).toBe("image/png");
  expect(media.headers()["x-content-type-options"]).toBe("nosniff");
  const body = await media.body();
  expect(body.subarray(0, 8).toString("hex")).toBe("89504e470d0a1a0a");
  expect(sha256(body)).toBe(sha);

  // Pixels: compare the drawn picture with the served PNG along the centre row
  // (checker-square centres, away from edges), with a gradient so blank/black
  // fails. The renderer fills the picture's cell box and crops what does not
  // fit when the viewer's real cell aspect differs from the tap's cell_px
  // (9x18), so only the centre row is expected to match exactly; the vertical
  // gradient is checked as monotonic instead of pixel-exact.
  const src = decodePng(body);
  const img = page.locator(".kbgfx-img").first();
  await img.scrollIntoViewIfNeeded();
  const shot = decodePng(await img.screenshot());
  const near = (a: Rgb | null, b: Rgb | null, tol: number): boolean =>
    !!a && !!b && a.every((v, i) => Math.abs(v - b[i]) <= tol);
  const bad: string[] = [];
  let worst = 0;
  const y = 50;
  for (const x of [30, 130, 210]) {
    if (x >= src.w || y >= src.h) continue;
    const want = pixel(src, x, y);
    const got = pixel(shot, (x / src.w) * shot.w, (y / src.h) * shot.h);
    expect(Math.max(...(want ?? [0])), "reference pixel is not black").toBeGreaterThan(100);
    if (!near(got, want, PIXEL_TOL)) bad.push(`(${x},${y}) got ${JSON.stringify(got)} want ${JSON.stringify(want)}`);
    if (got && want) worst = Math.max(worst, ...want.map((v, i) => Math.abs(v - got[i])));
  }
  test.info().annotations.push({ type: "pixel-delta", description: `worst per-channel delta on the centre row, DPR ${dpr}: ${worst} (tolerance ${PIXEL_TOL})` });
  expect(bad, "drawn pixels that differ from the served PNG").toEqual([]);
  const g = [0.25, 0.5, 0.75].map((f) => pixel(shot, 0.5 * shot.w, f * shot.h)?.[1] ?? -1);
  expect(g[0], `green rises down the picture: ${g.join(",")}`).toBeGreaterThanOrEqual(0);
  expect(g[1]).toBeGreaterThan(g[0] + 20);
  expect(g[2]).toBeGreaterThan(g[1] + 20);

  // No tofu: a 4 css px band right beside the picture box is plain background.
  const box = await img.boundingBox();
  expect(box).not.toBeNull();
  const b = box!;
  const band = decodePng(await page.screenshot({ clip: { x: b.x + b.width + 2, y: b.y + 2, width: 4, height: Math.max(4, b.height - 4) } }));
  const bg = pixel(decodePng(await page.screenshot({ clip: { x: b.x + b.width + 40, y: b.y + 2, width: 4, height: 4 } })), 1, 1);
  let nonBg = 0;
  for (let y = 0; y < band.h; y += 1) for (let x = 0; x < band.w; x += 1) if (!near(pixel(band, x, y), bg, 2)) nonBg += 1;
  expect(nonBg, "non-background pixels in the band beside the picture").toBe(0);
  return { sha };
}

function storeImgFiles(id: string): string[] {
  const dir = join(holderDir(), id, "store", "img");
  return existsSync(dir) ? readdirSync(dir) : [];
}

test("pictures draw with correct pixels at DPR 1", async ({ page, request }) => {
  test.setTimeout(180_000);
  await forceDom(page);
  const c = await prepare(page, request, kittyHarness());
  await openPanel(page, c.sid);
  await pictureChecks(page, c.api, c, 1);
  expect(storeImgFiles(c.term.id)).toHaveLength(1);
});

test("pictures draw with correct pixels at DPR 2 @dpr2", async ({ page, request }) => {
  test.setTimeout(180_000);
  await forceDom(page);
  const c = await prepare(page, request, kittyHarness());
  await openPanel(page, c.sid);
  await pictureChecks(page, c.api, c, 2);
});

test("rebuild retransmit keeps ids stable and the store deduped", async ({ page, request }) => {
  test.setTimeout(180_000);
  await forceDom(page);
  const gate = join(scratch("gate"), "go");
  // The picture AND its a=d,d=A / full retransmit under the same id arrive live.
  const c = await prepare(page, request, kittyHarness({ resizeReplay: true, gate }));
  await openPanel(page, c.sid);
  touch(gate);
  const { sha } = await pictureChecks(page, c.api, c, 1, 2);
  const defs = c.socks.flatMap((s) => decodeStream(s.rx).texts.filter((t) => t.t === "imagedef"));
  expect(new Set(defs.map((d) => Number(d.id)))).toEqual(new Set([KITTY_ID]));
  expect(new Set(defs.map((d) => String(d.sha256)))).toEqual(new Set([sha]));
  expect(storeImgFiles(c.term.id), "the same bytes are stored once").toEqual([sha]);
  // --resize-replay prints the placeholder block twice (the replay re-places the same id): two placements, one id, one stored file.
  expect(await page.locator(".kbgfx-img").count()).toBe(2);
});

// ================================================ ResumeFrom (Spec 8)

interface Conn {
  url: string;
  at: number;
  ws: import("@playwright/test").WebSocketRoute;
  server: import("@playwright/test").WebSocketRoute;
  serverClose?: { code: number | undefined; at: number };
  rx: Array<string | Buffer>;
}

/** A transparent proxy for the terminal sockets that can cut them and see the server's close code. */
async function proxySockets(page: Page): Promise<Conn[]> {
  const conns: Conn[] = [];
  await page.routeWebSocket(/\/api\/terms\/[^/]+\/ws/, (ws) => {
    const server = ws.connectToServer();
    const c: Conn = { url: ws.url(), at: Date.now(), ws, server, rx: [] };
    conns.push(c);
    ws.onMessage((m) => server.send(m));
    server.onMessage((m) => {
      c.rx.push(m);
      ws.send(m);
    });
    ws.onClose((code, reason) => {
      void server.close({ code, reason });
    });
    server.onClose((code, reason) => {
      c.serverClose = { code, at: Date.now() };
      void ws.close({ code, reason });
    });
  });
  return conns;
}

function cut(c: Conn): void {
  void c.server.close();
  void c.ws.close({ code: 1001, reason: "e2e forced drop" });
}

const fromOf = (c: Conn): number | null => {
  const v = new URL(c.url).searchParams.get("from");
  return v === null ? null : Number(v);
};

const epochOf = (c: Conn): string | null => new URL(c.url).searchParams.get("epoch");

/** The tap epoch a connection's hello announced (null before the hello arrives). */
const helloEpoch = (c: Conn): string | null => {
  const hello = decodeStream(c.rx).texts.find((t) => t.t === "hello");
  return hello === undefined ? null : String((hello as { epoch?: unknown }).epoch ?? "");
};

test("ResumeFrom after 3 forced drops, backoff bounded, scrollback complete once", async ({ page, request }) => {
  test.setTimeout(300_000);
  const conns = await proxySockets(page);
  const gate = join(scratch("gate"), "go");
  const h = numberedHarness({ gate, seed: 3000, live: 900, rebuildEvery: 0, rate: 50 });
  const c = await prepare(page, request, h);
  await openPanel(page, c.sid);
  touch(gate);
  for (let i = 0; i < 3; i += 1) {
    await new Promise((r) => setTimeout(r, 1500));
    const before = await panelAttrs(page);
    const n = conns.length;
    const t0 = Date.now();
    cut(conns[n - 1]);
    await expect.poll(() => conns.length, { timeout: 30_000 }).toBeGreaterThan(n);
    const next = conns[n];
    const from = fromOf(next);
    expect(from, `drop ${i + 1}: the reconnect URL carries from=`).not.toBeNull();
    expect(from!, `drop ${i + 1}: from= is the last applied offset`).toBeGreaterThan(0);
    expect(from!).toBeGreaterThanOrEqual(before.offset);
    // FXEPOCH: the reconnect echoes the epoch the previous connection's hello announced.
    const prevEpoch = helloEpoch(conns[n - 1]);
    expect(prevEpoch, `drop ${i + 1}: the dropped connection received a hello`).toBeTruthy();
    expect(epochOf(next), `drop ${i + 1}: the reconnect URL carries epoch=`).toBe(prevEpoch);
    const attempts = conns.filter((x) => x.at >= t0 && x.at - t0 < 1000).length;
    expect(attempts, `drop ${i + 1}: reconnect attempts within 1 s`).toBeLessThanOrEqual(3);
    await waitPhase(page, "live");
    // FXEPOCH: the tap matched the epoch and resumed from its ring: hello and
    // data, never a seed, and the terminal was not reset.
    expect(decodeStream(next.rx).texts.some((t) => t.t === "seed"), `drop ${i + 1}: a resume is not answered with a seed`).toBe(false);
    expect(helloEpoch(next), `drop ${i + 1}: same epoch after the resume`).toBe(prevEpoch);
    expect((await panelAttrs(page)).resets, `drop ${i + 1}: no term reset across the resume`).toBe(before.resets);
  }
  await waitFile(join(h.out, "expected.json"), 180_000);
  const exp = readExpected(h.out);
  const text = await waitDone(page);
  const r = checkNumbered(text, { expectTotal: exp.total });
  expect(r.ok, describeNumbered(r)).toBe(true);
});

test("a resume needs no term reset", async ({ page, request }) => {
  // The SPA echoes the tap epoch (and applied marker seq) on the reconnect URL,
  // the relay and the runner carry it to the tap, and the tap resumes from its
  // ring instead of full-joining.
  test.setTimeout(240_000);
  const conns = await proxySockets(page);
  const gate = join(scratch("gate"), "go");
  const h = numberedHarness({ gate, seed: 3000, live: 400, rebuildEvery: 0, rate: 50 });
  const c = await prepare(page, request, h);
  await openPanel(page, c.sid);
  touch(gate);
  await new Promise((r) => setTimeout(r, 1500));
  const before = await panelAttrs(page);
  const n = conns.length;
  cut(conns[n - 1]);
  await expect.poll(() => conns.length, { timeout: 30_000 }).toBeGreaterThan(n);
  await waitPhase(page, "live");
  expect(epochOf(conns[n]), "the reconnect URL carries the stored epoch").toBeTruthy();
  expect(decodeStream(conns[n].rx).texts.some((t) => t.t === "seed"), "a resume is not answered with a seed").toBe(false);
  const after = await panelAttrs(page);
  expect(after.resets, "term.reset count unchanged across a resume").toBe(before.resets);
});

// ===================================== FUTERMOWN: the grid follows the web

/** The live xterm's size, through the `__kbTerm` test seam. */
async function termSize(page: Page): Promise<{ cols: number; rows: number } | null> {
  return page.evaluate(() => {
    const host = document.querySelector("[data-kb-term]") as unknown as { __kbTerm?: { cols: number; rows: number } } | null;
    const t = host?.__kbTerm;
    return t ? { cols: t.cols, rows: t.rows } : null;
  });
}

/**
 * Where the wide line stands in the NORMAL buffer, row by row (no re-joining of
 * wrapped rows): `ok` = one row holds it whole and the next row is not a
 * continuation; `wrapped` = it is split over rows; `missing` = not drawn yet.
 */
async function wideLine(page: Page, width: number): Promise<"ok" | "wrapped" | "missing"> {
  return page.evaluate((w) => {
    const host = document.querySelector("[data-kb-term]") as unknown as {
      __kbTerm?: { buffer: { normal: { length: number; getLine(y: number): { isWrapped: boolean; translateToString(trim: boolean): string } | undefined } } };
    } | null;
    const buf = host?.__kbTerm?.buffer.normal;
    if (!buf) return "missing" as const;
    const whole = `W${"-".repeat(w - 2)}Z`;
    let sawStart = false;
    for (let i = 0; i < buf.length; i += 1) {
      const t = buf.getLine(i)?.translateToString(true) ?? "";
      if (t === whole) return buf.getLine(i + 1)?.isWrapped ? ("wrapped" as const) : ("ok" as const);
      if (t.startsWith("W--")) sawStart = true;
    }
    return sawStart ? ("wrapped" as const) : ("missing" as const);
  }, width);
}

const takeViaWeb = (api: Api, id: string, cols: number, rows: number) =>
  api.json<{ cols: number; rows: number; owner: string | null }>("POST", `/api/terms/${id}/take-size`, { data: { cols, rows } });

test("FUTERMOWN: a grid change is seen by the OPEN panel without a reconnect, and a 201-column line drawn after it is intact", async ({ page, request }) => {
  test.setTimeout(240_000);
  const gate = join(scratch("gate"), "go");
  const c = await prepare(page, request, wideHarness(gate, 201), { cols: 120, rows: 40 });
  await openPanel(page, c.sid);
  expect(await termSize(page)).toEqual({ cols: 120, rows: 40 });
  const socketsBefore = c.socks.length;
  const r = await takeViaWeb(c.api, c.term.id, 201, 48);
  expect(r.status, JSON.stringify(r.body)).toBe(200);
  await expect.poll(() => termSize(page), { timeout: 30_000 }).toEqual({ cols: 201, rows: 48 });
  await expect(page.locator('[data-testid="kb-term-grid"]')).toContainText("201x48");
  touch(gate);
  await expect.poll(() => wideLine(page, 201), { timeout: 60_000 }).toBe("ok");
  expect(c.socks.length, "the panel did not reconnect").toBe(socketsBefore);
  expect(c.errs).toEqual([]);
});

test("FUTERMOWN: a viewer opened AFTER the grid change starts at the new grid and sees the wide line intact", async ({ page, request }) => {
  test.setTimeout(240_000);
  const gate = join(scratch("gate"), "go");
  const c = await prepare(page, request, wideHarness(gate, 201), { cols: 120, rows: 40 });
  const r = await takeViaWeb(c.api, c.term.id, 201, 48);
  expect(r.status, JSON.stringify(r.body)).toBe(200);
  touch(gate);
  await waitFile(join(c.h.out, "printed"), 60_000);
  await openPanel(page, c.sid);
  expect(await termSize(page), "the profile carries the taken grid").toEqual({ cols: 201, rows: 48 });
  await expect(page.locator('[data-testid="kb-term-grid"]')).toContainText("201x48");
  await expect.poll(() => wideLine(page, 201), { timeout: 60_000 }).toBe("ok");
});

test("FUTERMOWN: a viewer that was away for a grid change resumes into it (the marker replays in order) with the wide line intact", async ({ page, request }) => {
  test.setTimeout(240_000);
  const conns = await proxySockets(page);
  const gate = join(scratch("gate"), "go");
  const c = await prepare(page, request, wideHarness(gate, 201), { cols: 120, rows: 40 });
  await openPanel(page, c.sid);
  // Hold the reconnect at its ticket mint: the viewer is away while the grid
  // changes and the wide line is drawn, then rejoins with its OLD offset.
  let releaseTickets!: () => void;
  const held = new Promise<void>((r) => {
    releaseTickets = r;
  });
  let heldMints = 0;
  await page.route(/\/api\/terms\/[^/]+\/ticket/, async (route) => {
    heldMints += 1;
    await held;
    await route.continue();
  });
  const n = conns.length;
  cut(conns[n - 1]);
  await expect.poll(() => heldMints, { timeout: 30_000 }).toBeGreaterThan(0);
  const r = await takeViaWeb(c.api, c.term.id, 201, 48);
  expect(r.status, JSON.stringify(r.body)).toBe(200);
  touch(gate);
  await waitFile(join(c.h.out, "printed"), 60_000);
  releaseTickets();
  await expect.poll(() => conns.length, { timeout: 30_000 }).toBeGreaterThan(n);
  await waitPhase(page, "live");
  const re = conns[n];
  expect(fromOf(re), "the reconnect carries the pre-change offset").not.toBeNull();
  expect(decodeStream(re.rx).texts.some((t) => t.t === "seed"), "a resume is not answered with a seed").toBe(false);
  await expect.poll(() => termSize(page), { timeout: 30_000 }).toEqual({ cols: 201, rows: 48 });
  await expect.poll(() => wideLine(page, 201), { timeout: 60_000 }).toBe("ok");
});

test("FUTERMOWN: the web take-size succeeds over a local attach owner; a local take never beats the web; release hands the grid back", async ({ request }) => {
  const api = new Api(request);
  const t = await launch(api, idleHarness(), { cols: 120, rows: 40 });
  // Play a local attach (and a stale one: nobody is behind local:9).
  const seed = await runnerRpc("take_size", { id: t.id, cols: 120, rows: 40, owner: "local:9", force: false });
  expect(seed.status, JSON.stringify(seed)).toBe("ok");
  const before = await api.json<{ owner: string | null }>("GET", `/api/terms/${t.id}/grid`);
  expect(before.body.owner).toBe("local:9");
  // The web outranks it, without force and without a 409.
  const took = await takeViaWeb(api, t.id, 90, 30);
  expect(took.status, JSON.stringify(took.body)).toBe(200);
  expect(took.body).toMatchObject({ cols: 90, rows: 30, owner: "kb-term" });
  // A local take is refused even forced, naming the web; nothing moved.
  for (const force of [false, true]) {
    const refused = await runnerRpc("take_size", { id: t.id, cols: 120, rows: 40, owner: "local:9", force });
    expect(refused.status).toBe("err");
    expect(refused.code).toBe("grid-owned");
    expect(refused.message ?? "").toContain("web panel");
  }
  const mid = await api.json<{ cols: number; rows: number; owner: string | null }>("GET", `/api/terms/${t.id}/grid`);
  expect(mid.body).toMatchObject({ cols: 90, rows: 30, owner: "kb-term" });
  // Between web viewers the latest explicit take wins.
  const again = await takeViaWeb(api, t.id, 100, 32);
  expect(again.status).toBe(200);
  // Release clears only the web owner; then a local take succeeds again.
  const rel = await api.json("POST", `/api/terms/${t.id}/release-size`, { data: {} });
  expect(rel.status).toBe(200);
  const after = await api.json<{ owner: string | null }>("GET", `/api/terms/${t.id}/grid`);
  expect(after.body.owner).toBeNull();
  const local = await runnerRpc("take_size", { id: t.id, cols: 120, rows: 40, owner: "local:9", force: false });
  expect(local.status, JSON.stringify(local)).toBe("ok");
});

test("FUTERMOWN: a phone (Pixel 7, touch) takes the grid from a local attach with 'Fit to phone', no 409", async ({ browser, request }) => {
  test.setTimeout(240_000);
  const { defaultBrowserType: _ignored, ...pixel } = devices["Pixel 7"];
  const ctx = await browser.newContext({ ...pixel });
  try {
    const page = await ctx.newPage();
    const api = new Api(request);
    const term = await launch(api, idleHarness(), { cols: 120, rows: 40 });
    const sid = newSid();
    await bindSession(api, sid, term);
    const seed = await runnerRpc("take_size", { id: term.id, cols: 120, rows: 40, owner: "local:9", force: false });
    expect(seed.status, JSON.stringify(seed)).toBe("ok");
    await page.goto(`${MAIN}/sessions/${sid}`);
    const open = page.locator('[data-testid="kb-term-open"]');
    if (!(await open.isVisible())) await page.locator('[data-testid="session-sec-terminal"] > summary').click();
    await expect(open).toBeVisible({ timeout: 30_000 });
    await open.click();
    const vp = page.locator("[data-kb-term-viewport]");
    await expect(vp).toBeVisible({ timeout: 30_000 });
    await expect(vp).toHaveAttribute("data-kb-term-phase", "live", { timeout: 45_000 });
    const fit = page.locator('[data-testid="kb-drive-fit"]');
    await expect(fit).toBeVisible({ timeout: 30_000 });
    const responded = page.waitForResponse((r) => /\/take-size$/.test(r.url()) && r.request().method() === "POST");
    await fit.click();
    // Another device owning the grid may still ask first (that rule belongs to the SPA): accept it.
    const go = page.locator(".confirm__go");
    await go.waitFor({ state: "visible", timeout: 3_000 }).then(() => go.click(), () => undefined);
    const resp = await responded;
    expect(resp.status(), "no 409 against a local owner").toBe(200);
    const asked = resp.request().postDataJSON() as { cols: number; rows: number };
    const g = await api.json<{ cols: number; rows: number; owner: string | null }>("GET", `/api/terms/${term.id}/grid`);
    expect(g.body).toMatchObject({ cols: asked.cols, rows: asked.rows, owner: "kb-term" });
  } finally {
    await ctx.close();
  }
});

test("aged-out offset forces a full join", async ({ page, request }) => {
  test.setTimeout(900_000);
  const conns = await proxySockets(page);
  const gate = join(scratch("gate"), "go");
  const floodGate = join(scratch("gate"), "flood");
  const h = numberedHarness({ gate, seed: 600, live: 100, rebuildEvery: 0, rate: 100, floodGate, floodBytes: 40_000_000, floodTail: 200 });
  const c = await prepare(page, request, h);
  await openPanel(page, c.sid);
  touch(gate);
  await waitDone(page);
  const before = await panelAttrs(page);
  // Keep the viewer away (no ticket, so no reconnect) while the 32 MiB ring wraps.
  await page.route("**/api/terms/*/ticket", (route) => route.abort());
  const n = conns.length;
  cut(conns[n - 1]);
  touch(floodGate);
  await waitFile(join(h.out, "tail-done"), 400_000);
  await page.unroute("**/api/terms/*/ticket");
  await expect.poll(() => conns.length, { timeout: 60_000 }).toBeGreaterThan(n);
  // Attempts made while the tap is still busy with the flood get no frame (the
  // viewer's first-frame watchdog retries); every one of them sends the stale offset.
  const attempts = conns.slice(n);
  for (const a of attempts) {
    expect(fromOf(a) ?? -1, "every reconnect after the drop sends the stale offset").toBeGreaterThanOrEqual(before.offset);
    // FXEPOCH: the epoch is right; only the offset has aged out, so the tap must full-join.
    expect(epochOf(a), "the stale-offset reconnect still carries the (valid) epoch").toBeTruthy();
  }
  expect(before.offset).toBeGreaterThan(0);
  // The tap parses the whole 40 MB flood before it can answer a join, and a join
  // waits for a quiet frame boundary: allow minutes, not seconds.
  await waitPhase(page, "live", 480_000);
  expect((await panelAttrs(page)).resets, "a full join (seed) reset the terminal").toBeGreaterThan(before.resets);
  const answered = conns.slice(n).filter((a) => decodeStream(a.rx).texts.some((t) => t.t === "seed"));
  expect(answered.length, "the aged-out offset was answered with a seed (a full join)").toBeGreaterThan(0);
  const text = await waitDone(page, "TAIL-DONE", 120_000);
  const r = checkNumbered(text, { expectTotal: 200 });
  expect(r.ok, describeNumbered(r)).toBe(true);
});

// ========================================= beats gate `enter` (Spec 11)

test("beat blocked gives 409 on enter; unblocked releases; wrong pid does not bind; a non-start beat does not rebind", async ({ request }) => {
  test.setTimeout(180_000);
  const api = new Api(request);
  const a = await launch(api, idleHarness());
  const b = await launch(api, idleHarness());
  const sidA = newSid();
  await bindSession(api, sidA, a);
  const rowOf = async (id: string) => (await listTerms(api)).find((t) => t.id === id)!;
  expect((await rowOf(a.id)).session_id).toBe(sidA);

  // blocked -> 409 before the runner is asked
  expect(await beat(request, { sid: sidA, event: "blocked", pid: a.panePid, term: a.id })).toBe(200);
  await expect.poll(async () => (await api.call("POST", `/api/terms/${a.id}/enter`, { data: {} })).status(), { timeout: 15_000 }).toBe(409);
  // unblocked -> released (the runner's quiet gate then writes Enter)
  expect(await beat(request, { sid: sidA, event: "unblocked", pid: a.panePid, term: a.id })).toBe(200);
  await expect.poll(async () => (await api.call("POST", `/api/terms/${a.id}/enter`, { data: {} })).status(), { timeout: 15_000 }).toBe(200);

  // a claim with the right term but the WRONG pid binds nothing
  const sidW = newSid();
  await seedAndBeat(request, api, sidW, "start", b.panePid + 1, b.id);
  await seedAndBeat(request, api, sidW, "blocked", b.panePid + 1, b.id);
  const rowB = await rowOf(b.id);
  expect(rowB.session_id ?? null, "a wrong-pid claim must not bind the terminal").toBeNull();
  expect((await api.call("POST", `/api/terms/${b.id}/enter`, { data: {} })).status(), "enter on an unbound terminal is not gated").toBe(200);

  // another session beating a non-start event for the same pid+term does not steal the binding
  const sidSub = newSid();
  expect(await beat(request, { sid: sidSub, event: "prompt", pid: a.panePid, term: a.id })).toBe(200);
  await listTerms(api);
  expect((await rowOf(a.id)).session_id, "a non-start beat from another session must not rebind").toBe(sidA);
});

async function seedAndBeat(request: import("@playwright/test").APIRequestContext, api: Api, sid: string, event: string, pid: number, term: string): Promise<void> {
  await listTerms(api);
  expect(await beat(request, { sid, event, pid, term })).toBe(200);
}

test("KNOWN LIMIT: a same-pid same-term `start` beat from another session steals the binding (terminal/binding.rs)", async ({ request }) => {
  // binding.rs documents it: `start` is the rebind signal, so anything that
  // beats `start` with the terminal's own pid and term id (a subagent harness
  // is the known source) takes the binding over. This pins the behaviour as it
  // is, so a change to it is a deliberate decision, not a silent drift.
  test.setTimeout(120_000);
  const api = new Api(request);
  const a = await launch(api, idleHarness());
  const sidA = newSid();
  await bindSession(api, sidA, a);
  const rowOf = async () => (await listTerms(api)).find((t) => t.id === a.id)!;
  expect((await rowOf()).session_id).toBe(sidA);
  const sidThief = newSid();
  // Binding times are whole seconds: leave a second so the later claim is strictly newer.
  await new Promise((r) => setTimeout(r, 1100));
  await listTerms(api);
  expect(await beat(request, { sid: sidThief, event: "start", pid: a.panePid, term: a.id })).toBe(200);
  await expect
    .poll(async () => (await rowOf()).session_id, { timeout: 15_000, message: "the same-pid same-term start beat took the binding" })
    .toBe(sidThief);
});

// ============================================= security (Spec 12)

test.describe("T25 gate matrix on the real server", () => {
  test("enabled=false gives a bodiless 404 on every /api/terms route", async ({ request }) => {
    const off = new Api(request, OFF);
    const id = "abcdefgh12";
    const sha = "0".repeat(64);
    const routes: Array<[string, string]> = [
      ["GET", "/api/terms"], ["POST", "/api/terms"], ["GET", `/api/terms/media/${sha}`],
      ...["ticket", "paste", "enter", "keys", "kill", "take-size", "release-size", "resize", "viewer-close"].map((r): [string, string] => ["POST", `/api/terms/${id}/${r}`]),
      ...["read", "history", "grid"].map((r): [string, string] => ["GET", `/api/terms/${id}/${r}`]),
    ];
    for (const [method, path] of routes) {
      const r = await off.call(method, path, { data: method === "POST" ? {} : undefined });
      expect(r.status(), `${method} ${path}`).toBe(404);
      expect((await r.body()).length, `${method} ${path} body`).toBe(0);
    }
    const ws = await rawUpgrade(PORT_OFF, `/api/terms/${id}/ws?ticket=${"a".repeat(32)}`, { Origin: OFF });
    expect(ws.status, "WS upgrade on the disabled server").toBe(404);
  });

  test("POST without the intent header is 403", async ({ request }) => {
    const api = new Api(request);
    const t = await launch(api, idleHarness());
    for (const path of [`/api/terms/${t.id}/ticket`, `/api/terms/${t.id}/enter`, `/api/terms/${t.id}/kill`, "/api/terms"]) {
      const r = await api.call("POST", path, { intent: false, data: {} });
      expect(r.status(), `POST ${path} without intent`).toBe(403);
    }
    expect((await api.call("POST", `/api/terms/${t.id}/ticket`, { data: {} })).status(), "control: with intent").toBe(200);
  });

  test("a wrong Origin is 403 on REST and on the WebSocket upgrade", async ({ request }) => {
    const api = new Api(request);
    const t = await launch(api, idleHarness());
    const evil = `http://evil.test:${PORT_MAIN}`;
    expect((await api.call("POST", `/api/terms/${t.id}/ticket`, { origin: evil, data: {} })).status()).toBe(403);
    expect((await api.call("GET", "/api/terms", { origin: evil })).status()).toBe(403);
    const { ticket } = await mintTicket(api, t.id);
    const refused = await rawUpgrade(PORT_MAIN, `/api/terms/${t.id}/ws?ticket=${ticket}`, { Origin: evil });
    expect(refused.status).toBe(403);
    // the refusal did not burn the ticket: the right Origin still gets in
    const ok = await rawUpgrade(PORT_MAIN, `/api/terms/${t.id}/ws?ticket=${ticket}`, { Origin: MAIN });
    expect(ok.status).toBe(101);
  });

  test("an ABSENT Origin is 403 (loopback gets no bypass) on REST and on the WebSocket upgrade", async ({ request }) => {
    const api = new Api(request);
    const t = await launch(api, idleHarness());
    expect((await api.call("POST", `/api/terms/${t.id}/ticket`, { origin: null, data: {} })).status()).toBe(403);
    expect((await api.call("GET", "/api/terms", { origin: null })).status()).toBe(403);
    expect((await api.call("GET", `/api/terms/${t.id}/read`, { origin: null })).status()).toBe(403);
    const { ticket } = await mintTicket(api, t.id);
    const refused = await rawUpgrade(PORT_MAIN, `/api/terms/${t.id}/ws?ticket=${ticket}`);
    expect(refused.status).toBe(403);
    expect((await rawUpgrade(PORT_MAIN, `/api/terms/${t.id}/ws?ticket=${ticket}`, { Origin: MAIN })).status).toBe(101);
  });

  test("every /api/terms route refuses no intent, a wrong Origin and an absent Origin, and a refusal touches no state", async ({ request }) => {
    test.setTimeout(180_000);
    const api = new Api(request);
    const h = numberedHarness({ seed: 50, live: 10, rebuildEvery: 0, rate: 100 });
    const t = await launch(api, h);
    await waitFile(join(h.out, "expected.json"), 60_000);
    const id = t.id;
    const evil = `http://evil.test:${PORT_MAIN}`;
    const ORIGIN_URN = "urn:kb:errors:terminal-origin-refused";
    const MIDDLEWARE_ORIGIN_URN = "urn:kb:errors:origin-refused";
    const INTENT_URN = "urn:kb:errors:terminal-intent-required";
    const png = Buffer.concat([Buffer.from("89504e470d0a1a0a", "hex"), Buffer.alloc(24)]);
    interface Route {
      name: string;
      method: "GET" | "POST";
      path: string;
      body?: unknown;
      headers?: Record<string, string>;
      /** The media GET: it DOES require the intent header (unlike the other GETs) and an absent Origin is its normal same-origin shape. */
      media?: boolean;
    }
    const routes: Route[] = [
      { name: "list", method: "GET", path: "/api/terms" },
      { name: "launch", method: "POST", path: "/api/terms", body: { harness: "omp", cwd: root() } },
      { name: "media", method: "GET", path: `/api/terms/media/${"0".repeat(64)}`, media: true },
      { name: "ticket", method: "POST", path: `/api/terms/${id}/ticket`, body: {} },
      { name: "paste", method: "POST", path: `/api/terms/${id}/paste`, body: { text: "GATEPASTE" } },
      { name: "enter", method: "POST", path: `/api/terms/${id}/enter`, body: {} },
      { name: "keys", method: "POST", path: `/api/terms/${id}/keys`, body: { keys: ["z", "q", "9"] } },
      { name: "take-size", method: "POST", path: `/api/terms/${id}/take-size`, body: { cols: 90, rows: 30 } },
      { name: "release-size", method: "POST", path: `/api/terms/${id}/release-size`, body: {} },
      { name: "resize", method: "POST", path: `/api/terms/${id}/resize`, body: { cols: 90, rows: 30 } },
      { name: "viewer-close", method: "POST", path: `/api/terms/${id}/viewer-close`, body: {} },
      { name: "upload", method: "POST", path: `/api/terms/${id}/upload`, body: png, headers: { "Content-Type": "image/png" } },
      { name: "read", method: "GET", path: `/api/terms/${id}/read` },
      { name: "history", method: "GET", path: `/api/terms/${id}/history` },
      { name: "grid", method: "GET", path: `/api/terms/${id}/grid` },
      { name: "kill", method: "POST", path: `/api/terms/${id}/kill`, body: {} },
    ];
    const call = (r: Route, o: { origin?: string | null; intent?: boolean } = {}) =>
      api.call(r.method, r.path, { ...o, data: r.body, headers: r.headers });
    const problemType = async (resp: import("@playwright/test").APIResponse): Promise<string> =>
      ((await resp.json()) as { type?: string }).type ?? "";

    // State the refusals must leave alone.
    const rowOf = async () => {
      const row = (await listTerms(api)).find((x) => x.id === id);
      return JSON.stringify({ pane_pid: row?.pane_pid, cols: row?.cols, rows: row?.rows, dead: row?.pane_dead ?? false, stale: row?.stale ?? false });
    };
    const stdinLog = (): string => (existsSync(join(h.out, "stdin.log")) ? readFileSync(join(h.out, "stdin.log"), "latin1") : "");
    const rowBefore = await rowOf();
    const countBefore = (await listTerms(api)).length;
    const stdinBefore = stdinLog();
    const mediaRef = (await call(routes.find((r) => r.media)!)).status();
    expect(mediaRef, "control: the media route with the right Origin and intent is not a gate refusal").not.toBe(403);

    for (const r of routes) {
      if (r.method === "POST" || r.media) {
        const resp = await call(r, { intent: false });
        expect(resp.status(), `${r.name}: no intent`).toBe(403);
        expect(await problemType(resp), `${r.name}: no intent -> the intent refusal`).toBe(INTENT_URN);
      }
      const wrong = await call(r, { origin: evil });
      expect(wrong.status(), `${r.name}: wrong Origin`).toBe(403);
      // A wrong Origin on a POST is stopped one layer earlier, by the daemon's
      // origin allowlist middleware (its own urn); a GET reaches the terminal gate.
      expect(await problemType(wrong), `${r.name}: wrong Origin -> the origin refusal`).toBe(r.method === "POST" ? MIDDLEWARE_ORIGIN_URN : ORIGIN_URN);
      const absent = await call(r, { origin: null });
      if (r.media) {
        // The media GET is same-origin `fetch`, which sends no Origin: absent is admitted (module docs, terminal/media.rs).
        expect(absent.status(), "media: an absent Origin is the normal same-origin shape, answered like the right Origin").toBe(mediaRef);
      } else {
        expect(absent.status(), `${r.name}: absent Origin`).toBe(403);
        expect(await problemType(absent), `${r.name}: absent Origin -> the origin refusal`).toBe(ORIGIN_URN);
      }
    }
    // The WebSocket upgrade: wrong and absent Origin refused with a LIVE ticket in hand.
    // Minted HERE, not before the ~48 calls above: a ticket lives 30 s and the
    // matrix can outlast that on a slow runner.
    const ticket = await mintTicket(api, id);
    expect((await rawUpgrade(PORT_MAIN, `/api/terms/${id}/ws?ticket=${ticket.ticket}`, { Origin: evil })).status, "ws: wrong Origin").toBe(403);
    expect((await rawUpgrade(PORT_MAIN, `/api/terms/${id}/ws?ticket=${ticket.ticket}`)).status, "ws: absent Origin").toBe(403);

    // Nothing the refusals asked for happened.
    expect(await rowOf(), "the terminal's row (pid, size, liveness) is unchanged").toBe(rowBefore);
    expect((await listTerms(api)).length, "no terminal was launched or killed").toBe(countBefore);
    expect(() => process.kill(t.panePid, 0), "the pane survived the refused kill").not.toThrow();
    expect(stdinLog(), "nothing was pasted, typed or entered into the pane").toBe(stdinBefore);
    expect((await rawUpgrade(PORT_MAIN, `/api/terms/${id}/ws?ticket=${ticket.ticket}`, { Origin: MAIN })).status, "the ticket was not spent by any refusal").toBe(101);

    // Control: with the right Origin and intent, no route is a gate refusal
    // (the 403 above came from the gate, not from the route or the body).
    for (const r of routes) {
      const body = r.name === "launch" ? {} : r.body; // an empty launch body is a 400, so the control creates nothing
      const resp = await api.call(r.method, r.path, { data: body, headers: r.headers });
      expect(resp.status(), `${r.name}: control with the right Origin and intent`).not.toBe(403);
    }
  });

  test("WebSocket tickets: none refused, replayed refused, a ticket for session A refused on B", async ({ request }) => {
    const api = new Api(request);
    const a = await launch(api, idleHarness());
    const b = await launch(api, idleHarness());
    const noTicket = await rawUpgrade(PORT_MAIN, `/api/terms/${a.id}/ws`, { Origin: MAIN });
    // `WsQuery.ticket` is a required `String` (terminal/ws.rs): after the Origin
    // and caller-class steps pass, the Query extractor rejects the missing
    // field with axum's 400, before any ticket lookup.
    expect(noTicket.status, "an upgrade with no ticket parameter").toBe(400);
    const bogus = await rawUpgrade(PORT_MAIN, `/api/terms/${a.id}/ws?ticket=${"0".repeat(32)}`, { Origin: MAIN });
    expect(bogus.status).toBe(403);
    const t1 = await mintTicket(api, a.id);
    expect((await rawUpgrade(PORT_MAIN, `/api/terms/${a.id}/ws?ticket=${t1.ticket}`, { Origin: MAIN })).status).toBe(101);
    expect((await rawUpgrade(PORT_MAIN, `/api/terms/${a.id}/ws?ticket=${t1.ticket}`, { Origin: MAIN })).status, "replayed ticket").toBe(403);
    const t2 = await mintTicket(api, a.id);
    expect((await rawUpgrade(PORT_MAIN, `/api/terms/${b.id}/ws?ticket=${t2.ticket}`, { Origin: MAIN })).status, "A's ticket on B").toBe(403);
  });

  test("ticket TTL: a ticket older than the short TTL is refused (4738, 2 s)", async ({ request }) => {
    const api = new Api(request);
    const t = await launch(api, idleHarness());
    const short = new Api(request, TTL);
    const fresh = await mintTicket(short, t.id);
    expect(fresh.expires_in, "this is the short-TTL server").toBe(2);
    expect((await rawUpgrade(PORT_TTL, `/api/terms/${t.id}/ws?ticket=${fresh.ticket}`, { Origin: TTL })).status, "control: a fresh ticket is accepted").toBe(101);
    const old = await mintTicket(short, t.id);
    await new Promise((r) => setTimeout(r, 3000));
    expect((await rawUpgrade(PORT_TTL, `/api/terms/${t.id}/ws?ticket=${old.ticket}`, { Origin: TTL })).status, "expired ticket").toBe(403);
  });
});

// ================================================ restart (Spec 10)

/** Record the close code the BROWSER's own WebSocket objects see (the browser leg, past the proxy). */
async function recordBrowserCloses(page: Page): Promise<void> {
  await page.addInitScript(() => {
    const Orig = window.WebSocket;
    window.WebSocket = new Proxy(Orig, {
      construct(target, args: ConstructorParameters<typeof WebSocket>) {
        const ws = new target(...args);
        ws.addEventListener("close", (e) => {
          const w = window as unknown as { __t25Closes?: Array<{ url: string; code: number }> };
          (w.__t25Closes ??= []).push({ url: ws.url, code: e.code });
        });
        return ws;
      },
    });
  });
}

const browserCloses = (page: Page): Promise<Array<{ url: string; code: number }>> =>
  page.evaluate(() => (window as unknown as { __t25Closes?: Array<{ url: string; code: number }> }).__t25Closes ?? []);

/**
 * While the daemon restarts its card route is briefly empty/unavailable, which
 * unmounts the session page's terminal slot (a product behaviour about the CARD
 * lane, not about the socket under test). Serving the last good terminal card
 * through the outage keeps the panel mounted, so the reconnect that follows is
 * always the panel's own, with its stored offset and epoch.
 */
async function pinTerminalCard(page: Page): Promise<void> {
  let lastGood: string | null = null;
  await page.route(/\/api\/sessions\/[^/]+\/card(\?.*)?$/, async (route) => {
    const good = (body: string): boolean => {
      try {
        return (JSON.parse(body) as { terminal?: { available?: boolean } }).terminal?.available === true;
      } catch {
        return false;
      }
    };
    try {
      const resp = await route.fetch();
      const body = await resp.text();
      if (resp.ok() && good(body)) {
        lastGood = body;
        await route.fulfill({ response: resp, body });
      } else if (lastGood !== null) {
        await route.fulfill({ status: 200, contentType: "application/json", body: lastGood });
      } else {
        await route.fulfill({ response: resp, body });
      }
    } catch {
      if (lastGood !== null) await route.fulfill({ status: 200, contentType: "application/json", body: lastGood });
      else await route.abort();
    }
  });
}

test.describe.serial("T25 in-process restart", () => {
  test("kb restart closes 1012 within 3 s (proxy and browser), holder survives, the panel resumes with from= and the epoch, scrollback intact", async ({ page, request }) => {
    test.setTimeout(300_000);
    const conns = await proxySockets(page);
    await recordBrowserCloses(page);
    await pinTerminalCard(page);
    const gate = join(scratch("gate"), "go");
    const h = numberedHarness({ gate, seed: 3000, live: 700, rebuildEvery: 0, rate: 40 });
    const c = await prepare(page, request, h);
    await openPanel(page, c.sid);
    touch(gate);
    await page.evaluate(() => document.querySelector("[data-kb-term]")?.setAttribute("data-t25-mark", "1"));
    const rowBefore = (await listTerms(c.api)).find((t) => t.id === c.term.id)!;
    expect(rowBefore.pane_pid).toBe(c.term.panePid);
    const stopBeats = keepBound(c.api, c.sid, c.term, 150);
    try {
      const cfg = (await (await request.get(`${MAIN}/api/config`)).json()) as { config: { server: { fanout_cap?: number } } };
      cfg.config.server.fanout_cap = (cfg.config.server.fanout_cap ?? 8) + 1;
      const t0 = Date.now();
      const put = await request.put(`${MAIN}/api/config`, { data: cfg.config, failOnStatusCode: false });
      expect(put.status(), await put.text()).toBe(200);
      await expect.poll(() => conns[0].serverClose?.code, { timeout: 10_000 }).toBe(1012);
      expect(conns[0].serverClose!.at - t0, "ms from the restart request to the 1012 close").toBeLessThan(3000);
      // The browser leg: the page's own WebSocket was closed with 1012, not
      // only the proxy's server side.
      await expect
        .poll(
          async () => (await browserCloses(page)).filter((x) => /\/api\/terms\/[^/]+\/ws(\?|$)/.test(x.url)).map((x) => x.code),
          { timeout: 10_000, message: "close codes the terminal WebSockets the page opened saw" },
        )
        .toContain(1012);

      // The daemon comes back; the runner and the tmux holder never moved.
      await expect
        .poll(async () => {
          try {
            return (await request.get(`${MAIN}/api/identity`)).ok();
          } catch {
            return false;
          }
        }, { timeout: 60_000, intervals: [250, 500, 1000] })
        .toBe(true);
      // The pinned route above hides a card lane that never recovers: ask the
      // REAL card (page.route does not intercept the request fixture) and
      // require the terminal lane to report available again.
      await expect
        .poll(async () => {
          try {
            const resp = await request.get(`${MAIN}/api/sessions/${c.sid}/card`);
            if (!resp.ok()) return `status ${resp.status()}`;
            const body = (await resp.json()) as { terminal?: { available?: boolean } };
            return body.terminal?.available === true ? "available" : `terminal ${JSON.stringify(body.terminal ?? null)}`;
          } catch (e) {
            return `error ${String(e)}`;
          }
        }, { timeout: 60_000, intervals: [250, 500, 1000], message: "the unstubbed session card's terminal lane after the restart" })
        .toBe("available");
      const rowAfter = (await listTerms(c.api)).find((t) => t.id === c.term.id)!;
      expect(rowAfter.pane_pid, "same pane pid").toBe(c.term.panePid);
      expect(rowAfter.pane_dead ?? false).toBe(false);
      expect(rowAfter.stale ?? false).toBe(false);
      expect(() => process.kill(c.term.panePid, 0)).not.toThrow();
      const holder = spawnSync(tmuxBin(), ["-S", join(holderDir(), `${c.term.id}.sock`), "list-sessions"], { encoding: "utf-8" });
      expect(holder.status, `tmux holder: ${holder.stderr}`).toBe(0);

      // The panel (never re-mounted, the card is pinned) reconnects on its own.
      await waitPhase(page, "live", 60_000);
      expect(
        await page.evaluate(() => document.querySelector("[data-kb-term]")?.getAttribute("data-t25-mark") === "1"),
        "the panel element survived the restart (no re-mount)",
      ).toBe(true);
      // Reconnect attempts while the daemon is down may be refused before they
      // get a hello; the resume is the first attempt the daemon answered.
      await expect.poll(() => conns.slice(1).some((x) => helloEpoch(x) !== null), { timeout: 60_000 }).toBe(true);
      const resumed = conns.slice(1).find((x) => helloEpoch(x) !== null)!;
      const from = fromOf(resumed);
      expect(from, "the reconnect after a restart carries from=").not.toBeNull();
      expect(from!, "from= is the last applied offset").toBeGreaterThan(0);
      const prevEpoch = helloEpoch(conns[0]);
      expect(prevEpoch, "the first connection received a hello").toBeTruthy();
      expect(epochOf(resumed), "the reconnect echoes the epoch the first connection announced").toBe(prevEpoch);
      await waitFile(join(h.out, "expected.json"), 180_000);
      const exp = readExpected(h.out);
      const text = await waitDone(page);
      const r = checkNumbered(text, { expectTotal: exp.total });
      expect(r.ok, describeNumbered(r)).toBe(true);
    } finally {
      stopBeats();
    }
  });
});
