// Decoy: same function name as paint_circle but unrelated file/module.
// Must NOT appear as a likely+ caller/callee target for the real paint_circle.

pub fn paint_circle() {
    // decoy body
}

pub fn caller_of_decoy() {
    paint_circle();
}
