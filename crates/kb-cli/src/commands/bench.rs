//! `kb bench` — retrieval-quality bake-off helpers.
//!
//! C1 (this file) ships the two data-collection sub-verbs:
//!
//! - `init` scaffolds a `queries.jsonl` template by sampling N random
//!   artifacts from a corpus dir. Pre-seeds each line's `relevant`
//!   array with the sampled artifact id so the operator can replace
//!   the empty `query` field by hand.
//! - `discover` drives `kb search --json` against the live daemon and
//!   prints the top hits so the operator can pick artifact ids to
//!   extend a query's `relevant` array.
//!
//! C2 adds `run` — load the labelled jsonl, drive the daemon across
//! every (kb × mode), and compute Recall@k / MRR / nDCG@k **plus
//! end-to-end query latency** (p50 / p95 / mean wall-time, and the
//! daemon's own `ms`). The bench emits a markdown report (per mode: a
//! quality table and a speed table, model rows × metric cols,
//! Δ-from-baseline columns when the operator passes `--baseline`) and
//! an optional machine-readable JSON sibling. This is the
//! quality-vs-speed tester: one run answers both questions for any
//! retrieval change — did relevance move, and did it cost (or save)
//! latency? Run it before a change, save `--json`, re-run after with
//! `--baseline <that.json>`, and read the signed Δ on both axes.
//!
//! The bench is a thin orchestrator on top of the daemon's
//! `/api/search` — no embedder weights or lance code; the only
//! contract is the JSON shape that `kb search --json` already
//! consumes (`{ hits: [{ id, title, path, ... }], ms }`).

use crate::http;
use anyhow::{anyhow, bail, Context, Result};
use kb_core::ids::ArtifactId;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// One labelled query row. The jsonl file is line-delimited so the
/// operator can edit incrementally (add a line, save, run discover,
/// extend relevant, save) without rewriting the whole file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryRow {
    /// The search query string. `init` writes this empty so the
    /// operator fills it in by hand.
    pub query: String,
    /// Artifact ids (12-hex per `kb_core::ids::ArtifactId`) considered
    /// relevant for `query`. Order doesn't matter — Recall@k and MRR
    /// use set membership; nDCG uses position in the daemon's ranked
    /// response, not this list.
    pub relevant: Vec<String>,
    /// Free-form note. Optional. Useful when the labeller wants to
    /// explain *why* a given id is relevant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

// ---- bench init -------------------------------------------------------

pub fn init(corpus: &Path, output: &Path, n: usize, seed: u64) -> Result<()> {
    if output.exists() {
        bail!(
            "refusing to overwrite {}; delete the file first if you really want to re-scaffold \
             (labelled query sets are easy to lose accidentally)",
            output.display()
        );
    }
    if !corpus.exists() {
        bail!("corpus dir does not exist: {}", corpus.display());
    }
    if !corpus.is_dir() {
        bail!("corpus path is not a directory: {}", corpus.display());
    }

    let html_files = collect_html_paths(corpus)?;
    if html_files.is_empty() {
        bail!(
            "no .html files found under {}; bench init needs at least one artifact",
            corpus.display()
        );
    }

    // Deterministic sample via SplitMix64 + Fisher–Yates partial shuffle.
    // Same seed → same scaffold (operator + collaborator stay in sync).
    let sampled = sample_n(&html_files, n.min(html_files.len()), seed);

    let mut buf = String::new();
    for absolute in &sampled {
        let rel = relative_to(corpus, absolute);
        let id = ArtifactId::from_path(&rel);
        let row = QueryRow {
            query: String::new(),
            relevant: vec![id.as_str().to_string()],
            notes: Some(format!("scaffold: {rel}")),
        };
        // One row per line; `serde_json::to_string` (no `_pretty`) keeps
        // each row on a single line which is the jsonl contract.
        buf.push_str(&serde_json::to_string(&row).context("serialise query row")?);
        buf.push('\n');
    }

    if let Some(parent) = output.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create parent dir {}", parent.display()))?;
        }
    }
    std::fs::write(output, buf).with_context(|| format!("write {}", output.display()))?;
    eprintln!(
        "✓ scaffolded {} query rows from {} HTML artifacts at seed {seed:#x}",
        sampled.len(),
        html_files.len()
    );
    eprintln!("  → {}", output.display());
    eprintln!(
        "  next: replace the empty `query` field on each line with the phrase you want to evaluate;\n         \
         use `kb bench discover --kb <name> --query \"...\"` to find additional relevant ids."
    );
    Ok(())
}

/// Walk `corpus` recursively, returning every `.html` (case-insensitive)
/// path. Sorted for determinism — without sorting, `sample_n`'s output
/// drifts between filesystem iteration orders.
fn collect_html_paths(corpus: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    walk_html(corpus, &mut out)?;
    out.sort();
    Ok(out)
}

fn walk_html(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(dir).with_context(|| format!("read_dir {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        let ft = entry.file_type()?;
        if ft.is_dir() {
            walk_html(&path, out)?;
        } else if ft.is_file() && is_html(&path) {
            out.push(path);
        }
    }
    Ok(())
}

fn is_html(path: &Path) -> bool {
    path.extension()
        .and_then(|s| s.to_str())
        .is_some_and(|s| s.eq_ignore_ascii_case("html") || s.eq_ignore_ascii_case("htm"))
}

/// `corpus` is the kb's source root; `absolute` is one of the files
/// inside it. The artifact id is derived from the source-relative path
/// (forward-slash separated to stay platform-stable).
fn relative_to(corpus: &Path, absolute: &Path) -> String {
    let canon_corpus = corpus.canonicalize();
    let canon_abs = absolute.canonicalize();
    let (root, p) = match (canon_corpus.as_ref(), canon_abs.as_ref()) {
        (Ok(r), Ok(p)) => (r.as_path(), p.as_path()),
        _ => (corpus, absolute),
    };
    p.strip_prefix(root)
        .map(|r| r.to_string_lossy().replace('\\', "/"))
        .unwrap_or_default()
}

/// SplitMix64 — same algorithm `kb synth` uses for `--seed`. Inline
/// here rather than re-exported so a future synth refactor doesn't
/// destabilise bench seeds.
struct SplitMix64(u64);
impl SplitMix64 {
    fn new(seed: u64) -> Self {
        Self(seed)
    }
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
}

/// Partial Fisher–Yates: shuffle in place up to position `n`, then
/// return the first `n` elements. O(n) regardless of input length.
fn sample_n(input: &[PathBuf], n: usize, seed: u64) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = input.to_vec();
    let mut rng = SplitMix64::new(seed);
    let len = v.len();
    for i in 0..n.min(len) {
        let pick = i + (rng.next_u64() as usize) % (len - i);
        v.swap(i, pick);
    }
    v.truncate(n.min(len));
    v
}

