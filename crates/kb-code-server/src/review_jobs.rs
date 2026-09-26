//! V76-R1a — daemon-side `start-pr` jobs (`POST /api/reviews/pr?async=1`).
//!
//! The first `start-pr` against a mirror always spends longer than the
//! CLI's old 10 s client timeout in `git fetch` — the fetch completes
//! server-side while the client has already given up, so a second call
//! "succeeds" and a third 409s on the duplicate binding. This module makes
//! the fetch + patchset creation an explicit daemon-side JOB:
//!
//! - `POST /api/reviews/pr?async=1` returns `202 {job_id, status:
//!   "running"}` immediately and runs the SAME
//!   [`crate::reviews::create_review_pr_value`] the synchronous route
//!   uses. A second `POST` for the same `(repo, pr_number)` while a job
//!   is running ATTACHES to it — same `job_id`, `"attached": true` —
//!   instead of starting a second fetch.
//! - `GET /api/reviews/jobs/{id}` (an ordinary bearer read) reports
//!   `{status: running|done|failed, progress: {stage}, review_id?,
//!   error?}`. A `done` job carries the full creation envelope under
//!   `result` — byte-for-byte what the synchronous `POST` would have
//!   returned. A `failed` job carries the refusal message verbatim in
//!   `error` and, when the refusal is typed (e.g. a closed review's
//!   `urn:kb:errors:review-closed`), its URN in `error_type`.
//!
//! Jobs are IN-MEMORY ONLY — a `parking_lot::Mutex<HashMap>` on
//! [`crate::state::AppState`], per-boot like `file_index`/`symbol_index`.
//! A job is a claim about work in flight in THIS process; persisted job
//! rows would be a second copy that can go stale in ways a missing entry
//! cannot (a rebooted daemon simply has no jobs). Root CLAUDE.md
//! invariant #15: the guard is taken and released inside one statement
//! and never crosses an `.await`. Entries are swept on every admission
//! and every read — there is no background reaper for a map this small.
//! The two horizons are deliberately UNLIKE each other: a SETTLED entry
//! is dropped [`JOB_TTL_SECS`] (1 h) after it settled, and a RUNNING one
//! is never swept on that rule at all — only by the stuck-job horizon
//! [`STUCK_JOB_HORIZON_SECS`] (6 h after creation). A `review sync
//! --open` loop legitimately runs for as long as its caller's poll
//! budget, so creation age is not a safe way to bound a running entry.
//!
//! The synchronous behaviour of `POST /api/reviews/pr` is untouched
//! (`?async=0` or the flag absent — see
//! [`crate::reviews::StartPrParams::wants_async`]), so every pre-V76
//! caller and test keeps its byte-identical contract; the CLI is the one
//! that opts into async (its `start-pr` always sends `?async=1` and
//! polls).

