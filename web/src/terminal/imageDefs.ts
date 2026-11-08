// T14 — the kitty `imagedef` store: `{id -> def}` fed by `onImageDef`
// frames (T15 draws from this map). Later defs replace earlier ones.

import type { ImageDefFrame } from "./wire";

export type ImageDef = ImageDefFrame;

export class ImageDefStore {
  private readonly byId = new Map<number, ImageDef>();

  put(def: ImageDef): void {
    this.byId.set(def.id, def);
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

  clear(): void {
    this.byId.clear();
  }
}
