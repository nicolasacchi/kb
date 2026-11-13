// Seed-state tests (T6 Spec 3): one hand-built stream per tail item,
// compared with stateOf. Each test first proves the bare-serialize seed is
// NOT clean (fails without the tail), then the buildSeed seed IS clean.
// The skip-mutation loop proves every tail item is load-bearing.
import { test, expect } from 'bun:test';
import { makeTwin, type Twin } from '../src/twin.ts';
import { buildSeed, SeedCharsetError, type TailItem } from '../src/seed.ts';
import { captureTwinState } from '../src/seed-state.ts';
import { GroundTracker } from '../src/ground.ts';
import { stateOf, diffState } from './matrix/lib.ts';
import { enc } from './helpers.ts';

const E = '\x1b';

async function twinWith(cols: number, rows: number, seq: string): Promise<Twin> {
  const twin = makeTwin(cols, rows);
  await twin.feed(enc(seq));
  return twin;
}

/** Stage-A diff classes of feeding `bytes` to a fresh viewer vs the twin. */
async function stageClasses(twin: Twin, bytes: Uint8Array): Promise<Set<string>> {
  const viewer = makeTwin(twin.cols, twin.rows);
  try {
    await viewer.feed(bytes);
    const diffs = diffState(stateOf(twin.term as never), stateOf(viewer.term as never));
    return new Set(diffs.map((d) => d.cls));
  } finally {
    viewer.dispose();
  }
}

function bareBody(twin: Twin): Uint8Array {
  const ser = twin.serialize as unknown as { serialize(o: { scrollback?: number }): string };
  return enc(ser.serialize.call(twin.serialize, { scrollback: 50000 }));
}
async function expectClean(twin: Twin, seedBytes: Uint8Array): Promise<void> {
  const classes = await stageClasses(twin, seedBytes);
  expect([...classes]).toEqual([]);
}

async function expectBareDirty(twin: Twin, cls: string): Promise<void> {
  const classes = await stageClasses(twin, bareBody(twin));
  expect(classes.has(cls)).toBe(true);
}

function groundOf(data: Uint8Array, pending: number): GroundTracker {
  // Helper-local assertion (both params are load-bearing): these fixtures
  // feed the twin synchronously, so the feed is always fully parsed and
  // pending is always 0. A non-zero value here would mean the fixture is
  // lying about quiescence; fail the test instead of building a bad seed.
  if (pending !== 0) throw new Error(`groundOf: pending must be 0 in synchronous fixtures, got ${pending}`);
  const g = new GroundTracker();
  g.push(data);
  return g;
}

test('DECSTBM scroll region is restored', async () => {
  const twin = await twinWith(40, 12, `abc${E}[3;10r${E}[5;5Hhello${E}[2;2H`);
  try {
    await expectBareDirty(twin, 'scroll region');
    const seed = buildSeed(twin, { scrollback: 50000, atOffset: 1, ground: groundOf(enc(''), 0), pending: 0 });
    await expectClean(twin, seed.bytes);
  } finally {
    twin.dispose();
  }
});

test('DECSC slot (position and pen) is restored', async () => {
  const twin = await twinWith(40, 12, `${E}[1;31m${E}[4;7H${E}7${E}[0m${E}[1;1Hxyz`);
  try {
    await expectBareDirty(twin, 'DECSC saved cursor');
    const seed = buildSeed(twin, { scrollback: 50000, atOffset: 1, ground: groundOf(enc(''), 0), pending: 0 });
    await expectClean(twin, seed.bytes);
    // probe: ESC 8 restores the twin slot position (4;7 -> x=6) in the viewer.
    const viewer = makeTwin(40, 12);
    try {
      await viewer.feed(seed.bytes);
      await viewer.feed(enc(`${E}8`));
      expect(stateOf(viewer.term as never)['normal.x']).toBe(captureTwinState(twin).normal.savedX);
    } finally {
      viewer.dispose();
    }
  } finally {
    twin.dispose();
  }
});

