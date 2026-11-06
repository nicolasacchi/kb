import { test } from '@playwright/test';
import { openPage, feed, shot, probe, record } from '../lib/fixture.mjs';
import { Scenario } from '../lib/stream.mjs';

// Diagnostic: xterm WebGL text at DPR 2 under different Chromium GL flag sets (headless software GL).
const VARIANTS = {
  'flags-current': ['--use-angle=swiftshader', '--use-gl=angle', '--enable-unsafe-swiftshader', '--ignore-gpu-blocklist', '--enable-webgl'],
  'flags-angle-only': ['--use-angle=swiftshader', '--enable-unsafe-swiftshader', '--ignore-gpu-blocklist'],
  'flags-none': [],
  'flags-egl': ['--use-gl=egl', '--ignore-gpu-blocklist', '--enable-unsafe-swiftshader'],
};
for (const [name, args] of Object.entries(VARIANTS)) {
  test.describe(`diag ${name}`, () => {
    test.use({ launchOptions: { args } });
    test(`k webgl text dpr2 ${name}`, async ({ browser }) => {
      const o = await openPage(browser, { renderer: 'webgl', dpr: 2, gfx: false });
      try {
        const info = async () => o.page.evaluate(() => {
          const cs = [...document.querySelectorAll('.xterm-screen canvas')].map((c) => ({ cls: c.className, w: c.width, h: c.height, sw: c.style.width, sh: c.style.height }));
          let gl = null; try { const c = document.createElement('canvas'); const g = c.getContext('webgl2'); const e = g && g.getExtension('WEBGL_debug_renderer_info'); gl = g ? (e ? g.getParameter(e.UNMASKED_RENDERER_WEBGL) : 'webgl2-no-ext') : 'no-webgl2'; } catch (e) { gl = 'err ' + e.message; }
          return { dpr: window.devicePixelRatio, canvases: cs, gl, dims: (() => { try { const d = window.T.term._core._renderService.dimensions; return { cssCell: [d.css.cell.width, d.css.cell.height], devCell: [d.device.cell.width, d.device.cell.height], devCanvas: [d.device.canvas.width, d.device.canvas.height] }; } catch (e) { return null; } })() };
        });
        const sc = new Scenario(o).head().textLines(3, 'header');
        await feed(o.page, sc.build());
        const before = await info();
        const png1 = await shot(o.page, `k-diag-${name}-1`);
        const after1 = await info();
        await o.page.waitForTimeout(600);
        const png2 = await shot(o.page, `k-diag-${name}-2`);
        // text pitch: where does the first row's text end? count non-bg pixels along y=row0 centre in 8px buckets
        const pts = []; for (let x = 0; x < 1800; x += 8) pts.push({ x, y: 16 });
        const row = await probe(o.page, png2, pts);
        const lit = row.map((c, i) => (c && c[0] + c[1] + c[2] > 120 ? i * 8 : -1)).filter((v) => v >= 0);
        const rightmost = lit.length ? Math.max(...lit) : -1;
        await o.page.evaluate(() => { window.T.term.refresh(0, window.T.term.rows - 1); });
        await o.page.waitForTimeout(300);
        const png3 = await shot(o.page, null, { save: false });
        const row3 = await probe(o.page, png3, pts);
        const lit3 = row3.map((c, i) => (c && c[0] + c[1] + c[2] > 120 ? i * 8 : -1)).filter((v) => v >= 0);
        record('k-diag', name, { before, after1, rightmostLitDevicePx: rightmost, expectedRightmostApprox: 56 * 16, afterRefreshRightmost: lit3.length ? Math.max(...lit3) : -1, litBuckets: lit.length });
      } finally { await o.close(); }
    });
  });
}