use crate::reviews::{CreateReviewPrBody, OnClosed, ERR_REVIEW_CLOSED};
use crate::routes::ApiError;
use crate::state::SharedState;
use axum::extract::{Path as AxumPath, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Settled-entry TTL: a `done`/`failed` entry — the kind a poller can
/// still read a result or a refusal out of — is dropped this long after
/// it SETTLED. The clock starts at the settle, not at creation, so a
/// job that took an hour still has a full hour of readable result.
///
/// A RUNNING entry is NEVER swept on this rule. `review sync --open` is
/// a whole-repo loop whose CLI poller waits up to 3600 s by default, so
/// a creation-age sweep killed legitimate work mid-run (the review row
/// it would have written was lost, and a re-run started a second loop).
/// Only [`STUCK_JOB_HORIZON_SECS`] may drop one. The review row, once
/// created, is the durable record — never the job.
pub const JOB_TTL_SECS: u64 = 3600;

/// Stuck-job horizon: the ONLY rule that may drop a RUNNING entry, and
/// only once that entry has been alive this long. It exists because
/// `ReviewJob::settled` is written in exactly one place — inside the
/// spawned task, immediately after `run` returns — so a job that never
/// gets there (a wedged git subprocess, a panic in the job body) is
/// otherwise retained for the entire process lifetime: it 409s every
/// later admission on its `(kind, repo, pr)` key, makes the same key
/// attach to a job that reports `"status": "running"` forever, and
/// leaks, because `sweep` is the only removal path.
///
/// 21 600 s = 6 h, six times the longest wait any caller actually
/// imposes: `kb-code review sync --open`'s `--wait` default is 3600 s
/// (`kb_code_cli::review_sync`'s `DEFAULT_WAIT_OPEN`; a single PR is
/// 600 s). A job that burns its entire client-side budget is still
/// five hours inside the horizon. When the horizon DOES fire the entry
/// is simply dropped: the next admission mints a FRESH job instead of
/// attaching to a corpse, and the corpse's own poller gets a 404
/// rather than a status that will never change.
pub const STUCK_JOB_HORIZON_SECS: u64 = 6 * 3600;

/// One in-flight (or settled) start-pr job.
#[derive(Debug, Clone)]
pub struct ReviewJob {
    pub id: String,
    /// RS-U10b — what the job runs: `start-pr` (V76-R1a) or `sync`
    /// (`crate::review_sync`). A job only ATTACHES to a running job of the
    /// SAME kind, so a `review sync` never hands a start-pr poller its
    /// differently-shaped result (or the reverse).
    pub kind: &'static str,
    pub repo: String,
    pub pr_number: u32,
    /// `running` | `done` | `failed` — the closed vocabulary the wire
    /// reports.
    pub status: &'static str,
    /// Coarse stage for `progress.stage`: `fetch` → `base` → `patchset`
    /// → `enrich` → `done`. Set by [`crate::reviews::create_review_pr_value`]
    /// via [`set_stage`]; deliberately NOT a percentage — the fetch's own
    /// progress is not observable from here and an invented `pct` would be
    /// a measured-looking guess.
    pub stage: &'static str,
    pub created: Instant,
    /// RS-U10b review fix — when the job settled (`done`/`failed`); the
    /// TTL runs from HERE, and a RUNNING entry (`None`) is never swept
    /// by the TTL — only by [`STUCK_JOB_HORIZON_SECS`], measured from
    /// `created`. It stays `None` for the whole life of a task that
    /// never returns, which is exactly what that horizon exists for.
    pub settled: Option<Instant>,
    /// RS-U10b review fix — a fingerprint of the request (sync: dry_run,
    /// open, merged_since, base, title, reopen). A request only attaches to
    /// a running job with the SAME key; a different one is a 409
    /// `job-conflict` naming the running job.
    pub key: String,
    pub review_id: Option<i64>,
    /// The full creation envelope on success (what the synchronous route
    /// returns as its body); also kept for a non-success `(status, body)`
    /// outcome (e.g. the duplicate-binding 409) so a poller sees the same
    /// payload the route would have sent.
    pub result: Option<serde_json::Value>,
    pub error: Option<String>,
    /// The refusal's RFC 7807 `type` URN when one was attached
    /// (e.g. `urn:kb:errors:review-closed`) — a poller branches on this,
    /// never on the prose.
    pub error_type: Option<&'static str>,
    /// RS-U10b — the HTTP status the synchronous route would have
    /// answered for a failed job (`400`, `409`, `502`, …), so a poller
    /// maps it onto its exit-code table without re-deriving it from prose.
    pub error_status: Option<u16>,
}

/// The job table: `job_id` → job. `parking_lot::Mutex` (the 2026-09-01
/// starvation incident's ruling for every short in-process lock in this
/// crate); the guard never crosses an `.await`.
pub type ReviewJobs = parking_lot::Mutex<HashMap<String, ReviewJob>>;

/// The progress handle [`crate::reviews::create_review_pr_value`] takes:
/// the shared table plus this job's id.
pub type JobHandle = (Arc<ReviewJobs>, String);

/// Coarse stage update — one lock, one mutation, no `.await` anywhere
/// near the guard.
pub fn set_stage(job: &Option<JobHandle>, stage: &'static str) {
    if let Some((jobs, id)) = job {
        if let Some(j) = jobs.lock().get_mut(id) {
            j.stage = stage;
        }
    }
}

/// Drop every SETTLED entry whose result has been readable for
/// [`JOB_TTL_SECS`], and every RUNNING entry that has outlived
/// [`STUCK_JOB_HORIZON_SECS`]. The horizon is how a job whose task never
/// settles is bounded at all (RS-U10b review fix: `settled` is only
/// written after `run` returns, so a wedged task leaves the entry
/// `running` forever) — a creation-age sweep is NOT, because a
/// `sync --open` loop may legitimately run past an hour and dropping it
/// mid-run lost the result and let a rerun start a second loop. Called
/// on admission and on every read — O(map), and the map is tiny by
/// construction.
fn sweep(jobs: &ReviewJobs) {
    let ttl = Duration::from_secs(JOB_TTL_SECS);
    let horizon = Duration::from_secs(STUCK_JOB_HORIZON_SECS);
    jobs.lock().retain(|_, j| match j.settled {
        Some(at) => at.elapsed() < ttl,
        None => j.created.elapsed() < horizon,
    });
}

/// The `urn:kb:errors:job-conflict` URN: a running job of the same kind for
/// the same `(repo, pr)` was started with a DIFFERENT request.
pub const URN_JOB_CONFLICT: &str = "urn:kb:errors:job-conflict";

/// The `?async=1` half of `POST /api/reviews/pr` — attach to a running
/// job for the same `(repo, pr_number)` or mint one and spawn the work.
/// The route's gate is unchanged (loopback-only, inherited from the
/// sub-router this handler hangs on).
pub async fn start_or_attach(
    state: SharedState,
    body: CreateReviewPrBody,
    on_closed: Option<OnClosed>,
) -> Result<Response, ApiError> {
    let repo = body.repo.clone();
    let pr_number = body.pr_number;
    start_job(
        state,
        "start-pr",
        repo,
        pr_number,
        String::new(),
        move |st, handle| async move {
            let _serial = crate::review_sync::repo_guard(&st, &body.repo).await;
            crate::reviews::create_review_pr_value(&st, body, Some(handle), on_closed).await
        },
    )
    .await
}

/// RS-U10a/U10b — the generalized daemon-side job: attach to a RUNNING
/// job of the same `(kind, repo, pr_number)` or mint one and spawn `run`.
/// `run` produces the same `(status, body)` pair its synchronous route
/// would answer; a success status settles `done` with the body under
/// `result`, anything else `failed`. `pr_number` is `0` for a job that is
/// not about one PR (`review sync --open`).
pub async fn start_job<F, Fut>(
    state: SharedState,
    kind: &'static str,
    repo: String,
    pr_number: u32,
    key: String,
    run: F,
) -> Result<Response, ApiError>
where
    F: FnOnce(SharedState, JobHandle) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = Result<(StatusCode, serde_json::Value), ApiError>>
        + Send
        + 'static,
{
    sweep(&state.review_jobs);

    // Attach: one fetch per (kind, repo, PR) at a time. A settled job
    // (done or failed) does NOT attach — a caller retrying after a failure
    // gets a fresh job, and a caller re-POSTing after success runs the
    // work again (start-pr: OPEN → reuse 200; CLOSED → 409 unless
    // `on_closed=reopen|new`; sync: idempotent by construction).
    let running = {
        let jobs = state.review_jobs.lock();
        jobs.values()
            .find(|j| {
                j.status == "running"
                    && j.kind == kind
                    && j.repo == repo
                    && j.pr_number == pr_number
            })
            .map(|j| (j.id.clone(), j.key == key))
    };
    if let Some((job_id, false)) = &running {
        return Ok((
            StatusCode::CONFLICT,
            [(header::CACHE_CONTROL, "no-store")],
            Json(serde_json::json!({
                "error": format!(
                    "a {kind} job for {repo} #{pr_number} is already running with a different request ({job_id}); wait for it or poll it"
                ),
                "type": URN_JOB_CONFLICT,
                "job_id": job_id,
                "kind": kind,
            })),
        )
            .into_response());
    }
    if let Some((job_id, true)) = running {
        return Ok((
            StatusCode::ACCEPTED,
            [(header::CACHE_CONTROL, "no-store")],
            Json(serde_json::json!({
                "job_id": job_id,
                "kind": kind,
                "status": "running",
                "attached": true,
            })),
        )
            .into_response());
    }

    // `job_` + 12 hex — the crate's established id shape
    // (`annotations::short_random_hex`, same as `set_`/`clm_`/`trl_`).
    let job_id = format!("job_{}", crate::annotations::short_random_hex());
    {
        state.review_jobs.lock().insert(
            job_id.clone(),
            ReviewJob {
                id: job_id.clone(),
                kind,
                repo,
                pr_number,
                settled: None,
                key,
                status: "running",
                stage: "fetch",
                created: Instant::now(),
                review_id: None,
                result: None,
                error: None,
                error_type: None,
                error_status: None,
            },
        );
    }

    let state2 = state.clone();
    let id2 = job_id.clone();
    tokio::spawn(async move {
        let handle: JobHandle = (state2.review_jobs.clone(), id2.clone());
        let outcome = run(state2.clone(), handle).await;
        // One lock, dropped before this task ends — never across an await.
        let mut jobs = state2.review_jobs.lock();
        if let Some(j) = jobs.get_mut(&id2) {
            j.settled = Some(Instant::now());
            match outcome {
                Ok((status, value)) if status.is_success() => {
                    j.status = "done";
                    j.stage = "done";
                    j.review_id = value
                        .get("id")
                        .or_else(|| value.get("review_id"))
                        .and_then(serde_json::Value::as_i64);
                    j.result = Some(value);
                }
                Ok((status, value)) => {
                    // A non-success VALUE outcome (closed-binding 409
                    // [`ERR_REVIEW_CLOSED`], historically also the
                    // duplicate-binding 409) — the poller sees the same
                    // payload the synchronous route would have returned.
                    j.status = "failed";
                    j.error_status = Some(status.as_u16());
                    j.error = value
                        .get("error")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string)
                        .or_else(|| Some(format!("{kind} failed")));
                    if value.get("type").and_then(|v| v.as_str()) == Some(ERR_REVIEW_CLOSED) {
                        j.error_type = Some(ERR_REVIEW_CLOSED);
                    }
                    j.result = Some(value);
                }
                Err(e) => {
                    j.status = "failed";
                    j.error_status = Some(e.status_code().as_u16());
                    j.error_type = e.problem_type();
                    j.error = Some(e.message().to_string());
                }
            }
        }
    });

    Ok((
        StatusCode::ACCEPTED,
        [(header::CACHE_CONTROL, "no-store")],
        Json(serde_json::json!({
            "job_id": job_id,
            "kind": kind,
            "status": "running",
        })),
    )
        .into_response())
}

