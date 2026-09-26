//! V77-P4b — the algorithmic-property CI gate for `haml::extract::outline`'s
//! per-node last-line computation.
//!
//! Background: P4 (`tests/haml_highlight_scaling.rs`) made the HAML
//! HIGHLIGHTER linear and recorded, out of scope for that unit, that
//! `haml::outline`'s `subtree_last_line` walked a node's entire subtree
//! FROM SCRATCH once per ANCESTOR — `outline` calls it once per
//! element/filter row, so a node sitting under `k` enclosing tags/filters
//! had its own descendants re-walked by all `k` of THEIR calls. On a deep
//! or long LINEAR nesting chain that is O(n²): quadrupling the nesting
//! depth quadruples both the node count and the per-node work.
//!
//! P4's own highlight fixture (`tests/haml_highlight_scaling.rs`) is
//! SIBLINGS, deliberately: many independent, shallow `.row-N` subtrees at
//! the top level, which keeps the highlighter's per-byte cost visible
//! without needing deep nesting. That fixture would NOT exercise this
//! bug — each sibling's own subtree stays small and constant-depth
//! regardless of how many siblings there are, so the old
//! `subtree_last_line` never revisited a large shared subtree. This test
//! needs the opposite shape: ONE long chain of nested tags, which is what
//! makes a node near the bottom sit under a growing number of ancestors.
//!
//! Like P4's test, this pins the fix as an ALGORITHMIC property (well
//! under a 4x² scale-up for a 4x size increase) rather than an absolute
//! number, for the same CI-hardware-variance reason (root CLAUDE.md's
//! "Host build rules": a shared, IO-bound HDD RAID5 box).

use kb_code_server::haml;
use std::fmt::Write as _;
use std::time::{Duration, Instant};

/// A single, deep, LINEAR nesting chain of `depth` tags — never siblings —
/// each one level inside the last, ending in a leaf paragraph. Every level
/// carries a class, an id and a small Ruby attribute hash so the fixture
/// stays "fragment-dense" the way the brief asks, not just deep. Stays
/// comfortably under `parser::MAX_DEPTH` (256) at every size this test
/// uses — past that cap deeper lines stop opening new levels at all, which
/// would silently flatten the fixture back into the shape this test must
/// NOT be.
fn synthetic_haml_nested(depth: usize) -> String {
    let mut out = String::with_capacity(depth * 64 + 64);
    for i in 0..depth {
        let indent = "  ".repeat(i);
        let _ = writeln!(
            out,
            "{indent}%div.level-{i}#node-{i}{{data: {{index: {i}, tag: \"n{i}\"}}}}"
        );
    }
    let indent = "  ".repeat(depth);
    let _ = writeln!(out, "{indent}%p.leaf= \"Hello, \" + name");
    out
}

/// Median wall-clock time of `iterations` calls to `f`, after one untimed
/// warmup (absorbs first-call allocator/page-fault noise so the median
/// reflects steady-state cost) — identical shape to
/// `haml_highlight_scaling.rs`'s own helper.
fn median_duration<F: FnMut()>(mut f: F, iterations: usize) -> Duration {
    f();
    let mut samples: Vec<Duration> = (0..iterations)
        .map(|_| {
            let t = Instant::now();
            f();
            t.elapsed()
        })
        .collect();
    samples.sort();
    samples[samples.len() / 2]
}

/// Quadrupling the nesting depth (and, with it, the file size — each level
/// contributes one near-constant-length line) must not scale time anywhere
/// near 4² — exactly the shape the old per-ancestor subtree revisit
/// produced. Linear-ish work (`compute_last_lines` is O(n log n) via
/// `LineIndex`, not strictly O(n), since each node still pays one binary
/// search) predicts well under 4x growth; the threshold below (`ratio² *
/// 0.5`, ~8x for a 4x increase) leaves generous headroom over that while
/// still failing hard on a quadratic reintroduction (which would land near
/// 16x).
#[test]
fn outline_time_scales_linearly_not_quadratically() {
    let small = synthetic_haml_nested(60);
    let big = synthetic_haml_nested(240);
    // The generator's line count is exact (unlike the highlight test's
    // byte-target loop), so compare against the ACTUAL byte ratio anyway —
    // digit-width growth in `level-{i}`/`node-{i}`/`index: {i}` means it is
    // not bit-for-bit 4.00x.
    let ratio_bytes = big.len() as f64 / small.len() as f64;
    let small_d = median_duration(|| drop(haml::outline(small.as_bytes())), 21);
    let big_d = median_duration(|| drop(haml::outline(big.as_bytes())), 21);
    let ratio_time = big_d.as_secs_f64() / small_d.as_secs_f64().max(1e-12);
    eprintln!(
        "haml outline scaling: {} -> {} bytes ({ratio_bytes:.2}x), {small_d:?} -> {big_d:?} ({ratio_time:.2}x)",
        small.len(),
        big.len(),
    );
    assert!(
        ratio_time < ratio_bytes * ratio_bytes * 0.5,
        "outline time scaled {ratio_time:.2}x for a {ratio_bytes:.2}x nesting-depth increase — \
         that looks quadratic, not linear (small={small_d:?} big={big_d:?})"
    );
}
