//! S-milestone S8 — `kb synth` generates a directory of synthetic
//! HTML artifacts for stress-testing the daemon against large corpora.
//! The kb at HEAD has 288 docs; without this command there's no
//! reproducible way to test the scale-mode pipeline at 50k.
//!
//! The output directory has the shape the indexer expects: realistic
//! folder fan-out (changelog/, ideas/, incidents/, pm/, ...), one
//! HTML file per artifact, each carrying `<title>`, a `<meta
//! name="kb-tags">`, occasional `<meta name="kb-status">` /
//! `kb-severity`, and varied word counts so capability/longread
//! flags split realistically.
//!
//! Determinism: a `--seed` makes the generated corpus byte-identical
//! across runs, so a benchmark suite can rebuild without invalidating
//! lance indices.
//!
//! Usage:
//!   kb synth --docs 50000 --out /tmp/synth-50k
//! After: point a daemon at the output dir via kb.toml and let the
//! indexer ingest.

use anyhow::{Context, Result};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

const FOLDERS: &[(&str, u32)] = &[
    // (folder, weight)
    ("changelog/daily", 28),
    ("ideas", 20),
    ("incidents/active", 7),
    ("incidents/resolved", 8),
    ("pm/initiatives", 5),
    ("pm", 3),
    ("guides", 5),
    ("docs/api", 5),
    ("docs/ops", 3),
    ("features", 6),
    ("runbooks", 4),
    ("decisions", 3),
    ("notes", 3),
];

const TAG_VOCAB: &[&str] = &[
    "rust",
    "python",
    "ts",
    "react",
    "axum",
    "lance",
    "sqlite",
    "atlas",
    "rails",
    "aws",
    "gcp",
    "k8s",
    "docker",
    "ci",
    "ops",
    "perf",
    "auth",
    "search",
    "embedding",
    "ux",
    "ci-cd",
    "infra",
    "ml",
    "data",
    "api",
    "frontend",
    "backend",
    "deploy",
    "alerting",
    "metrics",
    "logging",
    "tracing",
    "caching",
    "queue",
    "stream",
    "billing",
    "checkout",
    "catalog",
];

const STATUSES: &[&str] = &["draft", "in-progress", "done", "archived", "stalled"];
const SEVERITIES: &[&str] = &["info", "warning", "error", "critical"];

pub fn run(docs: u32, out: PathBuf, seed: u64) -> Result<()> {
    fs::create_dir_all(&out).with_context(|| format!("create out dir {}", out.display()))?;

    let total_weight: u32 = FOLDERS.iter().map(|(_, w)| w).sum();
    let mut rng = SplitMix64 { state: seed };

    // Per-folder running counters so within-folder filenames stay unique
    // and predictable.
    let mut per_folder: std::collections::HashMap<&str, u32> =
        FOLDERS.iter().map(|(f, _)| (*f, 0u32)).collect();

    let started = std::time::Instant::now();
    let mut last_report = std::time::Instant::now();

    for i in 0..docs {
        let folder = pick_weighted(&mut rng, total_weight);
        let n = per_folder.entry(folder).or_insert(0);
        *n += 1;
        let dir = out.join(folder);
        fs::create_dir_all(&dir).with_context(|| format!("create folder {}", dir.display()))?;

        let filename = format!("{:05}-{}.html", *n, slug_from_index(*n));
        let path = dir.join(&filename);
        write_artifact(&path, &mut rng, folder, i)?;

        if last_report.elapsed() >= std::time::Duration::from_secs(2) {
            let pct = ((i + 1) as f64 / docs as f64) * 100.0;
            eprintln!("synth: {} / {} ({:.1}%)", i + 1, docs, pct,);
            last_report = std::time::Instant::now();
        }
    }

    let elapsed = started.elapsed();
    println!(
        "wrote {docs} artifacts to {} in {:.1}s",
        out.display(),
        elapsed.as_secs_f64(),
    );
    println!("add to your kb.toml:");
    println!();
    println!("    [kb.synth]");
    println!("    path = \"{}\"", out.display());
    Ok(())
}

