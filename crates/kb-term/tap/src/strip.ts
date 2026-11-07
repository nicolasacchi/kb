// kb-term tap: query stripper (KBT-STRIP). Zero dependencies.
//
// Pipeline (stage order is part of the design, see pipeline.ts):
//   decode (caller) -> TmuxUnwrap x4 -> C1Filter -> Stripper
// Stages exchange Int16Array views where 0..255 are bytes and BOUNDARY (-1) means "any sequence
// in progress is aborted here" (an envelope edge, or a deleted C1 control). BOUNDARY is how a
// deletion can never splice the bytes before it onto the bytes after it. Stage outputs are views
// into per-stage reused buffers, valid until that stage's next call; QueryStripper.push converts
// the final stage to a fresh Uint8Array (one copy per chunk; the consumer owns it).
//
// Rules (round-3 findings):
//  * DCS-tmux envelopes are unwrapped (ESC un-doubled) BEFORE stripping; viewers never see them.
//    Deeper than TMUX_DEPTH (4) nested envelopes are dropped entirely (fail closed).
//  * An aborted or partial sequence is DROPPED, never emitted (no splice: `ESC[ ESC[c c` -> `c`).
//  * No timer flush. End of stream drops whatever is undecided. Hold-back caps: CSI 64 B,
//    OSC 256 B, DCS 4096 B, APC G sniff 512 B; past a cap the sequence passes verbatim, so the
//    responder is the backstop against a padded query.
//  * A string that was already forwarded (past a cap or not a query) and is then aborted gets a
//    CAN (0x18) appended so the receiving terminal also leaves the string state.
//  * C1: UTF-8 U+0080..U+009F and lone raw 0x80..0x9f bytes are deleted and abort what is in
//    progress.
//  * Queries are matched by SHAPE, never by harness name.
//
// Performance: QueryStripper.push has a byte fast path. When every stage is idle (all unwrap
// stages in ground state, C1 waiting on nothing, Stripper in ground), plain C0/text bytes
// (0x00..0x1a, 0x1c..0x7f) pass through the whole pipeline unchanged, so the fast path copies
// them straight to the output and only runs the state machines from the first byte that could
// start or continue a sequence (0x1b ESC, or >= 0x80). Everything below 0x80 except ESC is
// inert by construction: the C1 filter passes it, the unwrap passes it, and the stripper's
// ground state emits it. Bytes >= 0x80 are never fast-pathed (C1/UTF-8 policy lives there).

export const BOUNDARY = -1;
const ESC = 0x1b, BEL = 0x07, CAN = 0x18, SUB = 0x1a, BS = 0x5c;
export const CSI_CAP = 64;
export const OSC_CAP = 256;
export const DCS_CAP = 4096;
export const APC_SNIFF = 512;
export const TMUX_DEPTH = 4;

/** A stage-to-stage exchange: a view into a reusable buffer holding the first n values. */
export interface Ch { arr: Int16Array; n: number }

const EMPTY_CH: Ch = { arr: new Int16Array(0), n: 0 };

/** true if any of the first n values is >= 0x80 (the C1 filter's concern). */
function hasHighByte(a: Int16Array, n: number): boolean {
  for (let i = 0; i < n; i++) if (a[i] >= 0x80) return true;
  return false;
}

/** A growable Int16Array buffer; Ch views into it are valid until the owner reuses it. */
export class HBuf {
  arr: Int16Array = new Int16Array(64);
  n = 0;
  reset(): void { this.n = 0; }
  view(): Ch { return { arr: this.arr, n: this.n }; }
  push(v: number): void {
    if (this.n === this.arr.length) {
      const b = new Int16Array(Math.max(64, this.arr.length * 2));
      b.set(this.arr); this.arr = b;
    }
    this.arr[this.n++] = v;
  }
  pushCh(c: Ch): void {
    if (this.n + c.n > this.arr.length) {
      const b = new Int16Array(Math.max(64, (this.arr.length + c.n) * 2));
      b.set(this.arr.subarray(0, this.n)); this.arr = b;
    }
    this.arr.set(c.arr.subarray(0, c.n), this.n); this.n += c.n;
  }
  pushArr(a: Int16Array, n: number): void {
    this.pushCh({ arr: a, n });
  }
}

