import { test, expect } from '@playwright/test';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const HERE = path.dirname(fileURLToPath(import.meta.url));
const OUT = path.join(HERE, 'out');
fs.mkdirSync(path.join(OUT, 'shots'), { recursive: true });

function rec(name, data) {
  const proj = test.info().project.name;
  fs.appendFileSync(path.join(OUT, 'results.jsonl'), JSON.stringify({ project: proj, name, ...data }) + '\n');
}
const isChromium = () => test.info().project.use.browserName === 'chromium';

test.beforeEach(async ({ page }) => {
  page.on('pageerror', (e) => rec('pageerror', { message: String(e) }));
  await page.goto('/');
  await page.waitForFunction(() => window.kt && window.kt.init);
});

async function setupGrid(page, { cols = 120, rows = 30, fontSize = 12, nLines = 800 } = {}) {
  await page.evaluate(async (o) => {
    await kt.init(o);
    await kt.writeAsync(kt.lines(o.nLines));
    await kt.flush();
  }, { cols, rows, fontSize, nLines });
  return page.evaluate(() => kt.stats());
}

test.describe('a) momentum scroller + axis lock', () => {
  test('vertical drag scrolls by distance, fling adds momentum', async ({ page }) => {
    const s0 = await setupGrid(page);
    expect(s0.scrollWidth).toBeGreaterThan(s0.clientWidth); // grid wider than the phone => pan needed
    const lh = s0.cellH;
    const top = s0.baseY;
    expect(s0.viewportY).toBe(top);

    // slow drag, exactly 10 lines of finger travel, finger held still before release => no momentum
    await page.evaluate((lh) => kt.synthDrag({ x0: 200, y0: 100, x1: 200, y1: 100 + 10 * lh, steps: 20, stepMs: 12, holdMs: 200 }), lh);
    const slowY = await page.evaluate(() => kt.settle());
    const slowLines = top - slowY;

    // fling: 240 px in ~100 ms, released while moving
    const y1 = slowY;
    const g = await page.evaluate(async () => {
      await kt.synthDrag({ x0: 200, y0: 120, x1: 200, y1: 360, steps: 10, stepMs: 10 });
      const frames0 = kt.g.momentumFrames;
      const y = await kt.settle();
      return { y, frames: kt.g.momentumFrames - frames0, gestures: kt.g.gestures.slice(-3), sl: kt.wrap.scrollLeft };
    });
    const flingLines = y1 - g.y;
    const dragOnlyLines = Math.floor(240 / lh);
    rec('a-vertical', { lineHeightPx: +lh.toFixed(2), slowDragExpectedLines: 10, slowDragLines: slowLines, flingFingerTravelPx: 240, flingFingerLines: dragOnlyLines, flingTotalLines: flingLines, momentumFrames: g.frames, gestures: JSON.stringify(g.gestures), scrollLeftAfter: g.sl });
    expect(Math.abs(slowLines - 10)).toBeLessThanOrEqual(1);
    expect(flingLines).toBeGreaterThan(dragOnlyLines);   // momentum carried the view past the finger travel
    expect(g.frames).toBeGreaterThan(3);
    expect(g.sl).toBe(0);                                  // vertical gesture never panned horizontally
  });

  test('horizontal pan moves the wrapper, not the terminal', async ({ page }) => {
    const s0 = await setupGrid(page);
    const r = await page.evaluate(async () => {
      const before = kt.stats();
      await kt.synthDrag({ x0: 300, y0: 200, x1: 100, y1: 214, steps: 12, stepMs: 12, holdMs: 150 });  // dx=-200, dy=14: horizontal lock
      await kt.settle();
      return { before, after: kt.stats() };
    });
    rec('a-horizontal', { viewportYBefore: r.before.viewportY, viewportYAfter: r.after.viewportY, scrollLeftBefore: r.before.scrollLeft, scrollLeftAfter: r.after.scrollLeft, maxScrollLeft: r.after.scrollWidth - r.after.clientWidth, gestures: JSON.stringify(r.after.gestures) });
    expect(r.after.viewportY).toBe(r.before.viewportY);
    expect(r.after.scrollLeft).toBeGreaterThan(100);
    // and the inverse: a vertical drag with a wobble below the 8 px lock does not pan horizontally
    const v = await page.evaluate(async () => {
      const sl = kt.wrap.scrollLeft, y0 = kt.stats().viewportY;
      await kt.synthDrag({ x0: 200, y0: 200, x1: 206, y1: 330, steps: 10, stepMs: 12, holdMs: 150 });
      await kt.settle();
      return { dsl: kt.wrap.scrollLeft - sl, dy: y0 - kt.stats().viewportY };
    });
    rec('a-axis-lock-wobble', v);
    expect(v.dsl).toBe(0);
    expect(v.dy).toBeGreaterThan(0);
    expect(s0.viewportY).toBeGreaterThan(0);
  });

  test('sub-threshold movement locks nothing', async ({ page }) => {
    await setupGrid(page);
    const r = await page.evaluate(async () => {
      const a = kt.stats();
      await kt.synthDrag({ x0: 200, y0: 200, x1: 204, y1: 203, steps: 4, stepMs: 10, holdMs: 50 });
      await kt.settle();
      return { a, b: kt.stats() };
    });
    rec('a-below-lock', { viewportYDelta: r.b.viewportY - r.a.viewportY, scrollLeftDelta: r.b.scrollLeft - r.a.scrollLeft });
    expect(r.b.viewportY).toBe(r.a.viewportY);
    expect(r.b.scrollLeft).toBe(r.a.scrollLeft);
  });

  test('real touch input via CDP (chromium only)', async ({ page, context }) => {
    test.skip(!isChromium(), 'CDP touch is Chromium-only');
    const s0 = await setupGrid(page);
    const cdp = await context.newCDPSession(page);
    const touch = (type, x, y) => cdp.send('Input.dispatchTouchEvent', { type, touchPoints: type === 'touchEnd' ? [] : [{ x, y, id: 1 }] });
    const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
    await touch('touchStart', 200, 150);
    for (let i = 1; i <= 12; i++) { await sleep(10); await touch('touchMove', 200, 150 + i * 20); }
    await touch('touchEnd', 200, 390);
    const y1 = await page.evaluate(() => kt.settle());
    const vLines = s0.viewportY - y1;
    const sl0 = await page.evaluate(() => kt.wrap.scrollLeft);
    await touch('touchStart', 300, 200);
    for (let i = 1; i <= 10; i++) { await sleep(10); await touch('touchMove', 300 - i * 20, 202); }
    await sleep(120);
    await touch('touchEnd', 100, 202);
    const h = await page.evaluate(async () => { await kt.settle(); return kt.stats(); });
    rec('a-cdp-real-touch', { verticalLines: vLines, horizontalScrollLeft: h.scrollLeft, horizontalViewportYDelta: h.viewportY - y1, touchEventsStopped: await page.evaluate(() => kt.g.touchEventsStopped), gestures: JSON.stringify(h.gestures) });
    expect(vLines).toBeGreaterThan(5);
    expect(h.scrollLeft).toBeGreaterThan(sl0 + 50);
    expect(h.viewportY).toBe(y1);
  });
});

