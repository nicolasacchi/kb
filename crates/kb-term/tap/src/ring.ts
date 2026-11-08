// kb-term tap: the 32 MiB byte ring with absolute-offset numbering and
// marker sequence numbers. Zero dependencies.
//
// Every viewer-visible byte has one absolute offset (position in the
// post-transform stream) within one epoch. The ring keeps the last
// `capBytes` bytes over a single preallocated buffer so a reconnecting
// viewer receives only what it missed (ResumeFrom). Picture bytes are NEVER
// in the ring: IIP/SIXEL appear as T4 stub bytes plus a marker referencing
// the sha in the image store.

/** Ordered control marker: the effect applies AFTER all stream bytes < offset. */
export interface RingMarker {
  seq: number;
  offset: number;
  frame: string;
}

export const DEFAULT_CAP_BYTES: number = 32 * 1024 * 1024;

export class Ring {
  readonly cap: number;
  /** Absolute offset of the next byte to be appended. Starts at 0 per epoch. */
  head = 0;
  /** Number of backing-buffer allocations; always 1 (set at construction). */
  allocCount = 1;
  private buf: Uint8Array;
  private markers: RingMarker[] = [];
  private lastSeq = 0;

  constructor(capBytes: number = DEFAULT_CAP_BYTES) {
    if (!Number.isInteger(capBytes) || capBytes <= 0) {
      throw new RangeError(`bad cap bytes: ${String(capBytes)}`);
    }
    this.cap = capBytes;
    this.buf = new Uint8Array(capBytes);
    this.allocCount = 1;
  }

  /** Absolute offset of the oldest retained byte. */
  get tail(): number {
    return Math.max(0, this.head - this.cap);
  }

  /**
   * Append bytes; advance head; return the offset of the first appended byte.
   * A chunk larger than the cap keeps only its last `cap` bytes (head still
   * advances by the full length).
   */
  push(bytes: Uint8Array): number {
    const start = this.head;
    const n = bytes.length;
    this.head += n;
    if (n >= this.cap) {
      this.writeAt(this.head - this.cap, bytes.subarray(n - this.cap));
    } else if (n > 0) {
      this.writeAt(start, bytes);
    }
    this.pruneMarkers();
    return start;
  }

  /**
   * Copy bytes [from, min(head, from+maxLen)) across the wrap.
   * Returns null when `from < tail` (aged out). `from > head` is a caller
   * error the resume path handles before calling; defensively an empty
   * array is returned rather than throwing inside the fan-out loop.
   */
  read(from: number, maxLen: number): Uint8Array | null {
    if (from < this.tail) return null;
    if (from >= this.head || maxLen <= 0) return new Uint8Array(0);
    const n = Math.min(maxLen, this.head - from);
    const out = new Uint8Array(n);
    let copied = 0;
    let pos = from % this.cap;
    while (copied < n) {
      const take = Math.min(n - copied, this.cap - pos);
      out.set(this.buf.subarray(pos, pos + take), copied);
      copied += take;
      pos = 0;
    }
    return out;
  }

  /**
   * Assign the next marker seq, store {seq, offset, frame}, return the seq.
   * The builder solves the chicken-egg: the frame embeds the seq the ring
   * assigns, and the offset is always the current head.
   */
  addMarker(build: (seq: number, offset: number) => string): number {
    const seq = ++this.lastSeq;
    this.markers.push({ seq, offset: this.head, frame: build(seq, this.head) });
    return seq;
  }

  /** Markers with seq > mseq, in seq order (aging list is retained order). */
  markersAfter(mseq: number): RingMarker[] {
    return this.markers.filter((m) => m.seq > mseq);
  }

  private writeAt(offset: number, data: Uint8Array): void {
    let pos = offset % this.cap;
    let i = 0;
    while (i < data.length) {
      const take = Math.min(data.length - i, this.cap - pos);
      this.buf.set(data.subarray(i, i + take), pos);
      i += take;
      pos = 0;
    }
  }

  private pruneMarkers(): void {
    const tail = this.tail;
    let first = 0;
    while (first < this.markers.length && this.markers[first].offset < tail) first++;
    if (first > 0) this.markers.splice(0, first);
  }
}
