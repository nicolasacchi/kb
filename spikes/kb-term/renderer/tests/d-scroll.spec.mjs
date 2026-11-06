import { test, expect } from '@playwright/test';
import { openPage, feed, feedChunked, shot, checkPictures, record, guardWebgl, tag, CONFIGS } from '../lib/fixture.mjs';
import { bigScenario, Scenario } from '../lib/stream.mjs';

// (d) fast scroll through 2,000 rows with four pictures keeps them aligned (10 sampled positions)
for (const cfg of CONFIGS) {
  test(`d fast scroll ${tag(cfg)}`, async ({ browser }) => {
    const o = await openPage(browser, cfg);
    try {
      guardWebgl(test, o, cfg);
      const sc = bigScenario(o);
      await feedChunked(o.page, sc.build());
      const L = sc.truth.map((t) => t.line0), r = sc.truth[0].r, rows = o.m.rows;
      const positions = [
        L[0] - (rows - 3), L[0] - 10,
        L[1] + r - 3, L[1] + 3,
        L[2] - 10, L[2] - (rows - 3),
        L[3] + 3, L[3] + r - 3,
        L[0] + r + 60, L[2] + r + 100, // no picture in view
      ];
      // stress: scroll lines per frame through the whole buffer, count frames where the overlay lags the viewport
      const sweepStep = cfg.dpr === 1 ? 17 : 29;
      const sweep = await o.page.evaluate(([s]) => window.T.sweepCheck(s, 1), [sweepStep]);
      const fast = await o.page.evaluate((p) => window.T.fastScroll(p), positions);
      const samples = []; let allOk = true;
      let k = 0;
      for (const p of positions) {
        const vy = await o.page.evaluate((v) => window.T.scrollToLine(v), p);
        const png = await shot(o.page, k % 3 === 0 ? `d-scroll-${tag(cfg)}-pos${k}` : null);
        const res = await checkPictures(o.page, png, o, sc.truth, vy, { label: `pos${k}` });
        const ov = await o.page.evaluate(() => window.T.overlayInfo());
        const visibleTruth = sc.truth.filter((t) => t.line0 + t.r > vy && t.line0 < vy + rows).length;
        samples.push({ pos: p, vy, visibleTruth, overlayChildren: ov.children, probes: `${res.ok}/${res.total}`, bad: res.bad.slice(0, 2) });
        if (res.badCount || ov.children !== visibleTruth || vy !== p) allOk = false;
        k++;
      }
      const perf = await o.page.evaluate(() => ({ avgRenderMs: window.T.renderer.totalMs / window.T.renderer.frames, frames: window.T.renderer.frames }));
      record('d-scroll', tag(cfg), { bufLen: await o.page.evaluate(() => window.T.bufLen()), sweep: { step: sweepStep, ...sweep }, fastJumpMs: fast.ms, samples, perf, allOk });
      expect(sweep.stale).toBe(0);
      for (const s of samples) { expect(s.bad.length, JSON.stringify(s)).toBe(0); expect(s.overlayChildren).toBe(s.visibleTruth); }
    } finally { await o.close(); }
  });
}

// (e) buffer trimmed through the middle of a picture: the surviving rows must still draw, cropped
for (const cfg of CONFIGS.filter((c) => c.dpr !== 3)) {
  test(`e trimmed top ${tag(cfg)}`, async ({ browser }) => {
    const o = await openPage(browser, { ...cfg, scrollback: 100 });
    try {
      guardWebgl(test, o, cfg);
      const rowsTrim = 5, R = 12, C = 30;
      const sc = new Scenario(o).head().textLines(20, 'pre');
      sc.image({ id: 0x6e2f80, c: C, r: R, indent: 11, variant: 2 });
      const N = 20 + R + 122; // lines written so far + the rest => trimmed = N + 1 - (rows+scrollback) = 25 = 20 + rowsTrim
      sc.textLines(122, 'post');
      await feed(o.page, sc.build());
      const bufLen = await o.page.evaluate(() => window.T.bufLen());
      const trimmed = sc.line + 1 - bufLen;
      const truth = sc.truth.map((t) => ({ ...t, line0: t.line0 - trimmed }));
      const vy0 = await o.page.evaluate(() => window.T.scrollToLine(0));
      const png = await shot(o.page, `e-trimmed-${tag(cfg)}`);
      const res = await checkPictures(o.page, png, o, truth, vy0);
      const ov = await o.page.evaluate(() => window.T.overlayInfo());
      // one more: scroll so the surviving part is at the bottom edge of the viewport, still cropped correctly
      const vy1 = await o.page.evaluate(() => window.T.scrollToLine(5));
      const png1 = await shot(o.page, null, { save: false });
      const res1 = await checkPictures(o.page, png1, o, truth, vy1);
      record('e-trimmed', tag(cfg), { linesWritten: sc.line, bufLen, trimmed, expectedTrim: 20 + rowsTrim, firstVisibleImageRow: rowsTrim, probesAtTop: `${res.ok}/${res.total}`, probesAtVy5: `${res1.ok}/${res1.total}`, overlay: ov, bad: res.bad.slice(0, 3) });
      expect(trimmed).toBe(20 + rowsTrim);
      expect(ov.imgs).toBe(1);
      expect(res.badCount, JSON.stringify(res.bad)).toBe(0);
      expect(res1.badCount, JSON.stringify(res1.bad)).toBe(0);
    } finally { await o.close(); }
  });
}
