// Oracle fixture: closures capturing outer bindings (Rust).

fn outer_capture() {
    let outer = 10;
    let f = || {
        // ref `outer` → outer let
        outer
    };
    let _ = f;
}

fn closure_param_shadow() {
    let n = 1;
    let g = |n| {
        // ref `n` → closure param (not outer let)
        n
    };
    let _ = g(2);
}

fn nested_closure() {
    let a = 1;
    let h = || {
        let b = 2;
        let i = || {
            // ref `a` → outermost let; ref `b` → middle let
            a + b
        };
        let _ = i;
    };
    let _ = h;
}

fn call_helper() {
    // same-file fn call — locals exact to the fn def
    helper();
}

fn helper() {}
