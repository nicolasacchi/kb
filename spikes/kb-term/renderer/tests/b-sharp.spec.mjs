import { test, expect } from '@playwright/test';
import { openPage, feed, shot, record, guardWebgl, tag } from '../lib/fixture.mjs';
import { Scenario, checkerPng } from '../lib/stream.mjs';

const C = 30, R = 8;
async function render(browser, cfg, srcScale) {
  const o = await openPage(browser, cfg);
  guardWebgl(test, o, cfg);
  const src = checkerPng(srcScale * C * o.devCW, srcScale * R * o.devCH, 4 * srcScale / 2);
  const sc = new Scenario(o).head().textLines(2);
  sc.image({ id: 0x3a7f21, c: C, r: R, indent: 11, pngBuf: src });
  await feed(o.page, sc.build());
  const png = await shot(o.page, `b-sharp-${tag(cfg)}-src${srcScale}x`);
  const t = sc.truth[0];
  const box = { x: Math.round((o.m.screen.x + t.col0 * o.m.cw) * o.dpr), y: Math.round((o.m.screen.y + t.line0 * o.m.ch) * o.dpr), w: C * o.devCW, h: R * o.devCH };
  return { o, src, png, box, t };
}

// (b) sharpness at DPR>1: laplacian energy of the rendered crop vs a canvas-downscaled reference of the same source.
for (const renderer of ['dom', 'webgl']) for (const dpr of [2, 3]) {
  const cfg = { renderer, dpr };
  test(`b sharpness ${tag(cfg)}`, async ({ browser }) => {
    const r = await render(browser, cfg, 2);
    let ctl = null;
    try {
      const rect = { x: r.box.x + 1, y: r.box.y + 1, w: r.box.w - 2, h: r.box.h - 2 };
      const eRender = await r.o.page.evaluate(([b, rc]) => window.T.energy(b, rc), [r.png.toString('base64'), rect]);
      const eRef = await r.o.page.evaluate(([s, w, h]) => window.T.refEnergy(s, w, h), [r.src.toString('base64'), r.box.w, r.box.h]);
      // control: the same source rendered by the same renderer at DPR 1, then bilinear-upscaled xDPR (what a CSS-resolution raster looks like)
      ctl = await openPage(r.o.page.context().browser(), { renderer, dpr: 1 });
      const sc = new Scenario(ctl).head().textLines(2);
      sc.image({ id: 0x3a7f21, c: C, r: R, indent: 11, pngBuf: r.src });
      await feed(ctl.page, sc.build());
      const png1 = await shot(ctl.page, null, { save: false });
      const b1 = { x: Math.round(ctl.m.screen.x + 10 * ctl.m.cw), y: Math.round(ctl.m.screen.y + 2 * ctl.m.ch), w: C * ctl.devCW, h: R * ctl.devCH };
      const eCtl = await ctl.page.evaluate(([b, rc, f]) => window.T.upscaleEnergy(b, rc, f), [png1.toString('base64'), { x: b1.x + 1, y: b1.y + 1, w: b1.w - 2, h: b1.h - 2 }, r.dpr]);
      const ratio = eRender / eRef, ctlRatio = eCtl / eRef;
      record('b-sharpness', tag(cfg), { boxDevicePx: r.box, sourcePx: [Math.round(2 * C * r.o.devCW), Math.round(2 * R * r.o.devCH)], energyRender: eRender, energyReference: eRef, ratio, controlCssResolutionUpscaled: ctlRatio });
      expect(eRef).toBeGreaterThan(100); // the pattern really has high-frequency content
      expect(ratio).toBeGreaterThan(0.5);
    } finally { if (ctl) await ctl.close(); await r.o.close(); }
  });
}
