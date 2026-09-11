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
//!   `error` and, when the refusal is typed (the stale-mirror
//!   `urn:kb:errors:stale-mirror`), its URN in `error_type`.
//!
//! Jobs are IN-MEMORY ONLY — a `parking_lot::Mutex<HashMap>` on
//! [`crate::state::AppState`], per-boot like `file_index`/`symbol_index`.
//! A job is a claim about work in flight in THIS process; persisted job
//! rows would be a second copy that can go stale in ways a missing entry
//! cannot (a rebooted daemon simply has no jobs). Root CLAUDE.md
//! invariant #15: the guard is taken and released inside one statement
//! and never crosses an `.await`. Entries are swept after
//! [`JOB_TTL_SECS`] (1 h) on every admission and every read — there is no
//! background reaper for a map this small.
//!
//! The synchronous behaviour of `POST /api/reviews/pr` is untouched
//! (`?async=0` or the flag absent — see
//! [`crate::reviews::StartPrParams::wants_async`]), so every pre-V76
//! caller and test keeps its byte-identical contract; the CLI is the one
//! that opts into async (its `start-pr` always sends `?async=1` and
//! polls).

use crate::reviews::CreateReviewPrBody;
use crate::routes::ApiError;
use crate::state::SharedState;
use axum::extract::{Path as AxumPath, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Sweep horizon: a job entry (running or settled) is dropped one hour
/// after creation. A `start-pr` fetch that has genuinely run for an hour
/// is indistinguishable from a wedged one to a poller either way; the
/// review row, once created, is the durable record — never the job.
pub const JOB_TTL_SECS: u64 = 3600;

/// One in-flight (or settled) start-pr job.
#[derive(Debug, Clone)]
pub struct ReviewJob {
    pub id: String,
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
    pub review_id: Option<i64>,
    /// The full creation envelope on success (what the synchronous route
    /// returns as its body); also kept for a non-success `(status, body)`
    /// outcome (e.g. the duplicate-binding 409) so a poller sees the same
    /// payload the route would have sent.
    pub result: Option<serde_json::Value>,
    pub error: Option<String>,
    /// The refusal's RFC 7807 `type` URN when one was attached
    /// (`urn:kb:errors:stale-mirror`) — a poller branches on this, never
    /// on the prose.
    pub error_type: Option<&'static str>,
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

/// Drop every entry older than [`JOB_TTL_SECS`]. Called on admission and
/// on every read — O(map), and the map is tiny by construction (one entry
/// per in-flight or recently-settled start-pr).
fn sweep(jobs: &ReviewJobs) {
    let ttl = Duration::from_secs(JOB_TTL_SECS);
    jobs.lock().retain(|_, j| j.created.elapsed() < ttl);
}

/// The `?async=1` half of `POST /api/reviews/pr` — attach to a running
/// job for the same `(repo, pr_number)` or mint one and spawn the work.
/// The route's gate is unchanged (loopback-only, inherited from the
/// sub-router this handler hangs on).
pub async fn start_or_attach(
    state: SharedState,
    body: CreateReviewPrBody,
) -> Result<Response, ApiError> {
    sweep(&state.review_jobs);

    // Attach: one fetch per (repo, PR) at a time. A settled job (done or
    // failed) does NOT attach — a caller retrying after a failure gets a
    // fresh job, and a caller re-POSTing after success gets the ordinary
    // duplicate-binding 409 from inside the new job.
    let attached = {
        let jobs = state.review_jobs.lock();
        jobs.values()
            .find(|j| j.status == "running" && j.repo == body.repo && j.pr_number == body.pr_number)
            .map(|j| j.id.clone())
    };
    if let Some(job_id) = attached {
        return Ok((
            StatusCode::ACCEPTED,
            [(header::CACHE_CONTROL, "no-store")],
            Json(serde_json::json!({
                "job_id": job_id,
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
                repo: body.repo.clone(),
                pr_number: body.pr_number,
                status: "running",
                stage: "fetch",
                created: Instant::now(),
                review_id: None,
                result: None,
                error: None,
                error_type: None,
            },
        );
    }

    let state2 = state.clone();
    let id2 = job_id.clone();
    tokio::spawn(async move {
        let handle: JobHandle = (state2.review_jobs.clone(), id2.clone());
        let outcome = crate::reviews::create_review_pr_value(&state2, body, Some(handle)).await;
        // One lock, dropped before this task ends — never across an await.
        let mut jobs = state2.review_jobs.lock();
        if let Some(j) = jobs.get_mut(&id2) {
            match outcome {
                Ok((status, value)) if status.is_success() => {
                    j.status = "done";
                    j.stage = "done";
                    j.review_id = value.get("id").and_then(serde_json::Value::as_i64);
                    j.result = Some(value);
                }
                Ok((_status, value)) => {
                    // A non-success VALUE outcome (the duplicate-binding
                    // 409 is the only one today) — the poller sees the same
                    // payload the synchronous route would have returned.
                    j.status = "failed";
                    j.error = value
                        .get("error")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string)
                        .or_else(|| Some("start-pr failed".to_string()));
                    j.result = Some(value);
                }
                Err(e) => {
                    j.status = "failed";
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
            "no such review job {id:?} (unknown, or swept after the 1 h TTL)"
        )));
    };
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(serde_json::json!({
            "job_id": j.id,
            "status": j.status,
            "progress": { "stage": j.stage },
            "repo": j.repo,
            "pr_number": j.pr_number,
            "review_id": j.review_id,
            "error": j.error,
            "error_type": j.error_type,
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
    fn the_sweep_drops_only_entries_older_than_the_ttl() {
        let jobs: ReviewJobs = parking_lot::Mutex::new(HashMap::new());
        let fresh = ReviewJob {
            id: "job_fresh".into(),
            repo: "r".into(),
            pr_number: 1,
            status: "running",
            stage: "fetch",
            created: Instant::now(),
            review_id: None,
            result: None,
            error: None,
            error_type: None,
        };
        let mut stale = fresh.clone();
        stale.id = "job_stale".into();
        stale.created = Instant::now() - Duration::from_secs(JOB_TTL_SECS + 1);
        jobs.lock().insert(fresh.id.clone(), fresh);
        jobs.lock().insert(stale.id.clone(), stale);
        sweep(&jobs);
        let jobs = jobs.lock();
        assert!(jobs.contains_key("job_fresh"));
        assert!(!jobs.contains_key("job_stale"));
    }

    #[test]
    fn set_stage_is_a_no_op_without_a_handle_and_updates_with_one() {
        set_stage(&None, "base");
        let jobs: ReviewJobs = parking_lot::Mutex::new(HashMap::new());
        jobs.lock().insert(
            "job_x".to_string(),
            ReviewJob {
                id: "job_x".into(),
                repo: "r".into(),
                pr_number: 1,
                status: "running",
                stage: "fetch",
                created: Instant::now(),
                review_id: None,
                result: None,
                error: None,
                error_type: None,
            },
        );
        let handle: JobHandle = (Arc::new(jobs), "job_x".to_string());
        set_stage(&Some(handle.clone()), "patchset");
        assert_eq!(handle.0.lock()["job_x"].stage, "patchset");
        // An unknown id never panics (a swept job mid-run is unobservable).
        set_stage(&Some((handle.0.clone(), "job_gone".to_string())), "base");
    }
}
