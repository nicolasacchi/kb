//! The BLAME service (ADR-4, W3.1): a streamed `git blame --incremental`
//! subprocess (`incremental`) + a lazy `(commit, path)` region cache
//! (`cache`, Gitiles' own shape) + `blame.ignoreRevsFile` support + bounded
//! on-demand line timelines via `git log -L` (`timeline`). `incremental` and
//! `timeline` are the low-level subprocess+protocol layers; this module is
//! the SERVICE layer `routes::blame`/`routes::blame_timeline` call: it
//! decides the cache key (a resolved, immutable sha — never a moving ref),
//! whether a request is a "clean" (cacheable) or "dirty" (uncached,
//! `--contents`-fed) blame, and applies `blame.ignoreRevsFile`
//! auto-detection.
//!
//! # Clean vs dirty
//!
//! [`blame_file`]'s `rev` parameter distinguishes two shapes:
//!
//! - **`rev = Some(spec)`** — a HISTORICAL blame. `spec` is resolved to a
//!   concrete sha immediately (so the cache key is never a moving ref);
//!   commits are immutable, so this is ALWAYS cacheable, unconditionally —
//!   regardless of whatever the live working tree currently looks like.
//! - **`rev = None`** — blame the repo's CURRENT state. `HEAD` is resolved,
//!   then `path`'s current content is compared against the committed blob
//!   at `HEAD` ([`is_dirty`]): a match is functionally identical to
//!   `rev = Some("HEAD")` and is cached exactly the same way; a mismatch
//!   means there's a real, uncommitted edit on disk, which is blamed via
//!   `--contents -` — feeding the EXACT bytes this call itself read, not
//!   whatever git would independently stat off disk a moment later — and is
//!   NEVER cached: a working-tree edit can change on every keystroke, so
//!   caching it under `HEAD`'s sha would go stale on the very next request.
//!
//! `is_dirty` ALWAYS does a fresh `fs::read` + `ingest::git_blob_hash`
//! comparison against `HEAD`'s committed blob oid — deliberately NOT a
//! `store::Store` lookup. An earlier version of this fn preferred the
//! `files` table's live-mirror-maintained `blob_hash` (kept current by the
//! W1.4/W1.6 watcher, see `store.rs`'s module doc) as a fast path, on the
//! theory that a live daemon's store is "usually" fresh. That theory was
//! wrong in a way a fixture test caught directly: the store is only as
//! fresh as the watcher's last DEBOUNCED flush, so a request landing in the
//! window between an edit hitting disk and the watcher catching up would
//! read a still-`HEAD`-matching store row and misreport a genuinely dirty
//! file as clean — a real, if narrow, correctness bug, not a hypothetical
//! one. The extra `fs::read` this costs on every call is a rounding error
//! next to the `git blame`/`git log` subprocess spawn that follows either
//! way, so there is no meaningful perf trade being made by dropping the
//! store lookup — see `is_dirty`'s own doc.
//!
//! # Full-file caching ("Gitiles' shape")
//!
//! The cache always stores/returns the WHOLE file's region list for a given
//! `(repo, sha, path)` — never a `-L`-narrowed slice. A caller-supplied
//! `line_range` on the CACHEABLE path is applied as an in-process filter
//! AFTER the (possibly cached) full result comes back, so two requests for
//! different windows of the SAME commit's blame share one cache entry — the
//! shape Gerrit's Gitiles blame viewer uses (cache the whole file once,
//! serve every requested window from it), rather than a cache keyed
//! per-range, which would never accumulate reusable hits across different
//! range requests for the same immutable commit. The DIRTY (uncached) path
//! is the one exception: since it's never cached anyway, a `line_range` is
//! instead passed straight through to the `git blame` subprocess itself
//! (`incremental::BlameOptions::line_range`) — cheaper for a narrow view
//! into a large, currently-dirty file, with no cache-pollution downside.
//!
//! # v1 scope note (streaming upgrade path)
//!
//! `incremental::run_streaming` already supports a callback/channel form,
//! but `GET /api/blame` (`routes::blame`) uses the collect-all
//! `incremental::run_collect` for v1 — the SPA ladder this feeds consumes a
//! materialized region list, and no file in this Wave's target repos is
//! large enough for time-to-first-region to matter. A future SSE/chunked
//! route can switch to `run_streaming` without touching `incremental.rs` or
//! the cache/dirty-detection logic in this module at all.
//!
//! # Why `BlameError` isn't shared with its siblings
//!
//! `BlameError` is its own enum, deliberately NOT unified with this crate's
//! other git-subprocess wrappers (`diff::DiffError`, `checkout::
//! CheckoutError`, `sessiondiff::git_diff::DiffError`, `history::
//! HistoryError`, `mirror::reconcile`'s error-less subprocess call) — see
//! `diff.rs`'s module doc ("Why `DiffError` isn't shared with its
//! siblings") for the full rationale. This service layer's shape is the
//! most distinct of the six: it wraps
//! TWO subprocess-adjacent error sources it doesn't own (`GitError` from
//! `resolve`/`blob_oid`, `IncrementalError` from the `git blame --incremental`
//! wire-protocol parser in `incremental.rs`) plus its own `Read` variant for
//! the dirty-path `fs::read` — `routes.rs`'s `From<BlameError>` and the
//! separate `incremental_to_api_error` helper map each source to a
//! different HTTP status, which a merged enum would blur.