test.describe('b) pinch to font steps', () => {
  test('pinch out/in changes fontSize in steps; the grid stays the session size', async ({ page }) => {
    const s0 = await setupGrid(page, { fontSize: 12 });
    const out = await page.evaluate(async () => {
      await kt.synthPinch({ cx: 200, cy: 300, d0: 100, d1: 160 });      // x1.6
      await kt.flush();
      const a = kt.stats();
      await kt.synthPinch({ cx: 200, cy: 300, d0: 200, d1: 60 });       // x0.3
      await kt.flush();
      const b = kt.stats();
      const tr = kt.noTransform();
      return { a, b, transforms: tr, internalTransforms: kt.noTransform.internal.length, steps: kt.g.pinchSteps };
    });
    // test hook path as well
    const hook = await page.evaluate(async () => { const r = kt.pinchTo(1.2); await kt.flush(); return { r, s: kt.stats() }; });
    rec('b-pinch', { startFont: s0.fontSize, afterPinchOut: out.a.fontSize, afterPinchIn: out.b.fontSize, hookTarget: hook.r, cols: [s0.cols, out.a.cols, out.b.cols].join('/'), rows: [s0.rows, out.a.rows, out.b.rows].join('/'), cellW: [s0.cellW, out.a.cellW, out.b.cellW].map((x) => +x.toFixed(2)).join('/'), screenW: [s0.screenW, out.a.screenW, out.b.screenW].map((x) => Math.round(x)).join('/'), transformsFound: out.transforms.join(',') || 'none', xtermScrollbarSliderTransforms: out.internalTransforms, stepEvents: out.steps });
    expect(out.a.fontSize).toBeGreaterThan(s0.fontSize);
    expect(out.b.fontSize).toBeLessThan(out.a.fontSize);
    expect(out.a.cols).toBe(s0.cols); expect(out.a.rows).toBe(s0.rows);
    expect(out.b.cols).toBe(s0.cols); expect(out.b.rows).toBe(s0.rows);
    expect(out.a.screenW).toBeGreaterThan(s0.screenW);
    expect(out.b.screenW).toBeLessThan(out.a.screenW);
    expect(out.transforms).toEqual([]);
    expect(hook.s.cols).toBe(120);
  });
});

