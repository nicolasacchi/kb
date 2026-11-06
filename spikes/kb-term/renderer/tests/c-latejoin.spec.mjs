import { test, expect } from '@playwright/test';
import { createRequire } from 'node:module';
import { openPage, feedChunked, shot, checkPictures, record, guardWebgl, tag } from '../lib/fixture.mjs';
import { bigScenario } from '../lib/stream.mjs';

const require = createRequire(import.meta.url);
const { Terminal } = require('@xterm/headless');
const { SerializeAddon } = require('@xterm/addon-serialize');
const { Unicode11Addon } = require('@xterm/addon-unicode11');

const stripApc = (s) => s.replace(/\x1b_G[^\x1b]*\x1b\\/g, '');

async function twinSerialize(stream, cols, rows, scrollback) {
  const t = new Terminal({ cols, rows, scrollback, allowProposedApi: true });
  t.loadAddon(new Unicode11Addon()); t.unicode.activeVersion = '11';
  const ser = new SerializeAddon(); t.loadAddon(ser);
  await new Promise((r) => t.write(stripApc(stream.toString('utf8')), r));
  const out = ser.serialize({ scrollback });
  const info = { length: t.buffer.active.length, baseY: t.buffer.active.baseY };
  t.dispose();
  return { out, info };
}

// (c) late joiner: headless twin -> serialize() -> fresh page terminal (+ ImageDefs), compare against the live-fed page.
for (const cfg of [{ renderer: 'dom', dpr: 1 }, { renderer: 'webgl', dpr: 2 }]) {
  test(`c late joiner ${tag(cfg)}`, async ({ browser }) => {
    const live = await openPage(browser, cfg);
    let late = null;
    try {
      guardWebgl(test, live, cfg);
      const sc = bigScenario(live);
      const buf = sc.build();
      await feedChunked(live.page, buf);
      const defs = await live.page.evaluate(() => window.T.exportDefs());
      const t0 = Date.now();
      const { out, info } = await twinSerialize(buf, 100, 30, 5000);
      const serMs = Date.now() - t0;
      late = await openPage(browser, cfg);
      await late.page.evaluate((d) => window.T.importDefs(d), defs);
      await late.page.evaluate((s) => window.T.writeRaw(s), out);
      const [liveLen, lateLen] = await Promise.all([live.page.evaluate(() => window.T.bufLen()), late.page.evaluate(() => window.T.bufLen())]);
      const [liveCells, lateCells] = await Promise.all([live.page.evaluate(() => window.T.scanAll()), late.page.evaluate(() => window.T.scanAll())]);
      const sameCells = liveCells.length === lateCells.length && liveCells.every((c, i) => c === lateCells[i]);
      // pictures in scrollback: scroll both to each picture's top / middle and probe against truth
      const lines = sc.truth.map((t) => t.line0);
      const results = [];
      for (const t of sc.truth) {
        for (const off of [-8, 3]) {
          const vy = t.line0 + off;
          const lv = await live.page.evaluate((v) => window.T.scrollToLine(v), vy);
          const sv = await late.page.evaluate((v) => window.T.scrollToLine(v), vy);
          const a = await shot(live.page, null, { save: false });
          const b = await shot(late.page, off === -8 ? `c-latejoin-${tag(cfg)}-img${t.variant}` : null);
          const ra = await checkPictures(live.page, a, live, [t], lv, { label: 'live' });
          const rb = await checkPictures(late.page, b, late, [t], sv, { label: 'late' });
          results.push({ line0: t.line0, off, vyLive: lv, vyLate: sv, liveOk: `${ra.ok}/${ra.total}`, lateOk: `${rb.ok}/${rb.total}`, identicalBytes: a.equals(b), lateBad: rb.bad.slice(0, 2) });
        }
      }
      record('c-latejoin', tag(cfg), { liveLen, lateLen, twin: info, serializedBytes: out.length, serializeMs: serMs, placeholderCellsLive: liveCells.length, placeholderCellsLate: lateCells.length, sameCells, results, errors: late.errors.slice(0, 3) });
      expect(lateLen).toBe(liveLen);
      expect(lateCells.length).toBe(4 * 30 * 8);
      expect(sameCells).toBe(true);
      for (const r of results) { expect(r.vyLate).toBe(r.vyLive); expect(r.lateOk).toBe(r.liveOk); expect(r.lateBad.length).toBe(0); }
    } finally { await live.close(); if (late) await late.close(); }
  });
}
