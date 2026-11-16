// T25 end-to-end oracles: numbered-line completeness, remaining-query scan,
// and the ?2026 frame tracker. Pure functions over text / bytes; no Playwright.
//
// `scanForQueries` is backed by an independent whole-buffer VT parser copied
// (minus its strip-comparison helpers) from the spike branch's
// `spikes/kb-term/strip/oracle.ts` (origin/spike/kb-term-ci, head 48dd7f80,
// never merged). It shares no code with the tap's strip.ts, so it cannot
// agree with the implementation under test by construction.

// Independent minimal VT action parser. SHARES NO CODE with the tap's strip.ts.
// Whole-buffer (not streaming). It turns bytes into a list of ACTIONS the way a terminal's
// parser would (Williams-style state machine), with two deliberate policies that mirror the
// design: (1) a sequence cut short by ESC / CAN / SUB / a C1 code point yields NO action,
// (2) DCS-tmux envelopes are recursively unwrapped into their inner actions.
// Its own query-shape table (isQuery) is written from the strip-list text, not from strip.ts.

export interface Act { s: string; q?: string }     // s = canonical text of the action; q = query label if a query shape
const hex = (n: number) => n.toString(16).padStart(2, '0');
const TXT = (a: number[]) => a.map(hex).join('');

function csiQ(pre: string, ps: string, inter: string, f: string): string | undefined {
  const head = ps.split(';')[0].split(':')[0];
  const n = head === '' ? 0 : parseInt(head, 10);
  const key = pre + '|' + inter + '|' + f;
  switch (key) {
    case '||c': case '>||c': case '=||c': return n === 0 ? 'da' : undefined;
    case '>||q': return n === 0 ? 'xtversion' : undefined;
    case '||n': return n === 5 || n === 6 ? 'dsr' : undefined;
    case '?||n': return 'decdsr';
    case '?||u': return 'kbdq';
    case '||x': return n === 0 || n === 1 ? 'reqtparm' : undefined;
    case '||t': return [11, 13, 14, 15, 16, 18, 19, 20, 21].indexOf(n) >= 0 ? 'winops' : undefined;
    case '?||S': return 'xtsmgraphics';
    case '?||m': return 'xtqmodkeys';
    case '|$|p': case '?|$|p': return 'rqm';
    case '|$|u': return 'rqtsr';
    case '|$|w': return 'rqpsr';
    case '|*|y': return 'rqcra';
  }
  return undefined;
}

function oscQ(txt: string): string | undefined {
  const m = /^([0-9]+)(?:;(.*))?$/s.exec(txt);
  if (!m) return undefined;
  const id = parseInt(m[1], 10);
  const rest = m[2] === undefined ? [] : m[2].split(';');
  const hasQ = rest.some(x => x === '?');
  if ((id >= 10 && id <= 19) || id === 4 || id === 5 || id === 52) return hasQ ? 'osc?' : undefined;
  if (id === 21) return rest.some(x => x.length >= 2 && x.slice(-2) === '=?') ? 'osc21?' : undefined;
  if (id === 7501) return rest[0] === '?' ? 'osc7501' : undefined;
  if (id === 1337) { const h = rest[0] ?? ''; return h.startsWith('Capabilities') || h.startsWith('ReportCellSize') || h.startsWith('ReportVariable=') ? 'osc1337' : undefined; }
  if (id === 99) return (rest[0] ?? '').split(':').indexOf('p=?') >= 0 ? 'osc99' : undefined;
  if (id === 5522) return (rest[0] ?? '').split(':').indexOf('type=read') >= 0 ? 'osc5522' : undefined;
  return undefined;
}

const ESCB = 0x1b;
type S = 'ground' | 'esc' | 'csi' | 'osc' | 'dcsh' | 'dcsd' | 'str';