const latin1 = (b: Int16Array, n: number): string => { let s = ''; for (let i = 0; i < n; i += 8192) s += String.fromCharCode(...b.subarray(i, Math.min(n, i + 8192))); return s; };

// ---------------------------------------------------------------- stage 1: DCS-tmux unwrap
const TMUX_PFX = [ESC, 0x50, 0x74, 0x6d, 0x75, 0x78, 0x3b]; // ESC P t m u x ;
export class TmuxUnwrap {
  private st: 'G' | 'M' | 'W' | 'WE' = 'G';
  private m = 0;
  private out = new HBuf();
  held(): number { return this.st === 'M' ? this.m : 0; }
  /** ground state with no held envelope prefix. */
  isIdle(): boolean { return this.st === 'G'; }
  process(inp: Ch): Ch {
    this.out.reset();
    for (let i = 0; i < inp.n; i++) this.step(inp.arr[i]);
    return this.out.view();
  }
  flush(): Ch {
    this.out.reset();
    if (this.st === 'W' || this.st === 'WE') this.out.push(BOUNDARY);
    this.st = 'G'; this.m = 0;
    return this.out.view();
  }
  private step(c: number): void {
    if (c === BOUNDARY) {            // upstream abort: drop the held prefix, truncate the envelope
      this.st = 'G'; this.m = 0; this.out.push(BOUNDARY); return;
    }
    switch (this.st) {
      case 'G':
        if (c === ESC) { this.st = 'M'; this.m = 1; } else this.out.push(c);
        return;
      case 'M':
        if (c === TMUX_PFX[this.m]) {
          if (++this.m === TMUX_PFX.length) { this.st = 'W'; this.out.push(BOUNDARY); }
          return;
        }
        for (let i = 0; i < this.m; i++) this.out.push(TMUX_PFX[i]);   // not an envelope after all
        this.st = 'G'; this.m = 0;
        this.step(c);
        return;
      case 'W':
        if (c === ESC) this.st = 'WE'; else this.out.push(c);
        return;
      case 'WE':
        if (c === ESC) { this.out.push(ESC); this.st = 'W'; return; }
        if (c === BS) { this.out.push(BOUNDARY); this.st = 'G'; return; }
        // malformed (single ESC inside an envelope): the envelope is over, ESC c starts afresh
        this.out.push(BOUNDARY); this.st = 'G';
        this.step(ESC); this.step(c);
        return;
    }
  }
}

// ---------------------------------------------------------------- stage 2: C1 policy
export class C1Filter {
  private need = 0;          // continuation bytes still expected
  private heldC2 = false;
  private out = new HBuf();
  held(): number { return this.heldC2 ? 1 : 0; }
  /** no UTF-8 continuation expected and no held C2 lead byte. */
  isIdle(): boolean { return this.need === 0 && !this.heldC2; }
  process(inp: Ch): Ch { this.out.reset(); for (let i = 0; i < inp.n; i++) this.step(inp.arr[i]); return this.out.view(); }
  flush(): Ch { this.out.reset(); this.heldC2 = false; this.need = 0; return this.out.view(); }
  private step(c: number): void {
    if (c === BOUNDARY) { this.heldC2 = false; this.need = 0; this.out.push(BOUNDARY); return; }
    if (this.heldC2) {
      this.heldC2 = false;
      if (c >= 0x80 && c <= 0x9f) { this.out.push(BOUNDARY); return; }  // U+0080..U+009F deleted
      this.out.push(0xc2); this.need = 1;                               // U+00A0..U+00BF: fall into the continuation rule
    }
    if (this.need > 0) {
      if (c >= 0x80 && c <= 0xbf) { this.out.push(c); this.need--; return; }
      this.need = 0;                                                    // broken sequence, c starts afresh
    }
    if (c < 0x80) { this.out.push(c); return; }
    if (c <= 0x9f) { this.out.push(BOUNDARY); return; }                 // lone raw C1 byte
    if (c <= 0xbf) { this.out.push(c); return; }                        // lone continuation, not a control
    if (c === 0xc2) { this.heldC2 = true; return; }
    this.out.push(c);
    if (c >= 0xc3 && c <= 0xdf) this.need = 1;
    else if (c >= 0xe0 && c <= 0xef) this.need = 2;
    else if (c >= 0xf0 && c <= 0xf4) this.need = 3;
  }
}

