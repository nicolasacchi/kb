// Twin factory, pins and width tables (T5 tests 1-5). Bun test, no tmux.
import { test, expect } from 'bun:test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { metric, writeMetrics } from './helpers.ts';

const FIXTURES = new URL('./fixtures/', import.meta.url).pathname;

interface WidthsFile {
  schema: string;
  widths: Record<string, { alone: number; ctx: number }>;
}

const loadWidths = (name: string): WidthsFile =>
  JSON.parse(readFileSync(`${FIXTURES}${name}`, 'utf-8')) as WidthsFile;

function cursorX(twin: Twin): number {
  const buffer = twin.term.buffer as unknown as { active: { cursorX: number; cursorY: number } };
  return buffer.active.cursorX;
}

function cursorY(twin: Twin): number {
  const buffer = twin.term.buffer as unknown as { active: { cursorX: number; cursorY: number } };
  return buffer.active.cursorY;
}

/** Cursor advance for one glyph written at col 0 of a fresh line. */
async function aloneWidth(twin: Twin, text: string): Promise<number> {
  twin.reset();
  await twin.feed('\x1b[H');
  await twin.feed(text);
  return cursorX(twin);
}

test('twin_options_are_the_pinned_ones', async () => {
  expect(TWIN_OPTS).toEqual({ scrollback: 50000, scrollOnEraseInDisplay: true, allowProposedApi: true });
  expect(PHONE_SCROLLBACK).toBe(20000);
  const twin = makeTwin(200, 50);
  try {
    expect(twin.widthPin.version).toBe('0.4.0');
    expect(twin.widthPin.addon).toBe('unicode-graphemes');
    expect(twin.widthPin.activeVersion).toMatch(/grapheme/i);
    expect(twin.term.unicode.activeVersion).toBe(twin.widthPin.activeVersion);
    expect(twin.lineCount('normal')).toBe(50);

    // The 297 kitty row/column diacritics from the committed synth fixture:
    // each U+10EEEE + row diacritic + column diacritic must occupy exactly 1
    // cell (the diacritics combine into the placeholder cell). Without the
    // explicit activeVersion pin the spike measured 207 of these as split.
    const src = readFileSync(new URL('../../fixtures/synth_omp_kitty_stream.py', import.meta.url).pathname, 'utf-8');
    const m = /DIAC = \[([0-9,\s]+)\]/.exec(src);
    expect(m).not.toBeNull();
    const diac = (m?.[1] ?? '').split(',').map((s) => s.trim()).filter((s) => s.length > 0).map(Number);
    expect(diac.length).toBe(297);
    const placeholder = '\u{10EEEE}';
    let split = 0;
    for (let i = 0; i + 1 < diac.length; i += 2) {
      const glyph = placeholder + String.fromCodePoint(diac[i]) + String.fromCodePoint(diac[i + 1]);
      if ((await aloneWidth(twin, glyph)) !== 1) split++;
    }
    expect(split).toBe(0);
  } finally {
    twin.dispose();
  }
});

test('twin_throws_when_graphemes_registers_no_version', () => {
  class StubAddon {
    activate(_term: unknown): void {
      void _term;
    }
  }
  expect(() => makeTwin(80, 24, { graphemesAddonForTest: StubAddon })).toThrow(/graphemes addon registered no version/);
});

test('twin_never_answers_queries', async () => {
  const twin = makeTwin(80, 24);
  try {
    // DA1, DSR 6n, XTVERSION (DA2-ish shape \x1b[>0c), OSC 11 read: every one
    // would make xterm produce output if a reply hook were attached.
    await twin.feed('\x1b[c');
    await twin.feed('\x1b[6n');
    await twin.feed('\x1b[>0c');
    await twin.feed('\x1b]11;?\x07');
    await twin.feed('\x1b[?2026h');
    expect(twin.repliesDropped()).toBeGreaterThan(0);
    // The Twin exposes no reply hook: there is nowhere to write back to.
    for (const k of ['onData', 'pushData', 'reply', 'writeBack']) {
      expect(k in twin).toBe(false);
    }
  } finally {
    twin.dispose();
  }
});

