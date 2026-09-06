//! Behavioral ingestion: one `git log --numstat -M --format=…` shape for
//! full-window backfill and incremental `last..HEAD` updates.
//!
//! Window semantics: rebuilds are exact for `[behavioral] window_days`;
//! incremental updates are additive only (no subtract). See parent module.

use crate::config::{BehavioralSection, RepoEntry};
use crate::store::Store;
use serde::Serialize;
use std::collections::HashMap;
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use super::{numstat_final_path, ordered_pair};

/// Per-repo ingestion lock.
///
/// `apply_commits` is ADDITIVE, and both entry points can run a full
/// rebuild (`backfill_repo` explicitly; `incremental_update` when meta is
/// missing or the recorded sha is unreachable). Two rebuilds that
/// interleave — clear, clear, apply, apply — double every counter, which
/// is exactly what a concurrent `POST /api/behavioral/backfill` and a
/// mirror `repo.head_moved` rebuild produced (2026-08-02). Serialising per
/// repo also makes the meta re-read inside the lock authoritative: a
/// backfill that just finished leaves `last_commit_sha == HEAD`, so the
/// queued incremental becomes a no-op instead of a second rebuild.
///
/// Both holders are synchronous functions invoked from `spawn_blocking`;
/// the guard never crosses an `.await`.
fn repo_lock(repo_id: i64) -> Arc<Mutex<()>> {
    static LOCKS: OnceLock<Mutex<HashMap<i64, Arc<Mutex<()>>>>> = OnceLock::new();
    let map = LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = map.lock().unwrap_or_else(|e| e.into_inner());
    Arc::clone(guard.entry(repo_id).or_default())
}

/// Wire schema for backfill stats.
pub const SCHEMA: &str = "behavioral/1";

/// Record separator + field separator for the commit header line.
/// Format string: `%x1e%H%x1f%ae%x1f%at` → `\x1e<sha>\x1f<email>\x1f<unix>`.
const COMMIT_FMT: &str = "%x1e%H%x1f%ae%x1f%at";

#[derive(Debug, thiserror::Error)]
pub enum BehavioralError {
    #[error("spawn git log: {0}")]
    Spawn(std::io::Error),
    #[error("git log failed: {0}")]
    GitLog(String),
    #[error("store: {0}")]
    Store(#[from] crate::store::StoreError),
    #[error("git log task panicked: {0}")]
    Panicked(String),
}

pub type Result<T> = std::result::Result<T, BehavioralError>;

/// One commit's parsed delta from the unified log walk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitDelta {
    pub sha: String,
    pub author: String,
    pub commit_unix: i64,
    /// (path, lines_added, lines_deleted)
    pub files: Vec<(String, i64, i64)>,
}

/// [`backfill_repo`] / [`incremental_update`] result.
#[derive(Debug, Clone, Serialize)]
pub struct BehavioralStats {
    pub schema: &'static str,
    pub repo: String,
    /// Commits processed this run.
    pub commits: usize,
    /// Paths touched (unique count after apply — approximate on
    /// incremental: number of file-touch events processed).
    pub path_touches: usize,
    /// Cochange pairs incremented (0 when every commit exceeded
    /// `max_commit_files`).
    pub cochange_updates: usize,
    /// `true` when this run did a full window rebuild (vs pure incremental).
    pub full_rebuild: bool,
    pub duration_ms: u64,
    pub last_commit_sha: Option<String>,
}

