//! `fingerprint-verify/1` (V77-P1) — the documented safety net for
//! `kb-code doctor --verify-fingerprints N` (P1's task 5).
//!
//! The `sink.rs` fs-read fast path trusts an `fs::metadata` mtime+size
//! match as a proxy for "content unchanged," which is the standard
//! make/rsync/cargo heuristic and is right the overwhelming majority of the
//! time — but it is a HEURISTIC, not a proof: an editor that restores the
//! original mtime after a save (some do, deliberately, to avoid perturbing
//! build caches) or a same-second, same-length in-place edit can fool it.
//! This route re-hashes a SAMPLE of a repo's tracked files straight off the
//! working tree and compares the result against the stored
//! `files.blob_hash`, so an operator who suspects the heuristic has been
//! fooled (or wants routine reassurance) has a way to check without a full
//! reindex. It writes nothing — a pure read, same posture as
//! `reextract::bill_route`.
//!
//! Deliberately NOT a scheduled/automatic check: the daemon never spawns a
//! background maintenance pass between `Store::open` and the bind
//! (invariant 11's V72-B0 rule), and a periodic full-corpus re-hash would
//! be exactly that.

use axum::extract::{Query, State};
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::routes::{find_repo, ApiError};
use crate::state::SharedState;
use crate::store::StoreBlocking;

pub const FINGERPRINT_VERIFY_SCHEMA: &str = "fingerprint-verify/1";

/// Sample size when the caller names no `sample` — enough to be a useful
/// spot check without turning a `doctor` run into a full-corpus re-hash.
pub const DEFAULT_SAMPLE: usize = 20;
/// Hard cap on `?sample=`. Larger asks are asking for a full re-hash with
/// extra steps; `reextract::MAX_SAMPLE` sets the same kind of ceiling for
/// the same reason.
pub const MAX_SAMPLE: usize = 500;

#[derive(Debug, Deserialize)]
pub struct FingerprintVerifyParams {
    pub repo: String,
    /// Files to sample (default [`DEFAULT_SAMPLE`], capped at
    /// [`MAX_SAMPLE`]). `0` is a legal, if useless, "verify nothing."
    pub sample: Option<usize>,
}

/// One sampled file whose re-hashed content disagrees with the stored
/// fingerprint.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct FingerprintMismatch {
    pub path: String,
    pub stored_blob_hash: String,
    pub actual_blob_hash: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct FingerprintVerifyOut {
    pub schema: &'static str,
    pub repo: String,
    /// Total tracked files this repo has (`files.path` count) — so a
    /// caller can see what fraction `sampled` covers.
    pub total_files: u64,
    /// How many the sample actually asked for (post `MAX_SAMPLE` clamp).
    pub requested_sample: u64,
    /// How many were actually re-hashed — less than `requested_sample`
    /// when the repo has fewer files, or when some sampled paths were
    /// unreadable (see `unreadable`).
    pub sampled: u64,
    /// Sampled paths whose disk content no longer matches the stored
    /// fingerprint. Empty and present (never omitted) on a clean run.
    pub mismatches: Vec<FingerprintMismatch>,
    /// Sampled paths that could not be read from the working tree right
    /// now (deleted, permission, or any other `io::Error`) — reported
    /// honestly rather than silently dropped from `sampled`'s denominator.
    pub unreadable: Vec<String>,
}

pub async fn fingerprint_verify_route(
    State(state): State<SharedState>,
    Query(params): Query<FingerprintVerifyParams>,
) -> Result<impl IntoResponse, ApiError> {
    let (repo, repo_id) = find_repo(&state, &params.repo)?;
    let repo_root = repo.path.clone();
    let repo_name = params.repo.clone();
    let sample = params.sample.unwrap_or(DEFAULT_SAMPLE).min(MAX_SAMPLE);
    let body = state
        .store
        .run_blocking(move |store| -> Result<FingerprintVerifyOut, ApiError> {
            Ok(build_verify(
                store, &repo_root, repo_id, &repo_name, sample,
            )?)
        })
        .await?;
    Ok(Json(body))
}