// ---- bench discover ----------------------------------------------------

#[derive(Debug, Deserialize)]
struct SearchResp {
    hits: Vec<Hit>,
    #[serde(default)]
    ms: u64,
}

#[derive(Debug, Deserialize)]
struct Hit {
    id: String,
    title: String,
    path: String,
}

/// One query's latency, two ways. `wall_ms` is the client-measured
/// round-trip (send + server compute + body + deserialise) — the
/// latency a CLI/SPA caller actually feels. `server_ms` is the
/// daemon's own `ms` field (search compute only; excludes client
/// deserialise + scheduling jitter), kept as a cross-check so a wall
/// spike can be attributed to the server vs. the client.
#[derive(Debug, Clone, Copy)]
struct Timing {
    wall_ms: f64,
    server_ms: f64,
}

pub async fn discover(
    kb: &str,
    query: &str,
    limit: u32,
    mode: &str,
    daemon_url: Option<&str>,
    bearer: Option<&str>,
) -> Result<()> {
    let daemon = http::detect_daemon(daemon_url, bearer)
        .await
        .ok_or_else(|| anyhow!("daemon not reachable (tried {daemon_url:?})"))?;

    let url = format!(
        "{daemon}/api/search?q={}&mode={mode}&limit={limit}&kb={}",
        http::encode_path_segment(query),
        http::encode_path_segment(kb),
    );
    let client = http::client_with_timeout_and_bearer(10, bearer)?;
    let resp = client.get(&url).send().await?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        bail!("daemon returned {status}: {body}");
    }
    let body: SearchResp = resp.json().await?;

    if body.hits.is_empty() {
        eprintln!("(no hits — try a different query or check `kb model list` for the kb's model)");
        return Ok(());
    }

    println!(
        "# {} hits in {} ms — query: {query:?}, kb: {kb}, mode: {mode}",
        body.hits.len(),
        body.ms
    );
    println!(
        "# copy ids you consider relevant into the `relevant` array of the matching jsonl line."
    );
    println!();
    for (i, h) in body.hits.iter().enumerate() {
        let title = if h.title.is_empty() {
            "(no title)"
        } else {
            &h.title
        };
        println!("  {:>2}.  {}  {}", i + 1, h.id, title);
        println!("        {}", h.path);
    }
    Ok(())
}

// ---- bench run ---------------------------------------------------------

/// A single (kb, mode) cell in the report. Aggregates metrics across
/// the whole query set.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportRow {
    pub kb: String,
    pub mode: String,
    /// Recall@k for each k in the input. Ordered to match the input k
    /// vector so the markdown column ordering is stable.
    pub recall_at_k: Vec<(u32, f64)>,
    pub mrr: f64,
    pub ndcg_at_k_max: (u32, f64),
    /// Total queries scored. Identical across rows (same jsonl), but
    /// surfaced per row so a flaky daemon dropping responses is
    /// detectable.
    pub queries: usize,
    /// End-to-end wall-time per query, in milliseconds, summarised
    /// across the query set. p50/p95 are robust to a single cold
    /// outlier (the bench also fires one untimed warm-up query per
    /// cell); mean is included for the change-magnitude read. The four
    /// latency fields default to 0.0 so a pre-speed `--baseline` JSON
    /// still deserialises (its speed Δ is simply omitted).
    #[serde(default)]
    pub lat_p50_ms: f64,
    #[serde(default)]
    pub lat_p95_ms: f64,
    #[serde(default)]
    pub lat_mean_ms: f64,
    /// Daemon self-reported `ms` (search compute only), p50 across the
    /// set — isolates server work from client/network overhead.
    #[serde(default)]
    pub srv_p50_ms: f64,
}

#[derive(Debug, Serialize, Deserialize)]
struct Report {
    queries_file: String,
    queries_total: usize,
    rows: Vec<ReportRow>,
}

#[allow(clippy::too_many_arguments)]
pub async fn run(
    queries_path: &Path,
    kbs: &[String],
    modes: &[String],
    k_values: &[u32],
    daemon_url: Option<&str>,
    bearer: Option<&str>,
    output_md: Option<&Path>,
    output_json: Option<&Path>,
    baseline: Option<&Path>,
) -> Result<()> {
    if kbs.is_empty() {
        bail!("--kbs is empty; pass at least one kb name (e.g. --kbs research-small)");
    }
    if modes.is_empty() {
        bail!("--modes is empty; defaults to hybrid,semantic — pass at least one");
    }
    if k_values.is_empty() {
        bail!("--k is empty; pass at least one k (defaults are 1,5,10)");
    }
    let rows = load_query_rows(queries_path)?;
    if rows.is_empty() {
        bail!(
            "{} has no rows; run `kb bench init` first or check the file",
            queries_path.display()
        );
    }
    for (i, row) in rows.iter().enumerate() {
        if row.query.trim().is_empty() {
            bail!(
                "row {} of {} has an empty `query` field — fill in the labels before running",
                i + 1,
                queries_path.display()
            );
        }
    }

    let daemon = http::detect_daemon(daemon_url, bearer)
        .await
        .ok_or_else(|| anyhow!("daemon not reachable (tried {daemon_url:?})"))?;
    let client = http::client_with_timeout_and_bearer(30, bearer)?;
    // The largest k we ever need from the daemon: nDCG uses the
    // max-k window, so request that many hits and slice locally for
    // smaller k.
    let k_max = *k_values.iter().max().unwrap();

    let mut report_rows = Vec::with_capacity(kbs.len() * modes.len());
    for kb in kbs {
        for mode in modes {
            eprintln!(
                "→ {kb} / {mode} ({} queries × limit {k_max}) ...",
                rows.len()
            );
            let row = score_cell(&client, &daemon, kb, mode, &rows, k_values, k_max).await?;
            report_rows.push(row);
        }
    }

    let report = Report {
        queries_file: queries_path.to_string_lossy().to_string(),
        queries_total: rows.len(),
        rows: report_rows,
    };

    // Load the optional baseline report for Δ annotation.
    let baseline_report = match baseline {
        Some(p) => Some(load_baseline(p)?),
        None => None,
    };
    let md = render_markdown(&report, k_values, baseline_report.as_ref());
    match output_md {
        Some(path) => {
            if let Some(parent) = path.parent() {
                if !parent.as_os_str().is_empty() {
                    std::fs::create_dir_all(parent)?;
                }
            }
            std::fs::write(path, &md)?;
            eprintln!("✓ markdown report → {}", path.display());
        }
        None => print!("{md}"),
    }
    if let Some(json) = output_json {
        if let Some(parent) = json.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        std::fs::write(json, serde_json::to_string_pretty(&report)?)?;
        eprintln!("✓ json report → {}", json.display());
    }
    Ok(())
}

