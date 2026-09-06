//! `GET /api/impact?repo=&path=[&limit=]` + `kb-code impact` (W5.2) — the
//! co-change NEIGHBORHOOD for one file: which other files most often
//! change alongside it, plus a cheap textual "who mentions me" signal.
//! Both halves are APPROXIMATE (`ImpactOut::approximate` is always `true`):
//! neither is a real dependency-graph resolution.
//!
//! - **co_changes** — walk `git log --format=%H --max-count
//!   [`MAX_LOG_COMMITS`] -- <path>` to get the commits that touched `path`,
//!   then for EACH of those, `git show --name-only --format=` to list
//!   every OTHER file it also touched. Two-step BY NECESSITY, not
//!   laziness: a `-- <path>` pathspec on `git log --name-only` restricts
//!   the FILE LISTING to `path` itself too (git applies the diff filter to
//!   `--name-only`'s output, not just to commit selection), so there is no
//!   single-subprocess way to get "every file touched alongside `path`" —
//!   see this module's tests for a pinned repro of that behavior.
//! - **mentions** — a text-search pass (`search::text::search_text`,
//!   reused verbatim) for `path`'s basename STEM (`"bar.rs"` → `"bar"`),
//!   word-boundary + case-sensitive, across the repo's working tree — a
//!   file whose CONTENT names this module is a cheap reverse-dependency
//!   proxy, no real import-graph resolution.
//!
//! Ranked `(co_changes desc, mentions desc, path asc)`; `path` itself is
//! always excluded from its own results.

use crate::routes::{find_repo, safe_rel_path, ApiError};
use crate::search;
use crate::state::SharedState;
use crate::store::{Store, StoreBlocking};
use axum::extract::{Query, State};
use axum::http::{header, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

pub const SCHEMA: &str = "impact/1";
/// `git log`'s own bound — mirrors `provenance::report::DEFAULT_MAX_COUNT`'s
/// "generous enough for most repos in one pass" reasoning, applied to a
/// single-file history walk instead of the whole repo's.
pub const MAX_LOG_COMMITS: usize = 500;
pub const DEFAULT_LIMIT: usize = 50;
pub const MAX_LIMIT: usize = 200;

#[derive(Debug, Deserialize)]
pub struct ImpactParams {
    pub repo: String,
    pub path: String,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ImpactHit {
    pub path: String,
    pub co_changes: u32,
    pub mentions: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ImpactOut {
    pub schema: &'static str,
    pub repo: String,
    pub path: String,
    /// How many commits the co-change walk actually visited (bounded by
    /// [`MAX_LOG_COMMITS`]).
    pub commits_walked: usize,
    /// `commits_walked == MAX_LOG_COMMITS` — there may be MORE co-change
    /// history past the walked window (same best-effort signal as
    /// `provenance::report::ProvenanceReportOut::truncated`, not a proof).
    pub truncated: bool,
    pub results: Vec<ImpactHit>,
    /// Always `true` — see the module doc: neither `co_changes` nor
    /// `mentions` is a real dependency-graph resolution.
    pub approximate: bool,
}

/// `GET /api/impact?repo=&path=[&limit=]`.
pub async fn impact_route(
    State(state): State<SharedState>,
    Query(params): Query<ImpactParams>,
) -> Result<impl IntoResponse, ApiError> {
    let (repo, repo_id) = find_repo(&state, &params.repo)?;
    let path = safe_rel_path(&params.path)?.to_string();
    let limit = params.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);

    let out = build_impact(&state.store, &repo.path, &repo.name, repo_id, &path, limit).await?;
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(out)))
}

