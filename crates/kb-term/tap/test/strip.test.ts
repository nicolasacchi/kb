// Goldens (KBT-STRIP). Every golden runs: one chunk, every 2-way split, every 3-way split (short ones),
// and byte-by-byte; all must give the identical output.
import { test, expect, afterAll } from 'bun:test';
import { QueryStripper, stripAll, csiQuery, CSI_CAP, OSC_CAP, DCS_CAP, APC_SNIFF } from '../src/strip.ts';
import { E, enc, run, eq, hexs, wrap, metric, writeMetrics, cat } from './helpers.ts';
import { parse } from './oracle.ts';
import { goldens } from './goldens.ts';

const BEL = '\x07', ST = `${E}\\`;
const B = (...n: number[]) => Uint8Array.from(n);
const bytes = (x: string | Uint8Array) => (typeof x === 'string' ? enc(x) : x);


function splits(data: Uint8Array): number[][] {
  const r: number[][] = [];
  for (let i = 1; i < data.length; i++) r.push([i]);
  r.push(Array.from({ length: Math.max(0, data.length - 1) }, (_, i) => i + 1));
  if (data.length <= 36) for (let i = 1; i < data.length; i++) for (let j = i + 1; j < data.length; j++) r.push([i, j]);
  return r;
}

let goldenCases = 0;
test(`goldens: ${goldens.length} cases x whole/2-split/3-split/bytewise all equal the expectation`, () => {
  const fails: string[] = [];
  for (const [name, input, want] of goldens) {
    const data = bytes(input), w = bytes(want);
    const whole = run(data).out;
    if (!eq(whole, w)) fails.push(`${name}: got ${hexs(whole)} want ${hexs(w)}`);
    for (const cuts of splits(data)) {
      goldenCases++;
      const g = run(data, cuts).out;
      if (!eq(g, w)) { fails.push(`${name} cuts=${cuts.length > 3 ? 'bytewise' : cuts}: got ${hexs(g)} want ${hexs(w)}`); break; }
    }
  }
  metric('goldens', goldens.length); metric('golden_split_cases', goldenCases);
  expect(fails).toEqual([]);
});

test('splice repros from the review, verbatim (the OLD stripper turned these into live queries)', () => {
  const dec = (s: string) => hexs(stripAll(enc(s)));
  expect(dec(`${E}[${E}[cc`)).toBe('c');             // was ESC[c
  expect(dec(`${E}[6${E}[cn`)).toBe('n');            // was ESC[6n
  expect(dec(`${E}]11;${E}[c?${BEL}`)).toBe('?\\x07');   // was ESC]11;?BEL
  expect(dec(`${E}[?20${E}[c00h`)).toBe('00h');      // was ESC[?2000h
});

test('no query shape in the output of any golden (oracle view)', () => {
  const left: string[] = [];
  for (const [name, input] of goldens) for (const a of parse(stripAll(bytes(input)))) if (a.q) left.push(`${name}: ${a.s} ${a.q}`);
  expect(left).toEqual([]);
});

test('every strip-list shape is hit: stripped counters by label', () => {
  const s = new QueryStripper();
  for (const [, input] of goldens) s.push(bytes(input));
  s.flush();
  metric('strip_labels', s.kinds); metric('strip_total_over_goldens', s.stripped);
  for (const k of ['DA1', 'DA2', 'DA3', 'XTVERSION', 'DSR', 'DECDSR', 'DECRQM', 'XTGETTCAP', 'DECRQSS', 'DECREQTPARM', 'DECID', 'OSC4?', 'OSC5?', 'OSC10?', 'OSC11?', 'OSC12?', 'OSC52?', 'OSC7501?', 'OSC99?', 'OSC1337?', 'OSC5522-read', 'XTWINOPS', 'KITTYKBD', 'KITTYGFX-query', 'XTSMGRAPHICS', 'DECRQCRA', 'XTQMODKEYS', 'DECRQTSR', 'DECRQPSR', 'APC-25a1', 'OSC21?', 'DCS-tmux-nested'])
    expect(s.kinds[k] ?? 0).toBeGreaterThan(0);
});

