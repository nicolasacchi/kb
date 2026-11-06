import { test, expect } from '@playwright/test';
import { openPage, feed, shot, probe, record, guardWebgl, tag } from '../lib/fixture.mjs';
import { Scenario, checkerPng } from '../lib/stream.mjs';

const C = 30, R = 8;
// 'aligned' = 4-source-px squares (exact 2:1 downscale to 2 device px at 2x source), 'offgrid' = 3-source-px squares (1.5 device px: forces real resampling),
// 'coarse' = 4*dpr source px squares = 2 CSS px squares: detail that a DPR-1 (CSS resolution) raster can still hold, so the control is not degenerate.
const PATTERNS = { aligned: () => 4, offgrid: () => 3, coarse: (dpr) => 4 * dpr };

async function render(browser, cfg, sq) {
  const o = await openPage(browser, cfg);
  guardWebgl(test, o, cfg);
  const src = checkerPng(Math.round(2 * C * o.devCW), Math.round(2 * R * o.devCH), sq);
  const sc = new Scenario(o).head().textLines(2);
  sc.image({ id: 0x3a7f21, c: C, r: R, indent: 11, pngBuf: src });
  await feed(o.page, sc.build());
  const png = await shot(o.page, `b-sharp-${tag(cfg)}-${sq}`);
  const t = sc.truth[0];
  const box = { x: Math.round((o.m.screen.x + t.col0 * o.m.cw) * o.dpr), y: Math.round((o.m.screen.y + t.line0 * o.m.ch) * o.dpr), w: Math.round(C * o.devCW), h: Math.round(R * o.devCH) };
  return { o, src, png, box, t };
}

// (b) sharpness at DPR>1: Laplacian energy of the rendered crop vs a canvas-downscaled reference of the same source.
for (const renderer of ['dom', 'webgl']) for (const dpr of [2, 3]) for (const [pname, sqf] of Object.entries(PATTERNS)) {
  const cfg = { renderer, dpr }; const sq = sqf(dpr);
  test(`b sharpness ${tag(cfg)} ${pname}`, async ({ browser }) => {
    const r = await render(browser, cfg, sq);
    let ctl = null;
    try {
      const rect = { x: r.box.x + 2, y: r.box.y + 2, w: r.box.w - 4, h: r.box.h - 4 };
      const eRender = await r.o.page.evaluate(([b, rc]) => window.T.energy(b, rc), [r.png.toString('base64'), rect]);
      const eRef = await r.o.page.evaluate(([s, w, h]) => window.T.refEnergy(s, w, h), [r.src.toString('base64'), r.box.w, r.box.h]);
      // control: same source, same renderer at DPR 1, then bilinear-upscaled xDPR (what a CSS-resolution raster looks like)
      ctl = await openPage(browser, { renderer, dpr: 1 });
      const sc = new Scenario(ctl).head().textLines(2);
      sc.image({ id: 0x3a7f21, c: C, r: R, indent: 11, pngBuf: r.src });
      await feed(ctl.page, sc.build());
      const png1 = await shot(ctl.page, `b-sharp-control-${tag(cfg)}-${pname}`);
      const b1 = { x: Math.round(ctl.m.screen.x + 10 * ctl.m.cw), y: Math.round(ctl.m.screen.y + 2 * ctl.m.ch), w: Math.round(C * ctl.m.cw), h: Math.round(R * ctl.m.ch) };
      const eCtl = await ctl.page.evaluate(([b, rc, f]) => window.T.upscaleEnergy(b, rc, f), [png1.toString('base64'), { x: b1.x + 2, y: b1.y + 2, w: b1.w - 4, h: b1.h - 4 }, r.dpr]);
      const eCtl1x = await ctl.page.evaluate(([b, rc]) => window.T.energy(b, rc), [png1.toString('base64'), { x: b1.x + 2, y: b1.y + 2, w: b1.w - 4, h: b1.h - 4 }]);
      const ctlPix = await probe(ctl.page, png1, [0, 1, 2, 3, 4, 5, 6, 7].map((i) => ({ x: b1.x + 20 + i, y: b1.y + 20 })));
      const ratio = eRender / eRef, ctlRatio = eCtl / eRef;
      record('b-sharpness', `${tag(cfg)}-${pname}`, { controlBoxDpr1: b1, controlEnergyAt1x: eCtl1x, controlEnergyUpscaled: eCtl, controlRowPixels: ctlPix, squareSrcPx: sq, boxDevicePx: r.box, sourcePx: [Math.round(2 * C * r.o.devCW), Math.round(2 * R * r.o.devCH)], energyRender: eRender, energyReference: eRef, ratio, controlCssResolutionUpscaled: ctlRatio });
      expect(eRef).toBeGreaterThan(100); // the pattern really has high-frequency content
      expect(ratio).toBeGreaterThan(0.5);
    } finally { if (ctl) await ctl.close(); await r.o.close(); }
  });
}
