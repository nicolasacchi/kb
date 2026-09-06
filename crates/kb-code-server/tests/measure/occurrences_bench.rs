//! B2 measurement — the number this phase's "widen to more languages" call
//! is gated on: occurrences row count, sqlite size delta, and wall-clock
//! index-time delta for a full walk of a real-scale fixture, with and
//! without the B2 occurrences pass. Mirrors `tests/measure/latency.rs`'s own
//! fixture-building convention exactly (copy this workspace's `crates/`
//! tree into a fresh git repo, walk it via the real
//! `ingest::index_repo_working_tree`) rather than a synthetic corpus.
//!
//! `#[ignore]` — a bench-style measurement, not a routine CI assertion (no
//! numeric budget is enforced here; it's read by a human deciding whether
//! to widen `lang::TOKEN_LEVEL_LANG_IDS`). Run explicitly:
//! `cargo test -p kb-code-server --test measure occurrences_bench -- --ignored --nocapture`.
//!
//! Store B (the "before B2" baseline) is a hand-rolled, symbols+highlights-
//! ONLY walk over the SAME git tree, reusing every public `ingest`/`store`
//! primitive `ingest::index_file` itself uses (the tier checks, `lang::
//! detect`, `extract::extract_symbols`, `highlight::extract_highlights`) but
//! skipping the `occurrences::extract_occurrences` call — so Store A and
//! Store B end up with byte-identical `files`/`symbols`/`highlights` rows
//! (including B2's `signature`/`doc` columns, which `extract_symbols` always
//! populates for the four token-level languages regardless of the
//! occurrences pass) and differ ONLY in the `occurrences` table — an honest,
//! isolated before/after comparison.

use crate::common::git;
use kb_code_server::extract;
use kb_code_server::git::{EntryKind, GitRepo};
use kb_code_server::highlight;
use kb_code_server::ingest;
use kb_code_server::lang;
use kb_code_server::store::Store;
use std::path::Path;
use std::time::Instant;

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
    }
}

/// Same fixture `tests/measure/latency.rs` builds: `CARGO_MANIFEST_DIR/..` (this
/// workspace's `crates/` tree) copied into a fresh git repo and committed.
fn build_fixture_repo() -> (tempfile::TempDir, std::path::PathBuf) {
    let src = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("kb-code-server has a parent dir (crates/)")
        .to_path_buf();
    let tmp = tempfile::tempdir().unwrap();
    let dst = tmp.path().join("crates");
    copy_dir_recursive(&src, &dst);
    git(&dst, &["init", "-q", "-b", "main"]);
    git(&dst, &["config", "user.email", "bench@example.com"]);
    git(&dst, &["config", "user.name", "Occurrences Bench"]);
    git(&dst, &["add", "-A"]);
    git(
        &dst,
        &["commit", "-q", "-m", "fixture: workspace crates/ snapshot"],
    );
    (tmp, dst)
}

/// The "before B2" walk: identical tier classification to
/// `ingest::index_file`, but never calls `occurrences::extract_occurrences`.
/// Reuses every public primitive that fn itself uses.
fn walk_symbols_only(store: &Store, repo: &GitRepo, repo_id: i64) {
    fn walk_dir(store: &Store, repo: &GitRepo, repo_id: i64, dir_path: &str) {
        for entry in repo.list_tree("HEAD", dir_path).unwrap() {
            let full_path = if dir_path.is_empty() {
                entry.name.clone()
            } else {
                format!("{dir_path}/{}", entry.name)
            };
            match entry.kind {
                EntryKind::Dir => walk_dir(store, repo, repo_id, &full_path),
                EntryKind::File => {
                    let Ok(bytes) = repo.read_blob("HEAD", &full_path, ingest::MAX_PARSE_BYTES)
                    else {
                        continue;
                    };
                    index_file_symbols_only(store, repo_id, &full_path, &bytes, &entry.oid);
                }
                EntryKind::Symlink | EntryKind::Submodule => {}
            }
        }
    }
    walk_dir(store, repo, repo_id, "");
}