// ---------------------------------------------------------------- classifiers (SHAPE based)
/** complete CSI bytes (ESC [ ... final) -> label of the query shape, or null */
function csiQueryGet(get: (i: number) => number, n: number): string | null {
  let i = 2;
  let pre = '';
  if (get(i) >= 0x3c && get(i) <= 0x3f) { pre = String.fromCharCode(get(i)); i++; }
  let par = '';
  while (i < n && get(i) >= 0x30 && get(i) <= 0x3b) par += String.fromCharCode(get(i++));
  let inter = '';
  while (i < n && get(i) >= 0x20 && get(i) <= 0x2f) inter += String.fromCharCode(get(i++));
  if (i !== n - 1 || get(i) < 0x40 || get(i) > 0x7e) return null;   // anything odd: ignored by terminals, not a query
  const fin = String.fromCharCode(get(i));
  const first = par.split(';')[0].split(':')[0];
  const p0 = first === '' ? 0 : Number(first);
  if (inter === '') {
    switch (fin) {
      case 'c': return p0 === 0 ? (pre === '' ? 'DA1' : pre === '>' ? 'DA2' : pre === '=' ? 'DA3' : null) : null;
      case 'q': return pre === '>' && p0 === 0 ? 'XTVERSION' : null;
      case 'n': return pre === '?' ? 'DECDSR' : pre === '' && (p0 === 5 || p0 === 6) ? 'DSR' : null;
      case 'u': return pre === '?' ? 'KITTYKBD' : null;
      case 'x': return pre === '' && (p0 === 0 || p0 === 1) ? 'DECREQTPARM' : null;
      case 't': return pre === '' && [11, 13, 14, 15, 16, 18, 19, 20, 21].includes(p0) ? 'XTWINOPS' : null;
      case 'S': return pre === '?' ? 'XTSMGRAPHICS' : null;
      case 'm': return pre === '?' ? 'XTQMODKEYS' : null;
    }
    return null;
  }
  if (inter === '$') {
    if (fin === 'p' && (pre === '' || pre === '?')) return 'DECRQM';
    if (fin === 'u' && pre === '') return 'DECRQTSR';
    if (fin === 'w' && pre === '') return 'DECRQPSR';
  }
  if (inter === '*' && fin === 'y' && pre === '') return 'DECRQCRA';
  return null;
}

export function csiQuery(b: number[]): string | null {
  return csiQueryGet((i) => b[i], b.length);
}

/** OSC payload (between ESC ] and the terminator, ignorable C0 already removed) -> label or null */
export function oscQuery(body: string): string | null {
  const k = body.indexOf(';');
  const codeS = k < 0 ? body : body.slice(0, k);
  if (!/^[0-9]+$/.test(codeS)) return null;     // terminals discard a non-numeric OSC id
  const code = Number(codeS);
  const args = k < 0 ? [] : body.slice(k + 1).split(';');
  if (code === 4 || code === 5 || (code >= 10 && code <= 19)) return args.includes('?') ? `OSC${code}?` : null;
  if (code === 21) return args.some(a => a.endsWith('=?')) ? 'OSC21?' : null;
  if (code === 52) return args.includes('?') ? 'OSC52?' : null;
  if (code === 7501) return args[0] === '?' ? 'OSC7501?' : null;
  if (code === 1337) return /^(Capabilities|ReportCellSize|ReportVariable=)/.test(args[0] ?? '') ? 'OSC1337?' : null;
  if (code === 99) return (args[0] ?? '').split(':').includes('p=?') ? 'OSC99?' : null;
  if (code === 5522) return (args[0] ?? '').split(':').includes('type=read') ? 'OSC5522-read' : null;
  return null;
}