test('pending wrap at the last column is restored', async () => {
  const twin = await twinWith(10, 5, `${E}[1;31m${'x'.repeat(10)}`);
  try {
    // Bare serialize already reproduces pending wrap (x === cols), so
    // load-bearingness is pinned via skip + seed-content, not bare-dirty.
    const seed = buildSeed(twin, { scrollback: 50000, atOffset: 1, ground: groundOf(enc(''), 0), pending: 0 });
    await expectClean(twin, seed.bytes);
    const full = Buffer.from(seed.bytes).toString('latin1');
    const skippedSeed = buildSeed(twin, { scrollback: 50000, atOffset: 1, ground: groundOf(enc(''), 0), pending: 0, skip: ['pendingWrap'] });
    const skipped = Buffer.from(skippedSeed.bytes).toString('latin1');
    // Full tail rewrites the wrapped cell (CUP 1;10 + pen SGR + 'x'); the
    // skipped tail omits the rewrite char.
    expect(full.includes(`${E}[1;10H${E}[0;1;31mx`)).toBe(true);
    expect(skipped.includes(`${E}[1;10H${E}[0;1;31mx`)).toBe(false);
  } finally {
    twin.dispose();
  }
});

test('origin mode with a scroll region is restored', async () => {
  const twin = await twinWith(40, 12, `${E}[3;10r${E}[?6h${E}[2;2Hhi${E}[1;1H`);
  try {
    await expectBareDirty(twin, 'scroll region');
    const seed = buildSeed(twin, { scrollback: 50000, atOffset: 1, ground: groundOf(enc(''), 0), pending: 0 });
    await expectClean(twin, seed.bytes);
  } finally {
    twin.dispose();
  }
});

test('cursor style and visibility are restored', async () => {
  const twin = await twinWith(40, 12, `${E}[5 q${E}[?25l`); // bar, blink off; cursor hidden
  try {
    const seed = buildSeed(twin, { scrollback: 50000, atOffset: 1, ground: groundOf(enc(''), 0), pending: 0 });
    const viewer = makeTwin(40, 12);
    try {
      await viewer.feed(seed.bytes);
      expect(diffState(stateOf(twin.term as never), stateOf(viewer.term as never))).toEqual([]);
    } finally {
      viewer.dispose();
    }
  } finally {
    twin.dispose();
  }
});
test('DECSCUSR block/underline/bar x blink/steady round-trips', async () => {
  for (const n of [0, 1, 2, 3, 4, 5, 6]) {
    const twin = await twinWith(40, 12, `${E}[${n} q`);
    try {
      const seed = buildSeed(twin, { scrollback: 50000, atOffset: 1, ground: groundOf(enc(''), 0), pending: 0 });
      const viewer = makeTwin(40, 12);
      try {
        await viewer.feed(seed.bytes);
        const a = stateOf(twin.term as never);
        const b = stateOf(viewer.term as never);
        const tail = Buffer.from(seed.bytes).toString('latin1').slice(-60);
        try {
          expect(b['decCursorStyle']).toBe(a['decCursorStyle']);
          expect(b['decCursorBlink']).toBe(a['decCursorBlink']);
        } catch (err) {
          // On failure print the trailing seed bytes (the 'q' sequence
          // and everything after it) plus the observed values, so the
          // CI log shows what follows the DECSCUSR sequence.
          console.log(`DECSCUSR n=${n} twin=${JSON.stringify(a['decCursorStyle'])}/${JSON.stringify(a['decCursorBlink'])} viewer=${JSON.stringify(b['decCursorStyle'])}/${JSON.stringify(b['decCursorBlink'])} tail=${JSON.stringify(tail)}`);
          throw err;
        }
      } finally {
        viewer.dispose();
      }
    } finally {
      twin.dispose();
    }
  }
});

test('alt active: normal DECSC slot survives composition (uses ?47h, not ?1049h)', async () => {
  // Enter alt via ?47h (no save) so the normal ESC7 slot stays distinct
  // from the live main cursor; the alt slot is set in-alt, so a bare body
  // (which replays ?1049h but never ESC7) shows the difference on both.
  const twin = await twinWith(40, 12, `${E}[1;32m${E}[2;3H${E}7${E}[5;5H${E}[0m${E}[?47h${E}[8;8H${E}7${E}[2J${E}[9;9HALT`);
  try {
    await expectBareDirty(twin, 'DECSC saved cursor');
    const seed = buildSeed(twin, { scrollback: 50000, atOffset: 1, ground: groundOf(enc(''), 0), pending: 0 });
    const text = Buffer.from(seed.bytes).toString('latin1');
    expect(text.includes(`${E}[?47h`)).toBe(true);
    expect(text.includes(`${E}[?1049h`)).toBe(false);
    await expectClean(twin, seed.bytes);
  } finally {
    twin.dispose();
  }
});