// ---------------------------------------------------------------- caps + no timer
test('hold-back caps: exact boundaries (what is TRUE)', () => {
  const r: Record<string, unknown> = {};
  // CSI: non-final bytes held up to CSI_CAP (counting ESC and '['); a DA1 padded with zeros
  const da1 = (k: number) => `${E}[${'0'.repeat(k)}c`;
  let maxStripped = 0;
  for (let k = 0; k < 200; k++) if (stripAll(enc(da1(k))).length === 0) maxStripped = k;
  r.csi_padded_da1_max_zeros_still_stripped = maxStripped;
  r.csi_padded_da1_total_bytes_still_stripped = maxStripped + 3;
  expect(stripAll(enc(da1(maxStripped))).length).toBe(0);
  expect(hexs(stripAll(enc(da1(maxStripped + 1))))).toBe(hexs(enc(da1(maxStripped + 1))));   // past the cap: verbatim
  expect(maxStripped + 2).toBe(CSI_CAP);                                                        // ESC [ + zeros == the cap
  // a long, legitimate CSI passes verbatim and does not break the following query
  const longSgr = `${E}[${'1;'.repeat(60)}m`;
  expect(hexs(stripAll(enc(`${longSgr}${E}[c`)))).toBe(hexs(enc(longSgr)));
  // OSC: a padded colour query
  const oscq = (n: number) => `${E}]4;${'0;?;'.repeat(n)}0;?${BEL}`;
  let oscMax = 0;
  for (let n = 0; n < 200; n++) if (stripAll(enc(oscq(n))).length === 0) oscMax = n;
  r.osc4_query_max_bytes_still_stripped = enc(oscq(oscMax)).length;
  expect(enc(oscq(oscMax)).length).toBeLessThanOrEqual(OSC_CAP + 1);
  expect(stripAll(enc(oscq(oscMax + 1))).length).toBe(enc(oscq(oscMax + 1)).length);
  // a huge OSC 52 clipboard SET and OSC 1337 File pass untouched
  const big = `${E}]52;c;${'A'.repeat(100000)}${BEL}${E}]1337;File=inline=1:${'B'.repeat(50000)}${ST}`;
  expect(eq(stripAll(enc(big)), enc(big))).toBe(true);
  // ... and the query right after still works
  expect(eq(stripAll(enc(big + `${E}[6n`)), enc(big))).toBe(true);
  // DCS: sixel of 1 MB passes; a DCS header with no final past DCS_CAP passes
  const sixel = `${E}Pq${'~'.repeat(1_000_000)}${ST}`;
  expect(eq(stripAll(enc(sixel)), enc(sixel))).toBe(true);
  const longHdr = `${E}P${'1;'.repeat(DCS_CAP)}q~${ST}`;
  expect(eq(stripAll(enc(longHdr)), enc(longHdr))).toBe(true);
  // APC: kitty transmit with a large payload passes; control data longer than the sniff window passes (even a=q)
  const tx = `${E}_Ga=T,f=100,i=1;${'A'.repeat(300000)}${ST}`;
  expect(eq(stripAll(enc(tx)), enc(tx))).toBe(true);
  const ctl = (n: number) => `${E}_Ga=q,${'x=1,'.repeat(n)}i=1;AAAA${ST}`;
  let apcMax = 0;
  for (let n = 0; n < 300; n++) if (stripAll(enc(ctl(n))).length === 0) apcMax = n;
  r.apc_query_max_control_bytes_still_stripped = enc(ctl(apcMax)).length - 4 - 5 - 2 - 1; // G..; minus framing
  expect(stripAll(enc(ctl(apcMax + 1))).length).toBe(enc(ctl(apcMax + 1)).length);
  expect(apcMax).toBeGreaterThan(50);
  expect(APC_SNIFF).toBe(512);
  metric('caps', r);
});

