// Withhold/route (T7 test 4): mouse enables, kitty keyboard, OSC 52,
// exhaustive split invariance. Byte-at-a-time machine, no timers.
import { test, expect } from 'bun:test';
import { Withholder } from '../src/withhold.ts';

const E = '\x1b';
const BEL = '\x07';
const ST = `${E}\\`;
const enc = (s: string) => new TextEncoder().encode(s);
const dec = (b: Uint8Array) => new TextDecoder().decode(b);

function run(data: Uint8Array, cuts: number[] = []): { out: Uint8Array; clipboards: { offset: number; data_b64: string }[] } {
  const w = new Withholder();
  const parts: Uint8Array[] = [];
  const clipboards: { offset: number; data_b64: string }[] = [];
  let p = 0;
  let head = 0;
  for (const c of [...cuts, data.length]) {
    const r = w.push(data.subarray(p, c), head);
    parts.push(r.bytes);
    head += r.bytes.length;
    clipboards.push(...r.clipboards);
    p = c;
  }
  const tail = w.flush();
  parts.push(tail.bytes);
  const total = parts.reduce((a, b) => a + b.length, 0);
  const out = new Uint8Array(total);
  let o = 0;
  for (const part of parts) {
    out.set(part, o);
    o += part.length;
  }
  return { out, clipboards };
}

const out = (s: string): string => dec(run(enc(s)).out);

test('withhold_removes_every_main_screen_mouse_enable', () => {
  for (const pm of [9, 1000, 1001, 1002, 1003, 1005, 1006, 1015, 1016]) {
    expect(out(`${E}[?${pm}h`)).toBe('');
    expect(out(`a${E}[?${pm}hb`)).toBe('ab');
  }
});

test('withhold_mixed_mouse_list_drops_only_mouse_params', () => {
  expect(out(`${E}[?1000;25h`)).toBe(`${E}[?25h`);
  expect(out(`${E}[?25;1002;7h`)).toBe(`${E}[?25;7h`);
});

test('withhold_mouse_resets_pass', () => {
  expect(out(`${E}[?1000l${E}[?1002l`)).toBe(`${E}[?1000l${E}[?1002l`);
});

test('withhold_alt_screen_passes_everything', () => {
  expect(out(`${E}[?1049h${E}[?1000h`)).toBe(`${E}[?1049h${E}[?1000h`);
  expect(out(`${E}[?47h${E}[?1002h`)).toBe(`${E}[?47h${E}[?1002h`);
  expect(out(`${E}[?1047h${E}[?9h`)).toBe(`${E}[?1047h${E}[?9h`);
});

test('withhold_leaving_alt_restores_main_screen_removal', () => {
  expect(out(`${E}[?1049h${E}[?1049l${E}[?1000h`)).toBe(`${E}[?1049h${E}[?1049l`);
  expect(out(`${E}[?1049h${E}[?1047l${E}[?1000h`)).toBe(`${E}[?1049h${E}[?1047l`);
});

test('withhold_removes_kitty_keyboard_push_pop_set', () => {
  expect(out(`${E}[>1u`)).toBe('');
  expect(out(`${E}[<u${E}[<1u`)).toBe('');
  expect(out(`${E}[=7;1u`)).toBe('');
  expect(out(`x${E}[>4u${E}[<2uy`)).toBe('xy');
});

test('withhold_keeps_kitty_enhancement_flags_byte_for_byte', () => {
  expect(out(`${E}[>4;2m`)).toBe(`${E}[>4;2m`);
  expect(out(`${E}[>4m${E}[>15m`)).toBe(`${E}[>4m${E}[>15m`);
});

test('withhold_osc52_is_rerouted_as_side_event', () => {
  const r = run(enc(`ab${E}]52;c;aGVsbG8=${BEL}cd${E}]52;p;V29ybGQh${ST}e`));
  expect(dec(r.out)).toBe('abcde');
  expect(r.clipboards).toEqual([
    { offset: 2, data_b64: 'aGVsbG8=' },
    { offset: 4, data_b64: 'V29ybGQh' },
  ]);
});

test('withhold_osc52_without_separator_passes_verbatim', () => {
  const s = `${E}]52;${BEL}x${E}]52${ST}`;
  expect(out(s)).toBe(s);
});

test('withhold_other_osc_passes_verbatim', () => {
  const s = `${E}]0;title${BEL}x${E}]8;;https://example.invalid/${ST}link${E}]8;;${ST}`;
  expect(out(s)).toBe(s);
});