/// Parse stdout of `git log --numstat -M --format=%x1e%H%x1f%ae%x1f%at`.
pub fn parse_log_numstat(stdout: &str) -> Vec<CommitDelta> {
    let mut out = Vec::new();
    let mut cur: Option<CommitDelta> = None;

    for line in stdout.lines() {
        if let Some(rest) = line.strip_prefix('\u{1e}') {
            if let Some(done) = cur.take() {
                if !done.sha.is_empty() {
                    out.push(done);
                }
            }
            let mut parts = rest.split('\u{1f}');
            let sha = parts.next().unwrap_or("").to_string();
            let author = parts.next().unwrap_or("").to_string();
            let commit_unix: i64 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
            cur = Some(CommitDelta {
                sha,
                author,
                commit_unix,
                files: Vec::new(),
            });
            continue;
        }
        // numstat line: ins\tdel\tpath  (or empty line between commits)
        let line = line.trim_end();
        if line.is_empty() {
            continue;
        }
        let Some(c) = cur.as_mut() else {
            continue;
        };
        let mut fields = line.splitn(3, '\t');
        let ins = fields.next().unwrap_or("");
        let del = fields.next().unwrap_or("");
        let path_raw = fields.next().unwrap_or("");
        if path_raw.is_empty() {
            continue;
        }
        let (added, deleted) = if ins == "-" && del == "-" {
            (0i64, 0i64)
        } else {
            (
                ins.parse::<i64>().unwrap_or(0),
                del.parse::<i64>().unwrap_or(0),
            )
        };
        let path = numstat_final_path(path_raw);
        if path.is_empty() {
            continue;
        }
        c.files.push((path, added, deleted));
    }
    if let Some(done) = cur.take() {
        if !done.sha.is_empty() {
            out.push(done);
        }
    }
    // git log is newest-first; process oldest-first so first_seen/last_touch
    // and additive counters match chronological history.
    out.reverse();
    out
}

/// Run the ONE git-log shape. `range` is either empty (use `--since`) or
/// a rev range like `abc..HEAD`. `since_unix` is used only when `range` is
/// empty (full window).
///
/// This is the shared walk used by backfill/incremental **and** the
/// V3.4-C1 time-series route — do not fork a second log formula.
pub fn walk_commits(
    repo_root: &Path,
    range: Option<&str>,
    since_unix: Option<i64>,
) -> Result<Vec<CommitDelta>> {
    let (commits, _truncated) = walk_commits_capped(repo_root, range, since_unix, None)?;
    Ok(commits)
}

