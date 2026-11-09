// kb-term tap: ground tracker for the quiescent-join rule (T6).
//
// Pure byte-at-a-time tracker over the POST-transform viewer stream (the bytes
// the twin parses). `atGround()` reports whether the parser sits between
// complete byte sequences: not inside ESC, CSI, OSC, DCS, APC, PM or SOS, and
// not inside an incomplete UTF-8 sequence.
//
// Semantics mirror the xterm.js parser:
// - String states (OSC/DCS/APC/PM/SOS) end on BEL (OSC only), ST (ESC '\'),
//   or CAN/SUB (0x18/0x1A). An ESC inside a string begins a new introducer
//   (xterm.js dispatches the truncated string; so do we -- the ESC simply
//   moves us to the esc state, where the next byte decides CSI/OSC/...).
// - C0 controls inside a CSI/string execute immediately and do not leave the
//   state (except CAN/SUB, which abort to ground). DEL is ignored.
// - C1 single-byte controls (0x80-0x9F standing alone) are NOT treated as
//   introducers: the stream is UTF-8, so such a byte is either a UTF-8
//   continuation or garbage ground. Documented deviation from a C1-aware
//   parser; quiescent points in this project are LF/frame based, never C1.
// - Incomplete UTF-8: a lead byte arms 1-3 continuation bytes. An ESC (or any
//   non-continuation) abandons the partial sequence -- ground can only be
//   reached again via the normal state machine. Pure tracker: no imports,
//   and deliberately independent of the T3 stripper.
type State = 'ground' | 'esc' | 'csi' | 'osc' | 'dcs' | 'apc' | 'pm' | 'sos';

const ESC = 0x1b;
const BEL = 0x07;
const CAN = 0x18;
const SUB = 0x1a;

export class GroundTracker {
  private state: State = 'ground';
  private utf8Pending = 0;
  private total = 0;

  push(bytes: Uint8Array): void {
    for (let i = 0; i < bytes.length; i++) this.step(bytes[i]);
    this.total += bytes.length;
  }

  atGround(): boolean {
    return this.state === 'ground' && this.utf8Pending === 0;
  }

  /** Total bytes pushed so far. */
  offset(): number {
    return this.total;
  }

  /** Forget all tracking: back to ground with zero bytes seen. Reset on a
   * reseed so pre-reset mid-sequence state cannot leak across
   * twin.reset() (the twin starts from the capture, not the old stream). */
  reset(): void {
    this.state = 'ground';
    this.utf8Pending = 0;
    this.total = 0;
  }

  private step(b: number): void {
    // UTF-8 continuation accounting (applies in ground state; escape states
    // never carry UTF-8 continuations because ESC abandons a partial one).
    if (this.utf8Pending > 0) {
      if (b >= 0x80 && b <= 0xbf) {
        this.utf8Pending--;
        return;
      }
      // Invalid continuation: abandon the partial sequence and re-process
      // this byte in the state machine.
      this.utf8Pending = 0;
    }
    switch (this.state) {
      case 'ground': {
        if (b === ESC) {
          this.state = 'esc';
          return;
        }
        if (b >= 0xc2 && b <= 0xdf) this.utf8Pending = 1;
        else if (b >= 0xe0 && b <= 0xef) this.utf8Pending = 2;
        else if (b >= 0xf0 && b <= 0xf4) this.utf8Pending = 3;
        // 0x80..0xBF standalone (C1/continuation without a lead), 0xF5..0xFF
        // and C0 bytes: not introducers; stay at ground.
        return;
      }
      case 'esc': {
        if (b === ESC) return; // re-ESC: another introducer starts
        if (b === 0x5b) { this.state = 'csi'; return; }  // [
        if (b === 0x5d) { this.state = 'osc'; return; }  // ]
        if (b === 0x50) { this.state = 'dcs'; return; }  // P
        if (b === 0x5f) { this.state = 'apc'; return; }  // _
        if (b === 0x58) { this.state = 'sos'; return; }  // X
        if (b === 0x5e) { this.state = 'pm'; return; }   // ^
        if (b === CAN || b === SUB) { this.state = 'ground'; return; }
        // Intermediates (0x20..0x2F, e.g. ESC ( B charset designation) and
        // C0/DEL continue the escape; the final byte 0x30..0x7E ends it.
        if (b < 0x30 || b === 0x7f) return;
        this.state = 'ground';
        return;
      }
      case 'csi': {
        if (b === ESC) { this.state = 'esc'; return; }
        if (b === CAN || b === SUB) { this.state = 'ground'; return; }
        // Params/intermediates 0x20..0x3F stay; final 0x40..0x7E ends.
        if (b >= 0x40 && b <= 0x7e) this.state = 'ground';
        return;
      }
      case 'osc': {
        if (b === BEL || b === CAN || b === SUB) { this.state = 'ground'; return; }
        if (b === ESC) { this.state = 'esc'; return; } // new introducer
        return;
      }
      case 'dcs':
      case 'apc':
      case 'pm':
      case 'sos': {
        if (b === CAN || b === SUB) { this.state = 'ground'; return; }
        if (b === ESC) { this.state = 'esc'; return; } // new introducer (incl. ST)
        return;
      }
    }
  }
}