test('withhold_osc52_over_1mib_dropped_with_stream_unchanged', () => {
  const big = new Uint8Array(2 * 1024 * 1024).fill(0x41);
  const head = enc(`${E}]52;c;`);
  const tail = enc(`${BEL}after`);
  const data = new Uint8Array(head.length + big.length + tail.length);
  data.set(head);
  data.set(big, head.length);
  data.set(tail, head.length + big.length);
  const r = run(data);
  expect(dec(r.out)).toBe('after');
  expect(r.clipboards).toEqual([]);
});

test('withhold_overcap_csi_passes_verbatim_even_with_mouse_param', () => {
  // Over-cap CSI passes verbatim, so the mouse enable inside it is NOT
  // filtered (brief rule 7 over-cap pass-through). A short sequence IS.
  const s = `${E}[?1000${';1'.repeat(40)}h`;
  expect(enc(s).length).toBeGreaterThan(64);
  expect(out(s)).toBe(s);
  expect(out(`${E}[?1000;1h`)).toBe(`${E}[?1h`);
});

test('withhold_sync_mode_tracking_passes_through', () => {
  const w = new Withholder();
  expect(w.syncOn).toBe(false);
  w.push(enc(`${E}[?2026h`));
  expect(w.syncOn).toBe(true);
  w.push(enc(`${E}[?2026l`));
  expect(w.syncOn).toBe(false);
  expect(out(`${E}[?2026h${E}[?2026l`)).toBe(`${E}[?2026h${E}[?2026l`);
});

test('withhold_flush_emits_held_partial_verbatim', () => {
  const w = new Withholder();
  w.push(enc(`ab${E}[?10`));
  expect(w.pending()).toBeGreaterThan(0);
  const tail = w.flush();
  expect(dec(tail.bytes)).toBe(`${E}[?10`);
});

test('withhold_flush_overcap_csi_adds_nothing', () => {
  // Over-cap bytes already streamed out; flush must not append a stray ESC[.
  const w = new Withholder();
  const part = `${E}[?1000${';1'.repeat(40)}`;
  const r = w.push(enc(part));
  expect(dec(r.bytes)).toBe(part);
  expect(w.flush().bytes.length).toBe(0);
});

test('withhold_flush_partial_osc52_passes_verbatim', () => {
  // Its docstring promises verbatim: a truncated OSC 52 is still held in
  // full and goes out as-is.
  const w = new Withholder();
  w.push(enc(`${E}]52;c;aGk`));
  expect(dec(w.flush().bytes)).toBe(`${E}]52;c;aGk`);
});

const SPLIT_CORPUS = `hi${E}[?1000hX${E}[?1000;25hY${E}[>1uZ${E}]52;c;aGk=${BEL}W${E}[?1049h${E}[?1000hV${E}[?1049l${E}[?2026h${E}[?2026l${E}[>4;2m${E}[?1000l${E}]0;title${BEL}end`;

test('withhold_exhaustive_1cut_splits_equal_unsplit', () => {
  const data = enc(SPLIT_CORPUS);
  const whole = run(data);
  for (let i = 0; i <= data.length; i++) {
    const r = run(data, [i]);
    expect(Array.from(r.out)).toEqual(Array.from(whole.out));
    expect(r.clipboards).toEqual(whole.clipboards);
  }
});

test('withhold_exhaustive_2cut_splits_equal_unsplit', () => {
  const data = enc(`a${E}[?1000h${E}[>1ub${E}]52;c;eA==${BEL}c${E}[?1049h${E}[?1002h${E}[?1049l`);
  const whole = run(data);
  for (let i = 0; i <= data.length; i++) {
    for (let j = i; j <= data.length; j++) {
      const r = run(data, [i, j]);
      expect(Array.from(r.out)).toEqual(Array.from(whole.out));
      expect(r.clipboards).toEqual(whole.clipboards);
    }
  }
});

test('withhold_aborted_sequences_do_not_leak', () => {
  // A CSI aborted by a new ESC is dropped; the new sequence is judged on
  // its own (mouse enable on the main screen is withheld).
  expect(out(`${E}[?10${E}[?1000h`)).toBe('');
  // CAN aborts the held sequence; the CAN itself passes.
  expect(out(`${E}[?10\x18${E}[?1000h`)).toBe('\x18');
});