test('pen with a curly underline is restored', async () => {
  const twin = await twinWith(40, 12, `${E}[0;4:3;58;5;42mcurly`);
  try {
    // Bare serialize preserves the pen (its body restores _curAttrData),
    // so load-bearingness is pinned via skip + seed-content below.
    const seed = buildSeed(twin, { scrollback: 50000, atOffset: 1, ground: groundOf(enc(''), 0), pending: 0 });
    await expectClean(twin, seed.bytes);
    const full = Buffer.from(seed.bytes).toString('latin1');
    const skippedSeed = buildSeed(twin, { scrollback: 50000, atOffset: 1, ground: groundOf(enc(''), 0), pending: 0, skip: ['pen'] });
    const skipped = Buffer.from(skippedSeed.bytes).toString('latin1');
    expect(full.includes(`${E}[0;4:3;58:5:42m`)).toBe(true);
    expect(skipped.includes(`${E}[0;4:3;58:5:42m`)).toBe(false);
  } finally {
    twin.dispose();
  }
});

test('captureTwinState fails closed when an internal is missing', async () => {
  const twin = makeTwin(20, 8);
  try {
    const term = twin.term as unknown as { _core: { _bufferService: { buffers: { normal: Record<string, unknown> } } } };
    const buf = term._core._bufferService.buffers.normal;
    const saved = buf.savedX;
    delete buf.savedX;
    expect(() => captureTwinState(twin)).toThrow(/seed: xterm internals changed/);
    buf.savedX = saved;
  } finally {
    twin.dispose();
  }
});

test('skip mutations: region/DECSC/modes tail items are load-bearing', async () => {
  const seq = `${E}[1;31m${E}[3;10r${E}[?6h${E}[5 q${E}7${E}[0;32m${E}[2;2Hhi${E}[3;1H${'x'.repeat(40)}`;
  const twin = await twinWith(40, 12, seq);
  try {
    // 'pen' and 'pendingWrap' skips produce no stage-A class (bare
    // serialize already preserves them) so they are pinned by content
    // in their dedicated tests; only cases that actually produce a
    // class stay here.
    const cases: Array<[TailItem, string]> = [
      ['decstbm', 'scroll region'],
      ['decsc', 'DECSC saved cursor'],
      // Body re-emits ?1/?66/?4/?6/?2004/?1004, so modes-skip is observable
      // via DECSCUSR (cursor style class) and ?25 (cursor visibility).
      ['modes', 'cursor style'],
    ];
    for (const [item, cls] of cases) {
      const seed = buildSeed(twin, { scrollback: 50000, atOffset: 1, ground: groundOf(enc(''), 0), pending: 0, skip: [item] });
      const classes = await stageClasses(twin, seed.bytes);
      expect(classes.has(cls)).toBe(true);
    }
  } finally {
    twin.dispose();
  }
});

test('buildSeed fails closed on a non-default charset/glevel', async () => {
  const plain = makeTwin(20, 8);
  try {
    await plain.feed(enc('hello'));
    expect(() => buildSeed(plain, { scrollback: 50000, atOffset: 1, ground: groundOf(enc(''), 0), pending: 0 })).not.toThrow();
  } finally {
    plain.dispose();
  }
  const glevel = await twinWith(40, 12, `${E}n${E}[1;1Hhi`);
  try {
    expect(() => buildSeed(glevel, { scrollback: 50000, atOffset: 1, ground: groundOf(enc(''), 0), pending: 0 }))
      .toThrow(/seed: non-default charset\/glevel/);
  } finally {
    glevel.dispose();
  }
  const charset = await twinWith(40, 12, `${E}(0${E}7${E}[1;1Hhi`);
  try {
    expect(() => buildSeed(charset, { scrollback: 50000, atOffset: 1, ground: groundOf(enc(''), 0), pending: 0 }))
      .toThrow(/seed: non-default charset\/glevel/);
  } finally {
    charset.dispose();
  }
});

