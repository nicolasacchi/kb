//! V77-P4 measurement — the literal ratio the v7.6 large-repo run (a
//! GitLab CE mirror) named: `crate::haml`'s scanner highlighter, timed
//! PER BYTE against the Ruby tree-sitter highlighter it was compared to.
//! Mirrors `tests/measure/occurrences_bench.rs`'s posture exactly: an
//! `#[ignore]`d bench a human reads, not a routine CI assertion — an
//! absolute ns/byte figure across two different grammars is precisely the
//! kind of number a shared, IO-bound box (root CLAUDE.md's "Host build
//! rules") makes noisy, which is why the ALWAYS-ON CI gate for this fix is
//! the separate, self-relative `tests/haml_highlight_scaling.rs` instead.
//!
//! Run explicitly for a before/after report:
//! `cargo test -p kb-code-server --test measure haml_highlight_bench -- --ignored --nocapture`
//!
//! `KB_CODE_LAT_MULT` (same variable `tests/measure/latency.rs` reads,
//! default `1`) widens the pass/fail threshold on a loaded box rather than
//! this file inventing its own knob.

use kb_code_server::haml;
use kb_code_server::highlight;
use std::fmt::Write as _;
use std::time::{Duration, Instant};

fn lat_mult() -> f64 {
    std::env::var("KB_CODE_LAT_MULT")
        .ok()
        .and_then(|s| s.parse::<f64>().ok())
        .filter(|m| *m > 0.0)
        .unwrap_or(1.0)
}

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

/// A synthetic ~200 KB HAML file: nested tags with classes/ids, `=`/`-`
/// script lines, a Ruby attribute hash with interpolation, a `:javascript`
/// filter, and a `|` multiline block — the shapes V77-P4's brief named,
/// repeated as siblings so the fixture stays well-formed at any size.
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

fn push_ruby_unit(out: &mut String, i: usize) {
    let _ = writeln!(out, "def method_{i}(arg)");
    out.push_str("  if arg.valid?\n");
    out.push_str("    puts \"processing #{arg.name}\"\n");
    out.push_str("  else\n");
    out.push_str("    raise ArgumentError, \"invalid: #{arg.inspect}\"\n");
    out.push_str("  end\n");
    let _ = writeln!(
        out,
        "  data = {{ index: {i}, name: \"row-item\", flag: true }}"
    );
    out.push_str("  data.each do |k, v|\n");
    out.push_str("    puts \"#{k}: #{v}\"\n");
    out.push_str("  end\n");
    out.push_str("end\n");
}

/// A synthetic Ruby file, same density-of-punctuation spirit as
/// [`synthetic_haml`] (method defs, string interpolation, a hash literal,
/// a block) — the comparison the v7.6 measurement made.
fn synthetic_ruby(target_bytes: usize) -> String {
    let mut out = String::with_capacity(target_bytes + 4096);
    let mut i = 0usize;
    while out.len() < target_bytes {
        push_ruby_unit(&mut out, i);
        i += 1;
    }
    out
}

/// Median wall-clock time of `iterations` calls to `f`, after one untimed
/// warmup.
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

fn ns_per_byte(d: Duration, bytes: usize) -> f64 {
    d.as_secs_f64() * 1e9 / bytes as f64
}

/// The literal ratio the v7.6 measurement named. Threshold has real
/// headroom (8x, scaled further by `KB_CODE_LAT_MULT`) over what V77-P4
/// measured after the fix on the box it was pinned on — see the unit's
/// REPORT for the exact before/after figures.
#[test]
#[ignore = "measure lane — run with --ignored (KB_CODE_LAT_MULT widens the threshold)"]
fn haml_highlighter_is_within_headroom_of_ruby_per_byte() {
    let haml_src = synthetic_haml(200_000);
    let ruby_src = synthetic_ruby(200_000);

    let haml_d = median_duration(|| drop(haml::highlights(haml_src.as_bytes())), 7);
    let ruby_d = median_duration(
        || {
            drop(highlight::extract_highlights_host_only(
                "ruby",
                ruby_src.as_bytes(),
            ))
        },
        7,
    );
    let haml_ns = ns_per_byte(haml_d, haml_src.len());
    let ruby_ns = ns_per_byte(ruby_d, ruby_src.len());
    let ratio = haml_ns / ruby_ns.max(1e-9);
    let threshold = 8.0 * lat_mult();
    eprintln!(
        "haml={haml_ns:.1} ns/byte ({} bytes, {haml_d:?})  ruby={ruby_ns:.1} ns/byte ({} bytes, {ruby_d:?})  ratio={ratio:.2}x  threshold={threshold:.2}x",
        haml_src.len(),
        ruby_src.len(),
    );
    assert!(
        ratio < threshold,
        "HAML highlighter is {ratio:.2}x slower per byte than Ruby's tree-sitter highlighter \
         (haml={haml_ns:.1} ns/byte, ruby={ruby_ns:.1} ns/byte, threshold={threshold:.2}x) — \
         see V77-P4's REPORT for the before/after this was pinned against"
    );
}
