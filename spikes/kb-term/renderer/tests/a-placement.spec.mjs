import { test, expect } from '@playwright/test';
import { openPage, feed, shot, checkPictures, record, guardWebgl, tag, CONFIGS } from '../lib/fixture.mjs';
import { basicScenario, Scenario, placeholderRows, transmit, place, quadPng, fgFor, ESC } from '../lib/stream.mjs';

// (a) pictures sit exactly in their placeholder boxes
for (const cfg of CONFIGS) {
  test(`a placement ${tag(cfg)}`, async ({ browser }) => {
    const o = await openPage(browser, cfg);
    try {
      guardWebgl(test, o, cfg);
      const sc = basicScenario(o);
      await feed(o.page, sc.build());
      const png = await shot(o.page, `a-placement-${tag(cfg)}`);
      const res = await checkPictures(o.page, png, o, sc.truth, 0);
      const overlay = await o.page.evaluate(() => window.T.overlayInfo());
      const perf = await o.page.evaluate(() => ({ lastMs: window.T.renderer.lastMs, frames: window.T.renderer.frames, cells: window.T.renderer.lastCells }));
      const rstats = await o.page.evaluate(() => window.T.rstats());
      record('a-placement', tag(cfg), { ...res, overlay, perf, rstats, errors: o.errors.slice(0, 5) });
      expect(overlay.imgs).toBe(2);
      expect(res.total).toBeGreaterThan(40);
      expect(res.badCount, JSON.stringify(res.bad)).toBe(0);
    } finally { await o.close(); }
  });
}

// colour-id encodings: 38;2 (24-bit), 38;5 (palette) and kitty's "no diacritics = continue previous cell" shortcut
for (const cfg of [{ renderer: 'dom', dpr: 1 }, { renderer: 'webgl', dpr: 2 }]) {
  test(`a2 id encodings ${tag(cfg)}`, async ({ browser }) => {
    const o = await openPage(browser, cfg);
    try {
      guardWebgl(test, o, cfg);
      const sc = new Scenario(o).head().textLines(1).blank();
      sc.image({ id: 0x010203, c: 16, r: 5, indent: 3, variant: 0 });
      sc.blank();
      sc.image({ id: 77, c: 16, r: 5, indent: 3, variant: 1, mode: 'pal' });
      sc.blank();
      sc.image({ id: 0x0a0b0c, c: 16, r: 5, indent: 41, variant: 2, implicit: true });
      await feed(o.page, sc.build());
      const png = await shot(o.page, `a2-encodings-${tag(cfg)}`);
      const res = await checkPictures(o.page, png, o, sc.truth, 0);
      const cells = await o.page.evaluate(() => window.T.scan().map((c) => c.id));
      const ids = [...new Set(cells)].sort((a, b) => a - b);
      record('a2-encodings', tag(cfg), { ...res, decodedIds: ids, cells: cells.length });
      expect(ids).toEqual([77, 0x010203, 0x0a0b0c].sort((a, b) => a - b));
      expect(res.badCount, JSON.stringify(res.bad)).toBe(0);
    } finally { await o.close(); }
  });
}

// Which width table makes U+10EEEE + diacritics one cell?
for (const kind of ['none', '11', 'graphemes']) {
  test(`h width table ${kind}`, async ({ browser }) => {
    const o = await openPage(browser, { renderer: 'dom', dpr: 1 });
    try {
      const r = await o.page.evaluate((k) => window.T.widthTable(k), kind);
      record('h-width', kind, { total: r.total, badCount: r.bad.length, bad: r.bad.slice(0, 12) });
      // We assert what is TRUE per kind in the report; the invariant under test is that the chosen kind (11) has no bad rows.
      if (kind === '11') expect(r.bad.length).toBe(0);
    } finally { await o.close(); }
  });
}

// Pipeline end-to-end under each width table: does the renderer still find and place the pictures?
for (const unicode of ['none', '11', 'graphemes']) {
  test(`h2 pipeline unicode=${unicode}`, async ({ browser }) => {
    const o = await openPage(browser, { renderer: 'dom', dpr: 1, unicode });
    try {
      const sc = basicScenario(o);
      await feed(o.page, sc.build());
      const png = await shot(o.page, `h2-unicode-${unicode}`);
      const res = await checkPictures(o.page, png, o, sc.truth, 0);
      const n = await o.page.evaluate(() => window.T.scan().length);
      record('h2-pipeline', unicode, { cellsFound: n, expectedCells: 40 * 10 + 20 * 6, probesOk: res.ok, probesTotal: res.total, bad: res.bad.slice(0, 4) });
      if (unicode === '11') { expect(n).toBe(520); expect(res.badCount).toBe(0); }
    } finally { await o.close(); }
  });
}
