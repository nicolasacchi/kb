// kb-term tap: query stripper v2 (KBT-STRIP). Zero dependencies.
//
// Pipeline (stage order is part of the design, see TapPipeline):
//   decode (caller) -> TmuxUnwrap x4 -> C1Filter -> Stripper
// Stages exchange number[] where 0..255 are bytes and BOUNDARY (-1) means "any sequence in
// progress is aborted here" (an envelope edge, or a deleted C1 control). BOUNDARY is how a
// deletion can never splice the bytes before it onto the bytes after it.
//
// Rules (round-3 findings):
//  * DCS-tmux envelopes are unwrapped (ESC un-doubled) BEFORE stripping; viewers never see them.
//  * An aborted or partial sequence is DROPPED, never emitted (no splice: `ESC[ ESC[c c` -> `c`).
//  * No timer flush. End of stream drops whatever is undecided. Hold-back caps: CSI 64 B,
//    OSC 256 B, DCS 4096 B, APC G sniff 512 B; past a cap the sequence passes verbatim.
//  * A string that was already forwarded (past a cap or not a query) and is then aborted gets a
//    CAN (0x18) appended so the receiving terminal also leaves the string state.
//  * C1: UTF-8 U+0080..U+009F and lone raw 0x80..0x9f bytes are deleted and abort what is in progress.
//  * Queries are matched by SHAPE, never by harness name.

export const BOUNDARY = -1;
const ESC = 0x1b, BEL = 0x07, CAN = 0x18, SUB = 0x1a, BS = 0x5c;
export const CSI_CAP = 64;
export const OSC_CAP = 256;
export const DCS_CAP = 4096;
export const APC_SNIFF = 512;
export const TMUX_DEPTH = 4;

const latin1 = (b: number[]): string => { let s = ''; for (let i = 0; i < b.length; i += 8192) s += String.fromCharCode(...b.slice(i, i + 8192)); return s; };