fn index_file_symbols_only(store: &Store, repo_id: i64, path: &str, bytes: &[u8], blob_hash: &str) {
    let size = bytes.len() as u64;
    if size > ingest::MAX_PARSE_BYTES || std::str::from_utf8(bytes).is_err() {
        store
            .upsert_file(repo_id, path, blob_hash, "binary", size)
            .unwrap();
        return;
    }
    let Some(lang_info) = lang::detect(path, Some(bytes)) else {
        store
            .upsert_file(repo_id, path, blob_hash, "unknown", size)
            .unwrap();
        return;
    };
    store
        .upsert_file(repo_id, path, blob_hash, lang_info.id, size)
        .unwrap();
    if store.has_symbols(blob_hash, lang_info.salt).unwrap() {
        return;
    }
    let Ok(symbols) = extract::extract_symbols(lang_info.id, bytes) else {
        return;
    };
    let Ok(spans) = highlight::extract_highlights(lang_info.id, bytes) else {
        return;
    };
    store
        .replace_symbols(blob_hash, lang_info.salt, &symbols)
        .unwrap();
    store
        .put_highlights(blob_hash, lang_info.salt, &spans)
        .unwrap();
}

fn occurrences_row_count(db_path: &Path) -> u64 {
    let conn = rusqlite::Connection::open(db_path).unwrap();
    conn.query_row("SELECT COUNT(*) FROM occurrences", [], |r| r.get(0))
        .unwrap()
}

#[test]
#[ignore]
fn b2_occurrences_measurement_on_a_real_scale_corpus() {
    let (_repo_tmp, repo_root) = build_fixture_repo();
    let git_repo = GitRepo::open(&repo_root).unwrap();

    // --- Store A: the REAL, current pipeline (symbols+highlights+occurrences) ---
    let db_a_tmp = tempfile::tempdir().unwrap();
    let db_a_path = db_a_tmp.path().join("index.db");
    let store_a = Store::open(&db_a_path).unwrap();
    let repo_a_id = store_a
        .upsert_repo("kb", repo_root.to_str().unwrap())
        .unwrap();
    let start_a = Instant::now();
    let stats_a = ingest::index_repo_working_tree(
        &store_a,
        &git_repo,
        repo_a_id,
        "HEAD",
        true,
        false,
        &kb_code_server::comments::KeywordSet::defaults(),
    )
    .unwrap();
    let elapsed_a = start_a.elapsed();
    drop(store_a); // flush/close before stat-ing the file
    let size_a = std::fs::metadata(&db_a_path).unwrap().len();
    let occ_count = occurrences_row_count(&db_a_path);

    // --- Store B: the "before B2" baseline (symbols+highlights ONLY) ---
    let db_b_tmp = tempfile::tempdir().unwrap();
    let db_b_path = db_b_tmp.path().join("index.db");
    let store_b = Store::open(&db_b_path).unwrap();
    let repo_b_id = store_b
        .upsert_repo("kb", repo_root.to_str().unwrap())
        .unwrap();
    let start_b = Instant::now();
    walk_symbols_only(&store_b, &git_repo, repo_b_id);
    let elapsed_b = start_b.elapsed();
    drop(store_b);
    let size_b = std::fs::metadata(&db_b_path).unwrap().len();

    eprintln!(
        "[B2 bench] files={} parsed={} symbols={}",
        stats_a.files, stats_a.parsed, stats_a.symbols
    );
    eprintln!("[B2 bench] occurrences rows = {occ_count}");
    eprintln!(
        "[B2 bench] index.db size: with-occurrences={size_a}B without={size_b}B delta={}B ({:+.1}%)",
        size_a as i64 - size_b as i64,
        (size_a as f64 / size_b as f64 - 1.0) * 100.0
    );
    eprintln!(
        "[B2 bench] wall-clock: with-occurrences={elapsed_a:?} without={elapsed_b:?} delta={:?} ({:+.1}%)",
        elapsed_a.saturating_sub(elapsed_b),
        (elapsed_a.as_secs_f64() / elapsed_b.as_secs_f64() - 1.0) * 100.0
    );

    assert!(
        stats_a.files > 100,
        "expected a real-scale fixture (>100 files), got {stats_a:?}"
    );
    assert!(occ_count > 0, "expected at least one occurrences row");
}