/// Same log shape as [`walk_commits`], with an optional hard cap on how
/// many commits git returns (`--max-count`). When `max_count` is `Some(N)`
/// and the window contains more than N commits, returns the N **newest**
/// (after the usual oldest-first reverse) and `truncated = true`.
///
/// Used by the request-time time-series walk so a huge window cannot
/// unbounded-load the daemon; backfill still uses the uncapped path.
pub fn walk_commits_capped(
    repo_root: &Path,
    range: Option<&str>,
    since_unix: Option<i64>,
    max_count: Option<usize>,
) -> Result<(Vec<CommitDelta>, bool)> {
    let mut args: Vec<String> = vec![
        "log".into(),
        "--numstat".into(),
        "-M".into(),
        format!("--format={COMMIT_FMT}"),
    ];
    if let Some(r) = range {
        args.push(r.to_string());
    } else if let Some(since) = since_unix {
        args.push(format!("--since=@{since}"));
    }
    // Request one extra so we can detect truncation without a second walk.
    if let Some(n) = max_count {
        args.push(format!("--max-count={}", n.saturating_add(1)));
    }
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(&args)
        .output()
        .map_err(BehavioralError::Spawn)?;
    if !output.status.success() {
        return Err(BehavioralError::GitLog(
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
        ));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut commits = parse_log_numstat(&stdout);
    let truncated = match max_count {
        Some(n) if commits.len() > n => {
            // parse_log_numstat returns oldest-first of the newest (n+1).
            // Drop the oldest extras so the retained window is the n newest.
            let drop = commits.len() - n;
            commits.drain(0..drop);
            true
        }
        _ => false,
    };
    Ok((commits, truncated))
}

/// Apply commits to the store. Returns (path_touches, cochange_updates).
///
/// V3.2-B2: for each commit, looks up `commit_sessions` (join ladder cache).
/// When a session is resolved, dual-writes `author = "session:<id>"` beside
/// the human author. Does NOT fetch pain signals here (that is async /
/// optional via [`super::refresh_session_signals`]).
pub fn apply_commits(
    store: &Store,
    repo_id: i64,
    commits: &[CommitDelta],
    max_commit_files: u32,
) -> Result<(usize, usize)> {
    let mut path_touches = 0usize;
    let mut cochange_updates = 0usize;
    let max_files = max_commit_files as usize;
    for c in commits {
        path_touches += c.files.len();
        let co_pairs: Vec<(String, String)> = if c.files.len() > max_files || c.files.len() < 2 {
            Vec::new()
        } else {
            let paths: Vec<&str> = c.files.iter().map(|(p, _, _)| p.as_str()).collect();
            let mut pairs = Vec::new();
            for i in 0..paths.len() {
                for j in (i + 1)..paths.len() {
                    if paths[i] == paths[j] {
                        continue;
                    }
                    pairs.push(ordered_pair(paths[i], paths[j]));
                    cochange_updates += 1;
                }
            }
            // Dedup pairs within one commit (rare double path listing).
            pairs.sort();
            pairs.dedup();
            pairs
        };
        // Dual-author: human + session when join cache has a hit.
        let session_id = store
            .get_commit_session(repo_id, &c.sha)
            .ok()
            .flatten()
            .and_then(|row| row.session_id);
        store.apply_behavioral_commit(
            repo_id,
            &c.files,
            &c.author,
            c.commit_unix,
            &co_pairs,
            session_id.as_deref(),
        )?;
    }
    Ok((path_touches, cochange_updates))
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn resolve_head(repo_root: &Path) -> Result<Option<String>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["rev-parse", "HEAD"])
        .output()
        .map_err(BehavioralError::Spawn)?;
    if !output.status.success() {
        return Ok(None);
    }
    let sha = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if sha.is_empty() {
        Ok(None)
    } else {
        Ok(Some(sha))
    }
}

fn commit_reachable(repo_root: &Path, sha: &str) -> bool {
    // V70-A2 (SEC-17) — `sha` is store-derived, not caller text, but it is
    // still interpolated into argv below. Validating through the ONE
    // validator (`Revspec::parse`) rather than a local `starts_with('-')`
    // both keeps the predicate single-sourced and strengthens it: a real
    // sha never contains whitespace, `..` or `@{` either. Fail-closed —
    // an unvalidatable sha is simply "not reachable".
    if crate::git::Revspec::parse(sha).is_err() {
        return false;
    }
    Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["cat-file", "-e", &format!("{sha}^{{commit}}")])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Full window rebuild: clear counters, walk `--since`, re-apply, set meta.
pub fn backfill_repo(
    repo: &RepoEntry,
    repo_id: i64,
    cfg: &BehavioralSection,
    store: &Store,
) -> Result<BehavioralStats> {
    let lock = repo_lock(repo_id);
    let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
    backfill_repo_locked(repo, repo_id, cfg, store)
}

/// `backfill_repo` minus the lock — for callers already holding it.
fn backfill_repo_locked(
    repo: &RepoEntry,
    repo_id: i64,
    cfg: &BehavioralSection,
    store: &Store,
) -> Result<BehavioralStats> {
    let t0 = Instant::now();
    let window_secs = (cfg.window_days as i64).saturating_mul(86_400);
    let since = now_unix().saturating_sub(window_secs);

    store.clear_behavioral_stats(repo_id)?;
    let commits = walk_commits(&repo.path, None, Some(since))?;
    let (path_touches, cochange_updates) =
        apply_commits(store, repo_id, &commits, cfg.max_commit_files)?;
    let head = resolve_head(&repo.path)?;
    let updated = now_unix();
    store.set_behavioral_meta(repo_id, head.as_deref(), updated)?;

    Ok(BehavioralStats {
        schema: SCHEMA,
        repo: repo.name.clone(),
        commits: commits.len(),
        path_touches,
        cochange_updates,
        full_rebuild: true,
        duration_ms: t0.elapsed().as_millis() as u64,
        last_commit_sha: head,
    })
}

