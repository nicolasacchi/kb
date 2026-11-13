// T15 - the picture metadata the tap announces: `{id -> ImageDef}`.
//
// The tap decodes each kitty transmission once, stores the PNG under its
// sha256 and tells every viewer with an `imagedef` text frame (metadata
// only: the bytes come from the media route, see `media.ts`). A viewer
// never parses graphics APCs. This store is fed by those frames and read by
// the placeholder renderer.
//
// `version` counts distinct byte contents seen for an id (the renderer
// swaps an `<img>` src when it moves). The tap's `a=d` delete arrives as a
// `gfxreset` frame, which clears NOTHING here: the stored pictures are
// still addressable, and scrollback rows that name the id still draw.
// Only a FULL join (a fresh seed) empties the store; a resume never does.

import type { ImageDefFrame } from "../wire";

export interface ImageDef {
  id: number;
  gen: number;
  /** sha256 of the PNG bytes, lowercase hex (the media key). */
  sha: string;
  mime: string;
  /** Source pixel size. */
  w: number;
  h: number;
  /** The picture's box in cells (from the placement the tap recorded). */
  cols: number;
  rows: number;
  /** 1 for a new id; +1 each time the id returns with different bytes. */
  version: number;
}

export class ImageDefStore {
  private readonly byId = new Map<number, ImageDef>();
  private readonly listeners = new Set<() => void>();
  private resetCount = 0;
  private lastResetGen = 0;

  /**
   * Apply one `imagedef` frame. Returns true when the store changed.
   *
   * Same bytes (same sha) and same box: an identical retransmit, no change,
   * no event, no version bump. Same bytes with a new box (the tap re-sent
   * the placement): the box is updated and listeners fire, the version
   * stays (the picture itself did not change). Different bytes or a new
   * id: `version` becomes `old.version + 1` (or 1) and listeners fire.
   */
  apply(frame: ImageDefFrame): boolean {
    const old = this.byId.get(frame.id);
    if (old !== undefined && old.sha === frame.sha256) {
      const same =
        old.cols === frame.cols &&
        old.rows === frame.rows &&
        old.w === frame.w &&
        old.h === frame.h &&
        old.mime === frame.mime;
      if (same) return false;
      this.byId.set(frame.id, {
        ...old,
        gen: frame.gen,
        mime: frame.mime,
        w: frame.w,
        h: frame.h,
        cols: frame.cols,
        rows: frame.rows,
      });
      this.emit();
      return true;
    }
    this.byId.set(frame.id, {
      id: frame.id,
      gen: frame.gen,
      sha: frame.sha256,
      mime: frame.mime,
      w: frame.w,
      h: frame.h,
      cols: frame.cols,
      rows: frame.rows,
      version: old === undefined ? 1 : old.version + 1,
    });
    this.emit();
    return true;
  }

  /** A `gfxreset` frame: counted, and it clears nothing. */
  gfxReset(gen: number): void {
    this.resetCount += 1;
    this.lastResetGen = gen;
  }

  get resets(): number {
    return this.resetCount;
  }

  get resetGen(): number {
    return this.lastResetGen;
  }

  get(id: number): ImageDef | undefined {
    return this.byId.get(id);
  }

  all(): ImageDef[] {
    return [...this.byId.values()].sort((a, b) => a.id - b.id);
  }

  get size(): number {
    return this.byId.size;
  }

  /** FULL join only (a fresh seed re-announces every picture). Never on a resume. */
  clear(): void {
    if (this.byId.size === 0) return;
    this.byId.clear();
    this.emit();
  }

  onChange(fn: () => void): () => void {
    this.listeners.add(fn);
    return () => {
      this.listeners.delete(fn);
    };
  }

  private emit(): void {
    for (const fn of [...this.listeners]) fn();
  }
}