fn load_query_rows(path: &Path) -> Result<Vec<QueryRow>> {
    if !path.exists() {
        bail!("queries file does not exist: {}", path.display());
    }
    let body = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let mut out = Vec::new();
    for (i, line) in body.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let row: QueryRow = serde_json::from_str(trimmed)
            .with_context(|| format!("parse {} line {}: {trimmed}", path.display(), i + 1))?;
        out.push(row);
    }
    Ok(out)
}

async fn score_cell(
    client: &reqwest::Client,
    daemon: &str,
    kb: &str,
    mode: &str,
    queries: &[QueryRow],
    k_values: &[u32],
    k_max: u32,
) -> Result<ReportRow> {
    let mut per_query_recall: Vec<Vec<bool>> = vec![Vec::new(); k_values.len()];
    let mut per_query_rr: Vec<f64> = Vec::with_capacity(queries.len());
    let mut per_query_ndcg: Vec<f64> = Vec::with_capacity(queries.len());
    let mut wall_ms: Vec<f64> = Vec::with_capacity(queries.len());
    let mut server_ms: Vec<f64> = Vec::with_capacity(queries.len());

    // Warm-up: fire EVERY query once, untimed, so the timed pass below
    // measures warm-cache steady-state latency. This populates the
    // daemon's persistent query-embed cache for the whole set, loads
    // lance's ANN + FTS indexes, and removes per-query cold-start
    // outliers. The point is to isolate the *pipeline* cost — fusion,
    // title-boost, the arm queries, reranking — which is the thing a
    // retrieval change actually moves, from the one-time per-query
    // embed (a fixed model cost, ~90 ms for bge-large, that the cache
    // pays once). It also makes modes comparable regardless of run
    // order: without it, whichever mode runs first eats the cold embed
    // and the rest look artificially fast off its warmed cache. Warm-up
    // errors are ignored; the timed loop re-issues each query and
    // surfaces the real error with context.
    for q in queries {
        let _ = run_one_query(client, daemon, kb, &q.query, mode, k_max).await;
    }

    for q in queries {
        let (hits, timing) = run_one_query(client, daemon, kb, &q.query, mode, k_max).await?;
        wall_ms.push(timing.wall_ms);
        server_ms.push(timing.server_ms);
        let ids: Vec<&str> = hits.iter().map(|h| h.id.as_str()).collect();
        let relevant: std::collections::HashSet<&str> =
            q.relevant.iter().map(|s| s.as_str()).collect();

        // Recall@k: any relevant id appears in the top k?
        for (i, &k) in k_values.iter().enumerate() {
            let k = (k as usize).min(ids.len());
            let hit = ids[..k].iter().any(|id| relevant.contains(id));
            per_query_recall[i].push(hit);
        }

        // MRR: 1 / rank of the FIRST relevant id (1-indexed). 0 if no
        // relevant id is in the top-k_max window.
        let rr = ids
            .iter()
            .position(|id| relevant.contains(id))
            .map(|pos| 1.0 / (pos as f64 + 1.0))
            .unwrap_or(0.0);
        per_query_rr.push(rr);

        // nDCG@k_max: binary relevance per id (1.0 if relevant, 0.0
        // otherwise). DCG = Σ rel_i / log2(i + 2), 1-indexed positions.
        // IDCG = ideal ordering with all relevant ids at the top of
        // the window. nDCG = DCG / IDCG; 0.0 if no relevant id appears
        // (IDCG would be 0 too — define as 0 to avoid NaN).
        let mut dcg = 0.0_f64;
        for (i, id) in ids.iter().take(k_max as usize).enumerate() {
            if relevant.contains(id) {
                dcg += 1.0 / ((i as f64 + 2.0).log2());
            }
        }
        // IDCG: at best, min(relevant.len(), k_max) relevant ids land
        // in positions 1..=min(relevant.len(), k_max).
        let ideal_hits = relevant.len().min(k_max as usize);
        let mut idcg = 0.0_f64;
        for i in 0..ideal_hits {
            idcg += 1.0 / ((i as f64 + 2.0).log2());
        }
        let ndcg = if idcg > 0.0 { dcg / idcg } else { 0.0 };
        per_query_ndcg.push(ndcg);
    }

    let recall_at_k: Vec<(u32, f64)> = k_values
        .iter()
        .enumerate()
        .map(|(i, &k)| (k, mean_bool(&per_query_recall[i])))
        .collect();
    let mrr = mean_f64(&per_query_rr);
    let ndcg = mean_f64(&per_query_ndcg);

    Ok(ReportRow {
        kb: kb.to_string(),
        mode: mode.to_string(),
        recall_at_k,
        mrr,
        ndcg_at_k_max: (k_max, ndcg),
        queries: queries.len(),
        lat_p50_ms: percentile(&wall_ms, 50.0),
        lat_p95_ms: percentile(&wall_ms, 95.0),
        lat_mean_ms: mean_f64(&wall_ms),
        srv_p50_ms: percentile(&server_ms, 50.0),
    })
}

