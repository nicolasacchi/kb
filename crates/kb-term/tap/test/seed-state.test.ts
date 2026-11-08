// Seed-state tests (T6 Spec 3): one hand-built stream per tail item,
// compared with stateOf. Each test first proves the bare-serialize seed is
// NOT clean (fails without the tail), then the buildSeed seed IS clean.
// The skip-mutation loop proves every tail item is load-bearing.
import { test, expect } from 'bun:test';
import { makeTwin, type Twin } from '../src/twin.ts';
import { buildSeed, type TailItem } from '../src/seed.ts';
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
async function expectClean(twin: Twin, seedBytes: Uint8Array, _note: string): Promise<void> {
  void _note;
  const classes = await stageClasses(twin, seedBytes);
  expect([...classes]).toEqual([]);
}

async function expectBareDirty(twin: Twin, cls: string): Promise<void> {
  const classes = await stageClasses(twin, bareBody(twin));
  expect(classes.has(cls)).toBe(true);
}

function groundOf(data: Uint8Array): GroundTracker {
  const g = new GroundTracker();
  g.push(data);
  return g;
}

test('DECSTBM scroll region is restored', async () => {
  const twin = await twinWith(40, 12, `abc${E}[3;10r${E}[5;5Hhello${E}[2;2H`);
  try {
    await expectBareDirty(twin, 'scroll region');
    const seed = buildSeed(twin, { scrollback: 50000, atOffset: 1, ground: groundOf(enc('')) });
    await expectClean(twin, seed.bytes, 'region seed');
  } finally {
    twin.dispose();
  }
});

test('DECSC slot (position and pen) is restored', async () => {
  const twin = await twinWith(40, 12, `${E}[1;31m${E}[4;7H${E}7${E}[0m${E}[1;1Hxyz`);
  try {
    await expectBareDirty(twin, 'DECSC saved cursor');
    const seed = buildSeed(twin, { scrollback: 50000, atOffset: 1 });
    await expectClean(twin, seed.bytes, 'decsc seed');
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
    await expectBareDirty(twin, 'pending wrap');
    const seed = buildSeed(twin, { scrollback: 50000, atOffset: 1 });
    await expectClean(twin, seed.bytes, 'pending-wrap seed');
  } finally {
    twin.dispose();
  }
});

test('origin mode with a scroll region is restored', async () => {
  const twin = await twinWith(40, 12, `${E}[3;10r${E}[?6h${E}[2;2Hhi${E}[1;1H`);
  try {
    await expectBareDirty(twin, 'scroll region');
    const seed = buildSeed(twin, { scrollback: 50000, atOffset: 1 });
    await expectClean(twin, seed.bytes, 'origin seed');
  } finally {
    twin.dispose();
  }
});

test('cursor style and visibility are restored', async () => {
  const twin = await twinWith(40, 12, `${E}[5 q${E}[?25l`); // bar, blink off; cursor hidden
  try {
    const seed = buildSeed(twin, { scrollback: 50000, atOffset: 1 });
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

test('alt active: normal DECSC slot survives composition (uses ?47h, not ?1049h)', async () => {
  const twin = await twinWith(40, 12, `${E}[1;32m${E}[2;3H${E}7${E}[0m${E}[?1049h${E}[2J${E}[5;5HALT`);
  try {
    // Bare serialize composes with ?1049h, clobbering the slot: proven dirty.
    await expectBareDirty(twin, 'DECSC saved cursor');
    const seed = buildSeed(twin, { scrollback: 50000, atOffset: 1 });
    const text = Buffer.from(seed.bytes).toString('latin1');
    expect(text.includes(`${E}[?47h`)).toBe(true);
    expect(text.includes(`${E}[?1049h`)).toBe(false);
    await expectClean(twin, seed.bytes, 'alt seed');
  } finally {
    twin.dispose();
  }
});

test('pen with a curly underline is restored', async () => {
  const twin = await twinWith(40, 12, `${E}[0;4:3;58;5;42mcurly ${E}[0m`);
  try {
    await expectBareDirty(twin, 'pen (current SGR)');
    const seed = buildSeed(twin, { scrollback: 50000, atOffset: 1 });
    await expectClean(twin, seed.bytes, 'curly pen seed');
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

test('skip mutations: every tail item is load-bearing', async () => {
  const seq = `${E}[1;31m${E}[3;10r${E}[?6h${E}[5 q${E}7${E}[0;32m${E}[2;2Hhi${E}[3;1H${'x'.repeat(40)}`;
  const twin = await twinWith(40, 12, seq);
  try {
    const cases: Array<[TailItem, string]> = [
      ['decstbm', 'scroll region'],
      ['decsc', 'DECSC saved cursor'],
      ['pen', 'pen (current SGR)'],
      ['pendingWrap', 'pending wrap'],
      // Body re-emits ?1/?66/?4/?6/?2004/?1004, so modes-skip is observable
      // via DECSCUSR (cursor style class) and ?25 (cursor visibility).
      ['modes', 'cursor style'],
    ];
    for (const [item, cls] of cases) {
      const seed = buildSeed(twin, { scrollback: 50000, atOffset: 1, skip: [item] });
      const classes = await stageClasses(twin, seed.bytes);
      expect(classes.has(cls)).toBe(true);
    }
  } finally {
    twin.dispose();
  }
});

test('skip alt mutation: ?1049h clobbers the normal slot', async () => {
  const twin = await twinWith(40, 12, `${E}[1;32m${E}[2;3H${E}7${E}[0m${E}[?1049h${E}[2J${E}[5;5HALT`);
  try {
    const seed = buildSeed(twin, { scrollback: 50000, atOffset: 1, skip: ['alt'] });
    const classes = await stageClasses(twin, seed.bytes);
    expect(classes.has('DECSC saved cursor')).toBe(true);
  } finally {
    twin.dispose();
  }
});