test('no timer flush: undecided bytes are held indefinitely, dropped (never emitted) at end of stream', async () => {
  const s = new QueryStripper();
  expect(s.push(enc(`ab${E}[6`)).length).toBe(2);
  expect(s.pending()).toBe(3);
  await new Promise(r => setTimeout(r, 150));          // a flush timer would have fired by now
  expect(s.pending()).toBe(3);
  expect(s.push(enc('')).length).toBe(0);
  expect(s.push(enc('n')).length).toBe(0);              // completes DSR -> stripped
  expect(s.pending()).toBe(0);
  expect(s.push(enc(`${E}]11;`)).length).toBe(0);
  expect(s.pending()).toBeGreaterThan(0);
  expect(s.flush().length).toBe(0);                     // end of stream: dropped
  expect(s.pending()).toBe(0);
  const t = new QueryStripper();
  expect(t.push(enc(`${E}`)).length).toBe(0); expect(t.pending()).toBe(1);
  const u = new QueryStripper();
  expect(u.push(B(0x61, 0xc2)).length).toBe(1); expect(u.pending()).toBe(1);   // lone C2 waits for its pair
  expect(u.push(B(0xa9)).length).toBe(2);
});

test('csiQuery edge shapes', () => {
  const q = (s: string) => csiQuery(Array.from(enc(s)));
  expect(q(`${E}[?2026$p`)).toBe('DECRQM');
  expect(q(`${E}[6;1n`)).toBe('DSR');            // xterm answers CPR on p0 alone, so extra params do not help an attacker
  expect(q(`${E}[1;2c`)).toBeNull();
  expect(q(`${E}[?1 6n`)).toBeNull();              // intermediate then param = ignored by terminals
  expect(q(`${E}[6n`)).toBe('DSR');
});