async fn run_one_query(
    client: &reqwest::Client,
    daemon: &str,
    kb: &str,
    query: &str,
    mode: &str,
    limit: u32,
) -> Result<(Vec<Hit>, Timing)> {
    let url = format!(
        "{daemon}/api/search?q={}&mode={mode}&limit={limit}&kb={}",
        http::encode_path_segment(query),
        http::encode_path_segment(kb),
    );
    // Time the whole round-trip: the request URL is already built, so
    // this captures send + server compute + response body + JSON
    // deserialise — the wall-clock a caller actually waits on.
    let started = std::time::Instant::now();
    let resp = client.get(&url).send().await?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        bail!("daemon returned {status} for {kb}/{mode}: {body}");
    }
    let body: SearchResp = resp.json().await?;
    let timing = Timing {
        wall_ms: started.elapsed().as_secs_f64() * 1000.0,
        server_ms: body.ms as f64,
    };
    Ok((body.hits, timing))
}

fn mean_bool(v: &[bool]) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    v.iter().filter(|x| **x).count() as f64 / v.len() as f64
}

fn mean_f64(v: &[f64]) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    v.iter().sum::<f64>() / v.len() as f64
}

/// Nearest-rank percentile (`p` in 0..=100) over an unsorted slice.
/// Returns 0.0 for an empty slice. Clones + sorts a small vec (query
/// counts are tens, not millions) so the caller's order is preserved.
/// Nearest-rank (rank = ceil(p/100 · N), 1-indexed, clamped) is the
/// standard latency-reporting choice — deterministic, no interpolation,
/// and for tail percentiles it never under-reports by averaging.
fn percentile(samples: &[f64], p: f64) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    let mut v = samples.to_vec();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = v.len();
    let rank = ((p / 100.0) * n as f64).ceil() as usize;
    let idx = rank.clamp(1, n) - 1;
    v[idx]
}

/// Load a prior `--json` bench report so `render_markdown` can annotate
/// each metric cell with its Δ-from-baseline. Errors if the file is
/// missing or not a valid report (so a typo'd `--baseline` fails loud
/// rather than silently dropping the deltas).
fn load_baseline(path: &Path) -> Result<Report> {
    let body = std::fs::read_to_string(path)
        .with_context(|| format!("read baseline report {}", path.display()))?;
    serde_json::from_str(&body).with_context(|| {
        format!(
            "parse baseline report {} (expected a `--json` report)",
            path.display()
        )
    })
}

/// Flatten a report into a `(kb, mode, metric-key) → value` map for Δ
/// lookup. Metric keys: `r{k}` for Recall@k, `mrr`, `ndcg`.
fn build_metric_lookup(
    report: &Report,
) -> std::collections::HashMap<(String, String, String), f64> {
    let mut m = std::collections::HashMap::new();
    for row in &report.rows {
        for (k, v) in &row.recall_at_k {
            m.insert((row.kb.clone(), row.mode.clone(), format!("r{k}")), *v);
        }
        m.insert((row.kb.clone(), row.mode.clone(), "mrr".into()), row.mrr);
        m.insert(
            (row.kb.clone(), row.mode.clone(), "ndcg".into()),
            row.ndcg_at_k_max.1,
        );
        m.insert(
            (row.kb.clone(), row.mode.clone(), "lat_p50".into()),
            row.lat_p50_ms,
        );
        m.insert(
            (row.kb.clone(), row.mode.clone(), "lat_p95".into()),
            row.lat_p95_ms,
        );
        m.insert(
            (row.kb.clone(), row.mode.clone(), "lat_mean".into()),
            row.lat_mean_ms,
        );
        m.insert(
            (row.kb.clone(), row.mode.clone(), "srv_p50".into()),
            row.srv_p50_ms,
        );
    }
    m
}

/// Format a metric cell: `0.812` alone, or `0.812 (+0.031)` with a
/// baseline value. The Δ is signed and fixed at 3 decimals so columns
/// stay aligned and a regression (negative Δ) is unmistakable.
fn fmt_cell(cur: f64, base: Option<f64>) -> String {
    match base {
        Some(b) => {
            let d = cur - b;
            let sign = if d >= 0.0 { "+" } else { "-" };
            format!("{cur:.3} ({sign}{:.3})", d.abs())
        }
        None => format!("{cur:.3}"),
    }
}

/// Format a latency cell in ms: `123.4` alone, or `123.4 (-12.1)` with a
/// baseline. The Δ is `cur - base`; for latency **lower is better**, so a
/// speed-up reads as a *negative* number (the report header states the
/// convention). One decimal keeps the ms columns aligned.
fn fmt_ms_cell(cur: f64, base: Option<f64>) -> String {
    match base {
        Some(b) => {
            let d = cur - b;
            let sign = if d >= 0.0 { "+" } else { "-" };
            format!("{cur:.1} ({sign}{:.1})", d.abs())
        }
        None => format!("{cur:.1}"),
    }
}