/// Incremental: process `last_commit_sha..HEAD` if reachable; else fall
/// back to a full window rebuild (force-push / shallow).
pub fn incremental_update(
    repo: &RepoEntry,
    repo_id: i64,
    cfg: &BehavioralSection,
    store: &Store,
) -> Result<BehavioralStats> {
    // Same lock as `backfill_repo` — see `repo_lock`. The meta read below
    // happens INSIDE it, so a rebuild that landed while this call queued
    // collapses this one into the `last_sha == head` no-op arm.
    let lock = repo_lock(repo_id);
    let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
    let t0 = Instant::now();
    let head = match resolve_head(&repo.path)? {
        Some(h) => h,
        None => {
            return Ok(BehavioralStats {
                schema: SCHEMA,
                repo: repo.name.clone(),
                commits: 0,
                path_touches: 0,
                cochange_updates: 0,
                full_rebuild: false,
                duration_ms: t0.elapsed().as_millis() as u64,
                last_commit_sha: None,
            });
        }
    };

    let meta = store.behavioral_meta(repo_id)?;
    let last = meta.as_ref().and_then(|m| m.last_commit_sha.as_deref());

    if let Some(last_sha) = last {
        if last_sha == head {
            return Ok(BehavioralStats {
                schema: SCHEMA,
                repo: repo.name.clone(),
                commits: 0,
                path_touches: 0,
                cochange_updates: 0,
                full_rebuild: false,
                duration_ms: t0.elapsed().as_millis() as u64,
                last_commit_sha: Some(head),
            });
        }
        if commit_reachable(&repo.path, last_sha) {
            let range = format!("{last_sha}..{head}");
            let commits = walk_commits(&repo.path, Some(&range), None)?;
            let (path_touches, cochange_updates) =
                apply_commits(store, repo_id, &commits, cfg.max_commit_files)?;
            store.set_behavioral_meta(repo_id, Some(&head), now_unix())?;
            return Ok(BehavioralStats {
                schema: SCHEMA,
                repo: repo.name.clone(),
                commits: commits.len(),
                path_touches,
                cochange_updates,
                full_rebuild: false,
                duration_ms: t0.elapsed().as_millis() as u64,
                last_commit_sha: Some(head),
            });
        }
        tracing::warn!(
            repo = %repo.name,
            last = %last_sha,
            "behavioral: last_commit_sha unreachable — full window rebuild"
        );
    }

    // No meta or unreachable → full rebuild. `_locked` because this call
    // already holds the per-repo lock (std Mutex is NOT reentrant).
    let mut stats = backfill_repo_locked(repo, repo_id, cfg, store)?;
    stats.duration_ms = t0.elapsed().as_millis() as u64;
    Ok(stats)
}

#[cfg(test)]
mod parse_tests {
    use super::*;

    #[test]
    fn parse_log_numstat_two_commits_oldest_first() {
        // Newest first on the wire; parser reverses.
        let stdout = "\u{1e}bbb\u{1f}b@ex.com\u{1f}200\n\n\
                      1\t0\tb.rs\n\
                      \u{1e}aaa\u{1f}a@ex.com\u{1f}100\n\n\
                      2\t1\ta.rs\n\
                      3\t0\tb.rs\n";
        let commits = parse_log_numstat(stdout);
        assert_eq!(commits.len(), 2);
        assert_eq!(commits[0].sha, "aaa");
        assert_eq!(commits[0].files.len(), 2);
        assert_eq!(commits[1].sha, "bbb");
        assert_eq!(commits[1].files[0].0, "b.rs");
    }
}
