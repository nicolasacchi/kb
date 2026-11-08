// sgrFromAttr tests (T6 Spec 4): every SGR dimension round-trips through a
// fresh terminal -- write SGR to twin A, capture its pen, replay the string
// on twin B, require the pens to be identical. Fails without the encoder.
import { test, expect } from 'bun:test';
import { makeTwin } from '../src/twin.ts';
import { captureTwinState } from '../src/seed-state.ts';
import { sgrFromAttr, type AttrCopy } from '../src/sgr.ts';

async function penAfter(seq: string): Promise<AttrCopy> {
  const twin = makeTwin(40, 10);
  try {
    await twin.feed(seq);
    return captureTwinState(twin).pen;
  } finally {
    twin.dispose();
  }
}

async function roundTrip(seq: string): Promise<void> {
  const a = await penAfter(seq);
  const b = await penAfter(`\x1b[${sgrFromAttr(a)}m`);
  expect(b).toEqual(a);
}

test('all flag bits round-trip', async () => {
  for (const code of ['1', '2', '3', '4', '5', '7', '8', '9', '53']) {
    await roundTrip(`\x1b[0;${code}m`);
  }
  await roundTrip('\x1b[0;1;2;3;4;5;7;8;9;53m');
});

test('palette and RGB fg/bg round-trip', async () => {
  for (const seq of [
    '\x1b[0;31m', '\x1b[0;42m', '\x1b[0;90m', '\x1b[0;100m',
    '\x1b[0;38;5;200m', '\x1b[0;48;5;7m',
    '\x1b[0;38;2;1;2;3m', '\x1b[0;48;2;255;128;0m',
    '\x1b[0;1;38;2;10;20;30;48;5;99m',
  ]) {
    await roundTrip(seq);
  }
});

test('underline style and colour round-trip', async () => {
  for (const seq of [
    '\x1b[0;4:1m', '\x1b[0;4:2m', '\x1b[0;4:3m', '\x1b[0;4:4m', '\x1b[0;4:5m',
    '\x1b[0;4:3;58:5;42m', '\x1b[0;4:5;58:2::9;8;7m',
    '\x1b[0;1;4:4;58:5;1;38;2;200;100;50m',
  ]) {
    await roundTrip(seq);
  }
});

test('default-underline sentinel (SGR 59) encodes to plain reset', async () => {
  const a = await penAfter('\x1b[0;58;5;9;59m');
  expect(sgrFromAttr(a)).toBe('0');
  const b = await penAfter('\x1b[0;59m');
  expect(sgrFromAttr(b)).toBe('0');
});

test('sgrFromAttr is deterministic, starts with 0, colon forms for colour', () => {
  expect(sgrFromAttr({ fg: 0x3000000 | (1 << 16) | (2 << 8) | 3, bg: 0, underlineStyle: 0, underlineColor: 0 }))
    .toBe('0;38;2;1;2;3');
  expect(sgrFromAttr({ fg: 0, bg: 0, underlineStyle: 5, underlineColor: 0x3000000 | (9 << 16) | (8 << 8) | 7 }))
    .toBe('0;4:5;58:2::9:8:7');
  expect(sgrFromAttr(null)).toBe('0');
  expect(sgrFromAttr({ fg: 0, bg: 0, underlineStyle: 0, underlineColor: 0 })).toBe('0');
});