type Decision = 'more' | 'drop' | 'pass';
const startsWithStr = (get: (i: number) => number, n: number, s: string): boolean => { if (n < s.length) return false; for (let i = 0; i < s.length; i++) if (get(i) !== s.charCodeAt(i)) return false; return true; };
const isPrefixOf = (get: (i: number) => number, n: number, s: string): boolean => { if (n > s.length) return false; for (let i = 0; i < n; i++) if (get(i) !== s.charCodeAt(i)) return false; return true; };

interface DataView { get: (i: number) => number; n: number }

/** data = bytes after ESC P. ended = ST seen. Returns decision and a label for drops. */
function decideDcsView(d: DataView, ended: boolean): [Decision, string] {
  if (startsWithStr(d.get, d.n, 'tmux;')) return ['drop', 'DCS-tmux-nested'];
  if (!ended && isPrefixOf(d.get, d.n, 'tmux;')) return ['more', ''];
  let i = 0, inter = '';
  while (i < d.n && (d.get(i) < 0x20 || d.get(i) === 0x7f || (d.get(i) >= 0x30 && d.get(i) <= 0x3f))) i++;       // params (C0 ignored)
  while (i < d.n && d.get(i) >= 0x20 && d.get(i) <= 0x2f) inter += String.fromCharCode(d.get(i++));
  if (i >= d.n) return [ended ? 'pass' : 'more', ''];
  if (d.get(i) < 0x40 || d.get(i) > 0x7e) return ['pass', ''];                                // ignore state
  const ident = inter + String.fromCharCode(d.get(i));
  if (ident === '+q') return ['drop', 'XTGETTCAP'];
  if (ident === '$q') return ['drop', 'DECRQSS'];
  return ['pass', ''];
}

export function decideDcs(d: number[], ended: boolean): [Decision, string] {
  return decideDcsView({ get: (i) => d[i], n: d.length }, ended);
}

/** data = bytes after ESC _ */
function decideApcView(d: DataView, ended: boolean): [Decision, string] {
  if (d.n === 0) return [ended ? 'pass' : 'more', ''];
  if (d.get(0) === 0x47) {                                           // kitty graphics
    let semi = -1;
    for (let i = 0; i < d.n; i++) if (d.get(i) === 0x3b) { semi = i; break; }
    if (semi < 0) {
      if (!ended && d.n < APC_SNIFF) return ['more', ''];
      if (!ended) return ['pass', ''];
      semi = d.n;
    }
    const ctl = latin1FromGet(d.get, 1, semi).split(',');
    return ctl.includes('a=q') ? ['drop', 'KITTYGFX-query'] : ['pass', ''];
  }
  if (startsWithStr(d.get, d.n, '25a1')) return ['drop', 'APC-25a1'];
  if (!ended && isPrefixOf(d.get, d.n, '25a1')) return ['more', ''];
  return ['pass', ''];
}

function latin1FromGet(get: (i: number) => number, from: number, to: number): string {
  let s = '';
  for (let i = from; i < to; i += 8192) {
    const part: number[] = [];
    for (let j = i; j < Math.min(to, i + 8192); j++) part.push(get(j));
    s += String.fromCharCode(...part);
  }
  return s;
}

// ---------------------------------------------------------------- stage 3: the stripper
type St = 'G' | 'ESC' | 'CSI' | 'OSC' | 'SNIFF' | 'DROP' | 'PASS';
type PassKind = 'CSI' | 'ESCI' | 'OSC' | 'STR';