fn render_markdown(report: &Report, k_values: &[u32], baseline: Option<&Report>) -> String {
    use std::fmt::Write;
    let base_lookup = baseline.map(build_metric_lookup);
    let mut out = String::new();
    let _ = writeln!(out, "# Retrieval quality + speed");
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "Queries: **{}** from `{}`.",
        report.queries_total, report.queries_file
    );
    let _ = writeln!(
        out,
        "\nSpeed is **warm-cache** end-to-end wall-time per query (the whole \
         query set is fired once untimed first, so the query-embed cache is \
         warm — these numbers isolate the search *pipeline* from the one-time \
         per-query embed); `srv p50` is the daemon's own `ms` (search compute \
         only). Quality higher = better; latency lower = better."
    );
    if base_lookup.is_some() {
        let _ = writeln!(
            out,
            "\nΔ columns are vs. the `--baseline` report — for quality a \
             positive Δ is a win; for speed a **negative Δ (ms) is faster**."
        );
    }
    let _ = writeln!(out);

    // Per mode: a quality table then a speed table, kb rows in both.
    let modes: std::collections::BTreeSet<&str> =
        report.rows.iter().map(|r| r.mode.as_str()).collect();
    for mode in modes {
        let _ = writeln!(out, "## Mode: `{mode}`");
        let _ = writeln!(out);

        // ---- Quality: recall_k... | mrr | ndcg ----
        let _ = writeln!(out, "### Quality");
        let _ = writeln!(out);
        let mut header = String::from("| kb |");
        let mut sep = String::from("|---|");
        for k in k_values {
            let _ = write!(header, " Recall@{k} |");
            sep.push_str("---:|");
        }
        let _ = write!(header, " MRR |");
        sep.push_str("---:|");
        if let Some(first) = report.rows.iter().find(|r| r.mode == mode) {
            let _ = write!(header, " nDCG@{} |", first.ndcg_at_k_max.0);
            sep.push_str("---:|");
        }
        let _ = writeln!(out, "{header}");
        let _ = writeln!(out, "{sep}");
        for row in report.rows.iter().filter(|r| r.mode == mode) {
            let lookup = |key: String| -> Option<f64> {
                base_lookup
                    .as_ref()
                    .and_then(|m| m.get(&(row.kb.clone(), row.mode.clone(), key)).copied())
            };
            let mut line = format!("| {} |", row.kb);
            for (k, v) in &row.recall_at_k {
                let _ = write!(line, " {} |", fmt_cell(*v, lookup(format!("r{k}"))));
            }
            let _ = write!(line, " {} |", fmt_cell(row.mrr, lookup("mrr".into())));
            let _ = write!(
                line,
                " {} |",
                fmt_cell(row.ndcg_at_k_max.1, lookup("ndcg".into()))
            );
            let _ = writeln!(out, "{line}");
        }
        let _ = writeln!(out);

        // ---- Speed: p50 | p95 | mean | srv p50 (all ms) ----
        let _ = writeln!(out, "### Speed (ms; lower = faster)");
        let _ = writeln!(out);
        let _ = writeln!(out, "| kb | p50 | p95 | mean | srv p50 |");
        let _ = writeln!(out, "|---|---:|---:|---:|---:|");
        for row in report.rows.iter().filter(|r| r.mode == mode) {
            let lookup = |key: &str| -> Option<f64> {
                base_lookup.as_ref().and_then(|m| {
                    m.get(&(row.kb.clone(), row.mode.clone(), key.to_string()))
                        .copied()
                })
            };
            let _ = writeln!(
                out,
                "| {} | {} | {} | {} | {} |",
                row.kb,
                fmt_ms_cell(row.lat_p50_ms, lookup("lat_p50")),
                fmt_ms_cell(row.lat_p95_ms, lookup("lat_p95")),
                fmt_ms_cell(row.lat_mean_ms, lookup("lat_mean")),
                fmt_ms_cell(row.srv_p50_ms, lookup("srv_p50")),
            );
        }
        let _ = writeln!(out);
    }
    out
}

#[cfg(test)]
mod metric_tests {
    use super::*;

    fn rows(input: &[(&str, &[&str])]) -> Vec<QueryRow> {
        input
            .iter()
            .map(|(q, rel)| QueryRow {
                query: (*q).to_string(),
                relevant: rel.iter().map(|s| (*s).to_string()).collect(),
                notes: None,
            })
            .collect()
    }

    fn hit(id: &str) -> Hit {
        Hit {
            id: id.to_string(),
            title: String::new(),
            path: String::new(),
        }
    }

    /// Direct exerciser of the scoring math, bypassing the live daemon
    /// loop. Mirrors what `score_cell` does per query.
    fn score(queries: &[QueryRow], hits: &[Vec<Hit>], k_values: &[u32]) -> ReportRow {
        assert_eq!(queries.len(), hits.len());
        let k_max = *k_values.iter().max().unwrap();
        let mut per_recall: Vec<Vec<bool>> = vec![Vec::new(); k_values.len()];
        let mut rrs = Vec::new();
        let mut ndcgs = Vec::new();
        for (q, hs) in queries.iter().zip(hits.iter()) {
            let ids: Vec<&str> = hs.iter().map(|h| h.id.as_str()).collect();
            let rel: std::collections::HashSet<&str> =
                q.relevant.iter().map(|s| s.as_str()).collect();
            for (i, &k) in k_values.iter().enumerate() {
                let k = (k as usize).min(ids.len());
                per_recall[i].push(ids[..k].iter().any(|id| rel.contains(id)));
            }
            let rr = ids
                .iter()
                .position(|id| rel.contains(id))
                .map(|pos| 1.0 / (pos as f64 + 1.0))
                .unwrap_or(0.0);
            rrs.push(rr);
            let mut dcg = 0.0;
            for (i, id) in ids.iter().take(k_max as usize).enumerate() {
                if rel.contains(id) {
                    dcg += 1.0 / ((i as f64 + 2.0).log2());
                }
            }
            let ideal = rel.len().min(k_max as usize);
            let mut idcg = 0.0;
            for i in 0..ideal {
                idcg += 1.0 / ((i as f64 + 2.0).log2());
            }
            ndcgs.push(if idcg > 0.0 { dcg / idcg } else { 0.0 });
        }
        ReportRow {
            kb: "test".into(),
            mode: "hybrid".into(),
            recall_at_k: k_values
                .iter()
                .enumerate()
                .map(|(i, &k)| (k, mean_bool(&per_recall[i])))
                .collect(),
            mrr: mean_f64(&rrs),
            ndcg_at_k_max: (k_max, mean_f64(&ndcgs)),
            queries: queries.len(),
            // Latency isn't exercised by the math tests (they bypass the
            // daemon); the speed fields are covered separately.
            lat_p50_ms: 0.0,
            lat_p95_ms: 0.0,
            lat_mean_ms: 0.0,
            srv_p50_ms: 0.0,
        }
    }

    #[test]
    fn perfect_recall_when_first_hit_is_relevant() {
        let qs = rows(&[("q1", &["a"])]);
        let hs = vec![vec![hit("a"), hit("b"), hit("c")]];
        let r = score(&qs, &hs, &[1, 5]);
        assert_eq!(r.recall_at_k, vec![(1, 1.0), (5, 1.0)]);
        assert!((r.mrr - 1.0).abs() < 1e-12);
        // nDCG with one relevant in position 1: DCG = 1/log2(2) = 1.0,
        // IDCG = 1.0, ratio = 1.0.
        assert!((r.ndcg_at_k_max.1 - 1.0).abs() < 1e-12);
    }

    #[test]
    fn zero_recall_when_no_relevant_in_results() {
        let qs = rows(&[("q1", &["a"])]);
        let hs = vec![vec![hit("x"), hit("y"), hit("z")]];
        let r = score(&qs, &hs, &[1, 5]);
        assert_eq!(r.recall_at_k, vec![(1, 0.0), (5, 0.0)]);
        assert_eq!(r.mrr, 0.0);
        assert_eq!(r.ndcg_at_k_max.1, 0.0);
    }

