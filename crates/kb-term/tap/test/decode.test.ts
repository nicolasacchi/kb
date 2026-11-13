// Control-mode decoder tests (T3). Synthetic octal-escaped lines only.
// tmux escapes control characters inside %output values: \033 is ESC, \134 is a backslash.
import { test, expect } from 'bun:test';
import { ControlDecoder, decodeValue, type CtlEvent } from '../src/decode.ts';
import { E, enc, eq } from './helpers.ts';

const NL = '\n';

test('octal_unescape_matches_tmux', () => {
  // \033 (ESC), \134 (backslash), \000 (NUL), \177 (DEL)
  expect(eq(decodeValue(enc('\\033[6n')), Uint8Array.from([0x1b, 0x5b, 0x36, 0x6e]))).toBe(true);
  expect(eq(decodeValue(enc('a\\134b')), Uint8Array.from([0x61, 0x5c, 0x62]))).toBe(true);
  expect(eq(decodeValue(enc('x\\000y\\177')), Uint8Array.from([0x78, 0, 0x79, 0x7f]))).toBe(true);
  // raw UTF-8 untouched
  const u = enc('π ─ 日本 ❤️');
  expect(eq(decodeValue(u), u)).toBe(true);
  // malformed \9 and a trailing backslash: backslash emitted literally
  expect(eq(decodeValue(enc('\\9x')), enc('\\9x'))).toBe(true);
  expect(eq(decodeValue(enc('ab\\')), enc('ab\\'))).toBe(true);

  // through the decoder, bad escapes are counted
  const d = new ControlDecoder();
  const evs = d.push(enc('%output %0 a\\9b\\005\\' + NL));
  expect(evs.length).toBe(1);
  const ev = evs[0];
  expect(ev.kind).toBe('output');
  if (ev.kind === 'output') {
    expect(eq(ev.data, Uint8Array.from([0x61, 0x5c, 0x39, 0x62, 0x05, 0x5c]))).toBe(true);
    expect(ev.pane).toBe('%0');
  }
  expect(d.stats.badEscapes).toBe(2);   // \9 and the trailing lone backslash
  expect(d.stats.malformed).toBe(0);
});

test('line_split_across_chunks', () => {
  const d = new ControlDecoder();
  const line = enc(`%output %12 hello\\033[1m world` + NL + `%pause %12` + NL);
  // feed one byte at a time: a line may span arbitrary chunks
  const evs: CtlEvent[] = [];
  for (let i = 0; i < line.length; i++) evs.push(...d.push(line.subarray(i, i + 1)));
  expect(evs.length).toBe(2);
  expect(evs[0].kind).toBe('output');
  if (evs[0].kind === 'output') {
    expect(evs[0].pane).toBe('%12');
    expect(eq(evs[0].data, enc(`hello${E}[1m world`))).toBe(true);
  }
  expect(evs[1].kind).toBe('pause');
  expect(d.stats.badEscapes).toBe(0);
});

test('extended_output_value_is_after_first_space_colon_space', () => {
  const d = new ControlDecoder();
  // value itself contains ` : `; the first one is the separator, the rest is data
  const evs = d.push(enc('%extended-output %3 42 something : a : b\\033[hc' + NL));
  expect(evs.length).toBe(1);
  const ev = evs[0];
  expect(ev.kind).toBe('extended-output');
  if (ev.kind === 'extended-output') {
    expect(ev.pane).toBe('%3');
    expect(ev.ageMs).toBe(42);
    expect(eq(ev.data, enc(`a : b${E}[hc`))).toBe(true);
  }
  // missing ` : ` -> malformed++, 'other'
  const evs2 = d.push(enc('%extended-output %3 42 no separator here' + NL));
  expect(evs2.length).toBe(1);
  expect(evs2[0].kind).toBe('other');
  expect(d.stats.malformed).toBe(1);
});

test('extended_output_strips_one_trailing_cr_but_output_does_not', () => {
  const d = new ControlDecoder();
  // CRLF framing: extended-output value must not end with a stray 0x0d byte.
  const evs = d.push(enc('%extended-output %3 42 foo : v' + '\r' + NL));
  expect(evs.length).toBe(1);
  expect(evs[0].kind).toBe('extended-output');
  if (evs[0].kind === 'extended-output') expect(eq(evs[0].data, enc('v'))).toBe(true);
  // %output keeps a raw trailing \r verbatim (spec rule 1 strips only
  // non-%output control lines).
  const evs2 = d.push(enc('%output %0 v' + '\r' + NL));
  expect(evs2.length).toBe(1);
  expect(evs2[0].kind).toBe('output');
  if (evs2[0].kind === 'output') expect(eq(evs2[0].data, Uint8Array.from([0x76, 0x0d]))).toBe(true);
});

test('extended_output_empty_age_is_malformed', () => {
  const d = new ControlDecoder();
  // Number('') === 0 must not be accepted as age 0.
  const evs = d.push(enc('%extended-output %3  : v' + NL));
  expect(evs.length).toBe(1);
  expect(evs[0].kind).toBe('other');
  expect(d.stats.malformed).toBe(1);
});

