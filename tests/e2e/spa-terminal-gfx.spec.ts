import { test, expect, type Page } from "@playwright/test";
import { BASE } from "./helpers";
import {
  GfxFake, Scenario, backgroundOf, basicScenario, bigScenario, boxDownscale, checkPictures,
  checkerPng, decodePng, forceDom, frames, laplacianEnergy, metrics, openGfxTerminal, overlayInfo,
  pixel, quadPng, scanAll, scrollToLine, waitPictures,
  type Img, type Metrics, type Truth,
} from "./spa-terminal-gfx-lib";

// T15 - omp pictures drawn over kitty placeholder cells. CHROMIUM ONLY, in the
// `chromium-gfx-dpr{1,2,3}` projects (the plain `chromium` project skips
// every @gfx title; the DPR 2/3 projects launch a REAL --force-device-scale-
// factor because Playwright's emulated factor never reaches the
// devicePixelContentBoxSize that xterm's WebGL addon sizes from). WebGL is
// SwiftShader on CI, so sharpness numbers describe correctness of the raster
// path, not a real GPU. All content is synthetic.

const shot = async (page: Page): Promise<Img> => decodePng(await page.screenshot({ type: "png" }));

async function boot(page: Page, o: { dom?: boolean; scrollback?: number; fake?: GfxFake } = {}) {
  const fake = o.fake ?? new GfxFake({ scrollback: o.scrollback });
  if (o.dom) await forceDom(page);
  await fake.install(page);
  const opened = await openGfxTerminal(page);
  const m = await metrics(page);
  return { fake, opened, m };
}

/** Expect the picture-layer probes to match; print the first bad probes on failure. */
function expectGood(res: ReturnType<typeof checkPictures>, minTotal = 1): void {
  expect(res.total).toBeGreaterThanOrEqual(minTotal);
  expect(res.badCount, JSON.stringify(res.bad)).toBe(0);
}

async function settledMetrics(page: Page): Promise<Metrics> {
  await frames(page, 4);
  return metrics(page);
}

// ---------------------------------------------------------------- (a) placement

for (const [title, dom, edge] of [
  ["a placement dom @gfx-dpr1", true, 2],
  ["a placement webgl @gfx-dpr2", false, 1],
  ["a placement webgl @gfx-dpr3", false, 1],
] as const) {
  test(title, async ({ page }) => {
    const { fake, opened, m } = await boot(page, { dom });
    const sc = basicScenario(fake, m);
    fake.push(sc.build());
    await waitPictures(page, 2);
    const m2 = await settledMetrics(page);
    const img = await shot(page);
    const res = checkPictures(img, m2, sc.truth, m2.viewportY, { edgeDev: edge });
    expectGood(res, 40);
    expect((await overlayInfo(page))?.imgs).toBe(2);
    // The bytes were asked for once per picture, with the intent header and no auth.
    expect(new Set(fake.mediaRequests.map((r) => r.sha)).size).toBe(2);
    expect(fake.mediaRequests).toHaveLength(2);
    for (const r of fake.mediaRequests) {
      expect(r.headers["x-kb-intent"]).toBe("terminal");
      expect(r.headers["authorization"]).toBeUndefined();
    }
    expect(opened.errors).toEqual([]);
  });
}

test("a2 id encodings (rgb, palette, implicit continuation) @gfx-dpr1", async ({ page }) => {
  const { fake, m } = await boot(page, { dom: true });
  const sc = new Scenario(fake, m).head().textLines(1).blank();
  sc.image({ id: 0x010203, c: 16, r: 5, indent: 3, variant: 0 });
  sc.blank();
  sc.image({ id: 77, c: 16, r: 5, indent: 3, variant: 1, mode: "pal" });
  sc.blank();
  sc.image({ id: 0x0a0b0c, c: 16, r: 5, indent: 41, variant: 2, implicit: true });
  fake.push(sc.build());
  await waitPictures(page, 3);
  const m2 = await settledMetrics(page);
  const img = await shot(page);
  expectGood(checkPictures(img, m2, sc.truth, m2.viewportY), 40);
  const ids = [...new Set((await scanAll(page)).map((c) => Number(c.split(",")[2])))].sort((a, b) => a - b);
  expect(ids).toEqual([77, 0x010203, 0x0a0b0c].sort((a, b) => a - b));
});

