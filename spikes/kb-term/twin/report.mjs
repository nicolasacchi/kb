// usage: node report.mjs OUT_DIR   (reads matrix.json, widths-*.json; also the committed tmux/bun tables)
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
const HERE = path.dirname(fileURLToPath(import.meta.url));
const out = process.argv[2];
const M = JSON.parse(fs.readFileSync(path.join(out, 'matrix.json'), 'utf8'));
const L = [];
const P = (s = '') => L.push(s);
const med = (a) => { if (!a.length) return 0; const s = [...a].sort((x, y) => x - y); return s[Math.floor(s.length / 2)]; };
const summary = { schema: 'kbt-twin-summary/1', variants: {}, widths: {} };

P('# T1 twin seed fidelity and character widths (spike report)\n');
P(`Joins requested per stream: ${M.joinsRequested}. Stage A = viewer right after the seed vs the twin at the join (seed fidelity). Stage B = viewer after the remaining stream vs a reference that consumed the whole stream (convergence). Comparison is bottom-aligned over the whole normal buffer including scrollback, plus the alternate buffer, cell by cell on raw xterm cell words (chars, width, fg/bg mode+value, attributes, extended attrs: underline style, underline colour, OSC 8 link), and a state diff (cursor, pending wrap, DECSC, charsets, cursor style, scroll region, active buffer, modes, pen).\n`);

for (const variant of [...new Set(M.results.map((r) => r.variant))]) {
  const rs = M.results.filter((r) => r.variant === variant);
  P(`## Variant: ${variant}\n`);
  P('| stream | size | bytes | joins | inside ?2026 | seed bytes (min/med/max) | clean A | clean B | error |');
  P('|---|---|---|---|---|---|---|---|---|');
  const agg = { A: {}, B: {} }, V = { joins: 0, inside: 0, cleanA: 0, cleanB: 0, cleanAIn: 0, cleanBIn: 0, cleanA_noDecsc: 0, cleanB_noDecsc: 0 };
  const examples = { A: {}, B: {} };
  for (const r of rs) {
    let cA = 0, cB = 0, ins = 0;
    for (const j of r.joins) {
      const a = Object.keys(j.A.cells).length + j.A.state.length, b = Object.keys(j.B.cells).length + j.B.state.length;
      if (!a) cA++; if (!b) cB++; if (j.inside) ins++;
      V.joins++; if (j.inside) V.inside++;
      if (!Object.keys(j.A.cells).length && !j.A.state.filter((d) => d.cls !== 'DECSC saved cursor').length) V.cleanA_noDecsc++;
      if (!Object.keys(j.B.cells).length && !j.B.state.filter((d) => d.cls !== 'DECSC saved cursor').length) V.cleanB_noDecsc++;
      if (!a) { V.cleanA++; if (j.inside) V.cleanAIn++; }
      if (!b) { V.cleanB++; if (j.inside) V.cleanBIn++; }
      for (const st of ['A', 'B']) {
        const seen = new Set();
        for (const [c, n] of Object.entries(j[st].cells)) { const e = (agg[st][c] ??= { points: 0, cells: 0 }); e.points++; e.cells += n; seen.add(c); if (!examples[st][c] && j[st].examples) examples[st][c] = { stream: r.stream, off: j.off, ex: j[st].examples }; }
        if (st === 'B' && j.B.probe) { const e = (agg.B['probe: ESC 8 after remainder is ' + j.B.probe] ??= { points: 0, cells: 0, sample: j.B.probeDetail ? JSON.stringify(j.B.probeDetail).slice(0, 200) : '' }); e.points++; }
        for (const d of j[st].state) { const c = 'state: ' + d.cls; const e = (agg[st][c] ??= { points: 0, cells: 0, sample: `${d.key}: ${JSON.stringify(d.a)} vs ${JSON.stringify(d.b)} (stream ${r.stream} @${j.off})` }); e.points++; }
      }
    }
    const sb = r.joins.map((j) => j.seedBytes).sort((x, y) => x - y);
    P(`| ${r.stream} | ${r.cols}x${r.rows} | ${r.bytes} | ${r.joins.length} | ${ins} | ${sb[0] ?? 0}/${med(sb)}/${sb[sb.length - 1] ?? 0} | ${cA} | ${cB} | ${r.error ? 'YES' : ''} |`);
  }
  P();
  P(`Totals: ${V.joins} join points (${V.inside} inside a ?2026 frame). Fully clean seed (A): ${V.cleanA}/${V.joins} (inside-frame: ${V.cleanAIn}/${V.inside}). Fully converged after remainder (B): ${V.cleanB}/${V.joins} (inside-frame: ${V.cleanBIn}/${V.inside}). Ignoring the latent DECSC saved-cursor slot: A ${V.cleanA_noDecsc}/${V.joins}, B ${V.cleanB_noDecsc}/${V.joins}.\n`);
  summary.variants[variant] = { totals: V, classes: agg, errors: rs.filter((r) => r.error).map((r) => ({ stream: r.stream, error: r.error.split('\n')[0] })) };
  for (const st of ['A', 'B']) {
    P(`### ${variant} stage ${st} ${st === 'A' ? '(seed fidelity)' : '(convergence with reference)'}: mismatch classes\n`);
    const ks = Object.keys(agg[st]).sort((x, y) => agg[st][y].points - agg[st][x].points);
    if (!ks.length) { P('No mismatches of any class.\n'); continue; }
    P('| class | join points affected | differing cells (sum) | sample |'); P('|---|---|---|---|');
    for (const k of ks) {
      const e = agg[st][k], ex = examples[st][k];
      let sample = e.sample ?? '';
      if (!sample && ex) { const first = Object.values(ex.ex).flatMap((o) => Object.values(o)).flat()[0]; if (first) sample = `${ex.stream}@${ex.off} row-from-bottom ${first.rowFromBottom} col ${first.col}: ${JSON.stringify(first.a).slice(0, 110)} vs ${JSON.stringify(first.b).slice(0, 110)}`; }
      P(`| ${k} | ${e.points} | ${e.cells} | ${String(sample).replace(/\|/g, '/').slice(0, 260)} |`);
    }
    P();
  }
}