test('reply_block_lines_are_not_notifications', () => {
  const d = new ControlDecoder();
  const stream =
    `%begin 99 7 0` + NL +
    `%output %0 hello` + NL +      // reply text, NOT output: must not inject bytes
    `capture content %exit` + NL +
    `%end 99 7 0` + NL +
    `%output %0 real` + NL;
  const evs = d.push(enc(stream));
  expect(evs.map(e => e.kind)).toEqual(['begin', 'reply', 'reply', 'end', 'output']);
  const b = evs[0];
  expect(b.kind).toBe('begin');
  if (b.kind === 'begin') { expect(b.time).toBe('99'); expect(b.num).toBe('7'); expect(b.flags).toBe('0'); }
  const r = evs[1];
  expect(r.kind).toBe('reply');
  if (r.kind === 'reply') expect(eq(r.line, enc('%output %0 hello'))).toBe(true);
  const o = evs[4];
  expect(o.kind).toBe('output');
  // an unmatched %end outside a block is 'other'
  const d2 = new ControlDecoder();
  const orphan = d2.push(enc('%end 1 1 0' + NL));
  expect(orphan.length).toBe(1);
  expect(orphan[0].kind).toBe('other');
});

test('error_terminates_reply_block_and_next_output_decodes', () => {
  const d = new ControlDecoder();
  const stream =
    `%begin 99 7 0` + NL +
    `%output %0 hello` + NL +      // reply text, NOT output
    `%error 99 7 0` + NL +
    `%output %0 real` + NL;
  const evs = d.push(enc(stream));
  expect(evs.map(e => e.kind)).toEqual(['begin', 'reply', 'error', 'output']);
  const r = evs[1];
  expect(r.kind).toBe('reply');
  if (r.kind === 'reply') expect(eq(r.line, enc('%output %0 hello'))).toBe(true);
  const e = evs[2];
  expect(e.kind).toBe('error');
  if (e.kind === 'error') { expect(e.time).toBe('99'); expect(e.num).toBe('7'); expect(e.flags).toBe('0'); }
  const o = evs[3];
  expect(o.kind).toBe('output');
  if (o.kind === 'output') {
    expect(o.pane).toBe('%0');
    expect(eq(o.data, enc('real'))).toBe(true);
  }
});

test('pause_continue_exit_events', () => {
  const d = new ControlDecoder();
  const evs = d.push(enc(`%pause %0${NL}%continue %7${NL}%exit too far behind${NL}%exit${NL}`));
  expect(evs.map(e => e.kind)).toEqual(['pause', 'continue', 'exit', 'exit']);
  const p = evs[0]; const c = evs[1]; const x = evs[2]; const x2 = evs[3];
  expect(p.kind === 'pause' && p.pane === '%0').toBe(true);
  expect(c.kind === 'continue' && c.pane === '%7').toBe(true);
  expect(x.kind === 'exit' && x.reason === 'too far behind').toBe(true);
  expect(x2.kind === 'exit' && x2.reason === '').toBe(true);
});

test('exit_prefix_is_strict_and_pause_pane_tolerates_spacing', () => {
  const d = new ControlDecoder();
  // %exitFoo is not an exit: bare %exit or %exit + space + reason only.
  const evs = d.push(enc(`%exitFoo${NL}%pause  %0${NL}%continue  %7${NL}`));
  expect(evs.map(e => e.kind)).toEqual(['other', 'pause', 'continue']);
  expect(evs[1].kind === 'pause' && evs[1].pane === '%0').toBe(true);
  expect(evs[2].kind === 'continue' && evs[2].pane === '%7').toBe(true);
});

test('over_range_octal_escape_is_malformed_not_wrapped', () => {
  // \777 is three octal digits but not a byte: the backslash is literal and
  // counted, never a wrapped 255.
  expect(eq(decodeValue(enc('\\777')), enc('\\777'))).toBe(true);
  expect(eq(decodeValue(enc('a\\400b')), enc('a\\400b'))).toBe(true);
  const d = new ControlDecoder();
  const evs = d.push(enc('%output %0 \\777' + NL));
  expect(evs.length).toBe(1);
  expect(evs[0].kind === 'output' && eq(evs[0].data, enc('\\777'))).toBe(true);
  expect(d.stats.badEscapes).toBe(1);
});


test('oversize_line_is_discarded_and_resyncs', () => {
  const d = new ControlDecoder();
  const big = new Uint8Array(5 * 1024 * 1024).fill(0x61);
  const evs = d.push(big);                    // 5 MiB line with no newline
  expect(evs.length).toBe(0);
  expect(d.stats.oversizeLines).toBe(1);
  // resync: the rest of the line is dropped to the next \n, then the next line decodes
  const tail = d.push(enc(' tail of the big line\n%output %0 after\n'));
  expect(tail.length).toBe(1);
  expect(tail[0].kind).toBe('output');
  if (tail[0].kind === 'output') expect(eq(tail[0].data, enc('after'))).toBe(true);
  // one trailing \r is stripped from control lines
  const d2 = new ControlDecoder();
  const crlf = d2.push(enc('%pause %0\r\n'));
  expect(crlf.length).toBe(1);
  expect(crlf[0].kind).toBe('pause');
});