pub(crate) fn build_verify(
    store: &crate::store::Store,
    repo_root: &std::path::Path,
    repo_id: i64,
    repo_name: &str,
    sample: usize,
) -> Result<FingerprintVerifyOut, crate::store::StoreError> {
    let files = store.list_files(repo_id)?;
    let total_files = files.len() as u64;
    let idx = sample_indices(files.len(), sample, sample_seed());

    let mut mismatches = Vec::new();
    let mut unreadable = Vec::new();
    let mut sampled = 0u64;
    for i in idx {
        let row = &files[i];
        let abs = repo_root.join(&row.path);
        match std::fs::read(&abs) {
            Ok(bytes) => {
                sampled += 1;
                let actual = crate::ingest::git_blob_hash(&bytes);
                if actual != row.blob_hash {
                    mismatches.push(FingerprintMismatch {
                        path: row.path.clone(),
                        stored_blob_hash: row.blob_hash.clone(),
                        actual_blob_hash: actual,
                    });
                }
            }
            Err(_) => unreadable.push(row.path.clone()),
        }
    }

    Ok(FingerprintVerifyOut {
        schema: FINGERPRINT_VERIFY_SCHEMA,
        repo: repo_name.to_string(),
        total_files,
        requested_sample: sample as u64,
        sampled,
        mismatches,
        unreadable,
    })
}

/// A process-lifetime-varying seed (current time) for the route's own
/// sampling — a fresh sample on every call is more useful for a spot check
/// than a deterministic one. `sample_indices` itself is pure and unit-
/// tested with an explicit seed for determinism.
fn sample_seed() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
        // Never 0 — splitmix64 with a zero seed still mixes fine, but a
        // literal 0 reads like "no randomness was asked for" to a reader
        // scanning for a bug.
        .wrapping_add(1)
}

/// `want` distinct indices into `0..total`, via a tiny splitmix64 PRNG —
/// deliberately no `rand` dependency (a diagnostic sampler, not a security
/// primitive) for a crate that carries none today. `want >= total` returns
/// every index, in order (no shuffling needed or done).
fn sample_indices(total: usize, want: usize, seed: u64) -> Vec<usize> {
    if want >= total {
        return (0..total).collect();
    }
    let mut idx: Vec<usize> = (0..total).collect();
    let mut state = seed;
    let mut next_u64 = || {
        // splitmix64 (Vigna) — public-domain, no crate needed.
        state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    };
    // Partial Fisher-Yates: only the first `want` slots need to be final.
    for i in 0..want {
        let remaining = (total - i) as u64;
        let j = i + (next_u64() % remaining) as usize;
        idx.swap(i, j);
    }
    idx.truncate(want);
    idx
}

fn fingerprint_verify_params_accept_without(omit: &str) -> bool {
    let mut map = serde_json::Map::new();
    for (k, v) in [("repo", "r")] {
        if k != omit {
            map.insert(k.to_string(), serde_json::Value::String(v.to_string()));
        }
    }
    serde_json::from_value::<FingerprintVerifyParams>(serde_json::Value::Object(map)).is_ok()
}

pub const FINGERPRINT_VERIFY_ROUTE: crate::entities::RouteContract =
    crate::entities::RouteContract {
        path: "/api/fingerprints/verify",
        handler: "fingerprint_verify::fingerprint_verify_route",
        required_params: &["repo"],
        params_accept_without: fingerprint_verify_params_accept_without,
    };

/// The one route V77-P1 adds. Walked from the server side by this module's
/// own `every_declared_v77_p1_route_is_registered_and_requires_its_params`
/// test — the same LOCAL-test shape `reextract::V72_H2B_ROUTES` uses rather
/// than joining the big cross-milestone chain in `entities::mod`'s tests
/// (that chain's own module doc names it as one option, not the only one;
/// `reextract`'s bill route is the precedent for a route proven this way).
pub const V77_P1_ROUTES: &[crate::entities::RouteContract] = &[FINGERPRINT_VERIFY_ROUTE];

#[cfg(test)]
mod tests {
    use super::*;

    const ROUTER_SRC: &str = include_str!("router.rs");