// ---- widths ----
P('## Character widths\n');
const tables = {};
const load = (name, file) => { try { tables[name] = JSON.parse(fs.readFileSync(file, 'utf8')).widths; } catch (e) { P(`(table ${name} unavailable: ${e.message})\n`); } };
load('xterm-u6', path.join(out, 'widths-xterm-default.json'));
load('xterm-u11', path.join(out, 'widths-xterm-unicode11.json'));
load('xterm-graphemes', path.join(out, 'widths-xterm-graphemes.json'));
load('tmux-3.7c', path.join(HERE, 'widths-tmux-3.7c.json'));
load('bun', path.join(HERE, 'widths-bun.json'));
const glyphs = JSON.parse(fs.readFileSync(path.join(HERE, 'glyphs.json'), 'utf8')).glyphs;
const names = Object.keys(tables);
const get = (n, g, k) => tables[n]?.[g.id]?.[k];
const pairs = [];
for (let i = 0; i < names.length; i++) for (let j = i + 1; j < names.length; j++) pairs.push([names[i], names[j]]);
P('Agreement of standalone glyph advance (glyph alone at col 0), all glyphs, per pair of tools:\n');
P('| pair | agree | disagree |'); P('|---|---|---|');
summary.widths.pairs = {};
for (const [a, b] of pairs) {
  let ag = 0, dis = 0;
  for (const g of glyphs) (get(a, g, 'alone') === get(b, g, 'alone') ? ag++ : dis++);
  P(`| ${a} vs ${b} | ${ag} | ${dis} |`); summary.widths.pairs[`${a} vs ${b}`] = { agree: ag, disagree: dis };
}
P();
const byCls = {};
for (const g of glyphs) { const c = (byCls[g.cls] ??= { n: 0, perPairDis: {} }); c.n++; for (const [a, b] of pairs) if (get(a, g, 'alone') !== get(b, g, 'alone')) c.perPairDis[`${a} vs ${b}`] = (c.perPairDis[`${a} vs ${b}`] ?? 0) + 1; }
P('Disagreements by glyph class (count of glyphs on which the pair differs):\n');
P('| class | glyphs | ' + pairs.map((p) => p.join(' vs ')).join(' | ') + ' |'); P('|---|---|' + pairs.map(() => '---').join('|') + '|');
for (const [c, v] of Object.entries(byCls)) P(`| ${c} | ${v.n} | ` + pairs.map((p) => v.perPairDis[p.join(' vs ')] ?? 0).join(' | ') + ' |');
P();
P('Full per-glyph table (alone advance; ctx differs from alone is marked with a star on the cell):\n');
P('| glyph id | code points | ' + names.join(' | ') + ' |'); P('|---|---|' + names.map(() => '---').join('|') + '|');
for (const g of glyphs) {
  const vals = names.map((n) => { const a = get(n, g, 'alone'), c = get(n, g, 'ctx'); return `${a ?? '?'}${c !== a ? `*(${c})` : ''}`; });
  const dis = new Set(names.map((n) => get(n, g, 'alone'))).size > 1;
  P(`| ${dis ? '**' + g.id + '**' : g.id} | ${g.cps} | ${vals.join(' | ')} |`);
}
P('\nBold glyph ids are glyphs on which at least two tools disagree.\n');
// placeholder specifics
const ph = glyphs.filter((g) => g.cls === 'placeholder');
P('### U+10EEEE placeholder cells (the kb renderer needs exactly 1 cell per placeholder+diacritics)\n');
P('| id | ' + names.join(' | ') + ' |'); P('|---|' + names.map(() => '---').join('|') + '|');
for (const g of ph) P(`| ${g.id} | ${names.map((n) => get(n, g, 'alone') ?? '?').join(' | ')} |`);
P();
fs.writeFileSync(path.join(out, 'report.md'), L.join('\n') + '\n');
fs.writeFileSync(path.join(out, 'summary.json'), JSON.stringify(summary, null, 1));
console.error('report written');