export function parse(buf: number[] | Uint8Array): Act[] {
  const b = Array.from(buf);
  const acts: Act[] = [];
  let st: S = 'ground';
  let i = 0;
  // working registers
  let escInter = '';
  let pre = '', ps = '', inter = '', csiBad = false;
  let osc: number[] = [];
  let hdrP = '', hdrI = '', hdrF = '', hdrBad = false, dcsData: number[] = [];
  let strKind = '', strData: number[] = [];
  let need = 0;
  let sawEsc = false;                 // ESC seen inside a string (OSC/DCS data/APC/SOS)
  const reset = () => { st = 'ground'; sawEsc = false; };
  const startEsc = () => { st = 'esc'; escInter = ''; sawEsc = false; };
  const exec = (c: number) => acts.push({ s: 'X' + hex(c) });
  const isStr = () => st === 'osc' || st === 'dcsd' || st === 'str';

  while (i < b.length) {
    const c = b[i];
    // C1 code point U+0080..U+009F as UTF-8: aborts anything, produces a c1 marker
    if (c === 0xc2 && i + 1 < b.length && b[i + 1] >= 0x80 && b[i + 1] <= 0x9f) {
      acts.push({ s: 'C1' + hex(b[i + 1]), q: 'c1' }); reset(); need = 0; i += 2; continue;
    }
    // raw 0x80..0x9f that is not the continuation of a preceding UTF-8 lead is a lone 8-bit C1: deleted + abort
    if (c >= 0x80) {
      if (need > 0 && c <= 0xbf) { need--; }
      else if (c <= 0x9f) { acts.push({ s: 'C1raw' + hex(c), q: 'c1' }); reset(); need = 0; i++; continue; }
      else { need = c >= 0xc3 && c <= 0xdf ? 1 : c >= 0xe0 && c <= 0xef ? 2 : c >= 0xf0 && c <= 0xf4 ? 3 : c === 0xc2 ? 1 : 0; }
    } else need = 0;
    // inside a string, an earlier ESC decides: ST ends it, anything else aborts it
    if (isStr() && sawEsc) {
      if (c === 0x5c) {
        if (st === 'osc') { const t = String.fromCharCode(...osc.filter(x => x >= 0x20)); acts.push({ s: 'O' + TXT(osc.filter(x => x >= 0x20)), q: oscQ(t) }); }
        else if (st === 'dcsd') acts.push({ s: `D${hdrP}|${hdrI}|${hdrF}|${TXT(dcsData)}`, q: (hdrI + hdrF === '+q' || hdrI + hdrF === '$q') ? 'dcsq' : undefined });
        else acts.push({ s: `${strKind}${TXT(strData)}`, q: strKind === 'A' && (aq(strData) || TXT(strData).startsWith('32356131')) ? 'apcq' : undefined });
        reset(); i++; continue;
      }
      startEsc();            // aborted string; this ESC starts a new sequence; re-read c in that state
      continue;
    }
    // "anywhere" transitions
    if (c === ESCB) {
      if (isStr()) { sawEsc = true; i++; continue; }
      startEsc(); i++; continue;
    }
    if (c === 0x18 || c === 0x1a) { exec(c); reset(); i++; continue; }

    switch (st) {
      case 'ground':
        if (c < 0x20) exec(c);
        else if (c === 0x7f) { /* ignored */ }
        else acts.push({ s: 'P' + hex(c) });
        i++; break;
      case 'esc':
        if (c < 0x20) { exec(c); i++; break; }
        if (c === 0x7f) { i++; break; }
        if (c <= 0x2f) { escInter += String.fromCharCode(c); i++; break; }
        if (escInter === '') {
          if (c === 0x5b) { st = 'csi'; pre = ''; ps = ''; inter = ''; csiBad = false; i++; break; }
          if (c === 0x5d) { st = 'osc'; osc = []; i++; break; }
          if (c === 0x50) {
            // DCS. tmux passthrough is recognised by lookahead on the raw bytes.
            const want = [0x74, 0x6d, 0x75, 0x78, 0x3b];
            if (want.every((w, k) => b[i + 1 + k] === w)) {
              let j = i + 6; const inner: number[] = [];
              let after = b.length;
              for (; j < b.length; j++) {
                if (b[j] !== ESCB) { inner.push(b[j]); continue; }
                if (b[j + 1] === ESCB) { inner.push(ESCB); j++; continue; }
                if (b[j + 1] === 0x5c) { after = j + 2; break; }
                after = j; break;                          // malformed: ESC x is re-read as a fresh sequence
              }
              for (const a of parse(inner)) acts.push(a);
              if (j >= b.length) after = b.length;
              i = after; reset(); break;
            }
            st = 'dcsh'; hdrP = ''; hdrI = ''; hdrF = ''; hdrBad = false; dcsData = []; i++; break;
          }
          if (c === 0x5f) { st = 'str'; strKind = 'A'; strData = []; i++; break; }
          if (c === 0x58) { st = 'str'; strKind = 'S'; strData = []; i++; break; }
          if (c === 0x5e) { st = 'str'; strKind = 'V'; strData = []; i++; break; }
          if (c === 0x5a) { acts.push({ s: 'EZ', q: 'decid' }); reset(); i++; break; }
        }
        acts.push({ s: `E${escInter}|${hex(c)}` }); reset(); i++; break;
      case 'csi':
        if (c < 0x20) { exec(c); i++; break; }
        if (c === 0x7f) { i++; break; }
        if (c >= 0x40 && c <= 0x7e) {
          if (csiBad) acts.push({ s: `Ci|${hex(c)}` });
          else acts.push({ s: `C${pre}|${ps}|${inter}|${String.fromCharCode(c)}`, q: csiQ(pre, ps, inter, String.fromCharCode(c)) });
          reset(); i++; break;
        }
        if (c >= 0x20 && c <= 0x2f) { inter += String.fromCharCode(c); i++; break; }
        // 0x30..0x3f parameter area
        if (inter !== '' || (c >= 0x3c && (ps !== '' || pre !== ''))) csiBad = true;
        else if (c >= 0x3c) pre = String.fromCharCode(c);
        else ps += String.fromCharCode(c);
        i++; break;
      case 'osc':
        if (c === 0x07) { const t = String.fromCharCode(...osc.filter(x => x >= 0x20)); acts.push({ s: 'O' + TXT(osc.filter(x => x >= 0x20)), q: oscQ(t) }); reset(); i++; break; }
        osc.push(c); i++; break;
      case 'dcsh':
        if (c < 0x20 || c === 0x7f) { i++; break; }
        if (c <= 0x2f) { hdrI += String.fromCharCode(c); i++; break; }
        if (c <= 0x3f) { if (hdrI !== '') hdrBad = true; else hdrP += String.fromCharCode(c); i++; break; }
        if (c <= 0x7e && !hdrBad) { hdrF = String.fromCharCode(c); st = 'dcsd'; i++; break; }
        hdrBad = true; i++; break;      // ignore state: swallow until ST / abort, no action
      case 'dcsd':
        dcsData.push(c); i++; break;
      case 'str':
        strData.push(c); i++; break;
    }
    if (st === 'dcsh' && hdrBad) { /* swallowed: finished by ST (below) or abort */ }
  }
  return acts;
}