// ---------------------------------------------------------------- the final strip list, by shape
test('final_strip_list_is_pinned_by_shape', () => {
  const KEPT = `${E}[?2026h`;
  // one block per shape: each query form strips to empty bare, DCS-tmux x1, x4 (nested
  // with a kept sequence), and mixed with a kept sequence (only the kept survives).
  const shapes: [name: string, query: string, label: string][] = [
    ['DA1 c', `${E}[c`, 'DA1'], ['DA1 0c', `${E}[0c`, 'DA1'],
    ['DA2 >c', `${E}[>c`, 'DA2'], ['DA2 >0c', `${E}[>0c`, 'DA2'],
    ['DA3 =c', `${E}[=c`, 'DA3'],
    ['XTVERSION >q', `${E}[>q`, 'XTVERSION'], ['XTVERSION >0q', `${E}[>0q`, 'XTVERSION'],
    ['DSR 5n', `${E}[5n`, 'DSR'], ['DSR 6n', `${E}[6n`, 'DSR'],
    ['DECDSR ?6n', `${E}[?6n`, 'DECDSR'], ['DECDSR ?996n', `${E}[?996n`, 'DECDSR'], ['DECDSR ?25n', `${E}[?25n`, 'DECDSR'],
    ['DECRQM 4$p', `${E}[4$p`, 'DECRQM'], ['DECRQM ?2026$p', `${E}[?2026$p`, 'DECRQM'],
    ['KITTYKBD ?u', `${E}[?u`, 'KITTYKBD'],
    ['DECREQTPARM 0x', `${E}[0x`, 'DECREQTPARM'], ['DECREQTPARM 1x', `${E}[1x`, 'DECREQTPARM'],
    ['DECID ESC Z', `${E}Z`, 'DECID'],
    ...[11, 13, 14, 15, 16, 18, 19, 20, 21].map(n => [`XTWINOPS ${n}t`, `${E}[${n}t`, 'XTWINOPS'] as const),
    ['XTSMGRAPHICS ? S', `${E}[?1;1;0S`, 'XTSMGRAPHICS'],
    ['DECRQCRA *y', `${E}[1;1;1;1;10;10*y`, 'DECRQCRA'],
    ['XTQMODKEYS ? m', `${E}[?4m`, 'XTQMODKEYS'],
    ['DECRQTSR $u', `${E}[1$u`, 'DECRQTSR'],
    ['DECRQPSR $w', `${E}[2$w`, 'DECRQPSR'],
    ...[4, 5, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19].map(n => [`OSC ${n} ?`, `${E}]${n};?${BEL}`, `OSC${n}?`] as const),
    ['OSC21 =?', `${E}]21;foreground=?${ST}`, 'OSC21?'],
    ['OSC52 ?', `${E}]52;c;?${BEL}`, 'OSC52?'],
    ['OSC7501 ;?', `${E}]7501;?${BEL}`, 'OSC7501?'],
    ['OSC1337 Capabilities', `${E}]1337;Capabilities${ST}`, 'OSC1337?'],
    ['OSC1337 ReportCellSize', `${E}]1337;ReportCellSize${BEL}`, 'OSC1337?'],
    ['OSC1337 ReportVariable=', `${E}]1337;ReportVariable=Zm9v${BEL}`, 'OSC1337?'],
    ['OSC99 p=?', `${E}]99;i=x:p=?;${ST}`, 'OSC99?'],
    ['OSC5522 type=read', `${E}]5522;type=read${ST}`, 'OSC5522-read'],
    ['XTGETTCAP DCS +q', `${E}P+q4d73${ST}`, 'XTGETTCAP'],
    ['DECRQSS DCS $q', `${E}P$qm${ST}`, 'DECRQSS'],
    ['KITTYGFX-query APC a=q', `${E}_Gi=31337,s=1,v=1,a=q,t=d,f=24;AAAA${ST}`, 'KITTYGFX-query'],
    ['APC-25a1', `${E}_25a1;x${ST}`, 'APC-25a1'],
  ];
  const fails: string[] = [];
  for (const [name, q, label] of shapes) {
    const bare = run(enc(q));
    if (stripAll(enc(q)).length !== 0) fails.push(`${name}: not stripped bare: ${hexs(bare.out)}`);
    if ((bare.s.kinds[label] ?? 0) === 0) fails.push(`${name}: label ${label} not recorded (kinds=${JSON.stringify(bare.s.kinds)})`);
    if (stripAll(enc(wrap(q))).length !== 0) fails.push(`${name}: not stripped under one tmux envelope`);
    const w4 = wrap(wrap(wrap(wrap(q))));
    if (stripAll(enc(w4)).length !== 0) fails.push(`${name}: not stripped under four tmux envelopes`);
    const mixed = run(enc(KEPT + q));
    if (!eq(mixed.out, enc(KEPT))) fails.push(`${name}: mixed with kept gave ${hexs(mixed.out)}`);
  }
  // nested DCS-tmux deeper than 4 is dropped whole (fail closed), never forwarded
  const d5 = wrap(wrap(wrap(wrap(wrap(`${E}[1mY`)))));
  if (stripAll(enc(d5)).length !== 0) fails.push('DCS-tmux depth 5 not dropped entirely');
  // XTWINOPS 22/23 push/pop are KEPT, not queries
  for (const q of [`${E}[22;2t`, `${E}[23;2t`]) if (!eq(stripAll(enc(q)), enc(q))) fails.push(`${hexs(enc(q))}: XTWINOPS 22/23 not kept`);
  expect(fails).toEqual([]);
});

