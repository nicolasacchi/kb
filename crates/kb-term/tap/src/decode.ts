// kb-term tap: tmux control-mode line decoder. Zero dependencies.
//
// Control mode (`tmux -C attach`) is a line protocol on stdout. The tap's input
// is a stream of arbitrary chunks; this decoder splits them into lines and
// classifies each line. `%output` values carry octal escapes (`\033` is ESC,
// `\134` is a backslash); every other byte >= 0x20 appears raw (UTF-8 passes
// through untouched). Everything decoded here is still stripped by the query
// stripper downstream (pipeline.ts), so a malformed escape is not a query risk:
// the backslash is emitted literally and stats.badEscapes is counted.
//
// Rules (brief T3, spec 3):
//  * Lines end at `\n` ONLY. tmux escapes control characters inside `%output`
//    values, so a raw `\r` or `\n` inside a value never occurs. Exactly one
//    trailing `\r` is stripped from non-`%output` control lines.
//  * `%output %P <value>`: the value is everything after the single space
//    following the pane id. Octal escapes decode; a backslash not followed by
//    three octal digits is literal (stats.badEscapes++).
//  * `%extended-output %P <age> ... : <value>`: the value is the data after the
//    first ` : `; the age parses after the pane id. Missing ` : ` (or a
//    non-integer age) counts stats.malformed++ and yields 'other'.
//  * Reply blocks: after `%begin t n f`, every line until the matching
//    `%end`/`%error t n f` (same t and n) is a raw 'reply', even one that
//    starts with `%output`. An unmatched `%end`/`%error` outside a block
//    yields 'other'.
//  * Oversize guard: a line longer than 4 MiB is discarded to the next `\n`
//    with stats.oversizeLines++.
//  * `data` and reply lines are fresh arrays. No per-byte allocation.

export type CtlEvent =
  | { kind: 'output'; pane: string; data: Uint8Array }
  | { kind: 'extended-output'; pane: string; ageMs: number; data: Uint8Array }
  | { kind: 'pause'; pane: string }
  | { kind: 'continue'; pane: string }
  | { kind: 'exit'; reason: string }
  | { kind: 'begin' | 'end' | 'error'; time: string; num: string; flags: string }
  | { kind: 'reply'; line: Uint8Array }
  | { kind: 'other'; line: string };

const BS = 0x5c; // backslash
const LINE_MAX = 4 * 1024 * 1024;
const NL = 0x0a;

const isOctal = (b: number): boolean => b >= 0x30 && b <= 0x37;

/** Octal-unescape a control-mode value (fresh output array). */
export function decodeValue(v: Uint8Array): Uint8Array {
  return decodeValueCounted(v).data;
}

function decodeValueCounted(v: Uint8Array): { data: Uint8Array; bad: number } {
  let bad = 0;
  const out = new Uint8Array(v.length);
  let o = 0;
  for (let i = 0; i < v.length; i++) {
    const b = v[i];
    if (b !== BS) { out[o++] = b; continue; }
    if (i + 3 < v.length && isOctal(v[i + 1]) && isOctal(v[i + 2]) && isOctal(v[i + 3])) {
      out[o++] = ((v[i + 1] - 0x30) << 6) | ((v[i + 2] - 0x30) << 3) | (v[i + 3] - 0x30);
      i += 3;
    } else {
      bad++;          // malformed escape: emit the backslash literally
      out[o++] = b;
    }
  }
  return { data: out.slice(0, o), bad };
}

const latin1 = (b: Uint8Array, n: number): string => {
  let s = '';
  for (let i = 0; i < n; i += 8192) s += String.fromCharCode(...b.subarray(i, Math.min(n, i + 8192)));
  return s;
};

export class ControlDecoder {
  stats: { oversizeLines: number; badEscapes: number; malformed: number } = {
    oversizeLines: 0, badEscapes: 0, malformed: 0,
  };
  private buf = new Uint8Array(4096);
  private len = 0;
  private discarding = false; // inside an oversize line, dropping to the next \n
  private block: { time: string; num: string } | null = null;

