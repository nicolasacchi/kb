// GroundTracker tests (T6 Spec 1): CSI splits, OSC terminators, ESC
// restart inside strings, UTF-8 partials, agreement with an independent
// whole-buffer scan on generator streams, push-order independence.
import { test, expect } from 'bun:test';
import { GroundTracker } from '../src/ground.ts';
import { mulberry32, genStream, enc } from './helpers.ts';

// reset() backs the reseed handover: pre-reset mid-sequence state must
// not leak across twin.reset() (the twin restarts from the capture).
test('reset restores ground and zeroes the byte counter', () => {
  const t = new GroundTracker();
  t.push(enc('\x1b[1;31')); // mid-CSI
  expect(t.atGround()).toBe(false);
  expect(t.offset()).toBe(6);
  t.reset();
  expect(t.atGround()).toBe(true);
  expect(t.offset()).toBe(0);
  t.push(enc('plain'));
  expect(t.atGround()).toBe(true);
  expect(t.offset()).toBe(5);
});

test('csi split at every byte offset: not ground until the final byte', () => {
  const bytes = enc('\x1b[1;31m');
  const t = new GroundTracker();
  expect(t.atGround()).toBe(true);
  expect(t.offset()).toBe(0);
  for (let i = 0; i < bytes.length; i++) {
    t.push(bytes.subarray(i, i + 1));
    expect(t.offset()).toBe(i + 1);
    expect(t.atGround()).toBe(i === bytes.length - 1);
  }
});

test('osc ended by BEL and by ST', () => {
  const bel = new GroundTracker();
  bel.push(enc('\x1b]0;title\x07'));
  expect(bel.atGround()).toBe(true);

  const st = new GroundTracker();
  st.push(enc('\x1b]0;title\x1b\\'));
  expect(st.atGround()).toBe(true);

  // Unterminated OSC is not ground.
  const open = new GroundTracker();
  open.push(enc('\x1b]0;title'));
  expect(open.atGround()).toBe(false);
});

test('esc inside an OSC restarts as a new introducer', () => {
  const t = new GroundTracker();
  t.push(enc('\x1b]8;;https://example.invalid/x'));
  expect(t.atGround()).toBe(false);
  // ESC begins a CSI (the truncated OSC is dispatched by the parser).
  t.push(enc('\x1b[31m'));
  expect(t.atGround()).toBe(true);
});

test('dcs/apc/pm/sos end on ST and on CAN/SUB only', () => {
  for (const open of ['\x1bP+q', '\x1b_Ga=T', '\x1b^pm', '\x1bXdata']) {
    const t = new GroundTracker();
    t.push(enc(open));
    expect(t.atGround()).toBe(false);
    t.push(enc('\x1b\\'));
    expect(t.atGround()).toBe(true);

    const aborted = new GroundTracker();
    aborted.push(enc(open));
    aborted.push(enc('\x18'));
    expect(aborted.atGround()).toBe(true);
  }
});

test('utf-8 lead without continuation is not ground; completes to ground', () => {
  const emoji = enc('😀'); // F0 9F 98 80
  const t = new GroundTracker();
  t.push(emoji.subarray(0, 2));
  expect(t.atGround()).toBe(false);
  t.push(emoji.subarray(2));
  expect(t.atGround()).toBe(true);

  const two = new GroundTracker();
  two.push(enc('é').subarray(0, 1)); // lead only
  expect(two.atGround()).toBe(false);
  two.push(enc('é').subarray(1));
  expect(two.atGround()).toBe(true);
});

test('c1 single bytes are not introducers', () => {
  const t = new GroundTracker();
  t.push(Uint8Array.from([0x9b, 0x31, 0x6d])); // C1 CSI as standalone bytes: no CSI state
  expect(t.atGround()).toBe(true);
});

/** Independent whole-buffer scan (different structure on purpose). */
function scanGround(buf: Uint8Array, upto: number): boolean {
  let state = 'text';
  let cont = 0;
  for (let i = 0; i < upto; i++) {
    const b = buf[i];
    if (cont > 0) {
      if (b >= 0x80 && b <= 0xbf) { cont--; continue; }
      cont = 0;
    }
    if (state === 'text') {
      if (b === 0x1b) state = 'esc';
      else if (b >= 0xc2 && b <= 0xdf) cont = 1;
      else if (b >= 0xe0 && b <= 0xef) cont = 2;
      else if (b >= 0xf0 && b <= 0xf4) cont = 3;
    } else if (state === 'esc') {
      if (b === 0x5b) state = 'csi';
      else if (b === 0x5d) state = 'osc';
      else if (b === 0x50 || b === 0x5f || b === 0x58 || b === 0x5e) state = 'str';
      else if (b === 0x18 || b === 0x1a) state = 'text';
      else if (b < 0x30 || b === 0x7f) { /* stay */ }
      else state = 'text';
    } else if (state === 'csi') {
      if (b === 0x1b) state = 'esc';
      else if (b === 0x18 || b === 0x1a) state = 'text';
      else if (b >= 0x40 && b <= 0x7e) state = 'text';
    } else if (state === 'osc') {
      if (b === 0x07 || b === 0x18 || b === 0x1a) state = 'text';
      else if (b === 0x1b) state = 'esc';
    } else { // str
      if (b === 0x18 || b === 0x1a) state = 'text';
      else if (b === 0x1b) state = 'esc';
    }
  }
  return state === 'text' && cont === 0;
}

test('generator streams agree with the independent scan at every offset', () => {
  const r = mulberry32(20261108);
  for (let k = 0; k < 12; k++) {
    const buf = genStream(r);
    const inc = new GroundTracker();
    for (let i = 0; i < buf.length; i++) {
      inc.push(buf.subarray(i, i + 1));
      const want = scanGround(buf, i + 1);
      expect(inc.atGround()).toBe(want);
      expect(inc.offset()).toBe(i + 1);
    }
  }
});

test('push-order independence: chunked equals whole', () => {
  const r = mulberry32(7);
  const buf = genStream(r);
  const whole = new GroundTracker();
  whole.push(buf);
  const chunked = new GroundTracker();
  for (let i = 0; i < buf.length; i += 7) chunked.push(buf.subarray(i, Math.min(buf.length, i + 7)));
  expect(chunked.atGround()).toBe(whole.atGround());
  expect(chunked.offset()).toBe(whole.offset());
  expect(chunked.offset()).toBe(buf.length);
});