    #[test]
    fn mrr_at_position_2_is_one_half() {
        let qs = rows(&[("q1", &["b"])]);
        let hs = vec![vec![hit("a"), hit("b"), hit("c")]];
        let r = score(&qs, &hs, &[1, 5]);
        assert!((r.mrr - 0.5).abs() < 1e-12);
    }

    #[test]
    fn recall_at_1_misses_but_recall_at_5_hits() {
        // Relevant at position 3 — Recall@1 = 0, Recall@5 = 1.
        let qs = rows(&[("q1", &["c"])]);
        let hs = vec![vec![hit("a"), hit("b"), hit("c"), hit("d"), hit("e")]];
        let r = score(&qs, &hs, &[1, 5]);
        let recall_map: std::collections::HashMap<u32, f64> = r.recall_at_k.into_iter().collect();
        assert_eq!(recall_map[&1], 0.0);
        assert_eq!(recall_map[&5], 1.0);
    }

    #[test]
    fn averages_across_queries() {
        // Q1 hits at position 1 (MRR contribution = 1.0); Q2 misses
        // entirely (contribution = 0). Mean MRR = 0.5.
        let qs = rows(&[("q1", &["a"]), ("q2", &["zzz"])]);
        let hs = vec![vec![hit("a"), hit("b")], vec![hit("x"), hit("y")]];
        let r = score(&qs, &hs, &[1, 5]);
        assert!((r.mrr - 0.5).abs() < 1e-12);
    }

    #[test]
    fn ndcg_multiple_relevant_ids_in_order() {
        // Two relevant ids, both in top 2 in best order.
        // DCG = 1/log2(2) + 1/log2(3) = 1 + 0.6309... = 1.6309...
        // IDCG (2 relevant, ideal positions 1,2) = same. nDCG = 1.0.
        let qs = rows(&[("q1", &["a", "b"])]);
        let hs = vec![vec![hit("a"), hit("b"), hit("c")]];
        let r = score(&qs, &hs, &[5]);
        assert!((r.ndcg_at_k_max.1 - 1.0).abs() < 1e-9);
    }

    #[test]
    fn ndcg_degrades_when_relevant_pushed_lower() {
        // Relevant id at position 5 vs position 1 — nDCG drops sharply.
        let qs_top = rows(&[("q1", &["a"])]);
        let hs_top = vec![vec![hit("a"), hit("b"), hit("c"), hit("d"), hit("e")]];
        let r_top = score(&qs_top, &hs_top, &[5]);

        let qs_low = rows(&[("q1", &["e"])]);
        let hs_low = vec![vec![hit("a"), hit("b"), hit("c"), hit("d"), hit("e")]];
        let r_low = score(&qs_low, &hs_low, &[5]);

        assert!(r_top.ndcg_at_k_max.1 > r_low.ndcg_at_k_max.1);
        assert!((r_top.ndcg_at_k_max.1 - 1.0).abs() < 1e-12);
        assert!(r_low.ndcg_at_k_max.1 < 0.5);
    }

