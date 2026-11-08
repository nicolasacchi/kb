// kb-term tap: withhold/route stage (stage 6, after T4 graphics extraction,
// before ring/twin numbering). A byte-at-a-time state machine; the machine
// state persists across chunks and joins (it sits upstream of ring/twin, so a
// join never splits it) and there are no timers.
//
// On the main screen the tap withholds viewer-hostile sequences from the
// shared stream:
//   - mouse enables (CSI ? Pm h for the mouse modes below)
//   - kitty keyboard push/pop/set (CSI > Pn u, CSI < Pn u, CSI = ... u)
//   - OSC 52 clipboard sets (rerouted as side events, never in the ring)
// On the alt screen everything passes. Resets pass. The twin is fed the
// post-withhold stream, so a viewer (or the twin's serialized mode tail)
// can never be switched into a mouse mode that eats wheel scrolling.
//
// Held-back bytes are bounded: CSI <= 64 B, OSC prefix window <= 256 B.
// Over-cap CSI and OSC sequences pass verbatim EXCEPT OSC 52, which is
// streamed through the router (its payload never enters the ring path) and
// is dropped entirely above 1 MiB.
//
// Also tracked here (it sees every CSI): the alt screen (CSI ? 1049/1047/47
// h|l) and synchronized output (CSI ?2026h/l), which T7 joins use as the
// quiescent-point signal.

export interface ClipboardEvent {
  /** Ring head at the moment of removal: the offset the event applies at. */
  offset: number;
  data_b64: string;
}

export interface WithholdResult {
  bytes: Uint8Array;
  clipboards: ClipboardEvent[];
}

const ESC = 0x1b;
const BEL = 0x07;
const CAN = 0x18;
const SUB = 0x1a;
const BS = 0x5c;

const CSI_CAP = 64;
const OSC_PREFIX_CAP = 256;
const OSC52_CAP = 1024 * 1024;

const MOUSE_MODES = new Set([9, 1000, 1001, 1002, 1003, 1005, 1006, 1015, 1016]);
const ALT_MODES = new Set([47, 1047, 1049]);
const SYNC_MODE = 2026;

const OSC52 = '52;';

type State = 'ground' | 'esc' | 'csi' | 'csiOvercap' | 'osc' | 'oscEsc' | 'str' | 'strEsc';

export class Withholder {
  private state: State = 'ground';
  private csi: number[] = [];
  private osc: number[] = [];
  /** 0 = undecided, 1 = OSC 52 (collecting), 2 = other (streaming verbatim), 3 = OSC 52 over cap (dropping). */
  private oscKind = 0;
  private altScreen = false;
  private syncModeOn = false;

  /** Whether the machine is currently inside the alt screen. */
  get alt(): boolean {
    return this.altScreen;
  }

  /** Synchronized output (?2026h) currently active on the source stream. */
  get syncOn(): boolean {
    return this.syncModeOn;
  }

  /** Held-back bytes not yet decided/emitted. Bounded by CSI_CAP/OSC_PREFIX_CAP. */
  pending(): number {
    return this.csi.length + this.osc.length + (this.state === 'esc' || this.state === 'oscEsc' || this.state === 'strEsc' ? 1 : 0);
  }

  /**
   * Push one chunk. `headAtStart` is the ring head before this chunk's
   * emitted bytes land; each clipboard offset is headAtStart + emitted bytes
   * at the moment of removal (the queue-time offset the viewer applies at).
   */
  push(chunk: Uint8Array, headAtStart = 0): WithholdResult {
    const out: number[] = [];
    const clipboards: ClipboardEvent[] = [];
    const ctx = { out, clipboards, headAtStart };
    let i = 0;
    while (i < chunk.length) {
      const reprocess = this.feedByte(chunk[i], ctx);
      if (reprocess) {
        // byte consumed as an ESC aborted by a new ESC: the byte is the start
        // of the new escape; re-feed it in the esc state without advancing.
        this.state = 'esc';
        this.csi = [];
        this.osc = [];
        this.oscKind = 0;
        continue;
      }
      i++;
    }
    return { bytes: Uint8Array.from(out), clipboards };
  }

  /**
   * End of stream: emit held bytes verbatim (best effort; production calls
   * this only at tap shutdown). Partial sequences are not interpreted.
   */
  flush(): WithholdResult {
    const out: number[] = [];
    if (this.state === 'esc' || this.state === 'oscEsc' || this.state === 'strEsc') out.push(ESC);
    else if (this.state === 'csi' || this.state === 'csiOvercap') out.push(ESC, 0x5b, ...this.csi);
    else if (this.state === 'osc' && this.oscKind === 0) out.push(ESC, 0x5d, ...this.osc);
    this.state = 'ground';
    this.csi = [];
    this.osc = [];
    this.oscKind = 0;
    return { bytes: Uint8Array.from(out), clipboards: [] };
  }

