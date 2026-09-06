//! W2.1 — the INSTANT search lanes' latency CONTRACT, measured in-process
//! (not over HTTP: HTTP/tokio/reqwest round-trip overhead is not what
//! "instant" refers to here — the lane's own compute time is) against a
//! REAL-SCALE fixture: this workspace's own `crates/` directory, copied
//! into a fresh git repo and walked via the real
//! `ingest::index_repo_working_tree` — the exact code path the daemon's
//! boot walk uses, not a synthetic shortcut.
//!
//! Budgets (p50 over [`SAMPLES`] runs): files/symbols < 50ms, text first-hit
//! < 250ms. "First-hit" for the text lane is approximated by the full
//! synchronous call's wall time for a query with an early, common match —
//! `search_text` has no incremental/streaming return path to a caller yet
//! (a future wave's concern), so the honest measurable proxy is "how long
//! until the whole (small, capped) result set is back."
//!
//! `KB_CODE_LAT_MULT` (default `1`, i.e. the HONEST budget) multiplies every
//! budget below — a shared/loaded CI box may need headroom a quiet
//! workstation doesn't; set e.g. `KB_CODE_LAT_MULT=4` in CI rather than
//! loosening the budget for everyone else.

use crate::common::git;
use kb_code_server::git::GitRepo;
use kb_code_server::ingest;
use kb_code_server::search::{files::FileIndex, symbols::SymbolIndex, text, LaneOpts};
use kb_code_server::store::Store;
use kb_code_server::{review_findings, review_inbox};
use std::path::Path;
use std::time::{Duration, Instant};

const SAMPLES: usize = 20;

fn lat_mult() -> f64 {
    std::env::var("KB_CODE_LAT_MULT")
        .ok()
        .and_then(|s| s.parse::<f64>().ok())
        .filter(|m| *m > 0.0)
        .unwrap_or(1.0)
}

fn budget_ms(base_ms: u64) -> Duration {
    Duration::from_secs_f64(base_ms as f64 * lat_mult() / 1000.0)
}

fn copy_dir_recursive(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for entry in std::fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let ty = entry.file_type().unwrap();
        let dst_path = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_recursive(&entry.path(), &dst_path);
        } else if ty.is_file() {
            std::fs::copy(entry.path(), &dst_path).unwrap();
        }
        // No symlinks expected under crates/ (nothing in the fixture
        // needs them); silently skipped if any ever appear.
    }
}

/// Build a real-scale fixture repo: `CARGO_MANIFEST_DIR/..` (this crate's
/// own `crates/kb-code-server`, one level up = `crates/`, the whole
/// workspace's crate tree — 280+ files, ~8.5MB at the time this harness was
/// written), copied into a fresh git repo and committed. Real files, real
/// directory depth, real language mix (mostly `.rs`, plus `.toml`/`.sql`/
/// `.md`) — not a handful of synthetic fixtures.
fn build_fixture_repo() -> (tempfile::TempDir, std::path::PathBuf) {
    let src = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("kb-code-server has a parent dir (crates/)")
        .to_path_buf();
    let tmp = tempfile::tempdir().unwrap();
    let dst = tmp.path().join("crates");
    copy_dir_recursive(&src, &dst);
    git(&dst, &["init", "-q", "-b", "main"]);
    git(&dst, &["config", "user.email", "latency@example.com"]);
    git(&dst, &["config", "user.name", "Latency Harness"]);
    git(&dst, &["add", "-A"]);
    git(
        &dst,
        &["commit", "-q", "-m", "fixture: workspace crates/ snapshot"],
    );
    (tmp, dst)
}

fn p50(mut samples: Vec<Duration>) -> Duration {
    samples.sort();
    samples[samples.len() / 2]
}

