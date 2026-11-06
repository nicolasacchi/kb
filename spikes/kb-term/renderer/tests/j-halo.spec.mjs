import { test, expect } from '@playwright/test';
import { openPage, feed, shot, probe, record, guardWebgl, tag, CONFIGS, isBg } from '../lib/fixture.mjs';
import { basicScenario } from '../lib/stream.mjs';

// The U+10EEEE glyph (tofu + diacritics) is painted by xterm in the picture-id colour. Does it leak around the picture?
// Ring = every device pixel on a rectangle 3 device px outside picture A (c=40 r=10), except where text lives.
for (const halo of [false, true]) for (const cfg of CONFIGS) {
  test(`j halo ${halo ? 'hidden' : 'raw'} ${tag(cfg)}`, async ({ browser }) => {
    const o = await openPage(browser, { ...cfg, halo });
    try {
      guardWebgl(test, o, cfg);
      const sc = basicScenario(o);
      await feed(o.page, sc.build());
      const png = await shot(o.page, `j-halo-${halo ? 'hidden' : 'raw'}-${tag(cfg)}`);
      const t = sc.truth[0];
      const dpr = o.dpr;
      const x0 = Math.round((o.m.screen.x + t.col0 * o.m.cw) * dpr), x1 = Math.round((o.m.screen.x + (t.col0 + t.c) * o.m.cw) * dpr);
      const y0 = Math.round((o.m.screen.y + t.line0 * o.m.ch) * dpr), y1 = Math.round((o.m.screen.y + (t.line0 + t.r) * o.m.ch) * dpr);
      const g = 3;
      const pts = []; const side = [];
      for (let y = y0 + 4; y < y1 - 4; y++) { pts.push({ x: x0 - g, y }); side.push('left'); pts.push({ x: x1 + g - 1, y }); side.push('right'); }
      // rows directly above/below the picture are blank lines in this scenario
      for (let x = x0; x < x1; x++) { pts.push({ x, y: y0 - g }); side.push('top'); pts.push({ x, y: y1 + g - 1 }); side.push('bottom'); }
      const got = await probe(o.page, png, pts);
      const dirty = { left: 0, right: 0, top: 0, bottom: 0 }; const total = { left: 0, right: 0, top: 0, bottom: 0 }; let maxLum = 0;
      got.forEach((c, i) => { total[side[i]]++; if (!isBg(c)) { dirty[side[i]]++; maxLum = Math.max(maxLum, c[0] + c[1] + c[2]); } });
      const sum = Object.values(dirty).reduce((a, b) => a + b, 0);
      const rs = await o.page.evaluate(() => window.T.rstats());
      record('j-halo', `${halo ? 'hidden' : 'raw'}-${tag(cfg)}`, { ringPixels: got.length, dirtyPixels: sum, dirty, total, maxChannelSum: maxLum, stats: rs });
      if (halo) expect(sum, JSON.stringify(dirty)).toBe(0);
    } finally { await o.close(); }
  });
}