/// `GET /api/reviews/jobs/{id}` — the job read. BEARER (an ordinary read
/// on the `api` sub-router; the job carries no secrets — the `result`
/// envelope is exactly what the loopback-only POST would have returned to
/// the same operator's own CLI).
pub async fn review_job_route(
    State(state): State<SharedState>,
    AxumPath(id): AxumPath<String>,
) -> Result<Response, ApiError> {
    sweep(&state.review_jobs);
    let snap = { state.review_jobs.lock().get(&id).cloned() };
    let Some(j) = snap else {
        return Err(ApiError::not_found(format!(
            "no such review job {id:?} (unknown, or swept: the TTL once it settled, the stuck-job horizon while it ran)"
        )));
    };
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(serde_json::json!({
            "job_id": j.id,
            "kind": j.kind,
            "status": j.status,
            "progress": { "stage": j.stage },
            "repo": j.repo,
            "pr_number": j.pr_number,
            "review_id": j.review_id,
            "error": j.error,
            "error_type": j.error_type,
            "error_status": j.error_status,
            "result": j.result,
        })),
    )
        .into_response())
}

// --- the surface declaration (crate invariant 15) --------------------------

/// `GET /api/reviews/jobs/{id}` takes no query params (the id rides the
/// path), so there is nothing a request can omit — the contract still
/// ships so BOTH dead-surface walks cover the route (the
/// `syntax::no_params_accept_without` precedent).
fn no_params_accept_without(_omit: &str) -> bool {
    true
}

