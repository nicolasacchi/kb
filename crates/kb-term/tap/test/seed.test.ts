// Seed gate tests (T6 Spec 2/9): quiescent refusal + acceptance, the
// write-callback race rule, seed hygiene (no mouse/kitty/?2031), size/time
// bounds, and a deterministic-bytes golden. The golden's sha256 is pinned in
// GOLDEN_SHA256 after the first CI run (spec 8: print, then pin).
import { test, expect } from 'bun:test';
import { createHash } from 'node:crypto';
import { makeTwin, type Twin } from '../src/twin.ts';
import { buildSeed, isQuiescent, QuiescentJoinGate } from '../src/seed.ts';
import { GroundTracker } from '../src/ground.ts';
import { enc } from './helpers.ts';

const E = '\x1b';

// sha256 of buildSeed bytes for the fixed stream+offset below, pinned after
// the first CI run (the determinism print in this test emits it).
const GOLDEN_SHA256: string | null = null;

async function gatePair(cols = 40, rows = 12): Promise<{ twin: Twin; gate: QuiescentJoinGate; ground: GroundTracker }> {
  const twin = makeTwin(cols, rows);
  const ground = new GroundTracker();
  return { twin, gate: new QuiescentJoinGate(twin, ground), ground };
}

test('refuses a join inside a ?2026 frame; accepts after ?2026l', async () => {
  const { twin, gate, ground } = await gatePair();
  try {
    await gate.feed(enc(`${E}[?2026h${E}[2J${E}[31mR`));
    expect(isQuiescent(twin, ground)).toBe(false);
    expect(gate.currentJoinPoint()).toEqual({ offset: gate.parsed, quiescent: false, reason: 'sync-frame' });
    expect(() => buildSeed(twin, { scrollback: 50000, atOffset: gate.parsed, ground })).toThrow(/not quiescent/);
    // nextQuiescent times out without a frame end.
    const jp = await gate.nextQuiescent(20); // timeout contract: resolves non-quiescent
    expect(jp.quiescent).toBe(false);
    await gate.feed(enc(`${E}[?2026l`));
    expect(isQuiescent(twin, ground)).toBe(true);
    const seed = buildSeed(twin, { scrollback: 50000, atOffset: gate.parsed, ground });
    expect(seed.bytes.length).toBeGreaterThan(0);
    expect(seed.offset).toBe(gate.parsed);
  } finally {
    twin.dispose();
  }
});

test('refuses mid-sequence; converges once the sequence completes', async () => {
  const { twin, gate, ground } = await gatePair();
  try {
    await gate.feed(enc(`${E}[31`)); // split CSI: ESC [ 3 1 (no final)
    expect(isQuiescent(twin, ground)).toBe(false);
    expect(() => buildSeed(twin, { scrollback: 50000, atOffset: gate.parsed, ground })).toThrow(/not quiescent/);
    await gate.feed(enc('m'));
    expect(isQuiescent(twin, ground)).toBe(true);
    const seed = buildSeed(twin, { scrollback: 50000, atOffset: gate.parsed, ground });
    const viewer = makeTwin(40, 12);
    try {
      await viewer.feed(seed.bytes);
      await viewer.feed(enc('Z'));
    } finally {
      viewer.dispose();
    }
    expect(gate.currentJoinPoint().quiescent).toBe(true);
  } finally {
    twin.dispose();
  }
});

test('mid-escape: OSC title and UTF-8 cuts are not quiescent', async () => {
  const { twin, gate, ground } = await gatePair();
  try {
    await gate.feed(enc(`${E}]0;tit`));
    expect(isQuiescent(twin, ground)).toBe(false);
    await gate.feed(enc('le\x07'));
    expect(isQuiescent(twin, ground)).toBe(true);
    const e = enc('😀');
    await gate.feed(e.subarray(0, 2)); // partial UTF-8
    expect(isQuiescent(twin, ground)).toBe(false);
    await gate.feed(e.subarray(2));
    expect(isQuiescent(twin, ground)).toBe(true);
  } finally {
    twin.dispose();
  }
});

