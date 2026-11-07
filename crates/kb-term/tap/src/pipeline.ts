// kb-term tap: the tap pipeline glue. Zero dependencies.
//
// Feed raw tmux control-mode bytes; get viewer-bound pane bytes (query-stripped) and
// control events back. One QueryStripper per pane id; pane streams never share parser
// state. `%output` / `%extended-output` data go through that pane's stripper. Every other
// event (pause, continue, exit, begin, end, error, other) is passed through unchanged and
// does NOT flush or reset the stripper. `reply` lines are NOT stripped and NEVER enter a
// pane stream: they belong to the command issuer.

import { ControlDecoder, type CtlEvent } from './decode.ts';
import { QueryStripper } from './strip.ts';

export type Out = { pane: string; bytes: Uint8Array } | { event: CtlEvent };

export class TapPipeline {
  private dec = new ControlDecoder();
  private strips = new Map<string, QueryStripper>();
  private onStrip?: (pane: string, label: string, raw: Uint8Array) => void;

  constructor(opts?: { onStrip?: (pane: string, label: string, raw: Uint8Array) => void }) {
    this.onStrip = opts?.onStrip;
  }

  feed(chunk: Uint8Array): Out[] {
    const out: Out[] = [];
    for (const ev of this.dec.push(chunk)) {
      if (ev.kind === 'output' || ev.kind === 'extended-output') {
        let s = this.strips.get(ev.pane);
        if (!s) {
          s = new QueryStripper();
          const pane = ev.pane;
          if (this.onStrip) s.onStrip = (raw: Uint8Array, label: string) => this.onStrip?.(pane, label, raw);
          this.strips.set(pane, s);
        }
        out.push({ pane: ev.pane, bytes: s.push(ev.data) });
      } else {
        out.push({ event: ev });
      }
    }
    return out;
  }

  /** end of stream for one/all panes: undecided bytes are dropped, never emitted. */
  flush(pane?: string): void {
    if (pane !== undefined) {
      this.strips.get(pane)?.flush();
      return;
    }
    for (const s of this.strips.values()) s.flush();
  }

  /** held-back bytes for one pane (T5's quiescence logic). */
  pending(pane: string): number {
    return this.strips.get(pane)?.pending() ?? 0;
  }
}