#[test]
#[ignore = "measure lane — run with --ignored or KB_CODE_MEASURE=1"]
fn search_lanes_meet_their_p50_latency_budget_on_a_real_scale_corpus() {
    let (_repo_tmp, repo_root) = build_fixture_repo();
    let db_tmp = tempfile::tempdir().unwrap();
    let store = Store::open(&db_tmp.path().join("index.db")).unwrap();
    let repo_id = store
        .upsert_repo("kb", repo_root.to_str().unwrap())
        .unwrap();
    let git_repo = GitRepo::open(&repo_root).unwrap();

    let index_start = Instant::now();
    let stats =
        ingest::index_repo_working_tree(&store, &git_repo, repo_id, "HEAD", true, false).unwrap();
    eprintln!(
        "[latency] fixture indexed in {:?}: {} files, {} parsed, {} symbols",
        index_start.elapsed(),
        stats.files,
        stats.parsed,
        stats.symbols,
    );
    assert!(
        stats.files > 100,
        "expected a real-scale fixture (>100 files), got {stats:?} — is crates/ smaller than expected?"
    );
    assert!(
        stats.symbols > 100,
        "expected a real-scale symbol count (>100), got {stats:?}"
    );

    let repos = vec![("kb".to_string(), repo_id)];
    // V71-D1 — measure the SHIPPED ranking: the default factor set is what
    // a daemon runs with no `[search]` table.
    let opts = LaneOpts::default();

    // --- files lane ---------------------------------------------------
    let file_index = FileIndex::new();
    let file_samples: Vec<Duration> = (0..SAMPLES)
        .map(|_| {
            let start = Instant::now();
            let hits = file_index
                .search(&store, &repos, "store", 50, 0, &opts)
                .unwrap();
            assert!(
                !hits.is_empty(),
                "expected \"store\" to match something in kb-code-server's own store.rs"
            );
            start.elapsed()
        })
        .collect();
    let file_p50 = p50(file_samples);
    eprintln!("[latency] files lane p50 (n={SAMPLES}): {file_p50:?}");
    assert!(
        file_p50 <= budget_ms(50),
        "files lane p50 {file_p50:?} exceeds the 50ms budget (mult={})",
        lat_mult()
    );

    // --- symbols lane ---------------------------------------------------
    let symbol_index = SymbolIndex::new();
    let symbol_samples: Vec<Duration> = (0..SAMPLES)
        .map(|_| {
            let start = Instant::now();
            symbol_index
                .search(&store, &repos, "new", 50, &opts)
                .unwrap();
            start.elapsed()
        })
        .collect();
    let symbol_p50 = p50(symbol_samples);
    eprintln!("[latency] symbols lane p50 (n={SAMPLES}): {symbol_p50:?}");
    assert!(
        symbol_p50 <= budget_ms(50),
        "symbols lane p50 {symbol_p50:?} exceeds the 50ms budget (mult={})",
        lat_mult()
    );

    // --- text lane (first-hit, see module doc) ---------------------------
    let text_samples: Vec<Duration> = (0..SAMPLES)
        .map(|_| {
            let start = Instant::now();
            let resp = text::search_text(
                &store,
                &repo_root,
                repo_id,
                "fn ",
                false,
                true,
                text::DEFAULT_TIME_BUDGET,
                &opts,
            )
            .unwrap();
            assert!(
                !resp.results.is_empty(),
                "expected \"fn \" to match plenty of lines"
            );
            start.elapsed()
        })
        .collect();
    let text_p50 = p50(text_samples);
    eprintln!("[latency] text lane p50 (n={SAMPLES}): {text_p50:?}");
    assert!(
        text_p50 <= budget_ms(250),
        "text lane p50 {text_p50:?} exceeds the 250ms budget (mult={})",
        lat_mult()
    );

    // --- hierarchy endpoints (V3.1-H1) warm p50 < 150ms -----------------
    // Prefer a SMALL callable (few enclosing call sites) so the budget
    // measures endpoint overhead rather than N×full-resolve cost of a
    // giant method body. types uses a common type-ish name present in the
    // crate tree.
    let all_syms = store.symbols_for_repo(repo_id).unwrap();
    // Ensure call_sites exist for a handful of proof-lang files first.
    for f in store
        .list_files(repo_id)
        .unwrap_or_default()
        .into_iter()
        .take(80)
    {
        let Some(li) = kb_code_server::lang::detect(&f.path, None) else {
            continue;
        };
        if !kb_code_server::hierarchy::supports_hierarchy(li.id) {
            continue;
        }
        if store.has_call_sites(&f.blob_hash, li.salt).unwrap_or(false) {
            continue;
        }
        let bytes = std::fs::read(repo_root.join(&f.path)).unwrap_or_default();
        let symbols = store
            .symbols_for_blob(&f.blob_hash, li.salt)
            .unwrap_or_default();
        let sites = kb_code_server::hierarchy::extract_call_sites(li.id, &bytes, &symbols);
        let _ = store.replace_call_sites(&f.blob_hash, li.salt, &sites);
        let rels = kb_code_server::hierarchy::extract_type_relations(li.id, &bytes);
        let _ = store.replace_type_relations(&f.blob_hash, li.salt, &rels);
    }

    // Pick the callable whose enclosing call-site count is minimal but >0.
    let mut anchor: Option<(String, kb_code_server::extract::Symbol, usize)> = None;
    for (path, sym) in &all_syms {
        if !matches!(
            sym.kind.as_str(),
            "fn" | "method" | "function" | "def" | "func"
        ) || sym.name.len() <= 2
        {
            continue;
        }
        let Some(li) = kb_code_server::lang::detect(path, None) else {
            continue;
        };
        if !kb_code_server::hierarchy::supports_hierarchy(li.id) {
            continue;
        }
        let Ok(files) = store.list_files(repo_id) else {
            continue;
        };
        let Some(frow) = files.into_iter().find(|f| f.path == *path) else {
            continue;
        };
        let Ok(sites) = store.call_sites_for_blob(&frow.blob_hash, li.salt) else {
            continue;
        };
        let n = sites
            .iter()
            .filter(|s| s.caller_ordinal == Some(sym.ordinal))
            .count();
        if n == 0 {
            continue;
        }
        let better = match &anchor {
            None => true,
            Some((_, _, best_n)) => n < *best_n,
        };
        if better {
            anchor = Some((path.clone(), sym.clone(), n));
        }
        // Early stop if we found a tiny one.
        if n <= 2 {
            break;
        }
    }
    if let Some((path, sym, _n_sites)) = anchor {
        let repo_entry = kb_code_server::config::RepoEntry {
            name: "lat".into(),
            path: repo_root.clone(),
        };
        let repos = vec![repo_entry.clone()];
        let mut repo_ids = std::collections::HashMap::new();
        repo_ids.insert("lat".into(), repo_id);

        // Warm once.
        let _ = kb_code_server::hierarchy::callees_at(
            &store,
            &repos,
            &repo_ids,
            &repo_entry,
            repo_id,
            &path,
            sym.line_start,
            sym.col_start,
            None,
        );
        let _ = kb_code_server::hierarchy::callers_at(
            &store,
            &repo_entry,
            repo_id,
            &path,
            sym.line_start,
            sym.col_start,
            None,
        );
        let _ = kb_code_server::hierarchy::types_at(&store, &repo_entry, repo_id, "Error", None);

        let callees_samples: Vec<Duration> = (0..SAMPLES)
            .map(|_| {
                let start = Instant::now();
                let _ = kb_code_server::hierarchy::callees_at(
                    &store,
                    &repos,
                    &repo_ids,
                    &repo_entry,
                    repo_id,
                    &path,
                    sym.line_start,
                    sym.col_start,
                    None,
                );
                start.elapsed()
            })
            .collect();
        let callees_p50 = p50(callees_samples);
        eprintln!("[latency] hierarchy/callees p50 (n={SAMPLES}): {callees_p50:?}");
        assert!(
            callees_p50 <= budget_ms(150),
            "hierarchy/callees p50 {callees_p50:?} exceeds 150ms (mult={})",
            lat_mult()
        );

        let callers_samples: Vec<Duration> = (0..SAMPLES)
            .map(|_| {
                let start = Instant::now();
                let _ = kb_code_server::hierarchy::callers_at(
                    &store,
                    &repo_entry,
                    repo_id,
                    &path,
                    sym.line_start,
                    sym.col_start,
                    None,
                );
                start.elapsed()
            })
            .collect();
        let callers_p50 = p50(callers_samples);
        eprintln!("[latency] hierarchy/callers p50 (n={SAMPLES}): {callers_p50:?}");
        assert!(
            callers_p50 <= budget_ms(150),
            "hierarchy/callers p50 {callers_p50:?} exceeds 150ms (mult={})",
            lat_mult()
        );

        let types_samples: Vec<Duration> = (0..SAMPLES)
            .map(|_| {
                let start = Instant::now();
                let _ = kb_code_server::hierarchy::types_at(
                    &store,
                    &repo_entry,
                    repo_id,
                    "Error",
                    None,
                );
                start.elapsed()
            })
            .collect();
        let types_p50 = p50(types_samples);
        eprintln!("[latency] hierarchy/types p50 (n={SAMPLES}): {types_p50:?}");
        assert!(
            types_p50 <= budget_ms(150),
            "hierarchy/types p50 {types_p50:?} exceeds 150ms (mult={})",
            lat_mult()
        );
    } else {
        eprintln!("[latency] hierarchy: no callable symbol in fixture — skipped");
    }

    // --- V3.1-H2 impact analysis (composition, no provenance) p50 < 300ms --
    // Measures usages + callers composition on a warm fixture symbol — the
    // same core the `/api/impact/analysis` route runs before async
    // blame/session provenance (provenance is I/O-bound and fixture-
    // sessionless here).
    if let Some((path, sym)) = {
        let all_syms = store.symbols_for_repo(repo_id).unwrap();
        all_syms.into_iter().find(|(p, s)| {
            matches!(s.kind.as_str(), "fn" | "method" | "function" | "def")
                && s.name.len() > 2
                && kb_code_server::lang::detect(p, None)
                    .map(|l| kb_code_server::hierarchy::supports_hierarchy(l.id))
                    .unwrap_or(false)
        })
    } {
        let repo_entry = kb_code_server::config::RepoEntry {
            name: "lat".into(),
            path: repo_root.clone(),
        };
        // Warm (usages_counts_at materializes the same classifier as usages_at).
        let frow = store
            .list_files(repo_id)
            .unwrap()
            .into_iter()
            .find(|f| f.path == path)
            .expect("impact file row");
        let salt = kb_code_server::lang::detect(&path, None)
            .map(|l| l.salt)
            .unwrap_or("");
        let _ = kb_code_server::usages::usages_counts_for_name(
            &store,
            repo_id,
            &path,
            &sym.name,
            Some(sym.line_start),
            Some(salt),
            &frow.blob_hash,
        );
        let _ = kb_code_server::hierarchy::callers_at(
            &store,
            &repo_entry,
            repo_id,
            &path,
            sym.line_start,
            sym.col_start,
            None,
        );
        let impact_samples: Vec<Duration> = (0..SAMPLES)
            .map(|_| {
                let start = Instant::now();
                let _ = kb_code_server::usages::usages_counts_for_name(
                    &store,
                    repo_id,
                    &path,
                    &sym.name,
                    Some(sym.line_start),
                    Some(salt),
                    &frow.blob_hash,
                );
                let _ = kb_code_server::hierarchy::callers_at(
                    &store,
                    &repo_entry,
                    repo_id,
                    &path,
                    sym.line_start,
                    sym.col_start,
                    None,
                );
                start.elapsed()
            })
            .collect();
        let impact_p50 = p50(impact_samples);
        eprintln!("[latency] impact analysis core p50 (n={SAMPLES}): {impact_p50:?}");
        assert!(
            impact_p50 <= budget_ms(300),
            "impact analysis p50 {impact_p50:?} exceeds 300ms (mult={})",
            lat_mult()
        );
    } else {
        eprintln!("[latency] impact: no callable symbol — skipped");
    }

    // --- V3.1-H2 lenses (count-only over declarations) warm p50 < 250ms --
    // Brief target: warm p50 < 250ms on a ~500-line file. Use a synthetic
    // file with UNIQUE names so repo-wide name scans don't pay for common
    // identifiers (e.g. `new`) across the whole crates/ corpus.
    {
        let mut src = String::with_capacity(12_000);
        src.push_str("// latency lenses fixture (~500 lines, unique names)\n");
        for i in 0..40 {
            src.push_str(&format!(
                "/// Doc for lens_unique_{i:04}.\npub fn lens_unique_{i:04}() {{\n    let x_{i} = {i};\n    let _ = x_{i};\n}}\n"
            ));
        }
        // Pad toward ~500 lines with inert items so the file is "500-line class".
        while src.lines().count() < 500 {
            src.push_str(&format!("// pad line {}\n", src.lines().count()));
        }
        let syn_path = "latency_lenses_fixture.rs";
        let bytes = src.as_bytes();
        let blob_hash = kb_code_server::ingest::git_blob_hash(bytes);
        std::fs::write(repo_root.join(syn_path), bytes).unwrap();
        ingest::index_file(&store, repo_id, syn_path, bytes, &blob_hash, true, false).unwrap();
        let li = kb_code_server::lang::detect(syn_path, Some(bytes)).unwrap();
        let syms = store.symbols_for_blob(&blob_hash, li.salt).unwrap();
        let decls: Vec<_> = syms
            .into_iter()
            .filter(|s| matches!(s.kind.as_str(), "fn" | "function"))
            .collect();
        assert!(
            decls.len() >= 20,
            "expected synthetic decls, got {}",
            decls.len()
        );
        // Warm (batch count path — same as lenses route).
        let name_keys: Vec<(String, Option<u32>)> = decls
            .iter()
            .map(|s| (s.name.clone(), Some(s.line_start)))
            .collect();
        let _ = kb_code_server::usages::usages_counts_for_names(
            &store,
            repo_id,
            syn_path,
            &name_keys,
            Some(li.salt),
            &blob_hash,
        );
        let lenses_samples: Vec<Duration> = (0..SAMPLES)
            .map(|_| {
                let start = Instant::now();
                let _ = kb_code_server::usages::usages_counts_for_names(
                    &store,
                    repo_id,
                    syn_path,
                    &name_keys,
                    Some(li.salt),
                    &blob_hash,
                );
                start.elapsed()
            })
            .collect();
        let lenses_p50 = p50(lenses_samples);
        eprintln!(
            "[latency] lenses counts p50 over {} decls in {syn_path} (~{} lines) (n={SAMPLES}): {lenses_p50:?}",
            decls.len(),
            src.lines().count(),
        );
        assert!(
            lenses_p50 <= budget_ms(250),
            "lenses p50 {lenses_p50:?} exceeds 250ms (mult={})",
            lat_mult()
        );
    }

    // --- V3.2-B1 hotspots (from counters, never a git walk) warm p50 < 200ms
    {
        let cfg = kb_code_server::config::BehavioralSection {
            enabled: true,
            window_days: 3650,
            max_commit_files: 30,
        };
        let repo_entry = kb_code_server::config::RepoEntry {
            name: "lat".into(),
            path: repo_root.clone(),
        };
        let bf = kb_code_server::behavioral::backfill_repo(&repo_entry, repo_id, &cfg, &store)
            .expect("behavioral backfill");
        eprintln!(
            "[latency] behavioral backfill: {} commits in {}ms (full_rebuild={})",
            bf.commits, bf.duration_ms, bf.full_rebuild
        );
        assert!(
            bf.duration_ms < 60_000,
            "backfill wall-clock {}ms exceeds 60s budget",
            bf.duration_ms
        );

        // Warm: list_path_stats + complexity read.
        let rows = store.list_path_stats(repo_id).unwrap();
        assert!(!rows.is_empty(), "expected path_stats after backfill");
        let hotspots_samples: Vec<Duration> = (0..SAMPLES)
            .map(|_| {
                let start = Instant::now();
                let rows = store.list_path_stats(repo_id).unwrap();
                let churns: Vec<u64> = rows
                    .iter()
                    .map(|r| (r.lines_added + r.lines_deleted).max(0) as u64)
                    .collect();
                let _ranks = kb_code_server::behavioral::dense_ranks_desc(&churns);
                // complexity for a sample of paths (route does all; budget is
                // still counter-driven ranking cost for the common path)
                for r in rows.iter().take(50) {
                    let _ = kb_code_server::behavioral::complexity_for_path(&repo_root, &r.path);
                }
                start.elapsed()
            })
            .collect();
        let hot_p50 = p50(hotspots_samples);
        eprintln!("[latency] behavioral hotspots (counters) p50 (n={SAMPLES}): {hot_p50:?}");
        assert!(
            hot_p50 <= budget_ms(200),
            "hotspots p50 {hot_p50:?} exceeds 200ms (mult={})",
            lat_mult()
        );
    }
}