test('xterm_headless_version_is_6_0_0', () => {
  const readVersion = (pkg: string): string => {
    const p = join(new URL('../', import.meta.url).pathname, 'node_modules', pkg, 'package.json');
    return (JSON.parse(readFileSync(p, 'utf-8')) as { version: string }).version;
  };
  expect(readVersion('@xterm/headless')).toBe('6.0.0');
  expect(readVersion('@xterm/addon-serialize')).toBe('0.14.0');
  expect(readVersion('@xterm/addon-unicode-graphemes')).toBe('0.4.0');
});

test('width_tables_match_recorded_facts', async () => {
  const glyphsFile = JSON.parse(readFileSync(`${FIXTURES}glyphs.json`, 'utf-8')) as {
    schema: string;
    glyphs: { id: string; cls: string; text: string }[];
  };
  const tmux = loadWidths('widths-tmux-3.7c.json');
  const bun = loadWidths('widths-bun.json');
  expect(glyphsFile.schema).toBe('kbt-glyphs/1');
  expect(tmux.schema).toBe('kbt-widths/1');

  const byId = new Map(glyphsFile.glyphs.map((g) => [g.id, g]));
  const graphemes = makeTwin(200, 10);
  const u11 = makeTwin(200, 10, { variant: 'unicode11' });
  try {
    // Recorded facts: U+10EEEE alone and with diacritics is exactly 1 cell in
    // every table (xterm graphemes, xterm unicode11, tmux, Bun).
    for (const id of ['placeholder/bare', 'placeholder/row0', 'placeholder/row0col0', 'placeholder/row1col2']) {
      const g = byId.get(id);
      expect(g).toBeDefined();
      expect(tmux.widths[id].alone).toBe(1);
      expect(bun.widths[id].alone).toBe(1);
      expect(await aloneWidth(graphemes, g?.text ?? '')).toBe(1);
      expect(await aloneWidth(u11, g?.text ?? '')).toBe(1);
    }
    expect(await aloneWidth(graphemes, byId.get('placeholder/three-in-a-row')?.text ?? '')).toBe(3);
    expect(tmux.widths['placeholder/three-in-a-row'].alone).toBe(3);

    // A lone diacritic differs: xterm gives it 1 cell, tmux/Bun give 0.
    expect(tmux.widths['placeholder/lone-diacritic'].alone).toBe(0);
    expect(bun.widths['placeholder/lone-diacritic'].alone).toBe(0);
    expect(await aloneWidth(graphemes, byId.get('placeholder/lone-diacritic')?.text ?? '')).toBe(1);
    expect(await aloneWidth(u11, byId.get('placeholder/lone-diacritic')?.text ?? '')).toBe(1);

    // Graphemes matches tmux on VS16 (expectation = tmux's recorded width).
    for (const g of glyphsFile.glyphs.filter((x) => x.cls === 'vs16')) {
      expect(await aloneWidth(graphemes, g.text)).toBe(tmux.widths[g.id].alone);
    }

    // Post-Unicode-11 emoji lag in BOTH xterm tables: the recorded residual
    // is that xterm does not match tmux on at least one post-11 glyph in each
    // variant (documented known residual, not a pass condition). The count is
    // reported as a metric; agreement on pre-11 emoji is expected in both.
    let lagGraph = 0;
    let lag11 = 0;
    let totalPost = 0;
    for (const g of glyphsFile.glyphs.filter((x) => x.cls === 'emoji-post11')) {
      totalPost++;
      const expected = tmux.widths[g.id].alone;
      if ((await aloneWidth(graphemes, g.text)) !== expected) lagGraph++;
      if ((await aloneWidth(u11, g.text)) !== expected) lag11++;
    }
    expect(totalPost).toBe(16);
    expect(lagGraph).toBeGreaterThan(0);
    expect(lag11).toBeGreaterThan(0);
    for (const g of glyphsFile.glyphs.filter((x) => x.cls === 'emoji-pre11')) {
      expect(await aloneWidth(graphemes, g.text)).toBe(2);
      expect(await aloneWidth(u11, g.text)).toBe(2);
    }
    metric('twin', { post11LagGraphemes: lagGraph, post11LagUnicode11: lag11, post11Total: totalPost });
    writeMetrics('twin');
    expect(cursorX(graphemes)).toBeGreaterThanOrEqual(0);
    expect(cursorY(graphemes)).toBe(0);
  } finally {
    graphemes.dispose();
    u11.dispose();
  }
});