function aq(d: number[]): boolean {
  if (d[0] !== 0x47) return false;
  const semi = d.indexOf(0x3b);
  const ctl = String.fromCharCode(...(semi < 0 ? d.slice(1) : d.slice(1, semi)));
  return ctl.split(',').indexOf('a=q') >= 0;
}

/** the 25a1 APC query of omp (from source) is matched by its prefix */
export function isQueryAct(a: Act): boolean { return a.q !== undefined && a.q !== 'c1'; }

// ---------------------------------------------------------------------------
// scanForQueries: every query shape still present in a byte stream.

/** Whole-buffer scan: the query actions (DA, DSR, XTVERSION, DECRQM, OSC ?, ...) left in `bytes`. */
export function scanForQueries(bytes: Uint8Array | number[]): Act[] {
  return parse(bytes).filter(isQueryAct);
}

// ---------------------------------------------------------------------------
// checkNumbered: L000001..L<total> exactly once, in order, nothing missing.

export interface NumberedReport {
  highest: number;
  total: number;
  duplicates: number[];
  missing: number[];
  inOrder: boolean;
  ok: boolean;
}

const CSI_RE = /\x1b\[[0-9;:?<=>]*[ -/]*[@-~]/g;
const LINE_RE = /^L(\d{6})\b/gm;

/**
 * Check a screen/scrollback text dump. Lines match `^L\d{6}\b` after CSI is
 * stripped; any other line (markers, DONE, prompts) is ignored. `ok` is false
 * unless EVERY number 1..expectTotal appears exactly once, in increasing
 * order, and `highest === expectTotal`. The last clause is the point: the
 * 2026-11-07 zellij dump ended at L002714 of 3300 and a checker that only
 * tested 1..highest called it complete.
 */
export function checkNumbered(text: string, opts: { expectTotal: number }): NumberedReport {
  const plain = text.replace(CSI_RE, "");
  const seen = new Map<number, number>();
  const order: number[] = [];
  for (const m of plain.matchAll(LINE_RE)) {
    const n = parseInt(m[1], 10);
    seen.set(n, (seen.get(n) ?? 0) + 1);
    order.push(n);
  }
  const highest = order.reduce((a, b) => (b > a ? b : a), 0);
  const duplicates = [...seen.entries()].filter(([, c]) => c > 1).map(([n]) => n).sort((a, b) => a - b);
  const missing: number[] = [];
  for (let n = 1; n <= opts.expectTotal; n += 1) if (!seen.has(n)) missing.push(n);
  let inOrder = true;
  for (let i = 1; i < order.length; i += 1) {
    if (order[i] <= order[i - 1]) {
      inOrder = false;
      break;
    }
  }
  const ok =
    highest === opts.expectTotal && duplicates.length === 0 && missing.length === 0 && inOrder;
  return { highest, total: opts.expectTotal, duplicates, missing, inOrder, ok };
}

/** A short human line for an assertion message. */
export function describeNumbered(r: NumberedReport): string {
  const head = (xs: number[]): string => xs.slice(0, 8).join(",") + (xs.length > 8 ? `,...(${xs.length})` : "");
  return `highest=${r.highest}/${r.total} duplicates=[${head(r.duplicates)}] missing=[${head(r.missing)}] inOrder=${r.inOrder}`;
}

// ---------------------------------------------------------------------------
// frameTracker: ?2026 synchronized-update depth over a byte stream.

export interface FrameReport {
  /** True iff the first ?2026 marker seen is a reset (l): the bytes began inside a frame. */
  startsInsideFrame: boolean;
  /** Number of ?2026h markers. */
  opens: number;
  /** Number of ?2026l markers. */
  closes: number;
  /** Depth after the last byte (1 = the stream ends inside a frame). */
  endDepth: number;
}

/** Scan `bytes` for `ESC [ ? 2026 h` / `l` (also in a `?...;2026;...` list form is not used by omp). */
export function frameTracker(bytes: Uint8Array | number[]): FrameReport {
  const b = Array.from(bytes);
  const pat = [0x1b, 0x5b, 0x3f, 0x32, 0x30, 0x32, 0x36]; // ESC [ ? 2 0 2 6
  let first: "h" | "l" | null = null;
  let opens = 0;
  let closes = 0;
  let depth = 0;
  for (let i = 0; i + pat.length < b.length; i += 1) {
    let hit = true;
    for (let k = 0; k < pat.length; k += 1) {
      if (b[i + k] !== pat[k]) {
        hit = false;
        break;
      }
    }
    if (!hit) continue;
    const f = b[i + pat.length];
    if (f === 0x68) {
      opens += 1;
      depth = 1;
      if (first === null) first = "h";
    } else if (f === 0x6c) {
      closes += 1;
      depth = 0;
      if (first === null) first = "l";
    }
  }
  return { startsInsideFrame: first === "l", opens, closes, endDepth: depth };
}
