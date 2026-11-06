import { test, expect } from '@playwright/test';
import { openPage, feed, shot, probe, checkPictures, record, guardWebgl, tag, CONFIGS } from '../lib/fixture.mjs';
import { Scenario, transmit, place, gfxReset, quadPng, placeholderRows } from '../lib/stream.mjs';

// (f) unknown id -> neutral "image unavailable" box; the picture appears once the ImageDef arrives
for (const cfg of CONFIGS) {
  test(`f unknown id ${tag(cfg)}`, async ({ browser }) => {
    const o = await openPage(browser, cfg);
    try {
      guardWebgl(test, o, cfg);
      const id = 0x5566aa, c = 20, r = 5;
      const sc = new Scenario(o).head().textLines(2).blank();
      sc.image({ id, c, r, indent: 11, transmitIt: false });
      await feed(o.page, sc.build());
      const png = await shot(o.page, `f-unknown-${tag(cfg)}`);
      const ov = await o.page.evaluate(() => window.T.overlayInfo());
      const t = sc.truth[0];
      const x0 = o.m.screen.x + t.col0 * o.m.cw, y0 = o.m.screen.y + t.line0 * o.m.ch, x1 = x0 + c * o.m.cw, y1 = y0 + r * o.m.ch;
      const pts = [[x0 + 6, y0 + 6], [x1 - 7, y0 + 6], [x0 + 6, y1 - 7], [x1 - 7, y1 - 7]].map(([x, y]) => ({ x: x * o.dpr, y: y * o.dpr }));
      const got = await probe(o.page, png, pts);
      const neutral = got.every((p) => p && Math.abs(p[0] - 43) <= 8 && Math.abs(p[1] - 43) <= 8 && Math.abs(p[2] - 48) <= 8);
      // late ImageDef
      await feed(o.page, Buffer.from(transmit(id, quadPng(Math.round(c * o.devCW), Math.round(r * o.devCH), 0))));
      const png2 = await shot(o.page, `f-unknown-then-arrives-${tag(cfg)}`);
      const res = await checkPictures(o.page, png2, o, sc.truth, 0);
      const ov2 = await o.page.evaluate(() => window.T.overlayInfo());
      record('f-unknown', tag(cfg), { overlayBefore: ov, cornerPixels: got, neutral, overlayAfter: ov2, afterProbes: `${res.ok}/${res.total}` });
      expect(ov.missing).toBe(1);
      expect(ov.texts).toEqual(['image unavailable']);
      expect(neutral, JSON.stringify(got)).toBe(true);
      expect(ov2.missing).toBe(0);
      expect(ov2.imgs).toBe(1);
      expect(res.badCount, JSON.stringify(res.bad)).toBe(0);
    } finally { await o.close(); }
  });
}

// (g) GfxReset (kitty a=d) then re-transmit under the same id
for (const cfg of [{ renderer: 'dom', dpr: 1 }, { renderer: 'dom', dpr: 2 }, { renderer: 'webgl', dpr: 2 }]) {
  test(`g gfxreset retransmit ${tag(cfg)}`, async ({ browser }) => {
    const o = await openPage(browser, cfg);
    try {
      guardWebgl(test, o, cfg);
      const id = 0x9c5a13, c = 20, r = 5;
      const w = Math.round(c * o.devCW), h = Math.round(r * o.devCH);
      const A = quadPng(w, h, 0), B = quadPng(w, h, 2);
      const log = {};
      const st = () => o.page.evaluate(() => ({ resets: window.T.store.resets, transmits: window.T.store.transmits, version: window.T.store.defs.get(0x9c5a13)?.version ?? null, imgVers: window.T.imgSrcVersions(), apc: window.T.reasm.stats }));
      // phase 1: image A, rows at indent 11
      const sc = new Scenario(o).head().textLines(2).blank();
      sc.image({ id, c, r, indent: 11, pngBuf: A });
      await feed(o.page, sc.build());
      const p1 = await checkPictures(o.page, await shot(o.page, `g-1-A-${tag(cfg)}`), o, [{ ...sc.truth[0], variant: 0 }], 0);
      log.phase1 = { probes: `${p1.ok}/${p1.total}`, ...(await st()) };
      // phase 2: GfxReset alone: the store keeps the picture; rows already in the buffer must keep drawing
      await feed(o.page, Buffer.from(gfxReset()));
      const p2 = await checkPictures(o.page, await shot(o.page, null, { save: false }), o, [{ ...sc.truth[0], variant: 0 }], 0);
      log.phase2 = { probes: `${p2.ok}/${p2.total}`, ...(await st()) };
      // phase 3: identical re-transmit (what omp does after a resize): no visible change, ImageDef version unchanged
      await feed(o.page, Buffer.from(gfxReset() + transmit(id, A) + place(id, c, r)));
      const p3 = await checkPictures(o.page, await shot(o.page, null, { save: false }), o, [{ ...sc.truth[0], variant: 0 }], 0);
      log.phase3 = { probes: `${p3.ok}/${p3.total}`, ...(await st()) };
      // phase 4: reset + DIFFERENT bytes under the same id, plus a new placement lower down
      const before = sc.line;
      const tail = Buffer.from(gfxReset() + transmit(id, B) + place(id, c, r) + '\r\n' + placeholderRows(id, c, r, 41));
      sc.line += 1 + r;
      await feed(o.page, tail);
      const truthNew = { id, line0: before + 1, col0: 40, c, r, variant: 2 };
      const vy = await o.page.evaluate(() => window.T.viewportY());
      const png4 = await shot(o.page, `g-4-B-${tag(cfg)}`);
      const pNew = await checkPictures(o.page, png4, o, [truthNew], vy);
      // OLD rows are keyed by id only, so they now show B as well (kitty itself behaves the same way)
      const pOld = await checkPictures(o.page, png4, o, [{ ...sc.truth[0], variant: 2 }], vy);
      const pOldAsA = await checkPictures(o.page, png4, o, [{ ...sc.truth[0], variant: 0 }], vy);
      log.phase4 = { newPlacementProbes: `${pNew.ok}/${pNew.total}`, oldRowsShowB: `${pOld.ok}/${pOld.total}`, oldRowsShowA: `${pOldAsA.ok}/${pOldAsA.total}`, ...(await st()) };
      record('g-gfxreset', tag(cfg), log);
      expect(p1.badCount).toBe(0);
      expect(p2.badCount).toBe(0);
      expect(p3.badCount).toBe(0);
      expect(log.phase3.version).toBe(log.phase1.version); // identical bytes: no new ImageDef version
      expect(pNew.badCount, JSON.stringify(pNew.bad)).toBe(0);
      expect(pOld.badCount, JSON.stringify(pOld.bad)).toBe(0); // finding: retransmit with other bytes rewrites OLD rows
      expect(log.phase4.version).toBe(log.phase1.version + 1);
    } finally { await o.close(); }
  });
}