    #[test]
    fn every_declared_v77_p1_route_is_registered_and_requires_its_params() {
        for c in V77_P1_ROUTES {
            let nested = c
                .path
                .strip_prefix("/api")
                .expect("every route path is /api-nested");
            assert!(
                ROUTER_SRC.contains(&format!("\"{nested}\"")),
                "{}: declared but never registered in router.rs",
                c.path
            );
            assert!(
                ROUTER_SRC.contains(c.handler),
                "{}: registered path but no {} handler named in router.rs",
                c.path,
                c.handler
            );
            assert!(
                (c.params_accept_without)(""),
                "{}: its own params struct rejects a COMPLETE query map",
                c.path
            );
            for p in c.required_params {
                assert!(
                    !(c.params_accept_without)(p),
                    "{}: the route accepts a query missing required param {p:?}",
                    c.path
                );
            }
        }
    }

    // --- sample_indices ------------------------------------------------

    #[test]
    fn sample_indices_returns_every_index_when_want_covers_total() {
        assert_eq!(sample_indices(3, 5, 42), vec![0, 1, 2]);
        assert_eq!(sample_indices(3, 3, 42), vec![0, 1, 2]);
    }

    #[test]
    fn sample_indices_returns_the_requested_count_of_distinct_indices() {
        let idx = sample_indices(100, 10, 42);
        assert_eq!(idx.len(), 10);
        let mut sorted = idx.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), 10, "indices must be distinct: {idx:?}");
        assert!(idx.iter().all(|&i| i < 100));
    }

    #[test]
    fn sample_indices_is_deterministic_for_a_given_seed() {
        assert_eq!(sample_indices(50, 7, 123), sample_indices(50, 7, 123));
    }

    // --- build_verify ----------------------------------------------------

    fn fixture() -> (
        tempfile::TempDir,
        crate::store::Store,
        std::path::PathBuf,
        i64,
    ) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().join("repo");
        std::fs::create_dir_all(&root).unwrap();
        let store = crate::store::Store::open(&tmp.path().join("index.db")).expect("store");
        let repo_id = store
            .upsert_repo("fixture", &root.to_string_lossy())
            .expect("repo");
        for (rel, body) in [("a.rs", "fn a() {}\n"), ("b.rs", "fn b() {}\n")] {
            let abs = root.join(rel);
            std::fs::write(&abs, body).unwrap();
            let hash = crate::ingest::git_blob_hash(body.as_bytes());
            store
                .upsert_file(repo_id, rel, &hash, "rust", body.len() as u64)
                .unwrap();
        }
        (tmp, store, root, repo_id)
    }

    #[test]
    fn a_clean_repo_reports_no_mismatches() {
        let (_tmp, store, root, repo_id) = fixture();
        let out = build_verify(&store, &root, repo_id, "fixture", 10).unwrap();
        assert_eq!(out.total_files, 2);
        assert_eq!(out.sampled, 2);
        assert!(out.mismatches.is_empty());
        assert!(out.unreadable.is_empty());
    }

    #[test]
    fn a_disk_edit_that_the_stored_hash_never_saw_is_reported() {
        let (_tmp, store, root, repo_id) = fixture();
        // Simulate the exact editor-preserves-mtime failure mode: the file
        // on disk no longer matches what `files.blob_hash` records, with
        // no upsert in between (as if the fs-read fast path had wrongly
        // trusted a stale mtime+size fingerprint).
        std::fs::write(root.join("a.rs"), "fn a() { /* edited */ }\n").unwrap();
        let out = build_verify(&store, &root, repo_id, "fixture", 10).unwrap();
        assert_eq!(out.mismatches.len(), 1);
        assert_eq!(out.mismatches[0].path, "a.rs");
    }

    #[test]
    fn a_deleted_file_is_reported_as_unreadable_not_a_mismatch() {
        let (_tmp, store, root, repo_id) = fixture();
        std::fs::remove_file(root.join("a.rs")).unwrap();
        let out = build_verify(&store, &root, repo_id, "fixture", 10).unwrap();
        assert!(out.mismatches.is_empty());
        assert_eq!(out.unreadable, vec!["a.rs".to_string()]);
        assert_eq!(out.sampled, 1);
    }
}