pub const REVIEW_JOB_ROUTE: crate::entities::RouteContract = crate::entities::RouteContract {
    path: "/api/reviews/jobs/{id}",
    handler: "review_jobs::review_job_route",
    required_params: &[],
    params_accept_without: no_params_accept_without,
};

/// Every route V76-R1a adds. Walked from BOTH sides exactly as
/// `entities::V71_G0_ROUTES` is — see that list's doc. `POST
/// /api/reviews/pr` itself is absent for the reason
/// `boards::V74_L1_ROUTES` records for its own mutations: a
/// `RouteContract` describes a query-param surface, and that route's
/// contract is its JSON body (its own deserialization, enforced by axum's
/// `Json` extractor).
pub const V76_R1A_ROUTES: &[crate::entities::RouteContract] = &[REVIEW_JOB_ROUTE];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_declared_route_is_api_nested() {
        assert!(REVIEW_JOB_ROUTE.path.starts_with("/api/"));
    }

    #[test]
    fn a_job_id_uses_the_crates_established_shape() {
        // `job_` + 12 hex — the same shape `set_`/`clm_`/`trl_` mint.
        let id = format!("job_{}", crate::annotations::short_random_hex());
        assert_eq!(id.len(), 4 + 12, "{id}");
        assert!(id[4..].chars().all(|c| c.is_ascii_hexdigit()), "{id}");
    }

    #[test]
    fn the_sweep_drops_settled_entries_past_the_ttl_and_wedged_ones_past_the_horizon() {
        let jobs: ReviewJobs = parking_lot::Mutex::new(HashMap::new());
        let fresh = ReviewJob {
            id: "job_fresh".into(),
            kind: "start-pr",
            settled: None,
            key: String::new(),
            repo: "r".into(),
            pr_number: 1,
            status: "running",
            stage: "fetch",
            created: Instant::now(),
            review_id: None,
            result: None,
            error: None,
            error_type: None,
            error_status: None,
        };
        let long_ago = Instant::now() - Duration::from_secs(JOB_TTL_SECS + 1);
        // Running for over an hour: a long `sync --open` — still inside
        // the 6 h horizon, so the TTL does not touch it.
        let mut long_running = fresh.clone();
        long_running.id = "job_long".into();
        long_running.created = long_ago;
        // Settled over an hour ago: swept by the TTL.
        let mut stale = fresh.clone();
        stale.id = "job_stale".into();
        stale.status = "done";
        stale.created = long_ago;
        stale.settled = Some(long_ago);
        // Created long ago but settled just now: the TTL runs from settle.
        let mut just_done = stale.clone();
        just_done.id = "job_just_done".into();
        just_done.settled = Some(Instant::now());
        // Never settled at all (a wedged subprocess, a panic in the job
        // body) and past the horizon: this is the entry the TTL can
        // never reach, so the horizon is what bounds the leak.
        let mut wedged = fresh.clone();
        wedged.id = "job_wedged".into();
        wedged.created = Instant::now() - Duration::from_secs(STUCK_JOB_HORIZON_SECS + 1);
        for j in [fresh, long_running, stale, just_done, wedged] {
            jobs.lock().insert(j.id.clone(), j);
        }
        sweep(&jobs);
        let jobs = jobs.lock();
        assert!(jobs.contains_key("job_fresh"));
        assert!(jobs.contains_key("job_long"));
        assert!(jobs.contains_key("job_just_done"));
        assert!(!jobs.contains_key("job_stale"));
        assert!(!jobs.contains_key("job_wedged"));
    }

    /// The horizon is only honest if it is comfortably LONGER than the
    /// longest job anyone waits for: `review sync --open`'s `--wait`
    /// default is 3600 s, and a job that spends all of it must not be
    /// swept under a live poller.
    #[test]
    fn the_stuck_horizon_outlasts_the_longest_poll_budget() {
        const DEFAULT_WAIT_OPEN: u64 = 3600; // kb_code_cli::review_sync
        assert!(STUCK_JOB_HORIZON_SECS > DEFAULT_WAIT_OPEN);
        assert!(STUCK_JOB_HORIZON_SECS >= 2 * DEFAULT_WAIT_OPEN);
        assert!(STUCK_JOB_HORIZON_SECS > JOB_TTL_SECS);
    }

    async fn booted_state() -> (tempfile::TempDir, SharedState) {
        let tmp = tempfile::tempdir().unwrap();
        let paths = kb_core::paths::KbPaths::rooted_at(tmp.path(), "kb-code");
        let state = crate::build_state_for_test(crate::config::KbCodeConfig::default(), paths)
            .await
            .expect("build_state_for_test");
        (tmp, state)
    }

    /// Admit a `sync` job for `widget` #7 whose spawned task NEVER
    /// returns — the same shape as a wedged git subprocess, and the
    /// only way to keep a job `running` for the length of a test.
    async fn admit_running_sync(state: &SharedState, key: &str) -> (StatusCode, serde_json::Value) {
        let resp = start_job(
            state.clone(),
            "sync",
            "widget".into(),
            7,
            key.into(),
            never_settling,
        )
        .await
        .expect("start_job admits");
        let status = resp.status();
        let body = axum::body::to_bytes(resp.into_body(), 1 << 20)
            .await
            .expect("admission body");
        (
            status,
            serde_json::from_slice(&body).expect("admission json"),
        )
    }

    /// A spawned job body that never returns — the shape a wedged git
    /// subprocess leaves behind, and the only way to keep a job
    /// `running` (hence `settled: None`) for the length of a test.
    fn never_settling(
        _state: SharedState,
        _handle: JobHandle,
    ) -> std::future::Pending<Result<(StatusCode, serde_json::Value), ApiError>> {
        std::future::pending()
    }

    /// RS-U10b — the CONSUMER of `crate::review_sync::sync_job_key`.
    /// `sync_job_key`'s own unit test only pins the PRODUCER (that two
    /// different requests hash apart); delete the 409 arm below and the
    /// whole suite stays green while a real `sync --pr 7` silently
    /// attaches to a `--dry-run` sync of the same PR and is handed that
    /// job's answer.
    #[tokio::test]
    async fn a_running_job_409s_a_different_request_and_attaches_the_same_one() {
        let (_tmp, state) = booted_state().await;
        let (status, first) = admit_running_sync(&state, "key-a").await;
        assert_eq!(status, StatusCode::ACCEPTED, "{first}");
        assert_eq!(first["status"], "running", "{first}");
        assert_eq!(first.get("attached"), None, "a fresh job never attaches");
        let job_id = first["job_id"].as_str().expect("job_id").to_string();

        // Key B: same (kind, repo, pr), different request → 409 naming it.
        let (status, conflict) = admit_running_sync(&state, "key-b").await;
        assert_eq!(status, StatusCode::CONFLICT, "{conflict}");
        assert_eq!(conflict["type"], URN_JOB_CONFLICT, "{conflict}");
        assert_eq!(conflict["job_id"], job_id, "{conflict}");
        assert_eq!(conflict["kind"], "sync", "{conflict}");

        // Key A again: the same request still attaches to it.
        let (status, attach) = admit_running_sync(&state, "key-a").await;
        assert_eq!(status, StatusCode::ACCEPTED, "{attach}");
        assert_eq!(attach["attached"], true, "{attach}");
        assert_eq!(attach["job_id"], job_id, "{attach}");

        // The job itself never settled and is still readable as running.
        let job = state.review_jobs.lock()[&job_id].clone();
        assert_eq!(job.status, "running");
        assert_eq!(job.settled, None);
    }

    /// A DIFFERENT (kind, repo, pr) is not a conflict at all — the
    /// conflict arm keys on all three, and a wedged sync must not lock a
    /// `start-pr` of the same PR out of its own job.
    #[tokio::test]
    async fn a_running_sync_does_not_conflict_with_a_start_pr_of_the_same_pr() {
        let (_tmp, state) = booted_state().await;
        let (status, _) = admit_running_sync(&state, "key-a").await;
        assert_eq!(status, StatusCode::ACCEPTED);
        let resp = start_job(
            state.clone(),
            "start-pr",
            "widget".into(),
            7,
            "key-a".into(),
            never_settling,
        )
        .await
        .expect("start_job admits");
        let status = resp.status();
        let body = axum::body::to_bytes(resp.into_body(), 1 << 20)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(status, StatusCode::ACCEPTED, "{v}");
        assert_eq!(
            v.get("attached"),
            None,
            "a different kind never attaches: {v}"
        );
    }

    #[test]
    fn set_stage_is_a_no_op_without_a_handle_and_updates_with_one() {
        set_stage(&None, "base");
        let jobs: ReviewJobs = parking_lot::Mutex::new(HashMap::new());
        jobs.lock().insert(
            "job_x".to_string(),
            ReviewJob {
                id: "job_x".into(),
                kind: "start-pr",
                settled: None,
                key: String::new(),
                repo: "r".into(),
                pr_number: 1,
                status: "running",
                stage: "fetch",
                created: Instant::now(),
                review_id: None,
                result: None,
                error: None,
                error_type: None,
                error_status: None,
            },
        );
        let handle: JobHandle = (Arc::new(jobs), "job_x".to_string());
        set_stage(&Some(handle.clone()), "patchset");
        assert_eq!(handle.0.lock()["job_x"].stage, "patchset");
        // An unknown id never panics (a swept job mid-run is unobservable).
        set_stage(&Some((handle.0.clone(), "job_gone".to_string())), "base");
    }
}