export class Stripper {
  private st: St = 'G';
  private buf = new HBuf();
  private sem = new HBuf();      // OSC semantic body (ignorable C0 left out)
  private out = new HBuf();
  private inter = false;           // ESC state: an intermediate was seen
  private strIsApc = false;        // SNIFF: APC (else DCS)
  private pendEsc = false;         // OSC/SNIFF/DROP/PASS: ESC seen, waiting for '\' or a new sequence
  private passKind: PassKind = 'STR';
  private dropTmux = false;      // DROP of a nested DCS-tmux: ESC ESC is a literal pair, not an abort
  stripped = 0;
  aborted = 0;
  capPasses = 0;
  kinds: Record<string, number> = {};
  onStrip?: (bytes: Uint8Array, label: string) => void;

  held(): number { return this.buf.n + (this.pendEsc ? 1 : 0); }
  /** ground state with nothing held back. */
  isIdle(): boolean { return this.st === 'G' && this.buf.n === 0 && !this.pendEsc; }

  process(inp: Ch): Ch {
    this.out.reset();
    for (let i = 0; i < inp.n; i++) this.byte(inp.arr[i]);
    return this.out.view();
  }
  /** end of stream: whatever is undecided is an aborted sequence and is dropped. */
  flush(): Ch {
    this.out.reset();
    this.toG();
    return this.out.view();
  }

  private emit(b: HBuf) { this.out.pushArr(b.view().arr, b.view().n); }
  private strip(label: string) {
    this.stripped++; this.kinds[label] = (this.kinds[label] ?? 0) + 1;
    if (this.onStrip) {
      const v = new Uint8Array(this.buf.n);
      for (let i = 0; i < this.buf.n; i++) v[i] = this.buf.view().arr[i];
      this.onStrip(v, label);
    }
    this.buf.reset();
  }
  private toG() { this.st = 'G'; this.buf.reset(); this.sem.reset(); this.pendEsc = false; this.inter = false; }
  /** abort whatever is in progress; a forwarded string gets a CAN so the receiver also leaves it */
  private abort(count = true) {
    if (this.st === 'PASS') this.out.push(CAN);
    if (count && this.st !== 'G') this.aborted++;
    this.toG();
  }
  private restartEsc() { this.buf.reset(); this.buf.push(ESC); this.st = 'ESC'; this.inter = false; this.pendEsc = false; this.sem.reset(); }