test('kept_sequences_pass_byte_for_byte', () => {
  const keptList = [
    `${E}[?1000h${E}[?1002h${E}[?1003h${E}[?1006h`,
    `${E}[?2026h${E}[?2026l${E}[?7h${E}[?7l${E}[?25h${E}[?25l${E}[?2004h${E}[?1004h${E}[?2031h${E}[?5522h${E}[?1049h`,
    `${E}[H${E}[2J${E}[3J`,
    `${E}[>4;2m${E}[>4m`,
    `${E}[2 q${E}[5 q`,
    `${E}[3b`,
    `${E}]66;s=2;hi${BEL}`,
    `${E}]9;4;1;50${BEL}`,
    `${E}]7501;state=idle${ST}`,
    `${E}]8;;https://x/?q=1${ST}link${E}]8;;${ST}`,
    `${E}]12;#f8fafc${BEL}${E}]22;pointer${BEL}`,
    `${E}]52;c;aGVsbG8=${BEL}`,
    `${E}]1337;File=inline=1:AAAA${BEL}`,
    `${E}Pq#0;2;0;0;0~${ST}`,
    `${E}_Ga=T,f=100,i=7;iVBOR${ST}`,
    `${E}_Ga=d,d=A,q=2${ST}`,
    `${E}_Gi=1,qa=q;x${ST}`,
    `${E}_Gi=1,x=a=q;x${ST}`,
    `${E}[>7u${E}[<u${E}[<1u${E}[=5;1u`,
  ];
  for (const s of keptList) {
    expect(eq(stripAll(enc(s)), enc(s))).toBe(true);
    expect(eq(run(enc(s)).out, enc(s))).toBe(true);
  }
});

test('rebuild_frame_is_forwarded_whole', () => {
  const reprint = 'line one\r\nline two with π and ─\r\n';
  const queries = `${E}[c${E}[6n${E}]11;?${BEL}`;
  const frame = `${E}[?2026h${E}[H${E}[2J${E}[3J${reprint}${queries}${E}[?2026l`;
  const want = `${E}[?2026h${E}[H${E}[2J${E}[3J${reprint}${E}[?2026l`;
  expect(eq(stripAll(enc(frame)), enc(want))).toBe(true);
  expect(eq(run(enc(frame), Array.from({ length: frame.length - 1 }, (_, i) => i + 1)).out, enc(want))).toBe(true);
  const w1 = wrap(frame);
  expect(eq(stripAll(enc(w1)), enc(want))).toBe(true);
  const w4 = wrap(wrap(wrap(wrap(frame))));
  expect(eq(stripAll(enc(w4)), enc(want))).toBe(true);
});

test('nested_dcs_tmux_depth_5_is_dropped_entirely', () => {
  const d4kept = wrap(wrap(wrap(wrap(`${E}[1m`))));
  expect(eq(stripAll(enc(d4kept)), enc(`${E}[1m`))).toBe(true);
  expect(stripAll(enc(wrap(wrap(wrap(wrap(`${E}[6n`)))))).length).toBe(0);
  const d5 = wrap(wrap(wrap(wrap(wrap(`${E}[1mY`))))) + 'tail';
  const out = stripAll(enc(d5));
  expect(eq(out, enc('tail'))).toBe(true);
});

test('c1_is_deleted_and_aborts_in_progress', () => {
  const all = Array.from({ length: 32 }, (_, i) => String.fromCodePoint(0x80 + i)).join('') + 'x';
  expect(eq(stripAll(enc(all)), enc('x'))).toBe(true);
  expect(eq(stripAll(B(0x61, 0x9b, 0x98, 0x90, 0x62)), enc('ab'))).toBe(true);
  expect(eq(stripAll(enc(`${E}[\u009bc`)), enc('c'))).toBe(true);
  const ok = 'a b ‘x’ \u{10EEEE} \u{1F600}';
  expect(eq(stripAll(enc(ok)), enc(ok))).toBe(true);
  const s = new QueryStripper();
  expect(s.push(B(0x61, 0xc2)).length).toBe(1);
  expect(s.pending()).toBe(1);
  expect(s.push(B(0xa9)).length).toBe(2);
  expect(s.pending()).toBe(0);
});

afterAll(() => writeMetrics('goldens'));