  push(chunk: Uint8Array): CtlEvent[] {
    const events: CtlEvent[] = [];
    if (this.len + chunk.length > this.buf.length) {
      const nb = new Uint8Array(Math.max(this.buf.length * 2, this.len + chunk.length));
      nb.set(this.buf.subarray(0, this.len)); this.buf = nb;
    }
    this.buf.set(chunk, this.len); this.len += chunk.length;
    for (;;) {
      const nl = this.buf.indexOf(NL, 0);
      // indexOf from 0 each scan is fine: lines are consumed in order.
      const end = nl === -1 ? this.len : nl;
      if (this.discarding) {
        if (nl === -1) { this.len = 0; break; }
        // drop through the newline and resync
        this.consume(nl + 1);
        this.discarding = false;
        continue;
      }
      if (end > LINE_MAX) {
        // oversize line: discard it (to the next \n) and count it once
        this.stats.oversizeLines++;
        if (nl === -1) { this.discarding = true; this.len = 0; break; }
        this.consume(nl + 1);
        continue;
      }
      if (nl === -1) {
        if (this.len > LINE_MAX) {
          this.stats.oversizeLines++;
          this.discarding = true; this.len = 0;
        }
        break;
      }
      const line = this.buf.slice(0, nl); // fresh bytes
      this.consume(nl + 1);
      this.handleLine(line, events);
    }
    return events;
  }

  private consume(k: number): void {
    this.buf.copyWithin(0, k, this.len);
    this.len -= k;
    // erase the stale tail so a later indexOf never sees it as input
    this.buf.fill(0, this.len, this.len + k);
  }

  private handleLine(raw: Uint8Array, events: CtlEvent[]): void {
    if (this.block) {
      // Inside a %begin..%end/%error block every line is raw reply text, except
      // the matching %end / %error that closes the block.
      const line = latin1(raw, raw.length);
      if (line.startsWith('%end') || line.startsWith('%error')) {
        const m = splitTokens(line);
        if (m && m[1] === this.block.time && m[2] === this.block.num) {
          const kind: 'end' | 'error' = line.startsWith('%end') ? 'end' : 'error';
          this.block = null;
          events.push({ kind, time: m[1], num: m[2], flags: m[3] ?? '' });
          return;
        }
      }
      // raw reply bytes (line content without the \n); nothing is stripped
      events.push({ kind: 'reply', line: raw.slice(0) });
      return;
    }
    const line = latin1(raw, raw.length).replace(/\r$/, '');
    if (!line.startsWith('%')) { events.push({ kind: 'other', line }); return; }

    if (line.startsWith('%output ')) {
      // %output %P <value>: value = everything after the single space after the pane id.
      const afterPfx = line.slice('%output '.length);
      const sp = afterPfx.indexOf(' ');
      if (sp < 0) { events.push({ kind: 'other', line }); return; }
      const pane = afterPfx.slice(0, sp);
      // byte offsets equal string offsets (latin1 is 1:1 with bytes)
      const valueStart = '%output '.length + sp + 1;
      const { data, bad } = decodeValueCounted(raw.subarray(valueStart));
      this.stats.badEscapes += bad;
      events.push({ kind: 'output', pane, data });
      return;
    }
    if (line.startsWith('%extended-output ')) {
      const afterPfx = line.slice('%extended-output '.length);
      const sp = afterPfx.indexOf(' ');
      if (sp < 0) { this.stats.malformed++; events.push({ kind: 'other', line }); return; }
      const pane = afterPfx.slice(0, sp);
      const ageTok = afterPfx.slice(sp + 1).split(' ')[0];
      const ageMs = Number(ageTok);
      const sep = line.indexOf(' : ');
      if (sep < 0 || !Number.isInteger(ageMs)) {
        this.stats.malformed++;
        events.push({ kind: 'other', line });
        return;
      }
      const valueStart = sep + 3;
      const { data, bad } = decodeValueCounted(raw.subarray(valueStart));
      this.stats.badEscapes += bad;
      events.push({ kind: 'extended-output', pane, ageMs, data });
      return;
    }
    if (line.startsWith('%pause ') || line.startsWith('%continue ')) {
      const kind: 'pause' | 'continue' = line.startsWith('%pause') ? 'pause' : 'continue';
      const pane = line.split(' ')[1] ?? '';
      events.push(kind === 'pause' ? { kind, pane } : { kind, pane });
      return;
    }
    if (line.startsWith('%exit')) {
      const reason = line === '%exit' ? '' : line.slice('%exit'.length).trim();
      events.push({ kind: 'exit', reason });
      return;
    }
    if (line.startsWith('%end') || line.startsWith('%error')) {
      events.push({ kind: 'other', line });   // unmatched outside a block
      return;
    }
    if (line.startsWith('%begin')) {
      const m = splitTokens(line);
      if (!m || m.length < 4) { events.push({ kind: 'other', line }); return; }
      this.block = { time: m[1], num: m[2] };
      events.push({ kind: 'begin', time: m[1], num: m[2], flags: m[3] });
      return;
    }
    events.push({ kind: 'other', line });
  }
}

/** whitespace-separated tokens (control lines are ASCII structure). */
function splitTokens(line: string): string[] | null {
  const t = line.trim().split(/\s+/).filter(x => x.length > 0);
  return t.length === 0 ? null : t;
}