    #[test]
    fn load_query_rows_skips_blank_and_comment_lines() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("q.jsonl");
        std::fs::write(
            &p,
            "# header comment\n\n{\"query\":\"foo\",\"relevant\":[\"a\"]}\n\n",
        )
        .unwrap();
        let rows = load_query_rows(&p).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].query, "foo");
    }

    #[test]
    fn load_query_rows_errors_on_malformed_json() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("q.jsonl");
        std::fs::write(&p, "not json\n").unwrap();
        let err = load_query_rows(&p).unwrap_err();
        let chain = format!("{err:#}");
        assert!(chain.contains("parse"), "got: {chain}");
    }

    #[test]
    fn render_markdown_has_one_table_per_mode_with_kb_rows() {
        let report = Report {
            queries_file: "q.jsonl".into(),
            queries_total: 2,
            rows: vec![
                ReportRow {
                    kb: "small".into(),
                    mode: "hybrid".into(),
                    recall_at_k: vec![(5, 0.5)],
                    mrr: 0.4,
                    ndcg_at_k_max: (5, 0.55),
                    queries: 2,
                    lat_p50_ms: 90.0,
                    lat_p95_ms: 120.0,
                    lat_mean_ms: 95.0,
                    srv_p50_ms: 80.0,
                },
                ReportRow {
                    kb: "base".into(),
                    mode: "hybrid".into(),
                    recall_at_k: vec![(5, 0.75)],
                    mrr: 0.6,
                    ndcg_at_k_max: (5, 0.72),
                    queries: 2,
                    lat_p50_ms: 100.0,
                    lat_p95_ms: 130.0,
                    lat_mean_ms: 105.0,
                    srv_p50_ms: 92.0,
                },
                ReportRow {
                    kb: "small".into(),
                    mode: "semantic".into(),
                    recall_at_k: vec![(5, 0.3)],
                    mrr: 0.2,
                    ndcg_at_k_max: (5, 0.3),
                    queries: 2,
                    lat_p50_ms: 70.0,
                    lat_p95_ms: 95.0,
                    lat_mean_ms: 75.0,
                    srv_p50_ms: 60.0,
                },
            ],
        };
        let md = render_markdown(&report, &[5], None);
        assert!(md.contains("## Mode: `hybrid`"));
        assert!(md.contains("## Mode: `semantic`"));
        assert!(md.contains("| small |"));
        assert!(md.contains("| base |"));
        // Recall@5 + MRR + nDCG@5 cols present.
        assert!(md.contains("Recall@5"));
        assert!(md.contains("MRR"));
        assert!(md.contains("nDCG@5"));
    }

    #[test]
    fn fmt_cell_formats_with_and_without_baseline() {
        assert_eq!(fmt_cell(0.812, None), "0.812");
        assert_eq!(fmt_cell(0.812, Some(0.781)), "0.812 (+0.031)");
        assert_eq!(fmt_cell(0.700, Some(0.750)), "0.700 (-0.050)");
    }

    #[test]
    fn percentile_nearest_rank() {
        let v = [10.0, 20.0, 30.0, 40.0, 50.0];
        // p50 of 5 → rank ceil(2.5) = 3 → 3rd smallest = 30.
        assert_eq!(percentile(&v, 50.0), 30.0);
        // p100 → max; p0 clamps to rank 1 → min.
        assert_eq!(percentile(&v, 100.0), 50.0);
        assert_eq!(percentile(&v, 0.0), 10.0);
        // Unsorted input is sorted internally before ranking.
        let u = [50.0, 10.0, 40.0, 20.0, 30.0];
        assert_eq!(percentile(&u, 50.0), 30.0);
        // p95 of 20 evenly-spaced → rank ceil(19.0) = 19 → 19th value.
        let twenty: Vec<f64> = (1..=20).map(|i| i as f64).collect();
        assert_eq!(percentile(&twenty, 95.0), 19.0);
        // Empty → 0.0 (no panic).
        let empty: [f64; 0] = [];
        assert_eq!(percentile(&empty, 50.0), 0.0);
    }

    #[test]
    fn fmt_ms_cell_formats_and_signs_speedup_negative() {
        assert_eq!(fmt_ms_cell(123.4, None), "123.4");
        // Slower than baseline → positive Δ.
        assert_eq!(fmt_ms_cell(130.0, Some(120.0)), "130.0 (+10.0)");
        // Faster than baseline → negative Δ (the win direction for speed).
        assert_eq!(fmt_ms_cell(108.0, Some(120.0)), "108.0 (-12.0)");
    }

    #[test]
    fn render_markdown_includes_speed_table() {
        let report = Report {
            queries_file: "q.jsonl".into(),
            queries_total: 2,
            rows: vec![ReportRow {
                kb: "large".into(),
                mode: "hybrid".into(),
                recall_at_k: vec![(5, 0.8)],
                mrr: 0.7,
                ndcg_at_k_max: (5, 0.75),
                queries: 2,
                lat_p50_ms: 118.0,
                lat_p95_ms: 145.0,
                lat_mean_ms: 121.0,
                srv_p50_ms: 96.0,
            }],
        };
        let md = render_markdown(&report, &[5], None);
        assert!(md.contains("### Quality"), "quality heading: {md}");
        assert!(md.contains("### Speed"), "speed heading: {md}");
        assert!(md.contains("lower = faster"));
        assert!(md.contains("| p50 | p95 | mean | srv p50 |"));
        // Latency values rendered at one decimal.
        assert!(md.contains("118.0"), "p50 missing: {md}");
        assert!(md.contains("145.0"), "p95 missing: {md}");
        assert!(md.contains("96.0"), "srv p50 missing: {md}");
    }

    #[test]
    fn render_markdown_speed_delta_shows_negative_when_faster() {
        let mk = |lat: f64| Report {
            queries_file: "q.jsonl".into(),
            queries_total: 1,
            rows: vec![ReportRow {
                kb: "large".into(),
                mode: "hybrid".into(),
                recall_at_k: vec![(1, 0.8)],
                mrr: 0.8,
                ndcg_at_k_max: (1, 0.8),
                queries: 1,
                lat_p50_ms: lat,
                lat_p95_ms: lat + 20.0,
                lat_mean_ms: lat,
                srv_p50_ms: lat - 10.0,
            }],
        };
        let cur = mk(108.0); // faster than the 120.0 baseline → -12.0
        let base = mk(120.0);
        let md = render_markdown(&cur, &[1], Some(&base));
        assert!(md.contains("108.0 (-12.0)"), "speedup delta missing: {md}");
        assert!(md.contains("negative Δ (ms) is faster"));
    }

    #[test]
    fn render_markdown_with_baseline_annotates_deltas() {
        let mk = |kb: &str, r: f64, mrr: f64, ndcg: f64| ReportRow {
            kb: kb.into(),
            mode: "hybrid".into(),
            recall_at_k: vec![(1, r)],
            mrr,
            ndcg_at_k_max: (1, ndcg),
            queries: 2,
            lat_p50_ms: 0.0,
            lat_p95_ms: 0.0,
            lat_mean_ms: 0.0,
            srv_p50_ms: 0.0,
        };
        let cur = Report {
            queries_file: "q.jsonl".into(),
            queries_total: 2,
            rows: vec![mk("large", 0.812, 0.875, 0.868)],
        };
        let base = Report {
            queries_file: "q.jsonl".into(),
            queries_total: 2,
            rows: vec![mk("large", 0.656, 0.749, 0.779)],
        };
        let md = render_markdown(&cur, &[1], Some(&base));
        assert!(md.contains("(+0.156)"), "recall delta missing: {md}");
        assert!(md.contains("(+0.126)"), "mrr delta missing: {md}");
        assert!(md.contains("(+0.089)"), "ndcg delta missing: {md}");
        assert!(md.contains("vs. the `--baseline`"));
    }

    #[test]
    fn baseline_round_trips_through_json() {
        let report = Report {
            queries_file: "q.jsonl".into(),
            queries_total: 1,
            rows: vec![ReportRow {
                kb: "large".into(),
                mode: "hybrid".into(),
                recall_at_k: vec![(1, 0.5), (5, 0.9)],
                mrr: 0.6,
                ndcg_at_k_max: (10, 0.7),
                queries: 1,
                lat_p50_ms: 118.5,
                lat_p95_ms: 142.0,
                lat_mean_ms: 121.3,
                srv_p50_ms: 95.0,
            }],
        };
        let json = serde_json::to_string_pretty(&report).unwrap();
        let back: Report = serde_json::from_str(&json).unwrap();
        assert_eq!(back.rows.len(), 1);
        assert_eq!(back.rows[0].recall_at_k, vec![(1, 0.5), (5, 0.9)]);
        assert_eq!(back.rows[0].ndcg_at_k_max, (10, 0.7));
        // Latency fields round-trip too.
        assert_eq!(back.rows[0].lat_p50_ms, 118.5);
        assert_eq!(back.rows[0].lat_p95_ms, 142.0);
        assert_eq!(back.rows[0].lat_mean_ms, 121.3);
        assert_eq!(back.rows[0].srv_p50_ms, 95.0);
    }

    /// A pre-speed baseline JSON (no latency fields) must still
    /// deserialise — the `#[serde(default)]` on the four ms fields is
    /// what lets `--baseline old.json` keep working after this change.
    #[test]
    fn baseline_without_latency_fields_deserialises() {
        let legacy = r#"{
            "queries_file": "q.jsonl",
            "queries_total": 1,
            "rows": [{
                "kb": "large",
                "mode": "hybrid",
                "recall_at_k": [[1, 0.5]],
                "mrr": 0.6,
                "ndcg_at_k_max": [10, 0.7],
                "queries": 1
            }]
        }"#;
        let back: Report = serde_json::from_str(legacy).unwrap();
        assert_eq!(back.rows.len(), 1);
        // Missing latency fields default to 0.0.
        assert_eq!(back.rows[0].lat_p50_ms, 0.0);
        assert_eq!(back.rows[0].srv_p50_ms, 0.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write(p: &Path, body: &str) {
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let mut f = std::fs::File::create(p).unwrap();
        f.write_all(body.as_bytes()).unwrap();
    }

    #[test]
    fn collect_html_walks_recursively_and_skips_non_html() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(&root.join("a.html"), "<p>a</p>");
        write(&root.join("sub/b.html"), "<p>b</p>");
        write(&root.join("sub/notes.txt"), "ignored");
        write(&root.join("sub/c.HTM"), "<p>c</p>"); // case-insensitive
        let mut found = collect_html_paths(root).unwrap();
        found.sort();
        let names: Vec<String> = found
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["a.html", "b.html", "c.HTM"]);
    }

    #[test]
    fn sample_n_is_deterministic_for_same_seed() {
        let files: Vec<PathBuf> = (0..50)
            .map(|i| PathBuf::from(format!("/tmp/{i}.html")))
            .collect();
        let a = sample_n(&files, 5, 42);
        let b = sample_n(&files, 5, 42);
        assert_eq!(a, b, "same seed must produce identical samples");
        let c = sample_n(&files, 5, 43);
        assert_ne!(a, c, "different seeds must (almost certainly) differ");
    }

    #[test]
    fn sample_n_clamps_when_n_exceeds_input() {
        let files: Vec<PathBuf> = (0..3)
            .map(|i| PathBuf::from(format!("/tmp/{i}.html")))
            .collect();
        let s = sample_n(&files, 100, 1);
        assert_eq!(s.len(), 3, "must clamp at input length");
    }

    #[test]
    fn init_refuses_to_overwrite_existing_file() {
        let tmp = tempfile::tempdir().unwrap();
        let corpus = tmp.path().join("corpus");
        std::fs::create_dir_all(&corpus).unwrap();
        write(&corpus.join("a.html"), "<p>a</p>");

        let out = tmp.path().join("queries.jsonl");
        std::fs::write(&out, b"{\"query\":\"existing\",\"relevant\":[]}\n").unwrap();

        let err = init(&corpus, &out, 1, 1).unwrap_err();
        assert!(
            err.to_string().contains("refusing to overwrite"),
            "got: {err}"
        );
    }

    #[test]
    fn init_writes_one_jsonl_line_per_sample_with_pre_seeded_relevant() {
        let tmp = tempfile::tempdir().unwrap();
        let corpus = tmp.path().join("corpus");
        std::fs::create_dir_all(corpus.join("sub")).unwrap();
        write(&corpus.join("a.html"), "<p>a</p>");
        write(&corpus.join("sub/b.html"), "<p>b</p>");

        let out = tmp.path().join("queries.jsonl");
        init(&corpus, &out, 2, 1).unwrap();

        let body = std::fs::read_to_string(&out).unwrap();
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(lines.len(), 2);
        for line in &lines {
            let row: QueryRow = serde_json::from_str(line).unwrap();
            assert!(
                row.query.is_empty(),
                "query field must be empty for operator to fill"
            );
            assert_eq!(row.relevant.len(), 1, "scaffold seeds one id per query");
            // Id is 12-hex per ArtifactId::from_path.
            assert_eq!(row.relevant[0].len(), 12);
            assert!(row.relevant[0].chars().all(|c| c.is_ascii_hexdigit()));
            assert!(row.notes.is_some());
        }
    }

    #[test]
    fn init_errors_when_corpus_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let corpus = tmp.path().join("corpus");
        std::fs::create_dir_all(&corpus).unwrap();
        let out = tmp.path().join("queries.jsonl");
        let err = init(&corpus, &out, 10, 1).unwrap_err();
        assert!(err.to_string().contains("no .html files"), "got: {err}");
    }

    #[test]
    fn init_errors_when_corpus_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let corpus = tmp.path().join("nope");
        let out = tmp.path().join("queries.jsonl");
        let err = init(&corpus, &out, 10, 1).unwrap_err();
        assert!(err.to_string().contains("does not exist"), "got: {err}");
    }

    #[test]
    fn artifact_id_from_relative_path_is_stable() {
        // Stability contract: the id `init` writes must match the id the
        // daemon writes for the same source-relative path. If
        // `relative_to` returns "sub/b.html", `ArtifactId::from_path`
        // gives the same 12-hex the indexer uses (via doc_rel_path →
        // ArtifactId::from_path in indexer.rs).
        let id1 = ArtifactId::from_path("sub/b.html");
        let id2 = ArtifactId::from_path("sub/b.html");
        assert_eq!(id1.as_str(), id2.as_str());
        assert_eq!(id1.as_str().len(), 12);
    }

    #[test]
    fn query_row_round_trips_through_jsonl() {
        let row = QueryRow {
            query: "rust async".into(),
            relevant: vec!["abc123def456".into()],
            notes: Some("the actor pattern".into()),
        };
        let line = serde_json::to_string(&row).unwrap();
        assert!(!line.contains('\n'), "must be a single line for jsonl");
        let parsed: QueryRow = serde_json::from_str(&line).unwrap();
        assert_eq!(parsed.query, "rust async");
        assert_eq!(parsed.relevant, vec!["abc123def456"]);
        assert_eq!(parsed.notes.as_deref(), Some("the actor pattern"));
    }

    #[test]
    fn query_row_without_notes_omits_field_in_output() {
        let row = QueryRow {
            query: "x".into(),
            relevant: vec![],
            notes: None,
        };
        let line = serde_json::to_string(&row).unwrap();
        assert!(
            !line.contains("notes"),
            "None notes must be omitted: {line}"
        );
    }
}