  /**
   * Feed one byte. Returns true when the byte must be re-fed in the esc
   * state (it aborted the escape in progress and starts a new one).
   */
  private feedByte(c: number, ctx: { out: number[]; clipboards: ClipboardEvent[]; headAtStart: number }): boolean {
    const { out } = ctx;
    switch (this.state) {
      case 'ground': {
        if (c === ESC) {
          this.state = 'esc'; // held: the escape completes before anything emits
          return false;
        }
        out.push(c);
        return false;
      }
      case 'esc': {
        // The ESC byte is held, never emitted until its sequence decides.
        if (c === ESC) {
          // ESC aborts the held ESC: the first drops, this one re-arms.
          this.state = 'esc';
          return false;
        }
        if (c === 0x5b) {
          this.state = 'csi';
          this.csi = [];
          return false;
        }
        if (c === 0x5d) {
          this.state = 'osc';
          this.osc = [];
          this.oscKind = 0;
          return false;
        }
        if (c === 0x50 || c === 0x5f || c === 0x58 || c === 0x5e) {
          this.state = 'str';
          out.push(ESC, c);
          return false;
        }
        if (c === CAN || c === SUB) {
          this.state = 'ground';
          out.push(c);
          return false;
        }
        if (c < 0x20) {
          // C0 after a lone ESC: the ESC aborts; the C0 executes.
          this.state = 'ground';
          out.push(c);
          return false;
        }
        // Single-byte escape or nF-style escape: the whole 2 bytes pass.
        this.state = 'ground';
        out.push(ESC, c);
        return false;
      }
      case 'csi': {
        if (c === ESC) {
          this.state = 'esc';
          this.csi = [];
          return true; // byte was the ESC: reprocess as escape initiator
        }
        if (c === CAN || c === SUB) {
          this.state = 'ground';
          this.csi = [];
          out.push(c);
          return false;
        }
        if (c < 0x20) {
          out.push(c); // C0 executes in place; CSI continues
          return false;
        }
        if (c >= 0x40 && c <= 0x7e) {
          this.finishCsi(c, ctx);
          this.state = 'ground';
          this.csi = [];
          return false;
        }
        this.csi.push(c);
        if (this.csi.length > CSI_CAP) {
          // Over-cap CSI passes verbatim.
          out.push(ESC, 0x5b, ...this.csi);
          this.csi = [];
          this.state = 'csiOvercap';
        }
        return false;
      }
      case 'csiOvercap': {
        if (c === ESC) {
          this.state = 'esc';
          return true;
        }
        if (c === CAN || c === SUB) {
          this.state = 'ground';
          out.push(c);
          return false;
        }
        out.push(c);
        if (c >= 0x40 && c <= 0x7e) this.state = 'ground';
        return false;
      }
      case 'osc': {
        if (c === BEL) {
          this.finishOsc(ctx);
          this.state = 'ground';
          this.osc = [];
          this.oscKind = 0;
          return false;
        }
        if (c === ESC) {
          this.state = 'oscEsc';
          return false;
        }
        if (c === CAN || c === SUB) {
          this.state = 'ground';
          this.osc = [];
          this.oscKind = 0;
          out.push(c);
          return false;
        }
        if (c < 0x20) {
          // Ignored C0 inside OSC (matches the strip judgement window).
          return false;
        }
        if (this.oscKind === 2) {
          out.push(c); // decided: other OSC, streaming verbatim
          return false;
        }
        if (this.oscKind === 3) {
          return false; // OSC 52 over cap: drop entirely until terminator
        }
        this.osc.push(c);
        if (this.oscKind === 0) {
          const s = String.fromCharCode(...this.osc);
          if (s === OSC52) {
            this.oscKind = 1;
          } else if (!OSC52.startsWith(s)) {
            this.oscKind = 2;
            out.push(ESC, 0x5d, ...this.osc);
            this.osc = [];
          } else if (this.osc.length > OSC_PREFIX_CAP) {
            this.oscKind = 2;
            out.push(ESC, 0x5d, ...this.osc);
            this.osc = [];
          }
          return false;
        }
        // oscKind === 1: OSC 52 collecting.
        if (this.osc.length > OSC52_CAP) {
          this.oscKind = 3;
          this.osc = [];
        }
        return false;
      }
      case 'oscEsc': {
        if (c === BS) {
          const kind = this.oscKind;
          this.finishOsc(ctx);
          // The ST belongs to the OSC: streamed OSCs carry it out, withheld
          // ones take it with them.
          if (kind === 2 || kind === 0) out.push(ESC, BS);
          this.state = 'ground';
          this.osc = [];
          this.oscKind = 0;
          return false;
        }
        // Non-ST ESC aborted the OSC. A forwarded (streamed) OSC needs a
        // CAN so the receiver leaves it; a withheld one's partial drops.
        if (this.oscKind === 2) out.push(CAN);
        this.state = 'esc';
        this.osc = [];
        this.oscKind = 0;
        return true; // reprocess c in the esc state
      }
      case 'str': {
        if (c === ESC) {
          this.state = 'strEsc';
          return false;
        }
        if (c === CAN || c === SUB) {
          this.state = 'ground';
          out.push(c);
          return false;
        }
        out.push(c);
        return false;
      }
      case 'strEsc': {
        if (c === BS) {
          out.push(ESC, BS);
          this.state = 'ground';
          return false;
        }
        // Forwarded DCS/APC/SOS/PM aborted by an ESC: CAN terminates the
        // string, then the byte is reprocessed as the new escape's input.
        this.state = 'esc';
        out.push(CAN);
        return true;
      }
    }
    return false;
  }