  private byte(c: number): void {
    if (c === BOUNDARY) { this.abort(); return; }
    switch (this.st) {
      case 'G':
        if (c === ESC) this.restartEsc(); else this.out.push(c);
        return;

      case 'ESC': {
        if (c === ESC) { this.aborted++; this.restartEsc(); return; }
        if (c === CAN || c === SUB) { this.aborted++; this.toG(); this.out.push(c); return; }
        if (c < 0x20 || c === 0x7f) { this.out.push(c); return; }            // executed in place
        if (c <= 0x2f) {
          this.buf.push(c); this.inter = true;
          if (this.buf.n > CSI_CAP) { this.emit(this.buf); this.capPasses++; this.buf.reset(); this.st = 'PASS'; this.passKind = 'ESCI'; }
          return;
        }
        if (!this.inter) {
          switch (c) {
            case 0x5b: this.buf.push(c); this.st = 'CSI'; return;
            case 0x5d: this.buf.push(c); this.st = 'OSC'; this.sem.reset(); this.pendEsc = false; return;
            case 0x50: case 0x5f: this.buf.push(c); this.st = 'SNIFF'; this.strIsApc = c === 0x5f; this.pendEsc = false; return;
            case 0x58: case 0x5e: this.buf.push(c); this.emit(this.buf); this.buf.reset(); this.st = 'PASS'; this.passKind = 'STR'; this.pendEsc = false; return;
            case 0x5a: this.buf.push(c); this.strip('DECID'); this.toG(); return;
          }
        }
        this.buf.push(c); this.emit(this.buf); this.toG();
        return;
      }

      case 'CSI': {
        if (c === ESC) { this.aborted++; this.restartEsc(); return; }
        if (c === CAN || c === SUB) { this.aborted++; this.toG(); this.out.push(c); return; }
        if (c < 0x20 || c === 0x7f) { this.out.push(c); return; }
        this.buf.push(c);
        if (c >= 0x40 && c <= 0x7e) {
          const q = csiQueryGet((i) => this.buf.view().arr[i], this.buf.n);
          if (q) this.strip(q); else this.emit(this.buf);
          this.toG(); return;
        }
        if (this.buf.n > CSI_CAP) { this.emit(this.buf); this.capPasses++; this.buf.reset(); this.st = 'PASS'; this.passKind = 'CSI'; }
        return;
      }

      case 'OSC': {
        if (this.pendEsc) {
          if (c === BS) { this.buf.push(ESC); this.buf.push(c); this.finishOsc(); return; }
          this.aborted++; this.restartEsc(); this.byte(c); return;          // ESC + not-ST: partial dropped, new sequence
        }
        if (c === ESC) { this.pendEsc = true; return; }
        if (c === BEL) { this.buf.push(c); this.finishOsc(); return; }
        if (c === CAN || c === SUB) { this.aborted++; this.toG(); this.out.push(c); return; }
        this.buf.push(c);
        if (c >= 0x20) this.sem.push(c);
        if (this.buf.n > OSC_CAP) { this.emit(this.buf); this.capPasses++; this.buf.reset(); this.sem.reset(); this.st = 'PASS'; this.passKind = 'OSC'; this.pendEsc = false; }
        return;
      }

      case 'SNIFF': {
        if (this.pendEsc) {
          if (c === BS) { this.buf.push(ESC); this.buf.push(c); this.endSniff(true); return; }
          this.aborted++; this.restartEsc(); this.byte(c); return;
        }
        if (c === ESC) { this.pendEsc = true; return; }
        if (c === CAN || c === SUB) { this.aborted++; this.toG(); this.out.push(c); return; }
        this.buf.push(c);
        const data = { get: (i: number) => this.buf.view().arr[2 + i], n: this.buf.n - 2 };
        const [d, label] = this.strIsApc ? decideApcView(data, false) : decideDcsView(data, false);
        this.dropTmux = label === 'DCS-tmux-nested';
        if (d === 'drop') { this.stripped++; this.kinds[label] = (this.kinds[label] ?? 0) + 1; if (this.onStrip) this.onStrip(this.bufBytes(), label); this.buf.reset(); this.st = 'DROP'; this.pendEsc = false; return; }
        if (d === 'pass' || (!this.strIsApc && data.n >= DCS_CAP)) { this.emit(this.buf); this.buf.reset(); this.st = 'PASS'; this.passKind = 'STR'; this.pendEsc = false; return; }
        return;
      }

      case 'DROP': {
        if (this.pendEsc) {
          if (c === BS) { this.toG(); return; }
          if (c === ESC && this.dropTmux) { this.pendEsc = false; return; }
          this.aborted++; this.restartEsc(); this.byte(c); return;
        }
        if (c === ESC) { this.pendEsc = true; return; }
        if (c === CAN || c === SUB) { this.aborted++; this.toG(); this.out.push(c); return; }
        return;
      }

      case 'PASS': {
        if (this.passKind === 'CSI' || this.passKind === 'ESCI') {
          if (c === ESC) { this.abort(); this.restartEsc(); return; }
          this.out.push(c);
          if (c === CAN || c === SUB) { this.toG(); return; }
          const lo = this.passKind === 'CSI' ? 0x40 : 0x30;
          if (c >= lo && c <= 0x7e) this.toG();
          return;
        }
        if (this.pendEsc) {
          if (c === BS) { this.out.push(ESC); this.out.push(c); this.toG(); return; }
          this.pendEsc = false; this.out.push(CAN); this.aborted++; this.restartEsc(); this.byte(c); return;
        }
        if (c === ESC) { this.pendEsc = true; return; }
        this.out.push(c);
        if (c === CAN || c === SUB) { this.toG(); return; }
        if (c === BEL && this.passKind === 'OSC') this.toG();
        return;
      }
    }
  }

