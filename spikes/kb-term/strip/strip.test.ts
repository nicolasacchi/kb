// Goldens (KBT-STRIP). Every golden runs: one chunk, every 2-way split, every 3-way split (short ones),
// and byte-by-byte; all must give the identical output.
import { test, expect, afterAll } from 'bun:test';
import { QueryStripper, stripAll, csiQuery, CSI_CAP, OSC_CAP, DCS_CAP, APC_SNIFF } from './strip.ts';
import { E, enc, run, eq, hexs, wrap, metric, writeMetrics, cat } from './helpers.ts';
import { parse } from './oracle.ts';

const BEL = '\x07', ST = `${E}\\`;
type G = [name: string, input: string | Uint8Array, expected: string | Uint8Array];
const B = (...n: number[]) => Uint8Array.from(n);
const bytes = (x: string | Uint8Array) => (typeof x === 'string' ? enc(x) : x);

const kept = (name: string, s: string): G => [name, s, s];
const gone = (name: string, s: string): G => [name, s, ''];

const goldens: G[] = [
  // ---- (d) the strip list, by shape
  gone('DA1 bare', `${E}[c`), gone('DA1 0', `${E}[0c`), gone('DA1 00 (leading zeros)', `${E}[00c`),
  gone('DA2', `${E}[>c${E}[>0c`), gone('DA3', `${E}[=c${E}[=0c`),
  kept('DA with Ps=1 is not a query', `${E}[1c${E}[>1c`),
  gone('XTVERSION', `${E}[>q${E}[>0q`), kept('DECSCUSR kept', `${E}[0 q${E}[1 q${E}[5 q`), kept('CSI q w/o > kept', `${E}[2q`),
  gone('DSR 5n / CPR 6n', `${E}[5n${E}[6n`), kept('DSR 7n not a query', `${E}[7n`),
  gone('every CSI ? Ps n', `${E}[?6n${E}[?996n${E}[?15n${E}[?25n${E}[?26n${E}[?53n${E}[?62n${E}[?85n${E}[?n`),
  kept('CSI > n is not DSR', `${E}[>4n`),
  gone('DECRQM ansi + dec', `${E}[4$p${E}[?2026$p${E}[?1004$p${E}[?2031$p${E}[?2048$p${E}[?1016$p`),
  gone('XTGETTCAP', `${E}P+q4d73${ST}${E}P+q544e;436f${ST}`), gone('DECRQSS', `${E}P$qm${ST}${E}P$q q${ST}${E}P$q"p${ST}`),
  gone('DECREQTPARM', `${E}[0x${E}[1x${E}[x`), kept('CSI 2 x kept', `${E}[2x`),
  gone('ESC Z (DECID)', `${E}Z`),
  gone('OSC 4 palette ?', `${E}]4;0;?${BEL}${E}]4;1;?;2;?${ST}`), kept('OSC 4 set kept', `${E}]4;0;#000000${BEL}${E}]4;1;rgb:ff/00/00${ST}`),
  gone('OSC 5 special ?', `${E}]5;0;?${BEL}`),
  gone('OSC 10/11/12 ?', `${E}]10;?${BEL}${E}]11;?${ST}${E}]12;?${BEL}`),
  gone('OSC 13..19 ?', `${E}]13;?${BEL}${E}]14;?${BEL}${E}]15;?${BEL}${E}]16;?${BEL}${E}]17;?${BEL}${E}]18;?${BEL}${E}]19;?${BEL}`),
  gone('OSC 11 with leading zero', `${E}]011;?${BEL}`),
  kept('OSC colour sets kept', `${E}]10;rgb:ff/ff/ff${BEL}${E}]11;#000${ST}${E}]12;#f8fafc${BEL}${E}]22;pointer${BEL}`),
  gone('OSC 52 clipboard read', `${E}]52;c;?${BEL}${E}]52;;?${ST}`), kept('OSC 52 set kept', `${E}]52;c;aGVsbG8=${BEL}`),
  gone('OSC 7501;?', `${E}]7501;?${BEL}`), kept('OSC 7501 state kept', `${E}]7501;state=idle${ST}`),
  gone('OSC 99 p=?', `${E}]99;i=x:p=?;${ST}${E}]99;p=?;${BEL}`), kept('OSC 99 notification kept', `${E}]99;i=1:d=0;hello${ST}`),
  gone('OSC 1337 Capabilities/ReportCellSize/ReportVariable', `${E}]1337;Capabilities${ST}${E}]1337;ReportCellSize${BEL}${E}]1337;ReportVariable=Zm9v${BEL}`),
  kept('OSC 1337 File kept', `${E}]1337;File=inline=1:AAAA${BEL}`),
  gone('OSC 5522 read', `${E}]5522;type=read${ST}${E}]5522;type=read:id=7;${BEL}`), kept('OSC 5522 write kept', `${E}]5522;type=write;AAAA${ST}`),
  gone('OSC 21 colour ?', `${E}]21;foreground=?${ST}`),
  kept('OSC 8 with ? in the URL kept', `${E}]8;;https://x/?q=1${ST}link${E}]8;;${ST}`), kept('OSC 66 / 9;4 kept', `${E}]66;s=2;hi${BEL}${E}]9;4;1;50${BEL}`),
  gone('XTWINOPS 11,13,14,15,16,18,19,20,21', [11, 13, 14, 15, 16, 18, 19, 20, 21].map(n => `${E}[${n}t`).join('') + `${E}[14;2t${E}[13;2t`),
  kept('XTWINOPS 22/23 title push/pop kept', `${E}[22;2t${E}[23;2t${E}[8;24;80t${E}[1t`),
  gone('kitty keyboard query', `${E}[?u`), kept('kitty keyboard push/pop/set kept', `${E}[>7u${E}[<u${E}[<1u${E}[=5;1u`),
  gone('kitty graphics query', `${E}_Gi=31337,s=1,v=1,a=q,t=d,f=24;AAAA${ST}${E}_Ga=q,i=1${ST}`),
  kept('kitty a=d kept', `${E}_Ga=d,d=A,q=2${ST}`), kept('kitty transmit kept', `${E}_Ga=T,f=100,i=7;iVBOR${ST}`),
  kept('kitty key trap qa=q kept', `${E}_Gi=1,qa=q;x${ST}`), kept('kitty a=q as VALUE of another key kept', `${E}_Gi=1,x=a=q;x${ST}`),
  gone('APC 25a1 (omp glyph query)', `${E}_25a1;x${ST}`), kept('APC other kept', `${E}_hello${ST}`),
  gone('XTSMGRAPHICS', `${E}[?1;1;0S${E}[?2;1;0S`), kept('CSI S scroll-up kept', `${E}[3S`),
  gone('DECRQCRA', `${E}[1;1;1;1;10;10*y${E}[7;1*y`),
  gone('XTQMODKEYS', `${E}[?4m${E}[?0m`), kept('modifyOtherKeys set kept', `${E}[>4;2m${E}[>4m`),
  gone('DECRQTSR / DECRQPSR', `${E}[1$u${E}[2$w`),
  // ---- kept byte for byte
  kept('every mode set/reset kept', `${E}[?1000h${E}[?1002h${E}[?1003h${E}[?1006h${E}[?2004h${E}[?1004h${E}[?2026h${E}[?2026l${E}[?1049h${E}[?25l${E}[?7l${E}[?2031h${E}[?5522h${E}[?7h`),
  kept('ED2/ED3/SGR/cursor/REP kept', `${E}[H${E}[2J${E}[3J${E}[1;31mhi${E}[5;10H${E}[3b`),
  kept('sixel kept', `${E}Pq#0;2;0;0;0~${ST}`), kept('charset / ESC 7 8 = > kept', `${E}(B${E}7${E}8${E}=${E}>${E}c`),
  kept('utf8 kept', 'π > ─ 日本 ❤️ \u{10EEEE}'), kept('C0 kept', 'a\r\nb\tc\x07\x08'),
  ['mixed: text around', `a${E}[cb${E}[6nc${E}]11;?${BEL}d`, 'abcd'],
  // ---- (b) the splice bug: an aborted partial is DROPPED, a fragment never fuses with what follows a stripped query
  ['splice 1: ESC[ + DA1 + c', `${E}[${E}[cc`, 'c'],
  ['splice 2: ESC[6 + DA1 + n', `${E}[6${E}[cn`, 'n'],
  ['splice 3: ESC]11; + DA1 + ?BEL', `${E}]11;${E}[c?${BEL}`, `?${BEL}`],
  ['splice 4: ESC[?20 + DA1 + 00h', `${E}[?20${E}[c00h`, '00h'],
  ['splice 5: ESC]11;? + (aborted) DA1 + BEL', `${E}]11;${E}[c?${BEL}${E}]11;${E}[c${BEL}`, `?${BEL}${BEL}`],
  ['splice 6: DCS +q partial + DA1 + tail', `${E}P+q4d${E}[c73${ST}`, `73${ST}`],
  ['splice 7: APC partial + DA1', `${E}_Ga=${E}[cq;${ST}`, `q;${ST}`],
  ['aborted CSI (ESC) dropped', `${E}[?20${E}[1mx`, `${E}[1mx`],
  ['aborted CSI (CAN) dropped, CAN kept', `${E}[?2\x18c`, '\x18c'], ['aborted CSI (SUB) dropped', `${E}[6\x1an`, '\x1an'],
  ['aborted OSC (CAN) dropped', `${E}]11;?\x18x`, '\x18x'],
  ['aborted OSC by non-ST ESC dropped', `${E}]0;title${E}[1mx`, `${E}[1mx`],
  ['aborted ESC-intermediate dropped', `${E}(${E}[1mx`, `${E}[1mx`],
  ['C0 inside CSI executes in place', `${E}[6\nn`, '\n'], ['C0 inside OSC is ignored (query still recognised)', `${E}]11;\x01?${BEL}x`, 'x'],
  ['partial at end of stream is dropped', `ab${E}[6`, 'ab'], ['lone ESC at end dropped', `abc${E}`, 'abc'], ['partial OSC at end dropped', `x${E}]11;?`, 'x'],
  // ---- (a) DCS-tmux: unwrap (un-double ESC) BEFORE stripping; viewers never see DCS-tmux
  ['tmux: wrapped query', wrap(`${E}_Gi=31337,s=1,v=1,a=q,t=d,f=24;AAAA${ST}${E}[c`), ''],
  ['tmux: wrapped many queries', wrap(`${E}P+q4d73${ST}${E}[?2026$p`), ''],
  ['tmux: wrapped non-query is UNWRAPPED', wrap(`${E}_Ga=d,d=A,q=2${ST}`), `${E}_Ga=d,d=A,q=2${ST}`],
  ['tmux: mixed wrapper -> DA1 removed, rest unwrapped', wrap(`${E}_Ga=d${ST}${E}[c`), `${E}_Ga=d${ST}`],
  ['tmux: wrapped transmit + placeholders', wrap(`${E}_Ga=T,f=100,i=7,U=1;iVBOR${ST}`) + '\u{10EEEE}\u{10EEEE}', `${E}_Ga=T,f=100,i=7,U=1;iVBOR${ST}\u{10EEEE}\u{10EEEE}`],
  ['tmux: text around', `a${wrap(`${E}[c`)}b${wrap(`${E}[1m`)}c`, `ab${E}[1mc`],
  ['tmux: nested depth 2', wrap(wrap(`${E}[c${E}[1m`)), `${E}[1m`],
  ['tmux: nested depth 4 (the cap) unwrapped', wrap(wrap(wrap(wrap(`${E}[6nX`)))), 'X'],
  ['tmux: nested depth 5 is dropped, never forwarded', wrap(wrap(wrap(wrap(wrap(`${E}[1mY`))))), ''],
  ['tmux: truncated inner CSI dropped, no splice across the envelope end', `${wrap(`${E}[6`)}n`, 'n'],
  ['tmux: truncated inner DA1 prefix + outer c', `${wrap(`${E}[`)}c`, 'c'],
  ['tmux: inner OSC partial + outer ?BEL', `${wrap(`${E}]11;`)}?${BEL}`, `?${BEL}`],
  ['tmux: malformed (single ESC inside) ends the envelope, ESC c starts afresh', `${E}Ptmux;a${E}[1mb`, `a${E}[1mb`],
  ['tmux: unterminated at end of stream', `${E}Ptmux;${E}${E}[1mhi`, `${E}[1mhi`],
  ['tmux: prefix that is not an envelope kept', `${E}Ptmx;q${ST}${E}Pq~${ST}`, `${E}Ptmx;q${ST}${E}Pq~${ST}`],
  ['tmux: partial prefix at end dropped', `${E}Ptmu`, ''],
  // ---- (c) C1 policy: UTF-8 U+0080..U+009F and lone raw 0x80..0x9f are deleted AND abort what is in progress
  ['C1: U+009B (CSI) + 6n is not a DSR but plain text', `a\u009b6nb`, 'a6nb'],
  ['C1: U+009D OSC deleted', `a\u009d11;?${BEL}b`, `a11;?${BEL}b`],
  ['C1: U+0090 U+009F U+0085 deleted', `a\u0090q\u009fG\u0085b`, 'aqGb'],
  ['C1: every U+0080..U+009F deleted', Array.from({ length: 32 }, (_, i) => String.fromCodePoint(0x80 + i)).join('') + 'x', 'x'],
  ['C1: inside a partial CSI aborts it (no ESC[ + c splice)', `${E}[\u009bc`, 'c'],
  ['C1: inside a partial OSC aborts it', `${E}]11;\u009c?${BEL}`, `?${BEL}`],
  ['C1: U+00A0 (NBSP) and U+00A9 are NOT C1, kept', 'a b©c', 'a b©c'],
  ['C1: valid multibyte whose continuation is 0x80..0x9f kept', '‘x’ \u{10EEEE} \u{1F600} あ', '‘x’ \u{10EEEE} \u{1F600} あ'],
  ['C1: lone raw 0x9b, 0x9d, 0x90, 0x9f bytes deleted', B(0x61, 0x9b, 0x36, 0x6e, 0x9d, 0x62, 0x90, 0x9f), 'a6nb'],
  ['C1: raw 0x9b between ESC[ and c', B(0x1b, 0x5b, 0x9b, 0x63), 'c'],
  ['C1: a forwarded string aborted by C1 gets CAN', `${E}Pq~~\u009bx`, `${E}Pq~~\x18x`],
  ['C1: broken UTF-8 lead then ASCII', B(0xe2, 0x41, 0x42), B(0xe2, 0x41, 0x42)],
  // ---- strings aborted after they were forwarded (past the decision) get CAN so the receiver leaves the string
  ['forwarded DCS aborted by ESC[ + DA1 -> CAN, DA1 removed', `${E}Pq~${E}[cx`, `${E}Pq~\x18x`],
  ['forwarded APC aborted by a kept sequence', `${E}_Ga=T;AAA${E}[1mx`, `${E}_Ga=T;AAA\x18${E}[1mx`],
];

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

afterAll(() => writeMetrics('goldens'));