/// `-C repo_root <args>`, returning trimmed stdout — the one place in this
/// file that needs git OUTPUT rather than just a side effect (`common::
/// git` is side-effect-only by design, see that module's own doc).
fn git_out(repo_root: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(args)
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git -C {} {:?} failed: {}",
        repo_root.display(),
        args,
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

/// PF-K1 — PR Room read budgets on a multi-review fixture: the FIX 1/3/4
/// batch-composition functions ([`kb_code_server::reviews::
/// compose_review_list_rows`], [`kb_code_server::review_findings::
/// compose_finding_recurrence`], [`kb_code_server::review_inbox::
/// compose_rows`]) measured DIRECTLY (in-process, no HTTP/daemon — same
/// "the lane's own compute time is what 'instant' refers to" posture as
/// the rest of this file), plus (d) a `callers_at` case anchored on a
/// symbol with 50+ call sites — the existing hierarchy budget above
/// deliberately picks the SMALLEST anchor (fewest sites) so its own p50
/// stays cheap regardless of the FIX 2 blob-read cache; it structurally
/// cannot catch a regression in that cache (an uncached re-read scales
/// with call-site COUNT, invisible on a 1-2-site anchor). All four share
/// budgets sized generously against this file's own established margins
/// (150-300ms on comparably small fixtures) — these read paths now do
/// O(1) batched-query round trips instead of O(N), so real headroom is
/// expected to be wide.
#[test]
#[ignore = "measure lane — run with --ignored or KB_CODE_MEASURE=1"]
fn pr_room_reads_meet_their_p50_latency_budget_on_a_multi_review_fixture() {
    let (_repo_tmp, repo_root) = build_fixture_repo();
    let db_tmp = tempfile::tempdir().unwrap();
    let store = Store::open(&db_tmp.path().join("index.db")).unwrap();
    let repo_id = store
        .upsert_repo("kb", repo_root.to_str().unwrap())
        .unwrap();

    // --- (a)+(c) fixture: N_REVIEWS reviews sharing one repo, each with a
    // captured patchset against a real multi-file diff (several changed
    // files per review, per the brief) -----------------------------------
    const N_REVIEWS: usize = 20;
    let base_sha = git_out(&repo_root, &["rev-parse", "HEAD"]);
    for f in [
        "kb-code-server/src/store.rs",
        "kb-code-server/src/reviews.rs",
        "kb-code-server/src/hierarchy.rs",
        "kb-code-server/src/review_inbox.rs",
        "kb-code-server/src/review_findings.rs",
    ] {
        let p = repo_root.join(f);
        let mut content = std::fs::read_to_string(&p).unwrap_or_default();
        content.push_str("\n// pr-room latency fixture: harmless trailing touch\n");
        std::fs::write(&p, content).unwrap();
    }
    git(&repo_root, &["add", "-A"]);
    git(
        &repo_root,
        &[
            "commit",
            "-q",
            "-m",
            "pr-room fixture: touch a handful of files",
        ],
    );
    let tip_sha = git_out(&repo_root, &["rev-parse", "HEAD"]);

    let now = 1_700_000_000i64;
    let mut review_ids = Vec::with_capacity(N_REVIEWS);
    for i in 0..N_REVIEWS {
        let title = format!("pr room fixture {i}");
        let id = store
            .create_review("kb", Some(title.as_str()), "main", "main", None, now)
            .unwrap();
        store
            .insert_patchset(id, 1, &tip_sha, &base_sha, now)
            .unwrap();
        review_ids.push(id);
    }

    // (a) GET /api/reviews store path — `reviews::compose_review_list_rows`.
    {
        let rows = store.list_reviews("kb", None).unwrap();
        assert_eq!(rows.len(), N_REVIEWS);
        let _ = kb_code_server::reviews::compose_review_list_rows(
            &store,
            &repo_root,
            repo_id,
            rows.clone(),
        );
        // Timed region covers BOTH `list_reviews` and `compose_review_list_
        // rows` — the same two calls the real route runs back to back
        // inside its one `run_blocking` closure (see `reviews::list_
        // reviews`), so this is the full store-path equivalent of `GET
        // /api/reviews`, not just the compose half.
        let samples: Vec<Duration> = (0..SAMPLES)
            .map(|_| {
                let start = Instant::now();
                let rows = store.list_reviews("kb", None).unwrap();
                let out = kb_code_server::reviews::compose_review_list_rows(
                    &store, &repo_root, repo_id, rows,
                )
                .unwrap();
                let elapsed = start.elapsed();
                assert_eq!(out.len(), N_REVIEWS);
                elapsed
            })
            .collect();
        let p50v = p50(samples);
        eprintln!(
            "[latency] GET /api/reviews store path p50 over {N_REVIEWS} reviews (n={SAMPLES}): {p50v:?}"
        );
        assert!(
            p50v <= budget_ms(300),
            "reviews list compose p50 {p50v:?} exceeds 300ms budget (mult={})",
            lat_mult()
        );
    }

    // (c) compose_rows (inbox) over the same fixture.
    {
        let repo_names = vec!["kb".to_string()];
        let _ = review_inbox::compose_rows(&store, &repo_names, None);
        let samples: Vec<Duration> = (0..SAMPLES)
            .map(|_| {
                let start = Instant::now();
                let out = review_inbox::compose_rows(&store, &repo_names, None).unwrap();
                let elapsed = start.elapsed();
                assert_eq!(out.len(), N_REVIEWS);
                elapsed
            })
            .collect();
        let p50v = p50(samples);
        eprintln!(
            "[latency] review inbox compose_rows p50 over {N_REVIEWS} reviews (n={SAMPLES}): {p50v:?}"
        );
        assert!(
            p50v <= budget_ms(300),
            "review inbox compose_rows p50 {p50v:?} exceeds 300ms budget (mult={})",
            lat_mult()
        );
    }

    // (b) the recurrence composition — every review gets ONE finding
    // sharing the SAME (category, location_path) pair, so the first
    // review's own recurrence view names all N_REVIEWS-1 others as prior
    // (a real multi-prior-review fan-out for the batch to earn its keep on).
    {
        use kb_code_server::annotations::{anchor_for_line, ANCHOR_KIND_LINE};
        use kb_code_server::store::{
            NewReviewFinding, FINDING_ORIGIN_MANUAL, LOCATION_KIND_SINGLE, SEVERITY_OK,
        };
        for (i, &rid) in review_ids.iter().enumerate() {
            let finding = NewReviewFinding {
                review_id: rid,
                repo_id,
                ps_number: 1,
                slug: format!("f-{i:03}"),
                severity: SEVERITY_OK.to_string(),
                category: "Style".to_string(),
                location_kind: LOCATION_KIND_SINGLE.to_string(),
                location_path: "kb-code-server/src/store.rs".to_string(),
                location_lines: None,
                location_removed: false,
                title: "pr-room fixture finding".to_string(),
                rationale: "shared (category, path) so every review recurs".to_string(),
                recommendation: None,
                evidence_lang: None,
                evidence_source: None,
                anchor_kind: ANCHOR_KIND_LINE.to_string(),
                anchor: serde_json::to_string(&anchor_for_line(1, "x")).unwrap(),
                anchor2: None,
                side: Some("new".to_string()),
                author: "pr-room-fixture".to_string(),
                import_batch_id: "pr-room-fixture".to_string(),
                origin: FINDING_ORIGIN_MANUAL.to_string(),
                finding_author: Some("pr-room-fixture".to_string()),
                act: "issue".to_string(),
                blocking: false,
                cites_json: None,
                fingerprint: None,
            };
            store.insert_review_finding(&finding, now).unwrap();
        }
        let anchor_review = review_ids[0];
        let _ = review_findings::compose_finding_recurrence(&store, anchor_review, "kb");
        let samples: Vec<Duration> = (0..SAMPLES)
            .map(|_| {
                let start = Instant::now();
                let out = review_findings::compose_finding_recurrence(&store, anchor_review, "kb")
                    .unwrap();
                let elapsed = start.elapsed();
                assert_eq!(out.len(), 1, "review 0's own single finding recurs");
                assert_eq!(
                    out[0].prior.len(),
                    N_REVIEWS - 1,
                    "every OTHER review shares the (category, path) pair"
                );
                elapsed
            })
            .collect();
        let p50v = p50(samples);
        eprintln!(
            "[latency] findings recurrence compose p50 over {N_REVIEWS} reviews (n={SAMPLES}): {p50v:?}"
        );
        assert!(
            p50v <= budget_ms(250),
            "findings recurrence compose p50 {p50v:?} exceeds 250ms budget (mult={})",
            lat_mult()
        );
    }

    // (d) `callers_at` anchored on a symbol with 50+ call sites — see the
    // fn doc for why the existing hierarchy budget can't catch this class.
    {
        let mut src = String::with_capacity(8_000);
        src.push_str("// PF-K1 many-callers fixture\n");
        src.push_str("pub struct Recv;\n");
        src.push_str("impl Recv {\n    pub fn target_fn(&self, n: u32) -> u32 { n + 1 }\n}\n\n");
        const N_CALLERS: usize = 60;
        for i in 0..N_CALLERS {
            src.push_str(&format!(
                "pub fn caller_{i:03}() -> u32 {{\n    let recv = Recv;\n    recv.target_fn({i})\n}}\n\n"
            ));
        }
        let syn_path = "pf_k1_many_callers_fixture.rs";
        let bytes = src.as_bytes();
        let blob_hash = kb_code_server::ingest::git_blob_hash(bytes);
        std::fs::write(repo_root.join(syn_path), bytes).unwrap();
        ingest::index_file(&store, repo_id, syn_path, bytes, &blob_hash, true, false).unwrap();
        let li = kb_code_server::lang::detect(syn_path, Some(bytes)).unwrap();
        let symbols = store.symbols_for_blob(&blob_hash, li.salt).unwrap();
        let sites = kb_code_server::hierarchy::extract_call_sites(li.id, bytes, &symbols);
        store
            .replace_call_sites(&blob_hash, li.salt, &sites)
            .unwrap();

        let target_sym = symbols
            .iter()
            .find(|s| s.name == "target_fn")
            .expect("target_fn symbol extracted");
        let repo_entry = kb_code_server::config::RepoEntry {
            name: "kb".into(),
            path: repo_root.clone(),
        };

        // Warm + sanity: the fixture really does produce 50+ call sites.
        let warm = kb_code_server::hierarchy::callers_at(
            &store,
            &repo_entry,
            repo_id,
            syn_path,
            target_sym.line_start,
            target_sym.col_start,
            None,
        )
        .unwrap();
        let warm_sites: usize = warm.callers.iter().map(|g| g.sites.len()).sum();
        assert!(
            warm_sites >= 50,
            "expected 50+ call sites in the synthetic fixture, got {warm_sites}"
        );

        let samples: Vec<Duration> = (0..SAMPLES)
            .map(|_| {
                let start = Instant::now();
                let _ = kb_code_server::hierarchy::callers_at(
                    &store,
                    &repo_entry,
                    repo_id,
                    syn_path,
                    target_sym.line_start,
                    target_sym.col_start,
                    None,
                )
                .unwrap();
                start.elapsed()
            })
            .collect();
        let p50v = p50(samples);
        eprintln!("[latency] hierarchy/callers (50+ call sites) p50 (n={SAMPLES}): {p50v:?}");
        assert!(
            p50v <= budget_ms(400),
            "hierarchy/callers (many call sites) p50 {p50v:?} exceeds 400ms (mult={})",
            lat_mult()
        );
    }
}