  private bufBytes(): Uint8Array {
    const v = new Uint8Array(this.buf.n);
    for (let i = 0; i < this.buf.n; i++) v[i] = this.buf.view().arr[i];
    return v;
  }

  private finishOsc() {
    const q = oscQuery(latin1(this.sem.view().arr, this.sem.n));
    if (q) this.strip(q); else this.emit(this.buf);
    this.toG();
  }
  private endSniff(ended: boolean) {
    const data = { get: (i: number) => this.buf.view().arr[2 + i], n: this.buf.n - 2 - 2 };
    const [d, label] = this.strIsApc ? decideApcView(data, ended) : decideDcsView(data, ended);
    if (d === 'drop') this.strip(label); else this.emit(this.buf);
    this.toG();
  }
}

// ---------------------------------------------------------------- the composed tap stage
export class QueryStripper {
  private unwrap: TmuxUnwrap[] = [];
  private c1 = new C1Filter();
  private strip = new Stripper();
  private inBuf = new HBuf();          // byte chunk -> Int16 exchange (machine segments only)
  private outBytes = new Uint8Array(64); // accumulated output for one push (reused; returned copy is fresh)
  private outLen = 0;
  private flushBuf = new HBuf();

  constructor() {
    for (let i = 0; i < TMUX_DEPTH; i++) this.unwrap.push(new TmuxUnwrap());
    // Depth 5+ envelopes: the four unwrap stages remove four layers, so a surviving
    // `ESC P tmux;` reaches the Stripper, which classifies it DCS-tmux-nested and drops
    // it entirely (fail closed). No bytes of a depth-5 envelope ever reach a viewer.
  }
  get stripped() { return this.strip.stripped; }
  get aborted() { return this.strip.aborted; }
  get capPasses() { return this.strip.capPasses; }
  get kinds() { return this.strip.kinds; }
  set onStrip(f: ((b: Uint8Array, label: string) => void) | undefined) { this.strip.onStrip = f; }

  /** bytes held back right now (undecided). There is no timer: they wait for more input. */
  pending(): number {
    let p = this.c1.held() + this.strip.held();
    for (const u of this.unwrap) p += u.held();
    return p;
  }

  /** every stage in ground state: the fast path may copy inert bytes straight through. */
  private isIdle(): boolean {
    for (const u of this.unwrap) if (!u.isIdle()) return false;
    return this.c1.isIdle() && this.strip.isIdle();
  }

  /** first byte at/after `from` that the state machines must see (ESC, or >= 0x80). */
  private nextInteresting(chunk: Uint8Array, from: number): number {
    let h = chunk.length;
    for (let j = from; j < chunk.length; j++) {
      if (chunk[j] >= 0x80) { h = j; break; }
    }
    const e = chunk.indexOf(ESC, from);
    if (e === -1) return h;
    return e < h ? e : h;
  }

  private outAppend(b: Uint8Array): void {
    if (this.outLen + b.length > this.outBytes.length) {
      const nb = new Uint8Array(Math.max(64, (this.outLen + b.length) * 2));
      nb.set(this.outBytes.subarray(0, this.outLen)); this.outBytes = nb;
    }
    this.outBytes.set(b, this.outLen); this.outLen += b.length;
  }