pub mod cache;
pub mod incremental;
pub mod timeline;

pub use cache::BlameCache;
pub use incremental::{BlameOptions, BlameRegion, IncrementalError};
pub use timeline::{line_timeline, TimelineEntry, TimelineError, DEFAULT_MAX_ENTRIES};

use crate::git::{GitError, GitRepo};
use crate::ingest;
use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum BlameError {
    #[error(transparent)]
    Git(#[from] GitError),
    #[error(transparent)]
    Incremental(#[from] IncrementalError),
    #[error("failed to read {path:?}: {source}")]
    Read {
        path: String,
        #[source]
        source: std::io::Error,
    },
}

pub type Result<T> = std::result::Result<T, BlameError>;

/// A defensive cap on how many regions a single response ever carries — a
/// circuit breaker for a pathological file (an enormous generated/vendored
/// blob), never expected to trip on ordinary source. Mirrors `routes.rs`'s
/// own `MAX_SYMBOL_MATCHES` precedent for the same "bounded API response,
/// not a paginated one" posture.
pub const MAX_REGIONS: usize = 20_000;

#[derive(Debug, Clone)]
pub struct BlameResult {
    /// The concrete sha this result was blamed against — `HEAD`'s resolved
    /// sha for the `rev = None` case, or `rev`'s own resolution otherwise.
    pub resolved_ref: String,
    /// `true` only for the `rev = None` + uncommitted-edit case (see the
    /// module doc) — a `dirty` result is never `cached`.
    pub dirty: bool,
    /// `true` if this result was served from [`BlameCache`] rather than a
    /// fresh subprocess run.
    pub cached: bool,
    pub regions: Vec<BlameRegion>,
    /// `true` if [`MAX_REGIONS`] was hit and the tail was dropped.
    pub truncated: bool,
}

/// The service entry point `routes::blame` calls — see the module doc for
/// the clean/dirty/cache-key contract.
pub fn blame_file(
    cache: &BlameCache,
    repo: &GitRepo,
    repo_id: i64,
    repo_root: &Path,
    path: &str,
    rev: Option<&str>,
    line_range: Option<(u32, u32)>,
) -> Result<BlameResult> {
    let ignore_revs_file = detect_ignore_revs_file(repo, repo_root);

    if let Some(spec) = rev {
        let sha = repo.resolve(spec)?.to_string();
        let (regions, cached) = full_blame_cached(
            cache,
            repo_root,
            repo_id,
            &sha,
            path,
            ignore_revs_file.as_deref(),
        )?;
        let (regions, truncated) = apply_range(regions, line_range);
        return Ok(BlameResult {
            resolved_ref: sha,
            dirty: false,
            cached,
            regions,
            truncated,
        });
    }

    let head_sha = repo.resolve("HEAD")?.to_string();
    if is_dirty(repo, repo_root, path, &head_sha)? {
        let abs = repo_root.join(path);
        let bytes = std::fs::read(&abs).map_err(|e| BlameError::Read {
            path: path.to_string(),
            source: e,
        })?;
        let opts = incremental::BlameOptions {
            repo_root,
            path,
            rev: Some(head_sha.as_str()),
            contents: Some(&bytes),
            ignore_revs_file: ignore_revs_file.as_deref(),
            line_range,
        };
        let regions = incremental::run_collect(&opts)?;
        let (regions, truncated) = cap_regions(regions, MAX_REGIONS);
        return Ok(BlameResult {
            resolved_ref: head_sha,
            dirty: true,
            cached: false,
            regions,
            truncated,
        });
    }

    let (regions, cached) = full_blame_cached(
        cache,
        repo_root,
        repo_id,
        &head_sha,
        path,
        ignore_revs_file.as_deref(),
    )?;
    let (regions, truncated) = apply_range(regions, line_range);
    Ok(BlameResult {
        resolved_ref: head_sha,
        dirty: false,
        cached,
        regions,
        truncated,
    })
}

/// The cacheable path: a cache hit returns immediately; a miss runs a
/// FULL-FILE (no `-L`) `git blame --incremental` and populates the cache
/// before returning — see the module doc's "Gitiles' shape" section.
fn full_blame_cached(
    cache: &BlameCache,
    repo_root: &Path,
    repo_id: i64,
    sha: &str,
    path: &str,
    ignore_revs_file: Option<&Path>,
) -> Result<(Vec<BlameRegion>, bool)> {
    if let Some(regions) = cache.get(repo_id, sha, path) {
        return Ok((regions, true));
    }
    let opts = incremental::BlameOptions {
        repo_root,
        path,
        rev: Some(sha),
        contents: None,
        ignore_revs_file,
        line_range: None,
    };
    let regions = incremental::run_collect(&opts)?;
    cache.put(repo_id, sha, path, regions.clone());
    Ok((regions, false))
}

/// `true` if `path`'s current working-tree content differs from the
/// committed blob at `head_sha` — a fresh stat+hash check
/// (`ingest::git_blob_hash` over a live `fs::read`), reusing the SAME
/// git-compatible hashing routine `sink.rs`'s live-mirror ingest path
/// already relies on. See the module doc for why this is deliberately NOT a
/// `store::Store` lookup.
fn is_dirty(repo: &GitRepo, repo_root: &Path, path: &str, head_sha: &str) -> Result<bool> {
    let abs = repo_root.join(path);
    let bytes = std::fs::read(&abs).map_err(|e| BlameError::Read {
        path: path.to_string(),
        source: e,
    })?;
    let current_hash = ingest::git_blob_hash(&bytes);
    let head_oid = repo.blob_oid(head_sha, path)?;
    Ok(head_oid.as_deref() != Some(current_hash.as_str()))
}

/// Auto-detect a `blame.ignoreRevsFile`-style revision list, existence
/// checked so a stale/misconfigured entry never turns into an
/// `--ignore-revs-file` flag pointing at nothing (git would itself error on
/// that). Two sources, in order:
///
/// 1. The repo's OWN git config key `blame.ignoreRevsFile` — the same
///    setting an operator's ordinary `git blame` CLI already respects
///    (`git config blame.ignoreRevsFile <path>`), resolved relative to the
///    repo root per git's own convention for this key. Read directly via
///    `repo.repo.config_snapshot()` (the `git` module's `pub(crate) repo`
///    field) rather than adding a general-purpose config accessor to that
///    module — this is the one place in kb-code that needs a git CONFIG
///    read rather than an ODB read, and it's specific to this feature.
/// 2. The conventional filename `.git-blame-ignore-revs` at the repo root
///    (GitHub/GitLab's own "blame ignore" UI convention), used when no repo
///    config entry exists — so a repo that merely carries the file, without
///    an operator having ALSO run the `git config` command, still benefits.
fn detect_ignore_revs_file(repo: &GitRepo, repo_root: &Path) -> Option<std::path::PathBuf> {
    if let Some(configured) = repo.repo.config_snapshot().string("blame.ignoreRevsFile") {
        let configured = configured.to_string();
        if !configured.is_empty() {
            let resolved = repo_root.join(&configured);
            if resolved.is_file() {
                return Some(resolved);
            }
        }
    }
    let conventional = repo_root.join(".git-blame-ignore-revs");
    conventional.is_file().then_some(conventional)
}

/// Filter a full-file region list down to those overlapping `range` (1-based
/// inclusive), if given, then apply [`cap_regions`]. A region straddling a
/// range boundary is returned WHOLE (never cropped) — the same "show full
/// attribution groups overlapping the window" behaviour Gitiles' own viewer
/// uses, and consistent with how `git blame -L` itself only ever emits
/// whole groups.
fn apply_range(regions: Vec<BlameRegion>, range: Option<(u32, u32)>) -> (Vec<BlameRegion>, bool) {
    let filtered = match range {
        Some((start, end)) => regions
            .into_iter()
            .filter(|r| {
                let region_end = r.final_start + r.count.saturating_sub(1);
                r.final_start <= end && region_end >= start
            })
            .collect(),
        None => regions,
    };
    cap_regions(filtered, MAX_REGIONS)
}

fn cap_regions(mut regions: Vec<BlameRegion>, cap: usize) -> (Vec<BlameRegion>, bool) {
    if regions.len() > cap {
        regions.truncate(cap);
        (regions, true)
    } else {
        (regions, false)
    }
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

    fn init_repo(dir: &Path) {
        git(dir, &["init", "-q", "-b", "main"]);
        git(dir, &["config", "user.email", "test@example.com"]);
        git(dir, &["config", "user.name", "Test"]);
    }

    /// A repo_id for tests — purely an opaque cache-key component here
    /// (`blame_file` no longer touches any `store::Store`), so a fixed
    /// literal is as good as one minted by a real store.
    const REPO_ID: i64 = 1;

    /// Two commits: c1 (Alice) writes `f.txt`, c2 (Bob) edits it.
    fn two_commit_repo() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        init_repo(dir);
        std::fs::write(dir.join("f.txt"), "line1\nline2\nline3\n").unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["config", "user.name", "Alice"]);
        git(dir, &["commit", "-q", "-m", "c1"]);

        std::fs::write(dir.join("f.txt"), "line1\nline2-bob\nline3\n").unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["config", "user.name", "Bob"]);
        git(dir, &["commit", "-q", "-m", "c2"]);
        tmp
    }

    #[test]
    fn clean_blame_is_cache_miss_then_cache_hit() {
        let repo_tmp = two_commit_repo();
        let dir = repo_tmp.path();
        let git_repo = GitRepo::open(dir).unwrap();
        let cache = BlameCache::new(8);

        let first = blame_file(&cache, &git_repo, REPO_ID, dir, "f.txt", None, None).unwrap();
        assert!(!first.dirty);
        assert!(!first.cached, "first call must be a fresh compute");
        assert!(first.regions.iter().any(|r| r.author == "Bob"));

        let second = blame_file(&cache, &git_repo, REPO_ID, dir, "f.txt", None, None).unwrap();
        assert!(second.cached, "second identical call must hit the cache");
        assert_eq!(second.resolved_ref, first.resolved_ref);
        assert_eq!(second.regions, first.regions);
    }

    #[test]
    fn explicit_historical_ref_is_always_cacheable_regardless_of_working_tree_dirt() {
        let repo_tmp = two_commit_repo();
        let dir = repo_tmp.path();
        let git_repo = GitRepo::open(dir).unwrap();
        let cache = BlameCache::new(8);

        // Dirty the working tree — a historical ref query must ignore this
        // entirely (it's blaming an immutable past commit, not "the repo's
        // current state").
        std::fs::write(dir.join("f.txt"), "line1\nline2-uncommitted\nline3\n").unwrap();

        let result =
            blame_file(&cache, &git_repo, REPO_ID, dir, "f.txt", Some("HEAD"), None).unwrap();
        assert!(!result.dirty, "an explicit ref is never the dirty case");
        assert!(result.regions.iter().any(|r| r.author == "Bob"));
        assert!(
            result.regions.iter().all(|r| r.sha != "0".repeat(40)),
            "must reflect the COMMITTED HEAD content, not the dirty working tree"
        );

        // A second call with the SAME explicit ref hits the cache.
        let second =
            blame_file(&cache, &git_repo, REPO_ID, dir, "f.txt", Some("HEAD"), None).unwrap();
        assert!(second.cached);
    }

    #[test]
    fn dirty_working_tree_blames_the_live_bytes_and_is_never_cached() {
        let repo_tmp = two_commit_repo();
        let dir = repo_tmp.path();
        let git_repo = GitRepo::open(dir).unwrap();
        let cache = BlameCache::new(8);

        std::fs::write(dir.join("f.txt"), "line1\nline2-bob\nline3\nline4-live\n").unwrap();

        let result = blame_file(&cache, &git_repo, REPO_ID, dir, "f.txt", None, None).unwrap();
        assert!(result.dirty);
        assert!(!result.cached);
        // An overlap check, not an exact `final_start == 4` match — git
        // groups CONTIGUOUS uncommitted lines into one region. With only
        // one new line here it happens to start exactly at 4, but the
        // second edit below (two new lines) proves that's not a safe
        // assumption in general.
        let last_line = result
            .regions
            .iter()
            .find(|r| r.final_start <= 4 && r.final_start + r.count > 4)
            .expect("line 4 must be covered");
        assert_eq!(
            last_line.sha,
            "0".repeat(40),
            "the uncommitted line must be attributed to git's own \
             not-committed-yet sentinel sha"
        );
        assert_eq!(
            cache.len(),
            0,
            "a dirty blame must never populate the cache"
        );

        // Edit AGAIN, differently — proves the previous call's result was
        // genuinely not cached (a cached result would keep returning the
        // FIRST edit's content). Now TWO uncommitted lines (4 and 5) — git
        // groups them into ONE contiguous region (final_start=4, count=2),
        // so this checks coverage/overlap rather than an exact
        // `final_start == 5` match.
        std::fs::write(
            dir.join("f.txt"),
            "line1\nline2-bob\nline3\nline4-live\nline5-also-live\n",
        )
        .unwrap();
        let result2 = blame_file(&cache, &git_repo, REPO_ID, dir, "f.txt", None, None).unwrap();
        assert!(result2.regions.iter().any(|r| {
            r.sha == "0".repeat(40) && r.final_start <= 5 && r.final_start + r.count > 5
        }));
    }

    #[test]
    fn no_uncommitted_edit_is_reported_as_clean_not_dirty() {
        let repo_tmp = two_commit_repo();
        let dir = repo_tmp.path();
        let git_repo = GitRepo::open(dir).unwrap();
        let cache = BlameCache::new(8);

        // two_commit_repo() leaves the working tree clean after its last
        // commit — a fresh stat+hash check must agree.
        let result = blame_file(&cache, &git_repo, REPO_ID, dir, "f.txt", None, None).unwrap();
        assert!(!result.dirty);
        assert!(result.regions.iter().all(|r| r.sha != "0".repeat(40)));
    }

    #[test]
    fn ignore_revs_file_auto_detected_at_repo_root() {
        let repo_tmp = two_commit_repo();
        let dir = repo_tmp.path();

        // A third, reformat-only commit by a third author.
        std::fs::write(dir.join("f.txt"), "line1 \nline2-bob \nline3 \n").unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["config", "user.name", "Carol"]);
        git(dir, &["commit", "-q", "-m", "c3: carol reformat"]);
        let reformat_sha_out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap();
        let reformat_sha = String::from_utf8(reformat_sha_out.stdout)
            .unwrap()
            .trim()
            .to_string();
        std::fs::write(
            dir.join(".git-blame-ignore-revs"),
            format!("{reformat_sha}\n"),
        )
        .unwrap();

        let git_repo = GitRepo::open(dir).unwrap();
        let cache = BlameCache::new(8);

        let result = blame_file(&cache, &git_repo, REPO_ID, dir, "f.txt", None, None).unwrap();
        assert!(
            result.regions.iter().all(|r| r.author != "Carol"),
            "the auto-detected .git-blame-ignore-revs file must exclude the \
             reformat commit: {:?}",
            result.regions
        );
    }

    #[test]
    fn ignore_revs_file_via_git_config_blame_ignore_revs_file() {
        let repo_tmp = two_commit_repo();
        let dir = repo_tmp.path();

        std::fs::write(dir.join("f.txt"), "line1 \nline2-bob \nline3 \n").unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["config", "user.name", "Carol"]);
        git(dir, &["commit", "-q", "-m", "c3: carol reformat"]);
        let reformat_sha_out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap();
        let reformat_sha = String::from_utf8(reformat_sha_out.stdout)
            .unwrap()
            .trim()
            .to_string();
        // A NON-conventional filename, wired only via git config — proves
        // detection doesn't just hardcode ".git-blame-ignore-revs".
        std::fs::write(dir.join("ignore-list.txt"), format!("{reformat_sha}\n")).unwrap();
        git(dir, &["config", "blame.ignoreRevsFile", "ignore-list.txt"]);

        let git_repo = GitRepo::open(dir).unwrap();
        let cache = BlameCache::new(8);

        let result = blame_file(&cache, &git_repo, REPO_ID, dir, "f.txt", None, None).unwrap();
        assert!(
            result.regions.iter().all(|r| r.author != "Carol"),
            "the git-config-wired ignore-revs file must exclude the reformat \
             commit: {:?}",
            result.regions
        );
    }

    #[test]
    fn line_range_narrows_a_cached_full_file_result() {
        let repo_tmp = two_commit_repo();
        let dir = repo_tmp.path();
        let git_repo = GitRepo::open(dir).unwrap();
        let cache = BlameCache::new(8);

        // Prime the cache with the FULL file.
        let full = blame_file(&cache, &git_repo, REPO_ID, dir, "f.txt", None, None).unwrap();
        assert!(!full.cached);
        assert!(full.regions.len() >= 2);

        // A range-scoped call for line 2 only — must hit the SAME cache
        // entry (Gitiles' shape) and return just the overlapping region(s).
        let narrowed =
            blame_file(&cache, &git_repo, REPO_ID, dir, "f.txt", None, Some((2, 2))).unwrap();
        assert!(
            narrowed.cached,
            "a range query must still hit the full-file cache entry"
        );
        assert!(narrowed.regions.iter().all(|r| {
            let end = r.final_start + r.count.saturating_sub(1);
            r.final_start <= 2 && end >= 2
        }));
        assert!(!narrowed.regions.is_empty());
    }

    #[test]
    fn cap_regions_truncates_and_reports_truncated() {
        let regions: Vec<BlameRegion> = (0..5)
            .map(|i| BlameRegion {
                sha: format!("sha-{i}"),
                final_start: i,
                count: 1,
                ..BlameRegion::default()
            })
            .collect();
        let (capped, truncated) = cap_regions(regions.clone(), 3);
        assert!(truncated);
        assert_eq!(capped.len(), 3);

        let (untouched, truncated2) = cap_regions(regions, 10);
        assert!(!truncated2);
        assert_eq!(untouched.len(), 5);
    }
}