// ---------------------------------------------------------------- (b) sharpness

const PATTERNS: Array<[string, (dpr: number) => number]> = [
  ["aligned", () => 4],
  ["offgrid", () => 3],
  ["coarse", (dpr) => 4 * dpr],
];
for (const dpr of [2, 3]) {
  for (const renderer of ["dom", "webgl"] as const) {
    for (const [pname, sqf] of PATTERNS) {
      test(`b sharp ${renderer} ${pname} @gfx-dpr${dpr}`, async ({ page }) => {
        const { fake, m } = await boot(page, { dom: renderer === "dom" });
        expect(m.dpr).toBe(dpr);
        const C = 30;
        const R = 8;
        const src = checkerPng(Math.round(2 * C * m.devCW), Math.round(2 * R * m.devCH), sqf(dpr));
        const sc = new Scenario(fake, m).head().textLines(2);
        sc.image({ id: 0x3a7f21, c: C, r: R, indent: 11, pngBuf: src });
        fake.push(sc.build());
        await waitPictures(page, 1);
        const m2 = await settledMetrics(page);
        const img = await shot(page);
        const t = sc.truth[0];
        const box = {
          x: Math.round((m2.screen.x + t.col0 * m2.cw) * m2.dpr),
          y: Math.round((m2.screen.y + t.line0 * m2.ch) * m2.dpr),
          w: Math.round(C * m2.cw * m2.dpr),
          h: Math.round(R * m2.ch * m2.dpr),
        };
        const eRender = laplacianEnergy(img, { x: box.x + 2, y: box.y + 2, w: box.w - 4, h: box.h - 4 });
        const ref = boxDownscale(decodePng(src), box.w, box.h);
        const eRef = laplacianEnergy(ref, { x: 2, y: 2, w: box.w - 4, h: box.h - 4 });
        expect(eRef, "the pattern really has high-frequency content").toBeGreaterThan(100);
        expect(eRender / eRef, `render ${eRender} vs reference ${eRef}`).toBeGreaterThan(0.5);
      });
    }
  }
}

// ---------------------------------------------------------------- (c) late join

for (const [title, dom, edge] of [
  ["c late join dom @gfx-dpr1", true, 2],
  ["c late join webgl @gfx-dpr2", false, 1],
] as const) {
  test(title, async ({ page, context }) => {
    test.setTimeout(150_000);
    const live = await boot(page, { dom });
    const sc = bigScenario(live.fake, live.m);
    live.fake.push(sc.build());
    await expect.poll(async () => (await scanAll(page)).length, { timeout: 60_000 }).toBe(4 * 30 * 8);
    // A second panel joins mid-stream: its seed is the whole stream so far.
    const page2 = await context.newPage();
    const late = await boot(page2, { dom, fake: new GfxFake({ from: live.fake }) });
    await expect.poll(async () => (await scanAll(page2)).length, { timeout: 60_000 }).toBe(4 * 30 * 8);
    const liveCells = await scanAll(page);
    const lateCells = await scanAll(page2);
    expect(lateCells.length).toBe(4 * 30 * 8);
    expect(lateCells).toEqual(liveCells);
    for (const t of sc.truth) {
      for (const off of [-8, 3]) {
        for (const [pg, fk] of [[page, live], [page2, late]] as const) {
          await pg.bringToFront();
          await scrollToLine(pg, t.line0 + off);
          await expect
            .poll(async () => (await overlayInfo(pg))?.pending ?? -1, { timeout: 20_000 })
            .toBe(0);
          await expect.poll(async () => (await overlayInfo(pg))?.imgs ?? 0).toBeGreaterThan(0);
          const mm = await settledMetrics(pg);
          const img = await shot(pg);
          expectGood(checkPictures(img, mm, [t], mm.viewportY, { edgeDev: edge }), 5);
          expect(fk.opened.errors).toEqual([]);
        }
      }
    }
  });
}