// ---------------------------------------------------------------- stage 1: DCS-tmux unwrap
const TMUX_PFX = [ESC, 0x50, 0x74, 0x6d, 0x75, 0x78, 0x3b]; // ESC P t m u x ;
export class TmuxUnwrap {
  private st: 'G' | 'M' | 'W' | 'WE' = 'G';
  private m = 0;
  private out: number[] = [];
  held(): number { return this.st === 'M' ? this.m : 0; }
  process(inp: number[]): number[] {
    this.out = [];
    for (const c of inp) this.step(c);
    return this.out;
  }
  flush(): number[] {
    this.out = [];
    if (this.st === 'W' || this.st === 'WE') this.out.push(BOUNDARY);
    this.st = 'G'; this.m = 0;
    return this.out;
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
  private out: number[] = [];
  held(): number { return this.heldC2 ? 1 : 0; }
  process(inp: number[]): number[] { this.out = []; for (const c of inp) this.step(c); return this.out; }
  flush(): number[] { this.out = []; this.heldC2 = false; this.need = 0; return this.out; }
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
export function csiQuery(b: number[]): string | null {
  let i = 2;
  let pre = '';
  if (b[i] >= 0x3c && b[i] <= 0x3f) { pre = String.fromCharCode(b[i]); i++; }
  let par = '';
  while (i < b.length && b[i] >= 0x30 && b[i] <= 0x3b) par += String.fromCharCode(b[i++]);
  let inter = '';
  while (i < b.length && b[i] >= 0x20 && b[i] <= 0x2f) inter += String.fromCharCode(b[i++]);
  if (i !== b.length - 1 || b[i] < 0x40 || b[i] > 0x7e) return null;   // anything odd: ignored by terminals, not a query
  const fin = String.fromCharCode(b[i]);
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
const startsWithStr = (d: number[], s: string): boolean => { if (d.length < s.length) return false; for (let i = 0; i < s.length; i++) if (d[i] !== s.charCodeAt(i)) return false; return true; };
const isPrefixOf = (d: number[], s: string): boolean => { if (d.length > s.length) return false; for (let i = 0; i < d.length; i++) if (d[i] !== s.charCodeAt(i)) return false; return true; };

/** data = bytes after ESC P. ended = ST seen. Returns decision and a label for drops. */
export function decideDcs(d: number[], ended: boolean): [Decision, string] {
  if (startsWithStr(d, 'tmux;')) return ['drop', 'DCS-tmux-nested'];
  if (!ended && isPrefixOf(d, 'tmux;')) return ['more', ''];
  let i = 0, inter = '';
  while (i < d.length && (d[i] < 0x20 || d[i] === 0x7f || (d[i] >= 0x30 && d[i] <= 0x3f))) i++;       // params (C0 ignored)
  while (i < d.length && d[i] >= 0x20 && d[i] <= 0x2f) inter += String.fromCharCode(d[i++]);
  if (i >= d.length) return [ended ? 'pass' : 'more', ''];
  if (d[i] < 0x40 || d[i] > 0x7e) return ['pass', ''];                                // ignore state
  const ident = inter + String.fromCharCode(d[i]);
  if (ident === '+q') return ['drop', 'XTGETTCAP'];
  if (ident === '$q') return ['drop', 'DECRQSS'];
  return ['pass', ''];
}

/** data = bytes after ESC _ */
export function decideApc(d: number[], ended: boolean): [Decision, string] {
  if (d.length === 0) return [ended ? 'pass' : 'more', ''];
  if (d[0] === 0x47) {                                           // kitty graphics
    let semi = d.indexOf(0x3b);
    if (semi < 0) {
      if (!ended && d.length < APC_SNIFF) return ['more', ''];
      if (!ended) return ['pass', ''];
      semi = d.length;
    }
    const ctl = latin1(d.slice(1, semi)).split(',');
    return ctl.includes('a=q') ? ['drop', 'KITTYGFX-query'] : ['pass', ''];
  }
  if (startsWithStr(d, '25a1')) return ['drop', 'APC-25a1'];
  if (!ended && isPrefixOf(d, '25a1')) return ['more', ''];
  return ['pass', ''];
}

// ---------------------------------------------------------------- stage 3: the stripper
type St = 'G' | 'ESC' | 'CSI' | 'OSC' | 'SNIFF' | 'DROP' | 'PASS';
type PassKind = 'CSI' | 'ESCI' | 'OSC' | 'STR';

export class Stripper {
  private st: St = 'G';
  private buf: number[] = [];
  private sem: number[] = [];      // OSC semantic body (ignorable C0 left out)
  private out: number[] = [];
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

  held(): number { return this.buf.length + (this.pendEsc ? 1 : 0); }

  process(inp: number[]): number[] {
    this.out = [];
    for (const c of inp) this.byte(c);
    return this.out;
  }
  /** end of stream: whatever is undecided is an aborted sequence and is dropped. */
  flush(): number[] {
    this.out = [];
    this.toG();
    return this.out;
  }

  private emit(b: number[]) { for (const x of b) this.out.push(x); }
  private strip(label: string) {
    this.stripped++; this.kinds[label] = (this.kinds[label] ?? 0) + 1;
    if (this.onStrip) this.onStrip(Uint8Array.from(this.buf), label);
    this.buf = [];
  }
  private toG() { this.st = 'G'; this.buf = []; this.sem = []; this.pendEsc = false; this.inter = false; }
  /** abort whatever is in progress; a forwarded string gets a CAN so the receiver also leaves it */
  private abort(count = true) {
    if (this.st === 'PASS') this.out.push(CAN);
    if (count && this.st !== 'G') this.aborted++;
    this.toG();
  }
  private restartEsc() { this.buf = [ESC]; this.st = 'ESC'; this.inter = false; this.pendEsc = false; this.sem = []; }

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
          if (this.buf.length > CSI_CAP) { this.emit(this.buf); this.capPasses++; this.buf = []; this.st = 'PASS'; this.passKind = 'ESCI'; }
          return;
        }
        if (!this.inter) {
          switch (c) {
            case 0x5b: this.buf.push(c); this.st = 'CSI'; return;
            case 0x5d: this.buf.push(c); this.st = 'OSC'; this.sem = []; this.pendEsc = false; return;
            case 0x50: case 0x5f: this.buf.push(c); this.st = 'SNIFF'; this.strIsApc = c === 0x5f; this.pendEsc = false; return;
            case 0x58: case 0x5e: this.buf.push(c); this.emit(this.buf); this.buf = []; this.st = 'PASS'; this.passKind = 'STR'; this.pendEsc = false; return;
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
          const q = csiQuery(this.buf);
          if (q) this.strip(q); else this.emit(this.buf);
          this.toG(); return;
        }
        if (this.buf.length > CSI_CAP) { this.emit(this.buf); this.capPasses++; this.buf = []; this.st = 'PASS'; this.passKind = 'CSI'; }
        return;
      }

      case 'OSC': {
        if (this.pendEsc) {
          if (c === BS) { this.buf.push(ESC, c); this.finishOsc(); return; }
          this.aborted++; this.restartEsc(); this.byte(c); return;          // ESC + not-ST: partial dropped, new sequence
        }
        if (c === ESC) { this.pendEsc = true; return; }
        if (c === BEL) { this.buf.push(c); this.finishOsc(); return; }
        if (c === CAN || c === SUB) { this.aborted++; this.toG(); this.out.push(c); return; }
        this.buf.push(c);
        if (c >= 0x20) this.sem.push(c);
        if (this.buf.length > OSC_CAP) { this.emit(this.buf); this.capPasses++; this.buf = []; this.sem = []; this.st = 'PASS'; this.passKind = 'OSC'; this.pendEsc = false; }
        return;
      }

      case 'SNIFF': {
        if (this.pendEsc) {
          if (c === BS) { this.buf.push(ESC, c); this.endSniff(true); return; }
          this.aborted++; this.restartEsc(); this.byte(c); return;
        }
        if (c === ESC) { this.pendEsc = true; return; }
        if (c === CAN || c === SUB) { this.aborted++; this.toG(); this.out.push(c); return; }
        this.buf.push(c);
        const data = this.buf.slice(2);
        const [d, label] = this.strIsApc ? decideApc(data, false) : decideDcs(data, false);
        this.dropTmux = label === 'DCS-tmux-nested';
        if (d === 'drop') { this.stripped++; this.kinds[label] = (this.kinds[label] ?? 0) + 1; if (this.onStrip) this.onStrip(Uint8Array.from(this.buf), label); this.buf = []; this.st = 'DROP'; this.pendEsc = false; return; }
        if (d === 'pass' || (!this.strIsApc && data.length >= DCS_CAP)) { this.emit(this.buf); this.buf = []; this.st = 'PASS'; this.passKind = 'STR'; this.pendEsc = false; return; }
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
          if (c === BS) { this.out.push(ESC, c); this.toG(); return; }
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

  private finishOsc() {
    const q = oscQuery(latin1(this.sem));
    if (q) this.strip(q); else this.emit(this.buf);
    this.toG();
  }
  private endSniff(ended: boolean) {
    const data = this.buf.slice(2, this.buf.length - 2);
    const [d, label] = this.strIsApc ? decideApc(data, ended) : decideDcs(data, ended);
    if (d === 'drop') this.strip(label); else this.emit(this.buf);
    this.toG();
  }
}

// ---------------------------------------------------------------- the composed tap stage
export class QueryStripper {
  private unwrap = Array.from({ length: TMUX_DEPTH }, () => new TmuxUnwrap());
  private c1 = new C1Filter();
  private strip = new Stripper();
  get stripped() { return this.strip.stripped; }
  get aborted() { return this.strip.aborted; }
  get capPasses() { return this.strip.capPasses; }
  get kinds() { return this.strip.kinds; }
  set onStrip(f: ((b: Uint8Array, label: string) => void) | undefined) { this.strip.onStrip = f; }

  /** bytes held back right now (undecided). There is no timer: they wait for more input. */
  pending(): number {
    return this.unwrap.reduce((a, u) => a + u.held(), 0) + this.c1.held() + this.strip.held();
  }
  push(chunk: Uint8Array): Uint8Array {
    let v: number[] = Array.from(chunk);
    for (const u of this.unwrap) v = u.process(v);
    v = this.c1.process(v);
    return Uint8Array.from(this.strip.process(v));
  }
  /** end of stream: undecided bytes are dropped (aborted partial), never emitted. */
  flush(): Uint8Array {
    let v: number[] = [];
    for (const u of this.unwrap) v = u.process(v).concat(u.flush());   // envelope ends propagate downstream
    v = this.c1.process(v); this.c1.flush();
    const a = this.strip.process(v);
    return Uint8Array.from(a.concat(this.strip.flush()));
  }
}

export function stripAll(b: Uint8Array): Uint8Array {
  const s = new QueryStripper(); const a = s.push(b); const t = s.flush();
  const r = new Uint8Array(a.length + t.length); r.set(a, 0); r.set(t, a.length); return r;
}
