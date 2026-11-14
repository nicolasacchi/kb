// Pins test (T6 Spec 10): xterm versions, grapheme activation, and the full
// internal-path list captureTwinState depends on. The mutation check deletes
// a buffer field and requires captureTwinState to throw (fail closed).
import { test, expect } from 'bun:test';
import { createRequire } from 'node:module';
import { dirname, join } from 'node:path';
import { readFileSync } from 'node:fs';
import { makeTwin } from '../../src/twin.ts';
import { captureTwinState, INTERNALS_PATHS } from '../../src/seed-state.ts';

const require = createRequire(import.meta.url);

/** Read package.json by walking up from a resolved main entry (no exports map). */
function pkgVersion(spec: string): string {
  let dir = dirname(require.resolve(spec));
  for (let i = 0; i < 8; i++) {
    try {
      const pkg = JSON.parse(readFileSync(join(dir, 'package.json'), 'utf-8')) as { name?: string; version?: string };
      if (pkg.name === spec && typeof pkg.version === 'string') return pkg.version;
    } catch { /* keep walking */ }
    dir = dirname(dir);
  }
  throw new Error(`cannot find package.json for ${spec}`);
}

test('xterm package versions are the T6 pins', () => {
  expect(pkgVersion('@xterm/headless')).toBe('6.0.0');
  expect(pkgVersion('@xterm/addon-serialize')).toBe('0.14.0');
  expect(pkgVersion('@xterm/addon-unicode-graphemes')).toBe('0.4.0');
});

/** Wrap-cell line fields read by readWrapCell (seed-state.ts): instance
 * fields on lines.get(i), asserted on a content line since they are not
 * static paths in INTERNALS_PATHS. */
test('wrap-cell line fields exist on a content line', async () => {
  const twin = makeTwin(40, 12);
  try {
    await twin.feed(new TextEncoder().encode(`${'\x1b'}[1;31mx`));
    const term = twin.term as unknown as Record<string, unknown>;
    const core = term._core as Record<string, unknown>;
    const bs = core._bufferService as Record<string, unknown>;
    const buffers = bs.buffers as Record<string, Record<string, unknown>>;
    const line = (buffers.normal.lines as { get(i: number): Record<string, unknown> }).get(0);
    for (const field of ['_data', '_combined', '_extendedAttrs']) {
      expect(field in line).toBe(true);
    }
    const data = line._data;
    expect(data instanceof Uint32Array).toBe(true);
  } finally {
    twin.dispose();
  }
});

test('every internal path captureTwinState reads exists', () => {
  const twin = makeTwin(40, 12);
  try {
    const term = twin.term as unknown as Record<string, unknown>;
    for (const path of INTERNALS_PATHS) {
      let obj: unknown = term;
      const parts = path.split('.');
      for (const part of parts) {
        if (obj === null || typeof obj !== 'object' || !(part in (obj as Record<string, unknown>))) {
          throw new Error(`internal path missing: ${path} (at ${part})`);
        }
        obj = (obj as Record<string, unknown>)[part];
      }
      // cursorStyle/cursorBlink/savedCharset are legitimately
      // undefined until set (capture uses needMaybe and maps to null).
      const maybeUndef = path.endsWith('cursorStyle') || path.endsWith('cursorBlink') || path.endsWith('savedCharset');
    }
  } finally {
    twin.dispose();
  }
});

test('graphemes variant activates a grapheme width version', () => {
  const twin = makeTwin(40, 12);
  try {
    expect(twin.term.unicode.activeVersion).toMatch(/grapheme/i);
    expect(twin.widthPin.activeVersion).toMatch(/grapheme/i);
  } finally {
    twin.dispose();
  }
  const u11 = makeTwin(40, 12, { variant: 'unicode11' });
  try {
    expect(u11.term.unicode.activeVersion).toBe('11');
  } finally {
    u11.dispose();
  }
});

test('deleting savedX makes captureTwinState throw (mutation)', () => {
  const twin = makeTwin(20, 8);
  try {
    const term = twin.term as unknown as {
      _core: { _bufferService: { buffers: { normal: Record<string, unknown> } } };
    };
    const buf = term._core._bufferService.buffers.normal;
    const saved = buf.savedX;
    delete buf.savedX;
    expect(() => captureTwinState(twin)).toThrow(/seed: xterm internals changed/);
    buf.savedX = saved;
  } finally {
    twin.dispose();
  }
});

test('deleting lines makes captureTwinState throw (mutation, FXT6)', () => {
  const twin = makeTwin(20, 8);
  try {
    const term = twin.term as unknown as {
      _core: { _bufferService: { buffers: { normal: Record<string, unknown> } } };
    };
    const buf = term._core._bufferService.buffers.normal;
    const saved = buf.lines;
    delete buf.lines;
    // Fail-closed: lines (and its length) is load-bearing for the pending
    // wrap cell and scrollback count, so a missing lines is an internals
    // change, never a silent default.
    expect(() => captureTwinState(twin)).toThrow(/seed: xterm internals changed/);
    buf.lines = saved;
  } finally {
    twin.dispose();
  }
});

test('deleting the diagnostic charset property still captures (FXT6)', () => {
  // `_core._charsetService.charset` is diagnostic-only (charsetJson feeds
  // state comparison, never seed bytes): its absence must not fail the
  // capture. Pins seed-state.ts's tolerant read against a future
  // "helpful" fail-closed change.
  const twin = makeTwin(20, 8);
  try {
    const term = twin.term as unknown as {
      _core: { _charsetService: Record<string, unknown> };
    };
    const cs = term._core._charsetService;
    let saved: unknown;
    let had = false;
    if ('charset' in cs) { saved = cs.charset; had = true; delete cs.charset; }
    const st = captureTwinState(twin);
    expect(st.charsetJson).toBeNull();
    if (had) cs.charset = saved;
  } finally {
    twin.dispose();
  }
});