test.describe('c) join cost of a 20,000-line seed', () => {
  test('parse + render time', async ({ page, context }) => {
    const N = 3;
    const median = (a) => [...a].sort((x, y) => x - y)[Math.floor(a.length / 2)];
    const run = async (label, renderer = 'dom') => {
      const rs = [];
      for (let i = 0; i < N; i++) rs.push(await page.evaluate((r) => kt.joinCost('/data/seed.bin', { cols: 120, rows: 30, fontSize: 12, renderer: r }), renderer));
      const o = { mode: label, bytes: rs[0].bytes, bufferLines: rs[0].bufferLines, renderer: rs[0].renderer, runs: N };
      for (const k of ['parseMs', 'renderMs', 'totalMs', 'scrollTopMs', 'fontStepMs']) { o[k + 'Median'] = +median(rs.map((r) => r[k])).toFixed(1); o[k + 'All'] = rs.map((r) => +r[k].toFixed(0)).join('/'); }
      rec('c-join', o);
      expect(rs[0].bufferLines).toBeGreaterThanOrEqual(20000);
      expect(o.totalMsMedian).toBeLessThan(120000);
      return o;
    };
    await run('unthrottled');
    if (isChromium()) {
      const cdp = await context.newCDPSession(page);
      await cdp.send('Emulation.setCPUThrottlingRate', { rate: 4 });
      await run('cpu-throttle-4x');
      await cdp.send('Emulation.setCPUThrottlingRate', { rate: 1 });
      await run('unthrottled-webgl-attempt', 'webgl');   // software GL in CI; informational
    }
  });
});

