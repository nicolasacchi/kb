// Oracle fixture: param/block/loop shadowing traps (Rust).
// Dense same-name bindings; golden rows pin expected def sites.

fn value() -> i32 {
    1
}

fn run(value: i32) -> i32 {
    // ref `value` → param on this fn (not module fn)
    value
}

fn block_shadow() {
    let x = 10;
    {
        let x = 20;
        // ref `x` → inner let
        let _y = x;
    }
}

fn loop_shadow(item: i32) {
    for item in 0..3 {
        // ref `item` → loop binding (not param)
        let _z = item;
    }
    // ref `item` after loop → param (for-scope does not leak under our model)
    let _w = item;
}

fn unbound_use() {
    // deliberately unbound
    let _m = missing_name;
}
