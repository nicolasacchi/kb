// T25 - the terminal stack, end to end: a REAL tmux 3.7c holder, the compiled
// tap, kb-termd, three kb-server processes and the real SPA, driven by
// synthetic fake harnesses. Runs only in the opt-in `terminal-stack` projects
// (KB_E2E_TERMINAL=1, the `term-e2e` CI job); see terminal-stack/README.md.
//
// Every test launches its own session through the SPA's own REST route and
// cleans it up, so a retry never depends on earlier state. The restart test
// is serial and last. All content is synthetic (numbered lines, generated
// PNGs); no real harness, session, image or credential is involved.

import { test, expect, type Page, type TestInfo } from "@playwright/test";
import { spawnSync } from "node:child_process";
import { existsSync, mkdirSync, readdirSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { decodePng, forceDom, pixel, type Rgb } from "./spa-terminal-gfx-lib";
import {
  Api, MAIN, OFF, TTL, PORT_MAIN, PORT_TTL, PORT_OFF,
  beat, bindSession, collectPageErrors, decodeStream, holderDir, idleHarness, keepBound,
  killLaunched, kittyHarness, launch, listTerms, mintTicket, newSid, numberedHarness, openPanel,
  panelAttrs, rawUpgrade, readExpected, recordSockets, repoRoot, scratch,
  sha256, tmuxBin, touch, waitDone, waitFile, waitPhase,
  type Harness, type Launched, type Sock,
} from "./terminal-stack/stack";
import { checkNumbered, describeNumbered, frameTracker, scanForQueries } from "./terminal-stack/oracle";

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

test("late join between rebuilds sees every line once", async ({ page, request }) => {
  test.setTimeout(240_000);
  // 12 lines/s: ~8 s between rebuilds, so a join that lands after rebuild 1's
  // marker (+ the lead-in and frame) is between rebuilds 1 and 2.
  const h = numberedHarness({ seed: 3000, live: 400, rebuildEvery: 100, rate: 12, leadMs: 800 });
  const c = await prepare(page, request, h);
  await waitFile(join(h.out, "soon-1"), 120_000);
  await new Promise((r) => setTimeout(r, 1800));
  await openPanel(page, c.sid);
  await waitFile(join(h.out, "expected.json"), 180_000);
  const exp = readExpected(h.out);
  const text = await waitDone(page);
  const r = checkNumbered(text, { expectTotal: exp.total });
  expect(r.ok, describeNumbered(r)).toBe(true);
});

test("join waits for the frame to end: the first data after the seed is outside any ?2026 frame", async ({ page, request }) => {
  test.setTimeout(240_000);
  // A slow rebuild (16 chunks x 100 ms) keeps the synchronized frame open ~1.6 s;
  // the viewer is opened the moment the marker appears, so its join races it.
  const gate = join(scratch("gate"), "go");
  const h = numberedHarness({ gate, seed: 3000, live: 300, rebuildEvery: 100, rate: 60, leadMs: 1200, slowRebuildMs: 100 });
  const c = await prepare(page, request, h);
  touch(gate);
  await waitFile(join(h.out, "soon-1"), 120_000);
  await openPanel(page, c.sid);
  await waitFile(join(h.out, "expected.json"), 180_000);
  const exp = readExpected(h.out);
  const text = await waitDone(page);
  const r = checkNumbered(text, { expectTotal: exp.total });
  expect(r.ok, describeNumbered(r)).toBe(true);
  expect(c.socks.length).toBeGreaterThan(0);
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
  const y = 50;
  for (const x of [30, 130, 210]) {
    if (x >= src.w || y >= src.h) continue;
    const want = pixel(src, x, y);
    const got = pixel(shot, (x / src.w) * shot.w, (y / src.h) * shot.h);
    expect(Math.max(...(want ?? [0])), "reference pixel is not black").toBeGreaterThan(100);
    if (!near(got, want, 3)) bad.push(`(${x},${y}) got ${JSON.stringify(got)} want ${JSON.stringify(want)}`);
  }
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
    const attempts = conns.filter((x) => x.at >= t0 && x.at - t0 < 1000).length;
    expect(attempts, `drop ${i + 1}: reconnect attempts within 1 s`).toBeLessThanOrEqual(3);
    await waitPhase(page, "live");
  }
  await waitFile(join(h.out, "expected.json"), 180_000);
  const exp = readExpected(h.out);
  const text = await waitDone(page);
  const r = checkNumbered(text, { expectTotal: exp.total });
  expect(r.ok, describeNumbered(r)).toBe(true);
});