/// `pub(crate)` — narrow deps (`&Store` + a repo root, not `SharedState`)
/// so this is directly unit-testable against a real (small) git fixture
/// repo with no daemon boot.
pub(crate) async fn build_impact(
    store: &Arc<Store>,
    repo_root: &Path,
    repo_name: &str,
    repo_id: i64,
    path: &str,
    limit: usize,
) -> Result<ImpactOut, ApiError> {
    let (commits_walked, co_changes) = walk_co_changes(repo_root, path).await?;
    let truncated = commits_walked >= MAX_LOG_COMMITS;
    // `mention_counts` (a text-grep) keeps its own `&Store` signature — the
    // wrap happens here: a fresh blocking-pool round trip after the
    // `walk_co_changes` await above (store.rs's 2026-08-31 incident note).
    let repo_root_c = repo_root.to_path_buf();
    let path_c = path.to_string();
    let mentions = store
        .run_blocking(move |store| mention_counts(store, repo_id, &repo_root_c, &path_c))
        .await?;

    let mut merged: BTreeMap<String, (u32, u32)> = BTreeMap::new();
    for (p, n) in co_changes {
        merged.entry(p).or_insert((0, 0)).0 = n;
    }
    for (p, n) in mentions {
        merged.entry(p).or_insert((0, 0)).1 = n;
    }
    merged.remove(path);

    let mut results: Vec<ImpactHit> = merged
        .into_iter()
        .map(|(path, (co_changes, mentions))| ImpactHit {
            path,
            co_changes,
            mentions,
        })
        .collect();
    results.sort_by(|a, b| {
        b.co_changes
            .cmp(&a.co_changes)
            .then_with(|| b.mentions.cmp(&a.mentions))
            .then_with(|| a.path.cmp(&b.path))
    });
    results.truncate(limit);

    Ok(ImpactOut {
        schema: SCHEMA,
        repo: repo_name.to_string(),
        path: path.to_string(),
        commits_walked,
        truncated,
        results,
        approximate: true,
    })
}

/// Blocking two-step walk — see the module doc for why this is genuinely
/// two `git` calls per commit, not one call total. Run entirely inside ONE
/// `spawn_blocking` closure, mirroring every other git-subprocess call in
/// this crate (`provenance::report::walk_commits`'s own doc has the same
/// rationale). Returns `(commits_walked, [(other_path, co_change_count)])`.
async fn walk_co_changes(
    repo_root: &Path,
    path: &str,
) -> Result<(usize, Vec<(String, u32)>), ApiError> {
    let repo_root = repo_root.to_path_buf();
    let path = path.to_string();
    tokio::task::spawn_blocking(move || -> Result<(usize, Vec<(String, u32)>), ApiError> {
        let shas = commits_touching(&repo_root, &path)?;
        let mut counts: BTreeMap<String, u32> = BTreeMap::new();
        for sha in &shas {
            for file in commit_file_list(&repo_root, sha)? {
                if file != path {
                    *counts.entry(file).or_insert(0) += 1;
                }
            }
        }
        Ok((shas.len(), counts.into_iter().collect()))
    })
    .await
    .map_err(|e| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("git log task panicked: {e}"),
        )
    })?
}

fn run_git(repo_root: &Path, args: &[&str]) -> Result<Vec<u8>, ApiError> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(args)
        .output()
        .map_err(|e| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("spawn git {args:?}: {e}"),
            )
        })?;
    if !out.status.success() {
        return Err(ApiError::bad_request(
            String::from_utf8_lossy(&out.stderr).trim().to_string(),
        ));
    }
    Ok(out.stdout)
}

/// `git log --format=%H --max-count <MAX_LOG_COMMITS> -- <path>` — every
/// commit (newest first, git's own default order) that touched `path`,
/// bounded at [`MAX_LOG_COMMITS`].
fn commits_touching(repo_root: &Path, path: &str) -> Result<Vec<String>, ApiError> {
    let max = MAX_LOG_COMMITS.to_string();
    let stdout = run_git(
        repo_root,
        &["log", "--format=%H", "--max-count", &max, "--", path],
    )?;
    Ok(String::from_utf8_lossy(&stdout)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect())
}

/// `git show --name-only --format= <sha>` — every file `sha` touched, with
/// NO pathspec (unlike [`commits_touching`]'s own `git log`, this call
/// carries no `-- <path>` filter at all, so the listing is genuinely every
/// file the commit changed — see the module doc).
fn commit_file_list(repo_root: &Path, sha: &str) -> Result<Vec<String>, ApiError> {
    let stdout = run_git(repo_root, &["show", "--name-only", "--format=", sha])?;
    Ok(String::from_utf8_lossy(&stdout)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect())
}

