// usage: node widths.mjs glyphs.json OUT_DIR  -> widths-xterm-unicode11.json, widths-xterm-graphemes.json (+ unicode6 default)
import fs from 'node:fs';
import path from 'node:path';
import { createRequire } from 'node:module';
import { makeTerm, write } from './lib.mjs';
const require = createRequire(import.meta.url);
const [gp, outDir] = process.argv.slice(2);
const glyphs = JSON.parse(fs.readFileSync(gp, 'utf8')).glyphs;
const xv = require('@xterm/headless/package.json').version;
async function measure(variant) {
  const { t } = makeTerm(200, 10, variant);
  const widths = {};
  for (const g of glyphs) {
    await write(t, '\x1b[2J\x1b[H' + g.text);
    const alone = t.buffer.active.cursorX;
    const line = t.buffer.active.getLine(0);
    const cells = [];
    for (let x = 0; x < 12; x++) { const c = line.getCell(x); if (!c) break; cells.push([c.getChars(), c.getWidth()]); }
    await write(t, '\x1b[2J\x1b[H' + 'a' + g.text + 'b');
    const ctx = t.buffer.active.cursorX - 2;
    widths[g.id] = { alone, ctx, cells: cells.filter(([c, w], i) => c !== '' || w !== 1 || i < alone) };
  }
  t.dispose();
  return { schema: 'kbt-widths/1', tool: `@xterm/headless ${xv} + ${variant}`, method: 'write glyph at col 0 of a 200-col terminal; alone = cursorX; ctx = cursorX after a+glyph+b minus 2', widths };
}
for (const v of ['default', 'unicode11', 'graphemes']) {
  try { fs.writeFileSync(path.join(outDir, `widths-xterm-${v}.json`), JSON.stringify(await measure(v), null, 1)); console.error('widths ok', v); }
  catch (e) { console.error('widths FAILED', v, e); fs.writeFileSync(path.join(outDir, `widths-xterm-${v}.error.txt`), String(e?.stack ?? e)); }
}
