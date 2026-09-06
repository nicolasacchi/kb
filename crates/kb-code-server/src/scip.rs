//! S1 — `POST /api/scip/ingest`: the opt-in EXACT precision tier's server
//! half. Parsing a `.scip` index (protobuf) lives entirely in `kb-code-cli`
//! (the `scip` crate — Sourcegraph's — is a kb-code-cli-ONLY dependency, so
//! this daemon stays dependency-free of it, per the phase brief: "keep the
//! server dependency-free"). This module's ONLY job is to receive already-
//! mapped `{name, role, line, col_start, col_end}` rows per document path
//! (see `kb_code_cli`'s own `scip_map` module for how a `.scip` document's
//! occurrences become that shape) and turn them into `occurrences` rows —
//! resolving each path to its CURRENT `(blob_hash, salt)` and replacing
//! that blob's `source = 'scip'` rows (`store::Store::
//! replace_scip_occurrences`).
//!
//! # Staleness — never ingest positions against drifted content
//!
//! Each document in the request ALSO carries the `blob_hash` the CLI
//! computed by reading the file's CURRENT bytes off the SAME working tree
//! the `.scip` index was just generated against (see the CLI's own doc for
//! exactly when/how it reads them). This handler compares that
//! CLI-computed hash against the blob_hash THIS DAEMON currently has on
//! file for the path (`store::Store::get_file`) — a mismatch means the
//! file changed between whenever the CLI read it and whatever this daemon
//! has indexed (a fast-moving working tree, a race, or simply a daemon
//! that hasn't caught up yet), and the document is SKIPPED (`stale: true`
//! in the response) rather than writing SCIP positions against content
//! that may no longer be at those exact offsets. A path this daemon has
//! never indexed at all (`get_file` → `None`) is skipped the same way
//! (`"untracked"` — there is no trustworthy current blob_hash to compare
//! against), as is a path whose current `files.lang` isn't a recognised
//! language at all (`"unsupported-lang"` — binary/too-large/lfs/unknown
//! tier, or a language `lang::for_id` doesn't know, meaning there's no
//! `salt` to key the occurrences rows under).
//!
//! # Role mapping
//!
//! The CLI maps a SCIP `Occurrence.symbol_roles` bitset down to exactly
//! `"def"` (the `Definition` bit set) or `"ref"` (otherwise) — see
//! `kb_code_cli::scip_map`'s own doc. This handler trusts that mapping
//! verbatim (never re-derives or validates it) — the CLI is this daemon's
//! own sibling binary, not an untrusted third party, and this route is
//! LOOPBACK-ONLY (`router.rs`) regardless.
//!
//! # Ordinal space
//!
//! `store::Store::replace_scip_occurrences` continues the shared
//! `(blob_hash, salt, ordinal)` space rather than restarting at 0 — see
//! that method's own doc and migration `V0010__occurrences_source.sql`.
//!
//! # Staleness surfacing (PRR-N12, N1)
//!
//! On a successful (HTTP 200) call, this route ALSO appends one
//! `scip_runs` row (`store::Store::record_scip_run`) — the repo's git HEAD
//! at THIS moment (one extra `git rev-parse HEAD`-equivalent read via
//! `GitRepo::open`/`head_info`, at the repo path this handler already has
//! resolved via `find_repo`) plus `docs_accepted`. `GET /api/repos`'s
//! `ScipStatus` reads only the newest such row to answer "is this repo's
//! SCIP index fresh against its current HEAD" — see `routes::scip_status`.
//! Stamped unconditionally on success, even when `docs_accepted == 0`
//! (every doc was stale/untracked/unsupported-lang): that's still an
//! honest "an ingest ran at this HEAD," and `docs_covered` separately
//! surfaces the zero, so `fresh = true` with `docs_covered = 0` is never
//! misleading. A HEAD read that fails (a detached/unborn/unopenable repo)
//! is logged and SKIPPED — it never fails the primary ingest response,
//! which has already succeeded by the time this runs.

