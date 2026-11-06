import { test, expect } from '@playwright/test';
import { openPage, shot, record, guardWebgl, tag } from '../lib/fixture.mjs';
import { Scenario, checkerPng, ESC } from '../lib/stream.mjs';

const C = 30, R = 8;
const iipSeq = (pngBuf, extra) => `${ESC}]1337;File=inline=1;size=${pngBuf.length};${extra}:${pngBuf.toString('base64')}\x07`;

// IIP fallback: the same kind of picture through OSC 1337 File= and xterm's image addon. Measures blur and size.
for (const cfg of [{ renderer: 'dom', dpr: 1 }, { renderer: 'dom', dpr: 2 }, { renderer: 'dom', dpr: 3 }, { renderer: 'webgl', dpr: 2 }]) {
  test(`i iip blur ${tag(cfg)}`, async ({ browser }) => {
    const o = await openPage(browser, { ...cfg, iip: true, gfx: false });
    try {
      guardWebgl(test, o, cfg);
      const src = checkerPng(2 * C * o.devCW, 2 * R * o.devCH, 4);
      // case 1: told to fill CxR cells (iTerm width/height in cells); case 2: native pixels at CSS resolution (no size hint)
      for (const mode of ['cells', 'native-css-res']) {
        const pngBuf = mode === 'cells' ? src : checkerPng(C * o.m.cw | 0, R * o.m.ch | 0, 2);
        const pre = Buffer.from(`${ESC}[?25l${ESC}[2J${ESC}[3;11H`);
        await o.page.evaluate((s) => window.T.writeRaw(s), `${ESC}[?25l${ESC}[2J${ESC}[3;11H`);
        const seq = iipSeq(pngBuf, mode === 'cells' ? `width=${C};height=${R};preserveAspectRatio=0` : 'preserveAspectRatio=0');
        await o.page.evaluate((s) => window.T.writeRaw(s), seq);
        const png = await shot(o.page, `i-iip-${mode}-${tag(cfg)}`);
        const png64 = png.toString('base64');
        const bb = await o.page.evaluate((b) => window.T.bbox(b), png64);
        const box = { x: Math.round((o.m.screen.x + 10 * o.m.cw) * o.dpr), y: Math.round((o.m.screen.y + 2 * o.m.ch) * o.dpr), w: C * o.devCW, h: R * o.devCH };
        const e = await o.page.evaluate(([b, rc]) => window.T.energy(b, rc), [png64, { x: box.x + 1, y: box.y + 1, w: box.w - 2, h: box.h - 2 }]);
        const eRef = await o.page.evaluate(([s, w, h]) => window.T.refEnergy(s, w, h), [src.toString('base64'), box.w, box.h]);
        const canv = await o.page.evaluate(() => { const c = window.T.imageAddon.getImageAtBufferCell(10, 2); return c ? { w: c.width, h: c.height } : null; });
        record('i-iip-blur', `${mode}-${tag(cfg)}`, { dpr: o.dpr, sourcePx: [pngBuf.length, 'bytes'], boxDevicePx: box, drawnBboxDevicePx: bb, drawnCssPx: { w: bb.w / o.dpr, h: bb.h / o.dpr }, storedCanvas: canv, energy: e, energyReference: eRef, ratioVsReference: e / eRef, errors: o.errors.slice(0, 3) });
        if (mode === 'cells') expect(bb.w).toBeGreaterThan(0);
      }
    } finally { await o.close(); }
  });
}

// Storage limit: how many pictures survive before the FIFO evicts?
for (const [name, mb, n, w, h] of [['default-128MB-400x200', 128, 450, 400, 200], ['limit-8MB-100x100', 8, 300, 100, 100]]) {
  test(`i iip eviction ${name}`, async ({ browser }) => {
    test.setTimeout(240000);
    const o = await openPage(browser, { renderer: 'dom', dpr: 1, iip: true, gfx: false, scrollback: 20000, iipmb: mb });
    try {
      await o.page.evaluate((s) => window.T.writeRaw(s), `${ESC}[?25l${ESC}[2J${ESC}[H`);
      const t0 = Date.now();
      const lines = await o.page.evaluate(([n, w, h]) => window.T.iipFill(n, w, h), [n, w, h]);
      const fillMs = Date.now() - t0;
      const s = await o.page.evaluate((l) => window.T.iipSurvivors(l), lines);
      const limitPx = Math.floor(mb / 4 * 1e6);
      const theory = Math.floor(limitPx / (w * h));
      const bufLen = await o.page.evaluate(() => window.T.bufLen());
      // newest picture must be drawn; scroll to an evicted one and screenshot it for the report
      await o.page.evaluate((l) => window.T.scrollToLine(l), lines[0]);
      await shot(o.page, `i-iip-evicted-${name}`);
      record('i-iip-eviction', name, { imagesWritten: n, imagePx: [w, h], bytesPerImage: w * h * 4, storageLimitMB: mb, survivors: s.alive, firstSurvivorIndex: s.first, lastSurvivorIndex: s.last, theoreticalCapacity: theory, usageMB: s.usageMB, internalMapSize: s.internal, bufferLines: bufLen, fillMs });
      expect(s.last).toBe(n - 1);
      expect(s.alive).toBeLessThan(n);
      expect(s.alive).toBeGreaterThan(theory * 0.5);
    } finally { await o.close(); }
  });
}