test.describe('d) ResumeFrom after dropped connections', () => {
  test('server-side drops: final buffer equals from-start reference', async ({ page, request }) => {
    await request.get('/reset');
    const run = page.evaluate(() => kt.resumeRun({ cols: 100, rows: 30, pace: 3, chunk: 1024 }));
    const total = (await (await request.get('/stats')).json()).total;
    let drops = 0;
    for (const frac of [0.15, 0.4, 0.65, 0.85]) {
      await page.waitForFunction((t) => kt.progress().offset >= t || kt.progress().done, Math.floor(total * frac), { timeout: 60000 });
      const p = await page.evaluate(() => kt.progress());
      if (p.done) break;
      await request.post('/drop'); drops++;
    }
    const r = await run;
    const cmp = await page.evaluate(() => kt.compareToReference('/data/stream.bin', 100, 30));
    const stats = await (await request.get('/stats')).json();
    rec('d-resume-drops', { streamBytes: total, appliedBytes: r.final, drops, reconnects: r.reconnects, connectionsSeen: stats.log.length, resumeOffsets: r.opens.join(','), dupBytes: r.dupBytes, gaps: r.gaps, ...cmp, firstDiffLive: cmp.firstDiffLive, firstDiffRef: cmp.firstDiffRef });
    expect(r.final).toBe(total);
    expect(drops).toBeGreaterThanOrEqual(2);
    expect(r.reconnects).toBeGreaterThanOrEqual(drops);
    expect(cmp.textEqual).toBe(true);
    expect(cmp.attrEqual).toBe(true);
    expect(cmp.liveLen).toBe(cmp.refLen);
  });

  test('overlapping resume (server rewinds 37 bytes): dedupe by offset header', async ({ page, request }) => {
    await request.get('/reset');
    const run = page.evaluate(() => kt.resumeRun({ cols: 100, rows: 30, pace: 3, chunk: 1000, rewind: 37 }));
    const total = (await (await request.get('/stats')).json()).total;
    for (const frac of [0.3, 0.7]) {
      await page.waitForFunction((t) => kt.progress().offset >= t, Math.floor(total * frac), { timeout: 60000 });
      await request.post('/drop');
    }
    const r = await run;
    const cmp = await page.evaluate(() => kt.compareToReference('/data/stream.bin', 100, 30));
    rec('d-resume-overlap', { appliedBytes: r.final, reconnects: r.reconnects, dupBytes: r.dupBytes, gaps: r.gaps, textEqual: cmp.textEqual, attrEqual: cmp.attrEqual, liveLen: cmp.liveLen, refLen: cmp.refLen });
    expect(r.final).toBe(total);
    expect(r.dupBytes).toBeGreaterThan(0);
    expect(cmp.textEqual).toBe(true);
    expect(cmp.attrEqual).toBe(true);
  });

  test('offline/online (chromium only): resumes from last offset', async ({ page, context, request }) => {
    test.skip(!isChromium(), 'offline emulation of live sockets is Chromium-specific here');
    await request.get('/reset');
    const run = page.evaluate(() => kt.resumeRun({ cols: 100, rows: 30, pace: 4, chunk: 1024, watchdogMs: 1000 }));
    const total = (await (await request.get('/stats')).json()).total;
    await page.waitForFunction((t) => kt.progress().offset >= t, Math.floor(total * 0.3), { timeout: 60000 });
    const before = await page.evaluate(() => kt.progress());
    await context.setOffline(true);
    await page.waitForTimeout(3000);
    const during = await page.evaluate(() => kt.progress());
    await context.setOffline(false);
    const r = await run;
    const cmp = await page.evaluate(() => kt.compareToReference('/data/stream.bin', 100, 30));
    rec('d-resume-offline', { offsetAtOffline: before.offset, offsetDuringOffline: during.offset, closesDuringOffline: during.closes - before.closes, watchdogFired: r.watchdogs, reconnects: r.reconnects, resumeOffsets: r.opens.join(','), appliedBytes: r.final, textEqual: cmp.textEqual, attrEqual: cmp.attrEqual });
    expect(r.final).toBe(total);
    expect(cmp.textEqual).toBe(true);
    expect(cmp.attrEqual).toBe(true);
  });
});

test.describe('e) readability of a 120-column grid', () => {
  test('screenshots at the phone width', async ({ page }) => {
    const proj = test.info().project.name;
    const info = await page.evaluate(async () => {
      await kt.init({ cols: 120, rows: 30, fontSize: 12 });
      await kt.writeAsync(await kt.fetchBytes('/data/wide.bin'));
      await kt.flush();
      kt.term.scrollToBottom();
      await kt.flush();
      return kt.stats();
    });
    const shot = async (name) => { await page.screenshot({ path: path.join(OUT, 'shots', `${proj}-${name}.png`) }); };
    await shot('120col-12px-left');
    await page.evaluate(() => { kt.wrap.scrollLeft = (kt.wrap.scrollWidth - kt.wrap.clientWidth) / 2; });
    await shot('120col-12px-mid');
    await page.evaluate(() => { kt.wrap.scrollLeft = kt.wrap.scrollWidth; });
    await shot('120col-12px-right');
    const fit = await page.evaluate(async () => { kt.wrap.scrollLeft = 0; return kt.fitFont(); });
    await shot('120col-fit-width');
    rec('e-readability', {
      viewportCssPx: info.vw, dpr: info.dpr, defaultFontPx: info.fontSize, cellWCssPx: +info.cellW.toFixed(2), cellHCssPx: +info.cellH.toFixed(2),
      gridWidthCssPx: Math.round(info.screenW), visibleColsAtDefault: +(info.clientWidth / info.cellW).toFixed(1), panScreensNeeded: +(info.screenW / info.clientWidth).toFixed(2),
      fitWidthFontPx: fit.fontSize, fitWidthCellWCssPx: +fit.cellW.toFixed(2),
    });
    expect(info.cols).toBe(120);
    expect(info.scrollWidth).toBeGreaterThan(info.clientWidth);
  });
});
