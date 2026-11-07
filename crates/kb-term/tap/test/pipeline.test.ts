// Tap pipeline tests (T3): control-mode bytes in, viewer-bound (query-stripped) pane
// bytes out. One QueryStripper per pane; panes never share parser state.
import { test, expect } from 'bun:test';
import { TapPipeline } from '../src/pipeline.ts';
import { stripAll } from '../src/strip.ts';
import { E, enc, cat, eq, mulberry32, harness } from './helpers.ts';

const BEL = '\x07';
const esc = (s: string) => escBytes(enc(s));
/** control-mode octal escaping, as tmux puts stream bytes into a %output value */
const escBytes = (raw: Uint8Array): Uint8Array => {
  const out: number[] = [];
  for (const b of raw) {
    if (b < 0x20 || b === 0x7f || b === 0x5c) out.push(0x5c, 0x30 + (b >> 6), 0x30 + ((b >> 3) & 7), 0x30 + (b & 7));
    else out.push(b);
  }
  return Uint8Array.from(out);
};
/** one %output control line carrying raw stream bytes */
const outputLine = (pane: string, raw: Uint8Array): Uint8Array => cat([enc(`%output ${pane} `), escBytes(raw), enc('\n')]);

test('control_stream_to_viewer_bytes', () => {
  // A recorded-shape control stream: octal-escaped %output lines wrapping a
  // fake_harness.py slice with queries injected, split into random chunks.
  const base = harness(11, 60);
  const queries = `${E}[c${E}[6n${E}]11;?${BEL}`;
  const raw = cat([enc('prefix-'), base, enc(queries), enc('-suffix')]);
  const r = mulberry32(0x11fe);
  const p = new TapPipeline();
  const parts: Uint8Array[] = [];
  let pos = 0;
  const bytes = outputLine('%0', raw);
  while (pos < bytes.length) {
    const step = 1 + Math.floor(r() * 97);
    const end = Math.min(bytes.length, pos + step);
    for (const o of p.feed(bytes.subarray(pos, end))) {
      if ('pane' in o) parts.push(o.bytes);
      else throw new Error(`unexpected event ${o.event.kind}`);
    }
    pos = end;
  }
  p.flush('%0');
  const got = cat(parts);
  expect(eq(got, stripAll(raw))).toBe(true);
  expect(p.pending('%0')).toBe(0);
});

test('panes_do_not_share_parser_state', () => {
  const p = new TapPipeline();
  const out: { pane: string; bytes: Uint8Array }[] = [];
  const drain = (chunk: Uint8Array) => {
    for (const o of p.feed(chunk)) if ('pane' in o) out.push(o);
  };
  // pane %0 ends mid-ESC[ (unfinished CSI, no final byte yet); pane %1 carries a
  // plain 'c', which must stay plain text and must not complete %0's sequence.
  drain(enc(`%output %0 ${esc(`${E}[6`)}\n`));
  expect(p.pending('%0')).toBeGreaterThan(0);
  drain(enc(`%output %1 c\n`));
  expect(p.pending('%1')).toBe(0);
  drain(outputLine('%0', enc('nw')));   // completes pane %0's ESC[6n: DSR, stripped
  expect(p.pending('%0')).toBe(0);
  const p0 = cat(out.filter(o => o.pane === '%0').map(o => o.bytes));
  const p1 = cat(out.filter(o => o.pane === '%1').map(o => o.bytes));
  expect(eq(p0, enc('w'))).toBe(true);   // only 'w' survived: the DSR died, nothing spliced
  expect(eq(p1, enc('c'))).toBe(true);   // plain text on %1
});

test('reply_lines_never_reach_a_pane_stream', () => {
  const p = new TapPipeline();
  const outs = p.feed(cat([
    enc(`%begin 5 9 0\n`),
    enc(`%output %0 `), escBytes(enc(`${E}[c`)), enc('should-be-reply-not-output\n'),
    enc(`%end 5 9 0\n`),
    outputLine('%0', enc('plain')),
  ]));
  const paneBytes = cat(outs.flatMap(o => ('pane' in o ? [o.bytes] : [])));
  const kinds = outs.flatMap(o => ('event' in o ? [o.event.kind] : []));
  expect(kinds).toEqual(['begin', 'reply', 'end']);
  expect(eq(paneBytes, enc('plain'))).toBe(true);  // the reply line produced no pane bytes
});

test('pause_does_not_reset_the_stripper', () => {
  const p = new TapPipeline();
  const outs: { pane: string; bytes: Uint8Array }[] = [];
  const drain = (chunk: Uint8Array) => {
    for (const o of p.feed(chunk)) if ('pane' in o) outs.push(o);
  };
  drain(enc(`%output %0 ${esc(`${E}[6`)}\n`));
  expect(p.pending('%0')).toBeGreaterThan(0);     // undecided CSI held back
  drain(enc(`%pause %0\n`));
  drain(enc(`%continue %0\n`));
  drain(enc(`%output %0 ${esc('n')}done\n`));    // completes DSR -> stripped, then 'done'
  const got = cat(outs.map(o => o.bytes));
  expect(eq(got, enc('done'))).toBe(true);        // the unfinished ESC[6 survived pause/continue
  expect(p.pending('%0')).toBe(0);
});

test('onStrip_reports_pane_label_and_raw', () => {
  const seen: [string, string][] = [];
  const p = new TapPipeline({ onStrip: (pane, label, _raw) => { seen.push([pane, label]); } });
  p.feed(outputLine('%9', enc(`${E}[c${E}[1m` + 'x')));
  expect(seen).toEqual([['%9', 'DA1']]);
});