// ---------------------------------------------------------------- (d) fast scroll, (e) trimmed top

for (const [title, dom] of [
  ["d fast scroll dom @gfx-dpr1", true],
  ["d fast scroll webgl @gfx-dpr2", false],
] as const) {
  test(title, async ({ page }) => {
    test.setTimeout(150_000);
    const { fake, m, opened } = await boot(page, { dom });
    const sc = bigScenario(fake, m, 2040);
    fake.push(sc.build());
    await expect.poll(async () => (await scanAll(page)).length, { timeout: 60_000 }).toBe(4 * 30 * 8);
    const res = await page.evaluate(
      async ({ truths, step }) => {
        const host = document.querySelector("[data-kb-term]") as unknown as {
          __kbTerm: {
            rows: number;
            scrollToTop(): void;
            scrollLines(n: number): void;
            buffer: { active: { baseY: number; viewportY: number } };
          };
        };
        const t = host.__kbTerm;
        const raf = (): Promise<void> => new Promise((r) => requestAnimationFrame(() => r()));
        t.scrollToTop();
        await raf();
        await raf();
        let stale = 0;
        let n = 0;
        for (let y = 0; y < t.buffer.active.baseY; y += step) {
          t.scrollLines(step);
          await raf();
          await raf();
          n += 1;
          const vy = t.buffer.active.viewportY;
          const want = truths.filter((tr) => tr.line0 < vy + t.rows && tr.line0 + tr.r > vy).length;
          const clips = document.querySelectorAll(".kbgfx-overlay .kbgfx-clip").length;
          if (clips !== want) stale += 1;
        }
        return { n, stale };
      },
      { truths: sc.truth, step: 11 },
    );
    expect(res.n).toBeGreaterThan(100);
    expect(res.stale, "overlay groups must equal the pictures in view at every step").toBe(0);
    // And a landing on a picture is pixel-correct.
    const t = sc.truth[2];
    await scrollToLine(page, t.line0 - 4);
    await waitPictures(page, 1);
    const m2 = await settledMetrics(page);
    expectGood(checkPictures(await shot(page), m2, [t], m2.viewportY, { edgeDev: dom ? 2 : 1 }), 5);
    expect(opened.errors).toEqual([]);
  });
}

test("e trimmed top @gfx-dpr1", async ({ page }) => {
  const { fake, m } = await boot(page, { dom: true });
  const sc = bigScenario(fake, m, 400);
  fake.push(sc.build());
  await expect.poll(async () => (await scanAll(page)).length, { timeout: 30_000 }).toBe(30 * 8);
  const t: Truth = sc.truth[0];
  // Three picture rows above the viewport: the picture is cropped, never squashed.
  await scrollToLine(page, t.line0 + 3);
  await waitPictures(page, 1);
  const m2 = await settledMetrics(page);
  expect(m2.viewportY).toBe(t.line0 + 3);
  expectGood(checkPictures(await shot(page), m2, [t], m2.viewportY), 5);
});

// ---------------------------------------------------------------- (f) unknown id

