// Oracle fixture: arrow params + block shadowing (TSX).

const outer = "out";

export const Box = (label: string) => {
  // ref `label` → arrow param
  return label;
};

export const Shadow = (outer: string) => {
  // ref `outer` → param (not module const)
  return outer;
};

export function nested(): number {
  const n = 1;
  const f = (n: number) => {
    // ref `n` → arrow param
    return n;
  };
  return f(2);
}

export function blockLet(): number {
  let x = 1;
  {
    let x = 2;
    // ref `x` → inner
    return x;
  }
}

export function unbound(): string {
  return missingTsx;
}
