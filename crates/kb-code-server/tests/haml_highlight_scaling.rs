//! V77-P4 — the algorithmic-property CI gate for `crate::haml`'s scanner
//! highlighter.
//!
//! Background: the v7.6 large-repo measurement (a GitLab CE mirror) found
//! the HAML highlighter running ~46x slower PER BYTE than the Ruby
//! tree-sitter path, and because `reextract::build_bill` times every
//! language's sample against ONE shared 20s wall-clock budget
//! (`reextract.rs::SAMPLE_BUDGET`), HAML alone starved every language
//! sampled after it. The root cause was two O(n) "rescan from the start of
//! the file" line lookups — `haml::parser::line_index_at` (once per TAG
//! line) and `haml::extract::line_at` (once per Ruby fragment) — each
//! called once per something that scales WITH the file, making the whole
//! parse+highlight pass effectively O(n²).
//!
//! This test pins the fix as an ALGORITHMIC property rather than an
//! absolute number: quadrupling the input must not scale time by anything
//! close to SIXTEEN — a linear pass predicts ~4x, this asserts well under
//! 4x², and it is immune to the CI-hardware variance an absolute ns/byte
//! figure would carry (root CLAUDE.md's "Host build rules": this box is a
//! shared, IO-bound HDD RAID5). The literal ratio the v7.6 measurement
//! named (HAML vs. Ruby, ns/byte) is a SEPARATE, `#[ignore]`d bench —
//! `tests/measure/haml_highlight_bench.rs` — for exactly that reason.

use kb_code_server::haml;
use std::fmt::Write as _;
use std::time::{Duration, Instant};

fn push_haml_unit(out: &mut String, i: usize) {
    let _ = writeln!(
        out,
        "  .row-{i}#row-{i}.item.active{{data: {{index: {i}, name: \"row-item\", flag: true}}}}"
    );
    let _ = writeln!(out, "    - if item_{i}.visible?");
    let _ = writeln!(out, "      %p.desc= item_{i}.title");
    out.push_str("    - else\n");
    let _ = writeln!(out, "      %p.desc Hidden: #{{item_{i}.reason}}");
    out.push_str("    :javascript\n");
    let _ = writeln!(out, "      console.log(\"row {i} loaded\");");
    out.push_str("    %pre\n");
    out.push_str("      a very long string that keeps going here and |\n");
    out.push_str("      continues across two physical source lines end |\n");
}

/// A synthetic HAML file at least `target_bytes` long: nested tags with
/// classes/ids, `=`/`-` script lines, a Ruby attribute hash with
/// interpolation, a `:javascript` filter and a `|` multiline block — the
/// shapes V77-P4's brief named — repeated as SIBLINGS (not cumulative
/// nesting) so the fixture stays well-formed at any size and the repeat
/// count is the only thing that changes between the two sizes this test
/// compares.
fn synthetic_haml(target_bytes: usize) -> String {
    let mut out = String::with_capacity(target_bytes + 4096);
    out.push_str("%section.container\n");
    let mut i = 0usize;
    while out.len() < target_bytes {
        push_haml_unit(&mut out, i);
        i += 1;
    }
    out
}

/// Median wall-clock time of `iterations` calls to `f`, after one untimed
/// warmup (absorbs first-call allocator/page-fault noise so the median
/// reflects steady-state cost).
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

/// Quadrupling the file must not scale time anywhere near 4² — that is
/// exactly the shape an O(n²) "rescan from byte 0" regression produces.
/// Linear work predicts ~4x growth; the threshold below (`ratio_bytes² *
/// 0.5`, i.e. ~8x for a 4x size increase) leaves 2x headroom over pure
/// linear for allocator/cache noise while still failing hard on a
/// quadratic reintroduction.
#[test]
fn highlight_time_scales_linearly_not_quadratically() {
    let small = synthetic_haml(100_000);
    let big = synthetic_haml(400_000);
    // The generator only guarantees `>= target`; compare against the
    // ACTUAL byte ratio rather than assuming it hit the target exactly.
    let ratio_bytes = big.len() as f64 / small.len() as f64;
    let small_d = median_duration(|| drop(haml::highlights(small.as_bytes())), 9);
    let big_d = median_duration(|| drop(haml::highlights(big.as_bytes())), 9);
    let ratio_time = big_d.as_secs_f64() / small_d.as_secs_f64().max(1e-12);
    eprintln!(
        "haml highlight scaling: {} -> {} bytes ({ratio_bytes:.2}x), {small_d:?} -> {big_d:?} ({ratio_time:.2}x)",
        small.len(),
        big.len(),
    );
    assert!(
        ratio_time < ratio_bytes * ratio_bytes * 0.5,
        "highlight time scaled {ratio_time:.2}x for a {ratio_bytes:.2}x size increase — \
         that looks quadratic, not linear (small={small_d:?} big={big_d:?})"
    );
}