test("f unknown id then def arrives @gfx-dpr1", async ({ page }) => {
  const { fake, m } = await boot(page, { dom: true });
  const sc = new Scenario(fake, m).head().textLines(1).blank();
  sc.image({ id: 0x3a7f21, c: 16, r: 5, indent: 11, announce: false });
  fake.push(sc.build());
  await page.waitForSelector(".kbgfx-missing", { timeout: 15_000 });
  await frames(page, 4);
  const before = await overlayInfo(page);
  expect(before?.missing).toBe(1);
  expect(before?.texts).toEqual(["image unavailable"]);
  const rect = await page.evaluate(() => {
    const r = document.querySelector(".kbgfx-missing")!.getBoundingClientRect();
    return { x: r.left, y: r.top, w: r.width, h: r.height, dpr: window.devicePixelRatio };
  });
  const img = await shot(page);
  for (const [fx, fy] of [[0.15, 0.2], [0.85, 0.2], [0.15, 0.8], [0.85, 0.8]]) {
    const p = pixel(img, (rect.x + rect.w * fx) * rect.dpr, (rect.y + rect.h * fy) * rect.dpr);
    expect(p, `inner point ${fx},${fy}`).not.toBeNull();
    expect(Math.abs(p![0] - 43) + Math.abs(p![1] - 43) + Math.abs(p![2] - 48)).toBeLessThanOrEqual(6);
  }
  // The def and its bytes arrive: the box is replaced in place by the picture.
  fake.def({ id: 0x3a7f21, png: quadPng(Math.round(16 * m.devCW), Math.round(5 * m.devCH), 0), cols: 16, rows: 5 });
  await waitPictures(page, 1);
  const after = await overlayInfo(page);
  expect(after?.missing).toBe(0);
  expect(after?.imgs).toBe(1);
  const m2 = await settledMetrics(page);
  expectGood(checkPictures(await shot(page), m2, sc.truth, m2.viewportY), 5);
});

// ---------------------------------------------------------------- (g) gfxreset + retransmit

for (const [title, dom, edge] of [
  ["g gfxreset retransmit dom @gfx-dpr1", true, 2],
  ["g gfxreset retransmit webgl @gfx-dpr2", false, 1],
] as const) {
  test(title, async ({ page }) => {
    const { fake, m } = await boot(page, { dom });
    const ID = 0x3a7f21;
    const c = 20;
    const r = 5;
    const src = quadPng(Math.round(c * m.devCW), Math.round(r * m.devCH), 0);
    const sc = new Scenario(fake, m).head().textLines(1).blank();
    sc.image({ id: ID, c, r, indent: 3, pngBuf: src });
    sc.blank().textLines(1, "gap").blank();
    // The SAME id again, lower on the screen: "old rows" once the bytes change.
    sc.image({ id: ID, c, r, indent: 3, pngBuf: src, announce: false });
    fake.push(sc.build());
    await waitPictures(page, 2);
    expect((await overlayInfo(page))?.versions).toEqual(["1", "1"]);
    let m2 = await settledMetrics(page);
    expectGood(checkPictures(await shot(page), m2, sc.truth, m2.viewportY, { edgeDev: edge }), 10);

    // GfxReset plus an identical retransmit costs the viewer nothing.
    fake.text({ t: "gfxreset", gen: 2 });
    fake.def({ id: ID, png: src, cols: c, rows: r });
    await frames(page, 6);
    expect((await overlayInfo(page))?.versions).toEqual(["1", "1"]);
    expect(fake.mediaRequests).toHaveLength(1);
    m2 = await settledMetrics(page);
    expectGood(checkPictures(await shot(page), m2, sc.truth, m2.viewportY, { edgeDev: edge }), 10);

    // Other bytes under the same id: the new picture appears on the new AND
    // the old rows (cells carry only the id; accepted, documented limitation).
    const other = quadPng(Math.round(c * m.devCW), Math.round(r * m.devCH), 2);
    fake.def({ id: ID, png: other, cols: c, rows: r, gen: 2 });
    await expect.poll(async () => (await overlayInfo(page))?.versions.join(",")).toBe("2,2");
    await waitPictures(page, 2);
    m2 = await settledMetrics(page);
    const swapped = sc.truth.map((t) => ({ ...t, variant: 2 }));
    expectGood(checkPictures(await shot(page), m2, swapped, m2.viewportY, { edgeDev: edge }), 10);
  });
}