test('skip alt mutation: ?1049h clobbers the normal slot', async () => {
  const twin = await twinWith(40, 12, `${E}[1;32m${E}[2;3H${E}7${E}[5;5H${E}[0m${E}[?47h${E}[8;8H${E}7${E}[2J${E}[9;9HALT`);
  try {
    const seed = buildSeed(twin, { scrollback: 50000, atOffset: 1, ground: groundOf(enc(''), 0), pending: 0, skip: ['alt'] });
    const classes = await stageClasses(twin, seed.bytes);
    expect(classes.has('DECSC saved cursor')).toBe(true);
  } finally {
    twin.dispose();
  }
});

test('charset refusal is a typed SeedCharsetError with a stable code (FXT6)', async () => {
  const glevel = await twinWith(40, 12, `${E}n${E}[1;1Hhi`);
  try {
    let caught: unknown;
    try {
      buildSeed(glevel, { scrollback: 50000, atOffset: 1, ground: groundOf(enc(''), 0), pending: 0 });
    } catch (e) {
      caught = e;
    }
    expect(caught).toBeInstanceOf(SeedCharsetError);
    expect((caught as SeedCharsetError).code).toBe('seed/charset-unsupported');
  } finally {
    glevel.dispose();
  }
});

test('pending wrap after a wide grapheme is restored', async () => {
  // FXT6 defect 10: pending wrap whose last two columns hold a width-2
  // grapheme, plus the empty-cell case below. The wide grapheme's head
  // lives at cols-2 and travels in the serialize body; the wrap cell at
  // cols-1 is that grapheme's trailing spacer (blank, code 0), so the tail
  // rewrites it with a space (readWrapCell returns ' ' for the spacer) --
  // the write itself restores pending wrap, and expectClean proves the
  // viewer converges on cursor/pen state. Pinned bytes: CUP(1,10) + SGR + ' '.
  const cols = 10, rows = 5;
  // Fill row 1: 8 ASCII + 'あ' (width 2) -> cursor past last column (pending).
  const twin = await twinWith(cols, rows, `${E}[1;31m${'x'.repeat(8)}あ`);
  try {
    const seed = buildSeed(twin, { scrollback: 50000, atOffset: 1, ground: groundOf(enc(''), 0), pending: 0 });
    await expectClean(twin, seed.bytes);
    // Golden pin: the tail rewrites the wrap cell at column cols with a
    // space after an SGR (not the grapheme itself, which is the spacer's
    // neighbour in the body). The SGR value itself is not pinned here:
    // what matters is the rewrite char and that the viewer converges.
    const full = Buffer.from(seed.bytes).toString('utf8');
    expect(full.includes(`${E}[1;10H`)).toBe(true);
    expect(/\[1;10H\x1b\[[0-9;:]*m /.test(full)).toBe(true);
  } finally {
    twin.dispose();
  }
});

test('pending wrap on an empty cell rewrites a space', async () => {
  // Empty-cell pending wrap: the wrap cell is blank (code 0 -> ' '); the
  // rewrite must emit a space so the viewer also reports pending wrap.
  // Fill the row (pending wrap), then erase the line in place (ESC[2K keeps
  // the cursor, blanks the wrap cell).
  const cols = 10, rows = 5;
  const twin = await twinWith(cols, rows, `${E}[1;32m${'x'.repeat(9)}y${E}[2K`);
  try {
    const st = captureTwinState(twin);
    expect(st.normal.pendingWrap).toBe(true);
    const seed = buildSeed(twin, { scrollback: 50000, atOffset: 1, ground: groundOf(enc(''), 0), pending: 0 });
    await expectClean(twin, seed.bytes);
    const full = Buffer.from(seed.bytes).toString('utf8');
    expect(full.includes(`${E}[1;10H`)).toBe(true);
  } finally {
    twin.dispose();
  }
});