test("a resume needs no term reset (KNOWN GAP: the runner cannot carry the tap epoch, every reconnect is a full join)", async ({ page, request }) => {
  // Marked as an expected failure so the gap stays pinned in both directions:
  // kb-term's runner opens the stream with an empty epoch (crates/kb-term
  // runner/stream.rs), so the tap answers `from=` with a FULL join and the SPA
  // resets. When the epoch is plumbed through this test starts passing, and
  // Playwright reports an unexpected pass until the marker is removed.
  test.fail();
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
  const after = await panelAttrs(page);
  expect(after.resets, "term.reset count unchanged across a resume").toBe(before.resets);
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

  test("WebSocket tickets: none refused, replayed refused, a ticket for session A refused on B", async ({ request }) => {
    const api = new Api(request);
    const a = await launch(api, idleHarness());
    const b = await launch(api, idleHarness());
    const noTicket = await rawUpgrade(PORT_MAIN, `/api/terms/${a.id}/ws`, { Origin: MAIN });
    expect([400, 403]).toContain(noTicket.status);
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

test.describe.serial("T25 in-process restart", () => {
  test("kb restart closes 1012 within 3 s, holder survives, scrollback intact", async ({ page, request }) => {
    test.setTimeout(300_000);
    const conns = await proxySockets(page);
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
      const rowAfter = (await listTerms(c.api)).find((t) => t.id === c.term.id)!;
      expect(rowAfter.pane_pid, "same pane pid").toBe(c.term.panePid);
      expect(rowAfter.pane_dead ?? false).toBe(false);
      expect(rowAfter.stale ?? false).toBe(false);
      expect(() => process.kill(c.term.panePid, 0)).not.toThrow();
      const holder = spawnSync(tmuxBin(), ["-S", join(holderDir(), `${c.term.id}.sock`), "list-sessions"], { encoding: "utf-8" });
      expect(holder.status, `tmux holder: ${holder.stderr}`).toBe(0);

      // The SPA reconnects (new ticket); if the panel survived, with from=.
      // If the card lane flapped while the registry was empty the slot re-mounts closed: reopen it.
      const giveUp = Date.now() + 20_000;
      while (!(await page.locator("[data-kb-term]").count())) {
        if (await page.locator('[data-testid="kb-term-open"]').count()) {
          await page.locator('[data-testid="kb-term-open"]').click();
        } else if (Date.now() > giveUp) {
          await page.goto(`${MAIN}/sessions/${c.sid}`);
          await page.locator('[data-testid="kb-term-open"]').click();
        }
        await new Promise((r) => setTimeout(r, 500));
      }
      await expect.poll(() => conns.length, { timeout: 60_000 }).toBeGreaterThan(1);
      await waitPhase(page, "live", 60_000);
      const same = await page.evaluate(() => document.querySelector("[data-kb-term]")?.getAttribute("data-t25-mark") === "1");
      if (same) {
        const from = fromOf(conns[1]);
        expect(from, "the reconnect after a restart carries from=").not.toBeNull();
        expect(from!).toBeGreaterThan(0);
      } else {
        test.info().annotations.push({ type: "note", description: "the panel was re-mounted after the restart (card lane briefly unavailable): reconnected with a fresh join" });
      }
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