// ---------------------------------------------------------------- (j) halo

/**
 * Pixels in a band around each picture, outside the picture box, that differ
 * from the background. The band reaches half a cell past the box on every
 * side: far enough to catch a glyph spilling out of a placeholder cell, not
 * far enough to reach a neighbouring text row's descenders.
 */
function dirtyOutside(img: Img, m: Metrics, truths: Truth[]): number {
  const bg = backgroundOf(img, m);
  const pad = 3;
  let n = 0;
  for (const t of truths) {
    const bx0 = Math.round((m.screen.x + t.col0 * m.cw) * m.dpr) - pad;
    const bx1 = Math.round((m.screen.x + (t.col0 + t.c) * m.cw) * m.dpr) + pad;
    const by0 = Math.round((m.screen.y + t.line0 * m.ch) * m.dpr) - pad;
    const by1 = Math.round((m.screen.y + (t.line0 + t.r) * m.ch) * m.dpr) + pad;
    const x0 = Math.max(0, bx0 - Math.round(0.5 * m.cw * m.dpr));
    const x1 = Math.min(img.w, bx1 + Math.round(0.5 * m.cw * m.dpr));
    const y0 = Math.max(0, by0 - Math.round(0.5 * m.ch * m.dpr));
    const y1 = Math.min(img.h, by1 + Math.round(0.5 * m.ch * m.dpr));
    for (let y = y0; y < y1; y += 1) {
      for (let x = x0; x < x1; x += 1) {
        if (x >= bx0 && x < bx1 && y >= by0 && y < by1) continue;
        const p = pixel(img, x, y)!;
        if (Math.abs(p[0] - bg[0]) + Math.abs(p[1] - bg[1]) + Math.abs(p[2] - bg[2]) > 40) n += 1;
      }
    }
  }
  return n;
}

for (const dpr of [1, 2, 3]) {
  test(`j halo recolour leaves no dirty pixels @gfx-dpr${dpr}`, async ({ page }) => {
    const { fake, m } = await boot(page, { dom: true });
    const sc = basicScenario(fake, m);
    fake.push(sc.build());
    await waitPictures(page, 2);
    const m2 = await settledMetrics(page);
    const img = await shot(page);
    expect(dirtyOutside(img, m2, sc.truth)).toBe(0);
    const info = await overlayInfo(page);
    expect(info?.live).toBe(10 + 6);
  });
}

for (const dpr of [2, 3]) {
  test(`j halo control: recolour off spills at DPR>=2 @gfx-dpr${dpr}`, async ({ page }) => {
    const { fake, m } = await boot(page, { dom: true });
    // No decorations: the glyph recolour is the only thing under test.
    await page.evaluate(() => {
      const host = document.querySelector("[data-kb-term]") as unknown as { __kbTerm: { registerDecoration: unknown } };
      host.__kbTerm.registerDecoration = () => undefined;
    });
    const sc = basicScenario(fake, m);
    fake.push(sc.build());
    await waitPictures(page, 2);
    const m2 = await settledMetrics(page);
    expect(dirtyOutside(await shot(page), m2, sc.truth)).toBeGreaterThan(0);
  });
}

// ---------------------------------------------------------------- misc

test("h2 width pipeline finds every placeholder cell @gfx-dpr1", async ({ page }) => {
  const { fake, m } = await boot(page, { dom: true });
  const active = await page.evaluate(
    () => (document.querySelector("[data-kb-term]") as unknown as { __kbTerm: { unicode: { activeVersion: string } } }).__kbTerm.unicode.activeVersion,
  );
  expect(active).toMatch(/grapheme/i);
  const sc = basicScenario(fake, m);
  fake.push(sc.build());
  await waitPictures(page, 2);
  expect((await scanAll(page)).length).toBe(40 * 10 + 20 * 6);
  const info = await overlayInfo(page);
  expect(info?.clips).toBe(2);
});