use crate::git::GitRepo;
use crate::routes::{safe_rel_path, ApiError};
use crate::state::SharedState;
use crate::store::{ScipOccurrenceIn, Store, StoreBlocking};
use axum::extract::State;
use axum::http::{header, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
pub struct ScipIngestBody {
    pub repo: String,
    pub docs: Vec<ScipDocWire>,
}

#[derive(Debug, Deserialize)]
pub struct ScipDocWire {
    /// Repo-relative path (validated via [`safe_rel_path`] before use).
    pub path: String,
    /// The git-compatible blob hash (`ingest::git_blob_hash`) the CLI
    /// computed from the file's CURRENT bytes — see the module doc's
    /// "Staleness" section.
    pub blob_hash: String,
    pub occurrences: Vec<ScipOccWire>,
}

#[derive(Debug, Deserialize)]
pub struct ScipOccWire {
    pub name: String,
    /// `"def"` | `"ref"` — see the module doc's "Role mapping" section.
    pub role: String,
    pub line: u32,
    pub col_start: u32,
    pub col_end: u32,
}

#[derive(Debug, Serialize)]
pub struct SkippedDoc {
    pub path: String,
    /// `true` iff the CLI's `blob_hash` didn't match this daemon's current
    /// `files` row for `path` — the ONE reason the module doc calls
    /// "staleness" in the strict sense. `false` for `"untracked"`/
    /// `"unsupported-lang"` (a different, honestly distinct condition —
    /// see `reason`).
    pub stale: bool,
    /// `"stale"` | `"untracked"` | `"unsupported-lang"` — see the module
    /// doc's "Staleness" section for what each means.
    pub reason: &'static str,
}

#[derive(Debug, Serialize)]
pub struct ScipIngestResponse {
    pub repo: String,
    pub docs_received: usize,
    pub docs_accepted: usize,
    pub occurrences_written: usize,
    pub skipped: Vec<SkippedDoc>,
}

/// `POST /api/scip/ingest` — mounted on the LOOPBACK-ONLY sub-router
/// (`router.rs`), same gate as `checkout`/`session-diff`: this is a bulk
/// write of exact-tier positions the operator is trusted to have generated
/// themselves (`rust-analyzer scip .` / `scip-typescript index`), not a
/// browsing-class read.
pub async fn scip_ingest_route(
    State(state): State<SharedState>,
    Json(body): Json<ScipIngestBody>,
) -> Result<impl IntoResponse, ApiError> {
    // 2026-08-31 incident (store.rs module doc): this handler has no
    // `.await` anywhere in it (repo lookup, the per-doc store writes, and
    // the git-head + scip_runs stamp are all synchronous) — the whole body
    // runs as ONE closure on the blocking pool instead of parking an async
    // worker for the entire ingest loop.
    let state_bg = state.clone();
    let out = state
        .store
        .run_blocking(move |store| scip_ingest(store, &state_bg, body))
        .await?;
    Ok((
        StatusCode::OK,
        [(header::CACHE_CONTROL, "no-store")],
        Json(out),
    ))
}

/// The sync body of [`scip_ingest_route`] — split out so it can run as a
/// single `run_blocking` closure (2026-08-31 incident, store.rs module
/// doc). `body` is moved in by value; every field is read at most once
/// after `body.docs` is consumed by the loop below, same ownership shape
/// the original inline handler already relied on.
fn scip_ingest(
    store: &Store,
    state: &SharedState,
    body: ScipIngestBody,
) -> Result<ScipIngestResponse, ApiError> {
    let (repo, repo_id) = crate::routes::find_repo(state, &body.repo)?;

    let docs_received = body.docs.len();
    let mut docs_accepted = 0usize;
    let mut occurrences_written = 0usize;
    let mut skipped: Vec<SkippedDoc> = Vec::new();

    for doc in body.docs {
        let Ok(path) = safe_rel_path(&doc.path) else {
            skipped.push(SkippedDoc {
                path: doc.path,
                stale: false,
                reason: "untracked",
            });
            continue;
        };
        let Some(file) = store.get_file(repo_id, path)? else {
            skipped.push(SkippedDoc {
                path: doc.path,
                stale: false,
                reason: "untracked",
            });
            continue;
        };
        if file.blob_hash != doc.blob_hash {
            skipped.push(SkippedDoc {
                path: doc.path,
                stale: true,
                reason: "stale",
            });
            continue;
        }
        let Some(lang_info) = crate::lang::for_id(&file.lang) else {
            skipped.push(SkippedDoc {
                path: doc.path,
                stale: false,
                reason: "unsupported-lang",
            });
            continue;
        };

        let occs: Vec<ScipOccurrenceIn> = doc
            .occurrences
            .into_iter()
            .map(|o| ScipOccurrenceIn {
                name: o.name,
                role: o.role,
                line: o.line,
                col_start: o.col_start,
                col_end: o.col_end,
            })
            .collect();
        occurrences_written += occs.len();
        store.replace_scip_occurrences(&file.blob_hash, lang_info.salt, &occs)?;
        docs_accepted += 1;
    }

    // PRR-N12 (N1) — stamp a `scip_runs` row on success (see the module
    // doc's "Staleness surfacing" section). `repo` is the SAME `RepoEntry`
    // `find_repo` already resolved above — no second lookup.
    match GitRepo::open(&repo.path).and_then(|g| g.head_info()) {
        Ok(head) => {
            if let Some(sha) = head.sha {
                let ingested_at = chrono::Utc::now().timestamp();
                if let Err(e) =
                    store.record_scip_run(repo_id, &sha, ingested_at, docs_accepted as i64)
                {
                    tracing::warn!(
                        repo = %body.repo, error = %e,
                        "scip ingest: failed to stamp scip_runs (ingest itself still succeeded)"
                    );
                }
            } else {
                tracing::warn!(
                    repo = %body.repo,
                    "scip ingest: repo HEAD is unborn (no commits) — skipping scip_runs stamp"
                );
            }
        }
        Err(e) => {
            tracing::warn!(
                repo = %body.repo, error = %e,
                "scip ingest: could not read repo HEAD — skipping scip_runs stamp"
            );
        }
    }

    Ok(ScipIngestResponse {
        repo: body.repo,
        docs_received,
        docs_accepted,
        occurrences_written,
        skipped,
    })
}

#[cfg(test)]
mod tests {
    use crate::config::{KbCodeConfig, RepoEntry, ScipRepoEntry, ScipSection};
    use kb_core::paths::KbPaths;

    fn git(dir: &std::path::Path, args: &[&str]) {
        let out = std::process::Command::new("git")
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

    fn fixture_repo() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        git(dir, &["init", "-q", "-b", "main"]);
        git(dir, &["config", "user.email", "t@example.com"]);
        git(dir, &["config", "user.name", "T"]);
        std::fs::write(dir.join("a.rs"), "fn widget() -> i32 {\n    1\n}\n").unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "c1"]);
        tmp
    }

    async fn boot(repo_dir: &std::path::Path) -> (tempfile::TempDir, String) {
        boot_with_config(repo_dir, KbCodeConfig::default()).await
    }

    /// PRR-N12 (N1) — same as [`boot`], but lets a test supply the rest of
    /// `KbCodeConfig` (e.g. `[scip]`) while still pinning `repos` to the one
    /// fixture repo named `"r"`.
    async fn boot_with_config(
        repo_dir: &std::path::Path,
        extra: KbCodeConfig,
    ) -> (tempfile::TempDir, String) {
        let cfg = KbCodeConfig {
            repos: vec![RepoEntry {
                name: "r".to_string(),
                path: std::fs::canonicalize(repo_dir).unwrap(),
            }],
            ..extra
        };
        let tmp = tempfile::tempdir().unwrap();
        let paths = KbPaths::rooted_at(tmp.path(), "kb-code");
        let (addr, _task) = crate::serve_on_random_port_with_paths(cfg, paths)
            .await
            .expect("serve_on_random_port_with_paths");
        (tmp, format!("http://{addr}"))
    }

    fn real_blob_hash(bytes: &[u8]) -> String {
        crate::ingest::git_blob_hash(bytes)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn fresh_blob_is_accepted_and_written() {
        let repo_tmp = fixture_repo();
        let src = std::fs::read(repo_tmp.path().join("a.rs")).unwrap();
        let (_daemon_tmp, base) = boot(repo_tmp.path()).await;
        let client = reqwest::Client::new();

        // Wait for the boot-time initial index to register the file.
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(20);
        loop {
            let repos: serde_json::Value = client
                .get(format!("{base}/api/repos"))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            if repos[0]["file_count"].as_u64().unwrap_or(0) > 0
                || tokio::time::Instant::now() >= deadline
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }

        let resp = client
            .post(format!("{base}/api/scip/ingest"))
            .json(&serde_json::json!({
                "repo": "r",
                "docs": [{
                    "path": "a.rs",
                    "blob_hash": real_blob_hash(&src),
                    "occurrences": [
                        {"name": "widget", "role": "def", "line": 1, "col_start": 3, "col_end": 9}
                    ]
                }]
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["docs_received"], 1);
        assert_eq!(body["docs_accepted"], 1);
        assert_eq!(body["occurrences_written"], 1);
        assert_eq!(body["skipped"].as_array().unwrap().len(), 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn drifted_doc_is_skipped_as_stale() {
        let repo_tmp = fixture_repo();
        let (_daemon_tmp, base) = boot(repo_tmp.path()).await;
        let client = reqwest::Client::new();

        let resp = client
            .post(format!("{base}/api/scip/ingest"))
            .json(&serde_json::json!({
                "repo": "r",
                "docs": [{
                    "path": "a.rs",
                    "blob_hash": "0000000000000000000000000000000000000000",
                    "occurrences": [
                        {"name": "widget", "role": "def", "line": 1, "col_start": 3, "col_end": 9}
                    ]
                }]
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["docs_accepted"], 0);
        let skipped = body["skipped"].as_array().unwrap();
        assert_eq!(skipped.len(), 1);
        assert_eq!(skipped[0]["path"], "a.rs");
        assert_eq!(skipped[0]["stale"], true);
        assert_eq!(skipped[0]["reason"], "stale");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn untracked_path_is_skipped_honestly() {
        let repo_tmp = fixture_repo();
        let (_daemon_tmp, base) = boot(repo_tmp.path()).await;
        let client = reqwest::Client::new();

        let resp = client
            .post(format!("{base}/api/scip/ingest"))
            .json(&serde_json::json!({
                "repo": "r",
                "docs": [{
                    "path": "never-indexed.rs",
                    "blob_hash": "deadbeef",
                    "occurrences": []
                }]
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["docs_accepted"], 0);
        let skipped = body["skipped"].as_array().unwrap();
        assert_eq!(skipped.len(), 1);
        assert_eq!(skipped[0]["stale"], false);
        assert_eq!(skipped[0]["reason"], "untracked");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn non_loopback_caller_gets_404() {
        let repo_tmp = fixture_repo();
        let (_daemon_tmp, base) = boot(repo_tmp.path()).await;
        let client = reqwest::Client::new();

        let resp = client
            .post(format!("{base}/api/scip/ingest"))
            .header("X-Forwarded-For", "8.8.8.8")
            .json(&serde_json::json!({ "repo": "r", "docs": [] }))
            .send()
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            404,
            "scip ingest must 404 a non-loopback caller"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn unknown_repo_404s() {
        let repo_tmp = fixture_repo();
        let (_daemon_tmp, base) = boot(repo_tmp.path()).await;
        let client = reqwest::Client::new();

        let resp = client
            .post(format!("{base}/api/scip/ingest"))
            .json(&serde_json::json!({ "repo": "does-not-exist", "docs": [] }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);
    }

    // ── PRR-N12: scip_runs stamping + `GET /api/repos` ScipStatus ─────────

    fn rust_scip_config() -> KbCodeConfig {
        KbCodeConfig {
            scip: ScipSection {
                repos: vec![ScipRepoEntry {
                    name: "r".to_string(),
                    command: vec!["true".to_string()],
                    output: "index.scip".to_string(),
                    langs: vec!["rust".to_string()],
                }],
            },
            ..KbCodeConfig::default()
        }
    }

    /// Poll `GET /api/repos` until `pred` holds over `repos[0]`, or
    /// `timeout` elapses — mirrors `fresh_blob_is_accepted_and_written`'s
    /// own inline poll loop above, factored out since the new tests below
    /// need it twice each.
    async fn wait_for_repo0(
        client: &reqwest::Client,
        base: &str,
        timeout: std::time::Duration,
        pred: impl Fn(&serde_json::Value) -> bool,
    ) -> serde_json::Value {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let repos: serde_json::Value = client
                .get(format!("{base}/api/repos"))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            if pred(&repos["repos"][0]) || tokio::time::Instant::now() >= deadline {
                return repos["repos"][0].clone();
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn unconfigured_repo_reports_scip_status_defaults() {
        let repo_tmp = fixture_repo();
        let (_daemon_tmp, base) = boot(repo_tmp.path()).await;
        let client = reqwest::Client::new();

        let repo0 = wait_for_repo0(&client, &base, std::time::Duration::from_secs(20), |r| {
            r["file_count"].as_u64().unwrap_or(0) > 0
        })
        .await;

        let scip = &repo0["scip"];
        assert_eq!(scip["configured"], false);
        assert_eq!(scip["command"].as_array().unwrap().len(), 0);
        assert_eq!(scip["output"], "");
        assert_eq!(scip["langs"].as_array().unwrap().len(), 0);
        assert!(scip["last_ingested_at"].is_null());
        assert!(scip["head_sha_at_ingest"].is_null());
        assert_eq!(scip["fresh"], false);
        assert_eq!(scip["docs_covered"], 0);
        assert_eq!(scip["docs_total"], 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn configured_repo_before_any_ingest_reports_never_ingested() {
        let repo_tmp = fixture_repo();
        let (_daemon_tmp, base) = boot_with_config(repo_tmp.path(), rust_scip_config()).await;
        let client = reqwest::Client::new();

        let repo0 = wait_for_repo0(&client, &base, std::time::Duration::from_secs(20), |r| {
            r["file_count"].as_u64().unwrap_or(0) > 0
        })
        .await;

        let scip = &repo0["scip"];
        assert_eq!(scip["configured"], true);
        assert_eq!(scip["command"].as_array().unwrap().clone(), vec!["true"]);
        assert_eq!(scip["output"], "index.scip");
        assert_eq!(scip["langs"].as_array().unwrap().clone(), vec!["rust"]);
        // `a.rs` from `fixture_repo()` is the repo's one rust file.
        assert_eq!(scip["docs_total"], 1);
        assert_eq!(scip["docs_covered"], 0);
        assert_eq!(scip["fresh"], false);
        assert!(scip["last_ingested_at"].is_null());
        assert!(scip["head_sha_at_ingest"].is_null());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn successful_ingest_stamps_scip_runs_and_status_turns_fresh_then_stale() {
        let repo_tmp = fixture_repo();
        let src = std::fs::read(repo_tmp.path().join("a.rs")).unwrap();
        let (_daemon_tmp, base) = boot_with_config(repo_tmp.path(), rust_scip_config()).await;
        let client = reqwest::Client::new();

        let repo0 = wait_for_repo0(&client, &base, std::time::Duration::from_secs(20), |r| {
            r["file_count"].as_u64().unwrap_or(0) > 0
        })
        .await;
        let head_before = repo0["head"]["sha"].as_str().unwrap().to_string();

        // Ingest at the current HEAD.
        let resp = client
            .post(format!("{base}/api/scip/ingest"))
            .json(&serde_json::json!({
                "repo": "r",
                "docs": [{
                    "path": "a.rs",
                    "blob_hash": real_blob_hash(&src),
                    "occurrences": [
                        {"name": "widget", "role": "def", "line": 1, "col_start": 3, "col_end": 9}
                    ]
                }]
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let ingest_body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(ingest_body["docs_accepted"], 1);

        // Status must now report fresh, with docs_covered bumped and a
        // real `last_ingested_at`/`head_sha_at_ingest`.
        let repo1 = wait_for_repo0(&client, &base, std::time::Duration::from_secs(20), |r| {
            r["scip"]["fresh"].as_bool().unwrap_or(false)
        })
        .await;
        let scip1 = &repo1["scip"];
        assert_eq!(scip1["fresh"], true);
        assert_eq!(scip1["docs_covered"], 1);
        assert_eq!(scip1["docs_total"], 1);
        assert!(scip1["last_ingested_at"].as_str().is_some());
        assert_eq!(
            scip1["head_sha_at_ingest"].as_str().unwrap(),
            head_before.as_str()
        );
        assert_eq!(
            scip1["current_head_sha"].as_str().unwrap(),
            head_before.as_str()
        );

        // Move HEAD with an unrelated (non-rust) commit — status must fall
        // back to stale (`fresh = false`) without losing the recorded
        // ingest: `head_sha_at_ingest` stays pinned to the OLD sha,
        // `docs_covered` is untouched (a.rs's scip occurrence is still
        // there), `docs_total` is untouched (the new file isn't rust).
        std::fs::write(repo_tmp.path().join("b.txt"), b"unrelated\n").unwrap();
        git(repo_tmp.path(), &["add", "-A"]);
        git(repo_tmp.path(), &["commit", "-q", "-m", "c2"]);

        let repo2 = wait_for_repo0(&client, &base, std::time::Duration::from_secs(20), |r| {
            r["head"]["sha"].as_str().map(str::to_string) != Some(head_before.clone())
                && r["head"]["sha"].as_str().is_some()
        })
        .await;
        let scip2 = &repo2["scip"];
        assert_eq!(scip2["fresh"], false);
        assert_eq!(
            scip2["head_sha_at_ingest"].as_str().unwrap(),
            head_before.as_str()
        );
        assert_ne!(
            scip2["current_head_sha"].as_str().unwrap(),
            head_before.as_str()
        );
        assert_eq!(scip2["docs_covered"], 1);
        assert_eq!(scip2["docs_total"], 1);
    }
}