  private finishCsi(final: number, ctx: { out: number[]; clipboards: ClipboardEvent[]; headAtStart: number }): void {
    const body = this.csi;
    const prefix = body.length > 0 && (body[0] === 0x3f || body[0] === 0x3e || body[0] === 0x3c || body[0] === 0x3d) ? body[0] : 0;
    const start = prefix ? 1 : 0;
    // Params run until the first intermediate byte (0x20-0x2F); the rest of
    // the body (intermediates) is left out of the NUMERIC parse.
    let paramsEnd = body.length;
    for (let i = start; i < body.length; i++) {
      if (body[i] >= 0x20 && body[i] <= 0x2f) {
        paramsEnd = i;
        break;
      }
    }
    const paramStr = String.fromCharCode(...body.slice(start, paramsEnd));
    const tokens = paramStr === '' ? [] : paramStr.split(';');
    const nums = tokens.map((t) => Number(t));
    const hasMode = (set: Set<number>) => nums.some((n) => set.has(n));

    // Track alt screen + sync mode from the source stream (main and alt).
    if (prefix === 0x3f && (final === 0x68 || final === 0x6c)) {
      if (hasMode(ALT_MODES)) this.altScreen = final === 0x68;
      if (hasMode(new Set([SYNC_MODE]))) this.syncModeOn = final === 0x68;
    }

    const emit = (paramsOut: number[] | null, drop: boolean) => {
      if (drop) return;
      if (paramsOut === null) {
        ctx.out.push(ESC, 0x5b, ...body, final);
        return;
      }
      const head = paramsOut.length === 0 ? '' : `?${paramsOut.join(';')}`;
      ctx.out.push(...Buffer.from(`${ESC}[${head}${String.fromCharCode(final)}`, 'latin1'));
    };

    if (this.altScreen) {
      emit(null, false);
      return;
    }

    // Main screen: mouse enables off; kitty keyboard push/pop/set off.
    if (prefix === 0x3f && final === 0x68) {
      const kept = nums.filter((n) => !MOUSE_MODES.has(n));
      if (kept.length !== nums.length) {
        // Rebuild only when every param token is numeric, else keep verbatim.
        if (tokens.every((t) => t !== '' && Number.isInteger(Number(t)))) {
          if (kept.length === 0) return; // drop the sequence entirely
          ctx.out.push(...Buffer.from(`${ESC}[?${kept.join(';')}h`, 'latin1'));
          return;
        }
        emit(null, false);
        return;
      }
    }
    if ((prefix === 0x3e || prefix === 0x3c || prefix === 0x3d) && final === 0x75) {
      return; // kitty keyboard push/pop/set: withheld
    }
    emit(null, false);
  }

  private finishOsc(ctx: { out: number[]; clipboards: ClipboardEvent[]; headAtStart: number }): void {
    if (this.oscKind === 1) {
      const body = String.fromCharCode(...this.osc.slice(OSC52.length));
      const sep = body.indexOf(';');
      if (sep >= 0) {
        ctx.clipboards.push({ offset: ctx.headAtStart + ctx.out.length, data_b64: body.slice(sep + 1) });
      } else {
        // "52;" without a payload separator is not a set: pass it verbatim.
        ctx.out.push(ESC, 0x5d, ...this.osc);
      }
      return;
    }
    if (this.oscKind === 0) {
      // No terminator bytes beyond the decision window to hold for other
      // OSCs: they stream verbatim once decided, so a zero-length hold at
      // finish means the OSC never grew decided? It did: oscKind 0 with
      // content means the body is still a prefix of "52;" (e.g. "5") — it
      // passes verbatim as held prefix bytes.
      ctx.out.push(ESC, 0x5d, ...this.osc);
    }
    // oscKind 2 (streamed) and 3 (dropped) emit nothing at finish.
  }
}
