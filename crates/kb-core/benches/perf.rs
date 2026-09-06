//! Criterion microbenchmarks for the hot paths touched by the perf
//! review. Seeded with `parser::extract` (the HTML field/link extraction
//! that runs on every index) — it directly exercises the single-pass
//! `<script>` consolidation. Run with `cargo bench -p kb-core`.
//!
//! Before/after for a committed change: `git stash` is not enough (the
//! bench file is new), so to compare against the pre-sweep parser, run
//! this bench, then `git checkout main -- crates/kb-core/src/parser.rs`,
//! re-run, and diff the reported times.

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use kb_core::{links, parser};

/// Representative artifact: real heading hierarchy, tag/category metas,
/// inline + external scripts (exercises the has_script + js_total single
/// pass), styles with @keyframes, a handful of anchors (artifact-subdomain,
/// SPA permalink, relative, external), tables, code blocks, and enough
/// prose to make body-text extraction non-trivial.
fn sample_html() -> String {
    let mut body = String::from(
        r##"<!doctype html><html><head>
<title>Atlas determinism deep-dive</title>
<meta name="kb-tags" content="rust, atlas, lance, perf">
<meta name="kb-category" content="notes">
<meta name="kb-status" content="reviewed">
<style>body{font-family:system-ui}@keyframes fade{from{opacity:0}to{opacity:1}}</style>
</head><body>
<h1>Atlas determinism</h1>
<p>The layout path is IEEE-754 only, no transcendentals.</p>
<h2>Background</h2>
<p>UMAP over a KNN graph, then k-means, then PCA fallback.</p>
<a href="https://abc123def456.artifacts.localhost/">subdomain link</a>
<a href="/a/research/deadbeefcafe">permalink</a>
<a href="./sibling.html">relative</a>
<a href="https://example.com/external">external</a>
<a href="#anchor">in-doc</a>
<pre><code>fn compute_layout(emb: &[f32]) -> Vec<(f32,f32)> { todo!() }</code></pre>
<table><tr><td>a</td><td>b</td></tr></table>
<canvas></canvas>
"##,
    );
    // Pad the body so word/excerpt extraction has real work to do.
    for i in 0..300 {
        body.push_str(&format!(
            "<p>Paragraph {i} with several words of prose content here.</p>"
        ));
    }
    body.push_str(
        r##"<script>console.log('inline one')</script>
<script src="/static/app.js"></script>
<script>document.querySelector('h1')</script>
</body></html>"##,
    );
    body
}

fn bench_extract(c: &mut Criterion) {
    let html = sample_html();
    c.bench_function("parser::extract", |b| {
        b.iter(|| parser::extract(black_box(&html)))
    });
}

fn bench_extract_links(c: &mut Criterion) {
    let html = sample_html();
    let suffix = ".artifacts.localhost";
    // The single-parse combined extractor vs. the two-parse pair, so the
    // indexer's link-extraction speedup is measurable directly.
    c.bench_function("parser::extract_links_and_hrefs", |b| {
        b.iter(|| parser::extract_links_and_hrefs(black_box(&html), black_box(suffix)))
    });
    c.bench_function("parser::extract_links+hrefs (two parses)", |b| {
        b.iter(|| {
            let l = parser::extract_links(black_box(&html), black_box(suffix));
            let h = parser::extract_link_hrefs(black_box(&html));
            (l, h)
        })
    });
}

/// Synthetic notes corpus for the wikilink resolve path: N docs across a
/// few folders, some sharing basename shapes (tier 4) and short titles.
fn sample_corpus(n: usize) -> Vec<links::DocLite> {
    (0..n)
        .map(|i| links::DocLite {
            id: format!("{i:012x}"),
            rel_path: format!("folder{}/note-{i}.md", i % 7),
            title: format!("Note {} about topic {}", i, i % 97),
        })
        .collect()
}

fn bench_wikilink_resolve(c: &mut Criterion) {
    let docs = sample_corpus(2000);
    // A realistic per-note target mix: ids, folder-qualified paths (ext
    // elided), exact titles, bare basenames, and danglers.
    let targets: Vec<String> = (0..50)
        .map(|i| match i % 5 {
            0 => format!("{:012x}", i * 31),
            1 => format!("folder{}/note-{}", (i * 13) % 7, i * 13),
            2 => format!("Note {} about topic {}", i * 7, (i * 7) % 97),
            3 => format!("note-{}", i * 11),
            _ => format!("dangling-target-{i}"),
        })
        .collect();
    // The edge-record hook's shape: ONE index build per note, then every
    // target is hashmap work.
    c.bench_function("links::ResolveIndex build+50 targets (2k docs)", |b| {
        b.iter(|| {
            let idx = links::ResolveIndex::new(black_box(&docs));
            for t in &targets {
                black_box(idx.resolve(t));
            }
        })
    });
    // The one-shot form (single route-side lookups) for comparison.
    c.bench_function("links::resolve one-shot (2k docs)", |b| {
        b.iter(|| links::resolve(black_box(&targets[1]), black_box(&docs)))
    });
}

criterion_group!(
    benches,
    bench_extract,
    bench_extract_links,
    bench_wikilink_resolve
);
criterion_main!(benches);