  private outAppendCh(c: Ch): void {
    if (this.outLen + c.n > this.outBytes.length) {
      const nb = new Uint8Array(Math.max(64, (this.outLen + c.n) * 2));
      nb.set(this.outBytes.subarray(0, this.outLen)); this.outBytes = nb;
    }
    for (let i = 0; i < c.n; i++) this.outBytes[this.outLen++] = c.arr[i];
  }

  /** run the full machine chain over chunk[start,end) and append its output.
   *
   * A stage sitting in ground state is an identity pass for input free of the
   * bytes that can affect it (ESC for the unwrap stages and the stripper, high
   * bytes for the C1 filter): skipping the per-byte walk then is a memcpy, not
   * a semantic change, and the differential suite proves it byte-for-byte.
   */
  private runMachines(chunk: Uint8Array, start: number, end: number): void {
    this.inBuf.reset();
    for (let i = start; i < end; i++) this.inBuf.push(chunk[i]);
    let v: Ch = this.inBuf.view();
    for (const u of this.unwrap) {
      if (!(u.isIdle() && v.arr.subarray(0, v.n).indexOf(ESC) === -1)) v = u.process(v);
    }
    if (!(this.c1.isIdle() && !hasHighByte(v.arr, v.n))) v = this.c1.process(v);
    // The stripper consumes BOUNDARY tokens (aborting whatever is in progress),
    // so its identity condition must also require none.
    if (!(this.strip.isIdle() && v.arr.subarray(0, v.n).indexOf(ESC) === -1
        && v.arr.subarray(0, v.n).indexOf(BOUNDARY) === -1)) v = this.strip.process(v);
    this.outAppendCh(v);
  }

  push(chunk: Uint8Array): Uint8Array {
    this.outLen = 0;
    if (chunk.length === 0) return this.outBytes.slice(0, 0);
    // Fast path: idle stages pass 0x00..0x1a / 0x1c..0x7f through unchanged in every stage,
    // so when (and only when) every stage is idle, copy clean spans straight out and run the
    // machines only from each byte that could start or continue a sequence (0x1b, >= 0x80).
    // Machines are fed bounded slices; idle is re-checked after each slice, so a long clean
    // tail after a short sequence is fast-pathed again. Slice boundaries are invisible to
    // the machines (their state persists across slices exactly as across chunks).
    let pos = 0;
    while (pos < chunk.length) {
      if (!this.isIdle()) {
        const end = Math.min(chunk.length, pos + 4096);
        this.runMachines(chunk, pos, end);
        pos = end;
        continue;
      }
      const interesting = this.nextInteresting(chunk, pos);
      if (interesting > pos) this.outAppend(chunk.subarray(pos, interesting));
      pos = interesting;
      if (pos >= chunk.length) break;
      const end = Math.min(chunk.length, pos + 4096);
      this.runMachines(chunk, pos, end);
      pos = end;
    }
    return this.outBytes.slice(0, this.outLen);
  }

  /** end of stream: undecided bytes are dropped (aborted partial), never emitted. */
  flush(): Uint8Array {
    this.outLen = 0;
    // Envelope-ends propagate downstream: each unwrap stage's flush output joins the next
    // stage's input (concat, as in the reference), then C1, then the stripper.
    let v: Ch = EMPTY_CH;
    let tmp = this.flushBuf;
    for (const u of this.unwrap) {
      const a = u.process(v);
      tmp.reset(); tmp.pushCh(a); tmp.pushCh(u.flush());
      v = tmp.view();
      tmp = new HBuf(); // fresh accumulator for the next stage (process views stay live)
    }
    const b = this.c1.process(v); this.c1.flush();
    const s1 = this.strip.process(b);
    const s2 = this.strip.flush();
    this.outAppendCh(s1); this.outAppendCh(s2);
    return this.outBytes.slice(0, this.outLen);
  }
}

export function stripAll(b: Uint8Array): Uint8Array {
  const s = new QueryStripper(); const a = s.push(b); const t = s.flush();
  const r = new Uint8Array(a.length + t.length); r.set(a, 0); r.set(t, a.length); return r;
}

