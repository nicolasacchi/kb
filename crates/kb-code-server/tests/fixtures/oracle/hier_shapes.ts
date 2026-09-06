/** Hierarchy oracle: class extends + implements + interface-typed call. */

export interface Drawable {
  draw(): void;
}

export class Shape {
  area(): number {
    return 0;
  }
}

export class Circle extends Shape implements Drawable {
  draw(): void {
    this.area();
  }
}

export function useConcrete(c: Circle): void {
  c.draw();
  c.area();
}

export function useInterface(d: Drawable): void {
  d.draw();
}
