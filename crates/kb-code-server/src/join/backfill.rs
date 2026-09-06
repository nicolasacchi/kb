//! join/backfill/1 — W3.6, the PRECOMPUTE: proactively warms the join
//! ladder's `commit_sessions` cache (V0005, `join::ladder`'s module doc)
//! across a repo's commit history, so a live `kb-code why`/`story`/`join`
//! query against an old commit is served from a warm cache instead of
//! paying the resolution cost — trailer/exact/fuzzy/none plus a real kb
//! round trip where local signals don't settle it — on the spot.
//!
//! [`backfill_repo`] is genuinely simple: walk `git log --format=%H`
//! (bounded by `[backfill] depth` — see `config::BackfillSection`), then run
//! every walked sha through [`ladder::resolve_commit`] UNCHANGED — the same
//! six-arm ladder `GET /api/join/commit`/`kb-code join` already use. No new
//! resolution logic here, no new persisted state beyond `commit_sessions`
//! itself; this module's only job is orchestration + counting:
//!
//! - **Batch the commit-map fetch once up front.** `join::kb_client::
//!   KbClient::commit_map_snapshot` already caches its result for
//!   [`kb_client::SNAPSHOT_TTL`] (5 minutes); [`backfill_repo`] forces ONE
//!   fresh fetch (`force = true`) before walking anything, so the
//!   fuzzy/squash-subject/time-window arms (3-5) hit that warm cache for
//!   the rest of a same-run walk instead of each independently triggering
//!   the paginated fetch on first use. A run longer than the TTL still
//!   works correctly — the client just silently re-fetches partway through,
//!   exactly as it would for any other caller.
//! - **Rate-bound kindly.** Per-commit resolution ([`ladder::resolve_commit`])
//!   is the only thing that can make a NETWORK round trip per commit (the
//!   trailer arm's best-effort by-commit enrichment, or arm 2's own exact
//!   by-commit lookup for a commit with no local trailer — the commit-map
//!   snapshot arms 3-5 read the ALREADY-fetched snapshot, no further network
//!   call). [`MAX_CONCURRENCY`] bounds how many of those per-commit
//!   resolutions run at once — capping the daemon calls in flight together,
//!   without needing to reorder or special-case the ladder's own arm
//!   sequence (a deliberately UNCHANGED, already-tested six-arm contract).
//! - **The cache makes re-runs cheap.** `trailer`/`exact` cache rows never
//!   expire; `fuzzy`/`none` rows are TTL'd (`ladder::TTL_SECS`, 24h) — see
//!   that module's doc. A backfill re-run over the same window therefore
//!   mostly reads through an already-warm cache; [`BackfillStats::
//!   newly_cached`]/[`BackfillStats::upgraded`] make that visible: how many
//!   commits got a cache row for the FIRST time this run, and how many
//!   previously-`none` rows resolved to something better this time (the
//!   TTL's whole reason for existing — a session that finishes and gets
//!   indexed after an earlier "no match" backfill run).
//!
//! `degraded` is set the moment the up-front commit-map fetch fails
//! (disabled or unreachable kb daemon) — the trailer arm still resolves
//! fully locally regardless (see `ladder`'s module doc, arm 1), so this is a
//! DEGRADED signal for the caller to surface, never a hard failure: `kb-code
//! backfill` exits 0 either way (see that verb's own doc).

use std::path::Path;
use std::time::{Duration, Instant};

use futures::stream::{self, StreamExt};
use serde::Serialize;

use crate::config::RepoEntry;
use crate::join::kb_client::KbClient;
use crate::join::ladder::{self, Confidence};
use crate::store::{Store, StoreBlocking};
use std::sync::Arc;

/// Max concurrent per-commit ladder resolutions in flight at once — see the
/// module doc's "Rate-bound kindly." Deliberately small: a backfill is a
/// background/batch operation, never latency-sensitive, so there's no
/// upside to hammering a (likely local, single-operator) kb daemon harder
/// than this.
pub const MAX_CONCURRENCY: usize = 4;

/// Defensive ceiling on the walked commit count, applied via `git log
/// --max-count` REGARDLESS of `depth` (even `depth = "all"`) — mirrors
/// `provenance::report::MAX_ALLOWED_MAX_COUNT`'s "a runaway/enormous
/// history can't make one call unbounded" precedent.
pub const MAX_WALK_COMMITS: usize = 20_000;

/// join/backfill/1 — bump this (and any golden test pinning the shape)
/// only alongside a deliberate, documented shape change.
pub const SCHEMA: &str = "join/backfill/1";

#[derive(Debug, thiserror::Error)]
pub enum BackfillError {
    #[error("spawn git log: {0}")]
    Spawn(std::io::Error),
    #[error("git log failed: {0}")]
    GitLog(String),
    #[error("git log task panicked: {0}")]
    Panicked(String),
}

pub type Result<T> = std::result::Result<T, BackfillError>;

/// One `resolved_by_confidence` bucket — the ladder's 4-valued `Confidence`
/// enum, IN ORDER (`trailer, exact, fuzzy, none`), every bucket always
/// present even at zero (mirrors `provenance::report::confidence_buckets`'
/// "a client can render a stable table" rationale).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ConfidenceCount {
    pub confidence: &'static str,
    pub count: usize,
}