test('tracker runs in the write callback: a slow write never resolves early', async () => {
  const twin = makeTwin(40, 12);
  const ground = new GroundTracker();
  const gate = new QuiescentJoinGate(twin, ground);
  try {
    // Replace write with a deliberately slow one (callback after the data).
    let release: (() => void) | null = null;
    const origWrite = twin.term.write.bind(twin.term);
    (twin.term as { write: unknown }).write = (data: Uint8Array, cb?: () => void) => {
      release = () => origWrite(data, cb);
    };
    const fed = gate.feed(enc('hello'));
    expect(gate.parsed).toBe(0);
    expect(ground.atGround()).toBe(true); // tracker has not been advanced yet
    let resolved = false;
    void fed.then(() => { resolved = true; });
    await new Promise<void>((r) => setImmediate(r));
    expect(resolved).toBe(false);
    release?.();
    await fed;
    expect(resolved).toBe(true);
    expect(gate.parsed).toBe(5);
    expect(ground.offset()).toBe(5);
  } finally {
    twin.dispose();
  }
});

test('no mouse enables, kitty-keyboard push/set, ?2031 or queries in any seed', async () => {
  const seqs = [
    `${E}[31m${E}[?7lhello${E}[?7h`,
    `${E}[>4;2m${E}[1;32mhi`,
    `${E}[?2004h${E}[4h${E}[?25l${E}[?25h${E}[?2004l`,
    `${E}[?1049h${E}[2JALT${E}[?1049l`,
  ];
  for (const seq of seqs) {
    const twin = makeTwin(40, 12);
    try {
      await twin.feed(enc(seq));
      const seed = buildSeed(twin, { scrollback: 50000, atOffset: 1 });
      const s = Buffer.from(seed.bytes).toString('latin1');
      for (const pat of ['1000', '1002', '1003', '1005', '1006', '1015', '1016', '2031']) {
        expect(s.includes(`${E}[?${pat}`)).toBe(false);
      }
      expect(s.includes(`${E}[>`)).toBe(false);
      expect(s.includes(`${E}[?2026`)).toBe(false);
      expect(seed.withheld.mouse).toBe(0);
    } finally {
      twin.dispose();
    }
  }
});

test('mouse-on twin: body enables are filtered and counted', async () => {
  const twin = makeTwin(40, 12);
  try {
    await twin.feed(enc(`${E}[?1000h${E}[?1002h${E}[31mhi`));
    const seed = buildSeed(twin, { scrollback: 50000, atOffset: 1 });
    expect(seed.withheld.mouse).toBeGreaterThan(0);
    const s = Buffer.from(seed.bytes).toString('latin1');
    expect(s.includes(`${E}[?1000h`)).toBe(false);
    expect(s.includes(`${E}[?1002h`)).toBe(false);
  } finally {
    twin.dispose();
  }
});

test('deterministic bytes for a fixed stream+offset (golden sha256)', async () => {
  const seq = `${E}[?2026h${E}[H${E}[2J${E}[3J${E}[1;31mhello${E}[?2026l${E}[3;10r${'x'.repeat(40)}`;
  const twin = makeTwin(40, 12);
  try {
    await twin.feed(enc(seq));
    const a = buildSeed(twin, { scrollback: 50000, atOffset: 99 });
    const b = buildSeed(twin, { scrollback: 50000, atOffset: 99 });
    expect(a.bytes).toEqual(b.bytes);
    const sha = createHash('sha256').update(a.bytes).digest('hex');
    console.log(`seed golden sha256: ${sha}`);
    if (GOLDEN_SHA256 !== null) expect(sha).toBe(GOLDEN_SHA256);
  } finally {
    twin.dispose();
  }
});

test('size and build-time bounds on a 60k-line twin', async () => {
  const cols = 100, rows = 30;
  const twin = makeTwin(cols, rows);
  try {
    const LINE = 'line 123456 ' + 'x'.repeat(cols - 13) + '\r\n';
    const chunks: Uint8Array[] = [];
    for (let i = 0; i < 60000; i++) chunks.push(enc(LINE));
    const big = new Uint8Array(chunks.reduce((n, c) => n + c.length, 0));
    let off = 0;
    for (const c of chunks) { big.set(c, off); off += c.length; }
    await twin.feed(big);
    const t0 = Date.now();
    const desktop = buildSeed(twin, { scrollback: 50000, atOffset: big.length });
    const ms = Date.now() - t0;
    expect(desktop.bytes.length).toBeLessThanOrEqual(4.5 * 1024 * 1024);
    expect(ms).toBeLessThanOrEqual(3000);
    const phone = buildSeed(twin, { scrollback: 20000, atOffset: big.length });
    expect(phone.bytes.length).toBeLessThanOrEqual(2.2 * 1024 * 1024);
    console.log(`size/time: desktop=${desktop.bytes.length}B phone=${phone.bytes.length}B build=${ms}ms`);
  } finally {
    twin.dispose();
  }
}, 900_000);