/// The target's basename STEM (`"src/foo/bar.rs"` → `"bar"`) — see the
/// module doc's "mentions" signal. Falls back to the whole basename
/// (extension included) when the path has no extension to strip.
fn module_stem(path: &str) -> String {
    let base = Path::new(path)
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string());
    match Path::new(&base).file_stem() {
        Some(stem) => stem.to_string_lossy().into_owned(),
        None => base,
    }
}

fn mention_counts(
    store: &Store,
    repo_id: i64,
    repo_root: &Path,
    path: &str,
) -> Result<Vec<(String, u32)>, ApiError> {
    let stem = module_stem(path);
    if stem.is_empty() {
        return Ok(Vec::new());
    }
    let pattern = super::word_boundary_pattern(&stem);
    let resp = search::search_text(
        store,
        repo_root,
        repo_id,
        &pattern,
        true,
        true,
        super::TEXT_SEARCH_TIME_BUDGET,
        &search::LaneOpts::default(),
    )?;
    Ok(resp
        .results
        .into_iter()
        .filter(|f| f.path != path)
        .map(|f| (f.path, f.matches.len() as u32))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn git(dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .expect("git runs");
        assert!(
            out.status.success(),
            "git -C {} {:?} failed: {}",
            dir.display(),
            args,
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn init_repo() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        git(dir, &["init", "-q", "-b", "main"]);
        git(dir, &["config", "user.email", "test@example.com"]);
        git(dir, &["config", "user.name", "Test"]);
        tmp
    }

    fn commit_files(dir: &Path, files: &[(&str, &str)], message: &str) {
        for (path, content) in files {
            let abs = dir.join(path);
            if let Some(parent) = abs.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(abs, content).unwrap();
        }
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", message]);
    }

    #[test]
    fn module_stem_strips_extension_and_directories() {
        assert_eq!(module_stem("src/foo/bar.rs"), "bar");
        assert_eq!(module_stem("bar.rs"), "bar");
        assert_eq!(module_stem("Makefile"), "Makefile");
    }

    #[test]
    fn commit_file_list_lists_every_file_a_commit_touched_with_no_pathspec_filter() {
        let tmp = init_repo();
        let dir = tmp.path();
        commit_files(dir, &[("a.rs", "v1"), ("b.rs", "v1"), ("c.rs", "v1")], "c1");
        let sha = String::from_utf8(
            Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(["rev-parse", "HEAD"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap();
        let sha = sha.trim();

        let files = commit_file_list(dir, sha).unwrap();
        let mut files = files;
        files.sort();
        assert_eq!(files, vec!["a.rs", "b.rs", "c.rs"]);
    }

    #[test]
    fn a_pathspec_filtered_log_name_only_does_not_list_sibling_files() {
        // Pinned repro of the exact git behavior the module doc calls
        // out: `--name-only` under a `-- <path>` pathspec narrows the FILE
        // LISTING to that path too, not just which commits are selected —
        // this is why `walk_co_changes` needs a per-commit `git show`
        // rather than one `git log --name-only -- <path>` call.
        let tmp = init_repo();
        let dir = tmp.path();
        commit_files(dir, &[("target.rs", "v1"), ("sibling.rs", "v1")], "c1");

        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["log", "--name-only", "--format=%H", "--", "target.rs"])
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(text.contains("target.rs"));
        assert!(
            !text.contains("sibling.rs"),
            "git's own pathspec filtering must have excluded it: {text:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn walk_co_changes_tallies_a_crafted_history() {
        let tmp = init_repo();
        let dir = tmp.path();
        commit_files(dir, &[("target.rs", "v1"), ("always.rs", "v1")], "c1");
        commit_files(dir, &[("target.rs", "v2"), ("always.rs", "v2")], "c2");
        commit_files(dir, &[("target.rs", "v3"), ("sometimes.rs", "v1")], "c3");
        // A commit that never touches target.rs must not contribute at all.
        commit_files(dir, &[("unrelated.rs", "v1")], "c4: unrelated");

        let (commits_walked, co_changes) = walk_co_changes(dir, "target.rs").await.unwrap();
        assert_eq!(
            commits_walked, 3,
            "only commits touching target.rs are walked"
        );
        let map: BTreeMap<_, _> = co_changes.into_iter().collect();
        assert_eq!(map.get("always.rs"), Some(&2));
        assert_eq!(map.get("sometimes.rs"), Some(&1));
        assert!(!map.contains_key("unrelated.rs"));
        assert!(!map.contains_key("target.rs"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn build_impact_ranks_by_co_change_count_and_excludes_the_target() {
        let tmp = init_repo();
        let dir = tmp.path();
        commit_files(
            dir,
            &[
                ("target.rs", "fn t() {}"),
                ("frequent.rs", "v1"),
                ("rare.rs", "v1"),
            ],
            "c1",
        );
        commit_files(
            dir,
            &[("target.rs", "fn t2() {}"), ("frequent.rs", "v2")],
            "c2",
        );
        commit_files(
            dir,
            &[("target.rs", "fn t3() {}"), ("frequent.rs", "v3")],
            "c3",
        );

        let store_tmp = tempfile::tempdir().unwrap();
        // `Arc`-wrapped — `build_impact` now takes `&Arc<Store>` (2026-08-31
        // incident fix: must be able to hand the store off to
        // `run_blocking`'s blocking pool).
        let store = Arc::new(Store::open(&store_tmp.path().join("index.db")).unwrap());
        let repo_id = store.upsert_repo("r", dir.to_str().unwrap()).unwrap();
        for path in ["target.rs", "frequent.rs", "rare.rs"] {
            let content = std::fs::read(dir.join(path)).unwrap();
            let hash = crate::ingest::git_blob_hash(&content);
            store
                .upsert_file(repo_id, path, &hash, "rust", content.len() as u64)
                .unwrap();
        }

        let out = build_impact(&store, dir, "r", repo_id, "target.rs", 10)
            .await
            .unwrap();
        assert!(out.approximate);
        assert!(!out.truncated);
        assert_eq!(out.commits_walked, 3);
        assert!(!out.results.iter().any(|h| h.path == "target.rs"));
        assert_eq!(
            out.results[0].path, "frequent.rs",
            "3 co-changes beats 1: {:#?}",
            out.results
        );
        assert_eq!(out.results[0].co_changes, 3);
        assert!(out
            .results
            .iter()
            .any(|h| h.path == "rare.rs" && h.co_changes == 1));
    }

    fn open_store() -> (tempfile::TempDir, Store) {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(&tmp.path().join("index.db")).unwrap();
        (tmp, store)
    }

    fn write(root: &Path, path: &str, content: &str) {
        std::fs::write(root.join(path), content).unwrap();
    }

    #[test]
    fn mention_counts_respects_word_boundaries_and_excludes_the_target() {
        let (_tmp, store) = open_store();
        let root = tempfile::tempdir().unwrap();
        let repo_id = store
            .upsert_repo("r", root.path().to_str().unwrap())
            .unwrap();
        write(root.path(), "target.rs", "mod target;\n");
        write(
            root.path(),
            "user.rs",
            "use target::thing;\nfn targetify() {}\n",
        );
        store
            .upsert_file(repo_id, "target.rs", "h1", "rust", 12)
            .unwrap();
        store
            .upsert_file(repo_id, "user.rs", "h2", "rust", 40)
            .unwrap();

        let mentions = mention_counts(&store, repo_id, root.path(), "target.rs").unwrap();
        let map: BTreeMap<_, _> = mentions.into_iter().collect();
        assert_eq!(
            map.get("user.rs"),
            Some(&1),
            "\"targetify\" must NOT count toward the word-boundary match: {map:?}"
        );
        assert!(!map.contains_key("target.rs"));
    }
}