fn write_artifact(path: &Path, rng: &mut SplitMix64, folder: &str, idx: u32) -> Result<()> {
    let title = format!("{} note {idx}", title_case(folder));
    let n_tags = 1 + (rng.next() % 3) as usize;
    let tags: Vec<&str> = (0..n_tags)
        .map(|_| TAG_VOCAB[(rng.next() as usize) % TAG_VOCAB.len()])
        .collect();

    // Mix in optional facets so kb-status / kb-severity hit ~30% of
    // rows each, matching a real kb's distribution.
    let want_status = rng.next() % 3 == 0;
    let want_severity = rng.next() % 4 == 0;

    let body_paragraphs = 2 + (rng.next() % 12) as usize;
    let mut body = String::new();
    for p in 0..body_paragraphs {
        body.push_str("<p>");
        let sentences = 3 + (rng.next() % 6);
        for _ in 0..sentences {
            body.push_str(LOREM[(rng.next() as usize) % LOREM.len()]);
            body.push(' ');
        }
        body.push_str("</p>\n");
        if p == body_paragraphs / 2 && rng.next() % 2 == 0 {
            body.push_str("<pre><code>fn main() { println!(\"hello\"); }</code></pre>\n");
        }
    }

    let mut html = String::with_capacity(body.len() + 512);
    html.push_str("<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n");
    html.push_str("<title>");
    push_escaped(&mut html, &title);
    html.push_str("</title>\n");
    html.push_str("<meta name=\"kb-tags\" content=\"");
    push_escaped(&mut html, &tags.join(", "));
    html.push_str("\">\n");
    if want_status {
        let s = STATUSES[(rng.next() as usize) % STATUSES.len()];
        html.push_str(&format!("<meta name=\"kb-status\" content=\"{s}\">\n"));
    }
    if want_severity {
        let s = SEVERITIES[(rng.next() as usize) % SEVERITIES.len()];
        html.push_str(&format!("<meta name=\"kb-severity\" content=\"{s}\">\n"));
    }
    html.push_str("</head>\n<body>\n<h1>");
    push_escaped(&mut html, &title);
    html.push_str("</h1>\n");
    html.push_str(&body);
    html.push_str("</body>\n</html>\n");
    let _ = folder; // signals intent; folder layout already encoded in path
    let mut f = fs::File::create(path).with_context(|| format!("create {}", path.display()))?;
    f.write_all(html.as_bytes())?;
    Ok(())
}

fn push_escaped(out: &mut String, s: &str) {
    for c in s.chars() {
        match c {
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
}

fn pick_weighted(rng: &mut SplitMix64, total: u32) -> &'static str {
    let mut roll = (rng.next() % total as u64) as u32;
    for (folder, w) in FOLDERS {
        if roll < *w {
            return folder;
        }
        roll -= *w;
    }
    FOLDERS[0].0
}

fn slug_from_index(n: u32) -> String {
    // 4-syllable mnemonic so filenames look like real artifacts rather
    // than `00001.html` / `00002.html` (which would all collide on the
    // SPA's path-derived tag heuristic).
    const SYL: &[&str] = &[
        "alp", "bet", "gam", "del", "eps", "zet", "eta", "the", "iot", "kap", "lam", "mu", "nu",
        "xi", "omi", "pi",
    ];
    let mut s = String::with_capacity(16);
    let mut x = n;
    for _ in 0..3 {
        s.push_str(SYL[(x as usize) % SYL.len()]);
        s.push('-');
        x /= SYL.len() as u32;
    }
    s.pop();
    s
}

fn title_case(s: &str) -> String {
    let leaf = s.rsplit('/').next().unwrap_or(s);
    let mut out = String::with_capacity(leaf.len());
    let mut at_start = true;
    for c in leaf.chars() {
        if c == '-' || c == '_' {
            out.push(' ');
            at_start = true;
        } else if at_start {
            out.push(c.to_ascii_uppercase());
            at_start = false;
        } else {
            out.push(c);
        }
    }
    out
}

/// SplitMix64 — 64-bit linear-congruential variant; fast, no deps,
/// produces decent quality randomness for fixture generation. The
/// seed is the only state; identical seeds produce identical corpora.
struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    fn next(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
}

const LOREM: &[&str] = &[
    "Lorem ipsum dolor sit amet.",
    "Consectetur adipiscing elit but in a way that feels suspiciously like real prose.",
    "Pagination at scale is mostly about not loading what you don't see.",
    "Atlas dots map cleanly when the embedder doesn't disagree with itself.",
    "Lance scans are cheap when the projection drops embeddings.",
    "A folder filter is descendant-inclusive or it isn't a filter at all.",
    "Server-side truncation needs a stable sort or the dropped tail is whatever lance felt like.",
    "Group-by-folder is just a secondary sort axis when you squint.",
    "Virtual scroll matters when the DOM would otherwise hold the entire corpus.",
    "Canvas paints faster than SVG once dot counts cross five-figure territory.",
    "The slim projection drops everything the detail siblings popover doesn't read.",
    "Pagination cursors deserve a Link rel=next so curl scripts can walk the corpus.",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synth_writes_requested_count_and_seed_is_deterministic() {
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("a");
        let b = tmp.path().join("b");
        run(50, a.clone(), 42).unwrap();
        run(50, b.clone(), 42).unwrap();

        let count_html = |root: &Path| -> usize {
            walkdir::WalkDir::new(root)
                .into_iter()
                .filter_map(Result::ok)
                .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("html"))
                .count()
        };
        assert_eq!(count_html(&a), 50);
        assert_eq!(count_html(&b), 50);

        // Determinism: a sample file should be byte-identical between
        // the two runs.
        let pick: Vec<PathBuf> = walkdir::WalkDir::new(&a)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("html"))
            .map(|e| e.path().to_path_buf())
            .collect();
        assert!(!pick.is_empty());
        let rel = pick[0].strip_prefix(&a).unwrap();
        let av = std::fs::read(&pick[0]).unwrap();
        let bv = std::fs::read(b.join(rel)).unwrap();
        assert_eq!(av, bv, "same seed must produce byte-identical artifacts");
    }
}
