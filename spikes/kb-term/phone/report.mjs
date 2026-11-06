// Builds out/report.md from out/results.jsonl, out/pw.json and out/shots/.
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const HERE = path.dirname(fileURLToPath(import.meta.url));
const OUT = path.join(HERE, 'out');
const rows = fs.existsSync(path.join(OUT, 'results.jsonl'))
  ? fs.readFileSync(path.join(OUT, 'results.jsonl'), 'utf8').split('\n').filter(Boolean).map((l) => JSON.parse(l)) : [];
const names = [...new Set(rows.map((r) => r.name))];
const TITLES = {
  'a-vertical': '(a) vertical drag and fling', 'a-horizontal': '(a) horizontal pan', 'a-axis-lock-wobble': '(a) axis lock with sub-8px cross-axis wobble',
  'a-below-lock': '(a) movement below the 8 px lock', 'a-cdp-real-touch': '(a) real touch events through CDP (Chromium)',
  'b-pinch': '(b) pinch to font steps', 'c-join': '(c) join cost of the 20,000-line seed', 'd-resume-drops': '(d) ResumeFrom, server-side drops',
  'd-resume-overlap': '(d) ResumeFrom with overlapping resume', 'd-resume-offline': '(d) ResumeFrom, offline/online (Chromium)', 'e-readability': '(e) readability numbers', pageerror: 'page errors',
};
let md = '# kb-term T5 phone spike report\n\n';
md += 'Generated from `results.jsonl` (one JSON line per measurement). Engines: Playwright 1.65.0 Chromium and WebKit, xterm.js 6.0.0 DOM renderer, device profiles iPhone 15 and Pixel 7 (hasTouch) crossed with both engines. WebKit here is the Playwright Linux build, not iOS Safari.\n\n';

// test outcome table
try {
  const pw = JSON.parse(fs.readFileSync(path.join(OUT, 'pw.json'), 'utf8'));
  md += '## Test outcomes\n\n| project | test | status | ms |\n|---|---|---|---|\n';
  const walk = (s, pre) => {
    for (const sp of s.specs || []) for (const t of sp.tests || []) {
      const r = (t.results || []).slice(-1)[0] || {};
      md += `| ${t.projectName} | ${[...pre, sp.title].join(' / ')} | ${t.status}${r.status && r.status !== t.status ? ' (' + r.status + ')' : ''} | ${r.duration ?? ''} |\n`;
    }
    for (const c of s.suites || []) walk(c, [...pre, c.title]);
  };
  for (const s of pw.suites || []) walk(s, []);
  md += '\n';
  const fails = [];
  const collect = (s) => { for (const sp of s.specs || []) for (const t of sp.tests || []) for (const r of t.results || []) if (r.status !== 'passed' && r.error) fails.push(`${t.projectName} / ${sp.title}: ${String(r.error.message).split('\n').slice(0, 6).join(' | ').replace(/\x1b\[[0-9;]*m/g, '')}`); for (const c of s.suites || []) collect(c); };
  for (const s of pw.suites || []) collect(s);
  if (fails.length) md += '### Failures\n\n' + fails.map((f) => '- ' + f).join('\n') + '\n\n';
} catch (e) { md += `_pw.json unavailable: ${e.message}_\n\n`; }

const cell = (v) => String(v ?? '').replace(/\|/g, '\\|').replace(/\n/g, ' ');
for (const n of names) {
  const rs = rows.filter((r) => r.name === n);
  const keys = [...new Set(rs.flatMap((r) => Object.keys(r)))].filter((k) => k !== 'name' && k !== 'project');
  md += `## ${TITLES[n] || n}\n\n`;
  md += '| ' + ['project', ...keys].join(' | ') + ' |\n|' + '---|'.repeat(keys.length + 1) + '\n';
  for (const r of rs) md += '| ' + [r.project, ...keys.map((k) => cell(r[k]))].join(' | ') + ' |\n';
  md += '\n';
}

// join-cost verdicts against explicit budgets
const joins = rows.filter((r) => r.name === 'c-join');
if (joins.length) {
  md += '## Join-cost reading\n\nBudget used for the verdict: total (parse + first render) under 1500 ms is comfortable, under 4000 ms is tolerable for a one-time join, above that is a problem.\n\n';
  for (const j of joins) {
    const t = j.totalMsMedian;
    md += `- ${j.project} / ${j.mode} (${j.renderer}): median total ${t} ms (parse ${j.parseMsMedian}, render ${j.renderMsMedian}) -> ${t < 1500 ? 'comfortable' : t < 4000 ? 'tolerable' : 'too slow'}\n`;
  }
  md += '\n';
}

// screenshots
const shotDir = path.join(OUT, 'shots');
if (fs.existsSync(shotDir)) {
  md += '## Screenshots (for the operator to judge)\n\n';
  for (const f of fs.readdirSync(shotDir).sort()) md += `### ${f}\n\n![${f}](shots/${f})\n\n`;
}
fs.writeFileSync(path.join(OUT, 'report.md'), md);
console.log('wrote report.md', md.length, 'bytes');