test("rebuild redraw: ED2+ED3 reprint with the same ids @gfx-dpr1", async ({ page }) => {
  const { fake, opened, m } = await boot(page, { dom: true });
  const first = basicScenario(fake, m);
  fake.push(first.build());
  await waitPictures(page, 2);
  const created0 = (await overlayInfo(page))!.created;
  // The harness repaints the whole screen inside a synchronized update.
  const again = new Scenario(fake, m);
  const src1 = quadPng(Math.round(40 * m.devCW), Math.round(10 * m.devCH), 0);
  const src2 = quadPng(Math.round(20 * m.devCW), Math.round(6 * m.devCH), 1);
  again.textLines(2, "header").blank();
  again.image({ id: 0x3a7f21, c: 40, r: 10, indent: 11, variant: 0, pngBuf: src1, announce: false });
  again.blank().textLines(1, "middle").blank();
  again.image({ id: 0x1b4c9d, c: 20, r: 6, indent: 51, variant: 1, pngBuf: src2, announce: false });
  fake.push(`\x1b[?2026h\x1b[2J\x1b[3J\x1b[H${again.build()}\x1b[?2026l`);
  await expect
    .poll(async () => {
      const i = await overlayInfo(page);
      return i ? `${i.clips}:${i.live}` : "";
    }, { timeout: 15_000 })
    .toBe("2:16");
  const info = (await overlayInfo(page))!;
  expect(info.imgs).toBe(2);
  expect(info.created - info.disposed).toBe(info.live);
  expect(info.created).toBeGreaterThan(created0);
  const m2 = await settledMetrics(page);
  expectGood(checkPictures(await shot(page), m2, again.truth, m2.viewportY), 40);
  expect(opened.errors).toEqual([]);
});

test("lightbox opens on a tap and Esc closes it @gfx-dpr1", async ({ page }) => {
  const { fake, m } = await boot(page, { dom: true });
  const sc = basicScenario(fake, m);
  fake.push(sc.build());
  await waitPictures(page, 2);
  const m2 = await settledMetrics(page);
  const t = sc.truth[0];
  await page.mouse.click(
    m2.screen.x + (t.col0 + t.c / 2) * m2.cw,
    m2.screen.y + (t.line0 + t.r / 2) * m2.ch,
  );
  const dialog = page.getByRole("dialog", { name: "Picture" });
  await expect(dialog).toBeVisible();
  await expect
    .poll(() => page.evaluate(() => {
      const i = document.querySelector<HTMLImageElement>('[role="dialog"] img');
      return i ? i.complete && i.naturalWidth > 0 : false;
    }))
    .toBe(true);
  await page.keyboard.press("Escape");
  await expect(dialog).toBeHidden();
  const focused = await page.evaluate(() => document.activeElement?.className ?? "");
  expect(focused).toContain("xterm-helper-textarea");
});

test("media route is invisible when the tier is disabled @gfx-dpr1", async ({ request }) => {
  // The e2e daemon runs with `[terminal] enabled = false` (the default).
  const sha = "a".repeat(64);
  const withIntent = await request.get(`${BASE}/api/terms/media/${sha}`, {
    headers: { "X-Kb-Intent": "terminal" },
  });
  expect(withIntent.status()).toBe(404);
  expect((await withIntent.body()).length).toBe(0);
  const without = await request.get(`${BASE}/api/terms/media/${sha}`);
  expect(without.status()).toBeGreaterThanOrEqual(400);
  expect(without.status()).toBeLessThan(500);
  expect((await without.body()).length).toBe(0);
  // Not even a malformed key is distinguishable from a missing one.
  const bad = await request.get(`${BASE}/api/terms/media/not-a-hash`, {
    headers: { "X-Kb-Intent": "terminal" },
  });
  expect(bad.status()).toBe(404);
});