/// [`backfill_repo`]'s result — see the module doc for what each counter
/// means.
#[derive(Debug, Clone, Serialize)]
pub struct BackfillStats {
    pub schema: &'static str,
    pub repo: String,
    /// Every commit `git log` walked (bounded by `depth`/[`MAX_WALK_COMMITS`]).
    pub total: usize,
    pub resolved_by_confidence: Vec<ConfidenceCount>,
    /// Commits that had NO `commit_sessions` cache row before this run —
    /// every one of them got a fresh row written (a cache miss always
    /// triggers a real resolution + upsert; see `ladder::resolve_commit`).
    pub newly_cached: usize,
    /// Commits whose cache row existed BEFORE this run at `confidence:
    /// "none"`, and resolved to something better (`trailer|exact|fuzzy`)
    /// THIS run — i.e. a stale `none` row (past `ladder::TTL_SECS`) that a
    /// later session capture let the ladder finally settle.
    pub upgraded: usize,
    pub duration_ms: u64,
    /// `true` when the up-front commit-map fetch (see the module doc)
    /// failed — the federated kb daemon was disabled or unreachable for at
    /// least part of this run. The trailer arm still resolves fully
    /// locally regardless (see `ladder`'s module doc) — this is a
    /// DEGRADED signal, never a hard failure.
    pub degraded: bool,
}

fn confidence_buckets(counts: [usize; 4]) -> Vec<ConfidenceCount> {
    [
        Confidence::Trailer,
        Confidence::Exact,
        Confidence::Fuzzy,
        Confidence::None,
    ]
    .into_iter()
    .zip(counts)
    .map(|(confidence, count)| ConfidenceCount {
        confidence: confidence.as_str(),
        count,
    })
    .collect()
}

fn confidence_index(c: Confidence) -> usize {
    match c {
        Confidence::Trailer => 0,
        Confidence::Exact => 1,
        Confidence::Fuzzy => 2,
        Confidence::None => 3,
    }
}

/// Blocking `git log --format=%H [--since=@<cutoff>] --max-count=<n> HEAD`,
/// newest-first — run inside `spawn_blocking`, mirroring every other
/// git-subprocess call in this crate (`provenance::report::walk_commits`'
/// doc has the same rationale). `since` is a unix-seconds cutoff (already
/// resolved from `[backfill] depth` by the caller); `None` walks the whole
/// history (still capped by [`MAX_WALK_COMMITS`]).
async fn walk_commits(repo_root: &Path, since: Option<i64>) -> Result<Vec<String>> {
    let repo_root = repo_root.to_path_buf();
    tokio::task::spawn_blocking(move || -> Result<Vec<String>> {
        let max = MAX_WALK_COMMITS.to_string();
        let mut args: Vec<String> = vec![
            "log".to_string(),
            "--format=%H".to_string(),
            "--max-count".to_string(),
            max,
        ];
        if let Some(since) = since {
            args.push(format!("--since=@{since}"));
        }
        args.push("HEAD".to_string());
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(&repo_root)
            .args(&args)
            .output()
            .map_err(BackfillError::Spawn)?;
        if !out.status.success() {
            return Err(BackfillError::GitLog(
                String::from_utf8_lossy(&out.stderr).trim().to_string(),
            ));
        }
        let text = String::from_utf8_lossy(&out.stdout);
        Ok(text
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect())
    })
    .await
    .map_err(|e| BackfillError::Panicked(e.to_string()))?
}

/// The precompute's entry point — `routes::backfill_route`
/// (`POST /api/backfill?repo=`), `kb-code backfill`, and the optional
/// `[backfill] on_boot` background run (`lib.rs::bind_and_spawn`) all call
/// this directly. `depth` is already-resolved (`config::BackfillSection::
/// resolved_depth`) — `None` walks the whole history, `Some(d)` bounds the
/// walk to commits authored within `d` of "now."
pub async fn backfill_repo(
    repo: &RepoEntry,
    repo_id: i64,
    depth: Option<Duration>,
    store: &Arc<Store>,
    kb_client: &KbClient,
) -> Result<BackfillStats> {
    let start = Instant::now();
    let since = depth.map(|d| chrono::Utc::now().timestamp() - d.as_secs() as i64);
    let shas = walk_commits(&repo.path, since).await?;
    let total = shas.len();

    // See the module doc's "Batch the commit-map fetch once up front" —
    // best-effort: a failure here (disabled/unreachable) does not stop the
    // walk (the trailer arm resolves fully locally regardless), it only
    // flips `degraded`.
    let degraded = kb_client.commit_map_snapshot(true).await.is_err();

    let resolutions = stream::iter(shas.into_iter().map(move |sha| async move {
        // Snapshot the PRE-existing cache row (if any) before resolving —
        // `newly_cached`/`upgraded` are both defined relative to this. Its
        // own blocking-pool round trip (store.rs's 2026-08-31 incident
        // note) — `resolve_commit`'s own store segments are wrapped
        // internally.
        let before = {
            let sha_c = sha.clone();
            store
                .run_blocking(move |store| store.get_commit_session(repo_id, &sha_c))
                .await
                .ok()
                .flatten()
        };
        let attribution = ladder::resolve_commit(repo, repo_id, &sha, store, kb_client).await;
        (before, attribution)
    }))
    .buffer_unordered(MAX_CONCURRENCY)
    .collect::<Vec<_>>()
    .await;

    let mut counts = [0usize; 4];
    let mut newly_cached = 0usize;
    let mut upgraded = 0usize;
    for (before, attribution) in resolutions {
        counts[confidence_index(attribution.confidence)] += 1;
        match before {
            None => newly_cached += 1,
            Some(row) => {
                let was_none = Confidence::parse(&row.confidence) == Some(Confidence::None);
                if was_none && attribution.confidence != Confidence::None {
                    upgraded += 1;
                }
            }
        }
    }

    Ok(BackfillStats {
        schema: SCHEMA,
        repo: repo.name.clone(),
        total,
        resolved_by_confidence: confidence_buckets(counts),
        newly_cached,
        upgraded,
        duration_ms: start.elapsed().as_millis() as u64,
        degraded,
    })
}

#[cfg(test)]
#[path = "backfill_tests.rs"]
mod tests;
