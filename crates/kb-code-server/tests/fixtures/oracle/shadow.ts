// Oracle fixture: param/block/function-hoist shadowing traps (TypeScript).

const value = 1;

function run(value: number): number {
  // ref `value` → param (not module const)
  return value;
}

function blockShadow(): void {
  let x = 10;
  {
    let x = 20;
    // ref `x` → inner let
    const y = x;
  }
}

function hoistCall(): number {
  // function decl is hoisted in scope → later()
  return later();
  function later(): number {
    return 1;
  }
}

function letNotBefore(): void {
  // `later` is a let — NOT visible before declaration → unbound
  const y = later;
  let later = 1;
}

function unboundUse(): void {
  const m = missingName;
}
