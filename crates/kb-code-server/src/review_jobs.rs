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
//! [`STUCK_JOB_HORIZON_SECS`] (6 h after creation), which is an ABSOLUTE
//! ceiling on any running job, and which CANCELS the task it drops (the
//! table holds each task's abort handle, so an entry and the work behind
//! it leave as one event — see [`sweep`]).
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
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering};
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
/// Only [`STUCK_JOB_HORIZON_SECS`] may drop one, and it CANCELS the
/// task it drops. The review row, once created, is the durable record —
/// never the job.
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
/// **It is a ceiling, not a promise.** 21 600 s = 6 h is longer than any
/// budget the CLI imposes BY DEFAULT — `kb-code review sync --open`'s
/// `--wait` default is 3600 s (`kb_code_cli::review_sync`'s
/// `DEFAULT_WAIT_OPEN`; a single PR is 600 s), so a job that burns its
/// whole client-side budget is still five hours inside it — but
/// `--wait` is an unbounded `Option<u64>`, so a caller that asks to poll
/// LONGER than this has its job CANCELLED and a 404 at the horizon,
/// rather than after its own budget. Nothing server-side can extend it:
/// the horizon runs from creation and the admission API carries no wait
/// budget ([`start_job`] takes none). A poller must therefore read a 404
/// past the horizon as "cancelled, re-submit", never as "already done".
///
/// What the horizon DOES guarantee is that the entry and the work behind
/// it leave as one event: [`sweep`] holds each live task's
/// [`tokio::task::AbortHandle`], CANCELS the task at the horizon, and
/// drops the entry only once that cancellation has landed — so a corpse
/// can never keep holding [`crate::review_sync::repo_guard`] (the
/// per-repo lock the next admission blocks on) while the map has already
/// forgotten it. The corpse's own poller gets a 404 rather than a status
/// that will never change.
pub const STUCK_JOB_HORIZON_SECS: u64 = 6 * 3600;

/// Mirror of `kb_code_cli::review_sync`'s `DEFAULT_WAIT_OPEN` — the budget
/// a poller imposes on a running job unless the operator asks for another.
/// That constant is private to the CLI crate, which does not depend on
/// this one, so the value is restated here on purpose; the assertions
/// below are what stop this from silently drifting away from the CLI's
/// actual default.
///
/// It is the DEFAULT, not the maximum: `--wait` is an unbounded
/// `Option<u64>`, and no server-side pin can turn an operator's larger
/// budget into a longer horizon (see [`STUCK_JOB_HORIZON_SECS`]).
const DEFAULT_WAIT_OPEN: u64 = 3600;

/// The horizon is only honest if it is comfortably LONGER than the
/// budget a poller waits for BY DEFAULT: a `review sync --open` poller
/// waits up to [`DEFAULT_WAIT_OPEN`] s unless told otherwise, and a job
/// that spends all of it must not be cancelled from under a live poller;
/// `>= 2x` leaves the second wait a client makes after a job 404s inside
/// the horizon, and the horizon must also outlast the [`JOB_TTL_SECS`] a
/// settled entry gets, or a long job would be indistinguishable from a
/// wedged one.
///
/// These were a `#[test]`. Every operand is a constant, so the relation
/// belongs to the COMPILER: it is checked on every build, by everyone
/// who touches this file, instead of only when someone runs the suite
/// (`clippy::assertions_on_constants` is right, and the lint's own
/// suggested form is the one used here).
const _: () = assert!(STUCK_JOB_HORIZON_SECS > DEFAULT_WAIT_OPEN);
const _: () = assert!(STUCK_JOB_HORIZON_SECS >= 2 * DEFAULT_WAIT_OPEN);
const _: () = assert!(STUCK_JOB_HORIZON_SECS > JOB_TTL_SECS);

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
    /// never returns, which is exactly what that horizon exists for —
    /// and a horizon sweep CANCELS that task, so the entry and the work
    /// it describes never outlive each other.
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

/// The job table: `job_id` → job, plus the abort handle of every task
/// that entry still has behind it. `parking_lot::Mutex` (the 2026-09-01
/// starvation incident's ruling for every short in-process lock in this
/// crate); the guard never crosses an `.await`.
///
/// The task map is what makes [`sweep`] honest. A running job's body
/// holds [`crate::review_sync::repo_guard`] for its whole life, so
/// dropping its entry while the body goes on is not a cleanup — it is a
/// lie the map tells (the next admission mints a fresh job that then
/// blocks on a lock the "forgotten" corpse still holds) and a lock leak
/// nothing else can see. [`tokio::task::AbortHandle`] rather than the
/// `JoinHandle` because it is `Clone` + `Send` and this table hands out
/// clones of itself; the `JoinHandle` stays with the spawn site, which
/// drops it, exactly as before.
#[derive(Debug, Default)]
pub struct JobTable {
    jobs: HashMap<String, ReviewJob>,
    tasks: HashMap<String, tokio::task::AbortHandle>,
    /// Outstanding `spawn_blocking` closures per job (see
    /// [`spawn_blocking_tracked`]). `abort()` cancels the async task but
    /// can never interrupt a closure already on the blocking pool, so
    /// "the task finished" is NOT "the work stopped": [`sweep`] keeps an
    /// entry while this is non-zero.
    blocking: HashMap<String, Arc<AtomicUsize>>,
    /// Jobs [`sweep`] itself aborted at the horizon: their watcher must
    /// not re-settle them (the entry is on its way out, not a result).
    aborted: HashSet<String>,
}

tokio::task_local! {
    /// The in-flight blocking-work counter of the job whose body is
    /// running on this task (set by [`start_job`]).
    static CURRENT_BLOCKING: Arc<AtomicUsize>;
}

/// The per-repo sync lock's guard, SHARED (K2 carry): the job body holds one
/// `Arc`, and every [`spawn_blocking_tracked`] closure started under it
/// holds another. `abort()` drops the body's `Arc` at the horizon but can
/// never interrupt a closure already on the blocking pool, so the lock is
/// released only when the LAST holder — the orphaned closure included — is
/// gone, never while git is still working on the repo.
pub(crate) type SharedRepoGuard = Arc<tokio::sync::OwnedMutexGuard<()>>;

tokio::task_local! {
    /// The repo guard the running job body took, if any (set by
    /// [`register_repo_guard`], read by [`spawn_blocking_tracked`]).
    static CURRENT_REPO_GUARD: parking_lot::Mutex<Option<std::sync::Weak<tokio::sync::OwnedMutexGuard<()>>>>;
}

/// Record `guard` as the running job's repo guard, so every blocking closure
/// the job starts WHILE the body still holds it keeps it alive. The slot is
/// a `Weak`: it must never extend the guard's life past the body's own scope
/// (a strong clone parked here made a bulk job's second `repo_guard` wait on
/// its own first one forever). A no-op outside a job.
pub(crate) fn register_repo_guard(guard: &SharedRepoGuard) {
    let _ = CURRENT_REPO_GUARD.try_with(|slot| *slot.lock() = Some(Arc::downgrade(guard)));
}

/// Held by a `spawn_blocking` closure for exactly as long as the closure
/// exists (queued or running); drops the job's in-flight count and its
/// share of the repo guard.
struct BlockingGuard(
    Arc<AtomicUsize>,
    #[allow(dead_code)] Option<SharedRepoGuard>,
);

impl BlockingGuard {
    fn current() -> Option<Self> {
        CURRENT_BLOCKING
            .try_with(|c| {
                c.fetch_add(1, Ordering::SeqCst);
                let repo_guard = CURRENT_REPO_GUARD
                    .try_with(|slot| slot.lock().as_ref().and_then(std::sync::Weak::upgrade))
                    .ok()
                    .flatten();
                BlockingGuard(c.clone(), repo_guard)
            })
            .ok()
    }
}

impl Drop for BlockingGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// `tokio::task::spawn_blocking`, but when called from inside a job body
/// the closure is COUNTED against that job until it has finished. Outside
/// a job it is exactly `spawn_blocking`. Every blocking call on the
/// start-pr / sync paths goes through this, so [`sweep`] can tell "the
/// task was cancelled" from "the work behind it stopped".
pub fn spawn_blocking_tracked<F, R>(f: F) -> tokio::task::JoinHandle<R>
where
    F: FnOnce() -> R + Send + 'static,
    R: Send + 'static,
{
    let guard = BlockingGuard::current();
    tokio::task::spawn_blocking(move || {
        let _in_flight = guard;
        f()
    })
}

/// A stable 32-bit slot (FNV-1a) for a job that is about a string key rather
/// than one PR number — `review start`'s head ref. Two different keys that
/// collide are only ever refused as "a different request is running" (409),
/// never silently merged, because the job's full key is compared as well.
pub fn key_slot(key: &str) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for b in key.bytes() {
        h ^= u32::from(b);
        h = h.wrapping_mul(0x0100_0193);
    }
    h
}

/// The shared table. `Default` is what `AppState` builds per boot.
pub type ReviewJobs = parking_lot::Mutex<JobTable>;

/// The progress handle [`crate::reviews::create_review_pr_value`] takes:
/// the shared table plus this job's id.
pub type JobHandle = (Arc<ReviewJobs>, String);

/// Coarse stage update — one lock, one mutation, no `.await` anywhere
/// near the guard.
pub fn set_stage(job: &Option<JobHandle>, stage: &'static str) {
    if let Some((jobs, id)) = job {
        if let Some(j) = jobs.lock().jobs.get_mut(id) {
            j.stage = stage;
        }
    }
}

/// Drop every SETTLED entry whose result has been readable for
/// [`JOB_TTL_SECS`], and CANCEL every RUNNING entry that has outlived
/// [`STUCK_JOB_HORIZON_SECS`]. The horizon is how a job whose task never
/// settles is bounded at all (RS-U10b review fix: `settled` is only
/// written after `run` returns, so a wedged task leaves the entry
/// `running` forever) — a creation-age sweep is NOT, because a
/// `sync --open` loop may legitimately run past an hour and dropping it
/// mid-run lost the result and let a rerun start a second loop. Called
/// on admission and on every read — O(map), and the map is tiny by
/// construction.
///
/// **The removal and the cancellation are ONE event, in that order.**
/// An entry is only dropped once there is provably no work left behind
/// it; an expired entry whose task is still alive is `abort()`ed and
/// KEPT, and the next sweep drops it once
/// [`tokio::task::AbortHandle::is_finished`] reports the cancellation
/// landed. Both halves matter, and the defect this fixes is what
/// happens when only the first one does:
///
/// * dropping an entry whose body is still running makes the map lie.
///   The next `POST …?async=1` mints a fresh job which blocks on the
///   `repo_guard` the "forgotten" corpse still holds, and
///   `GET /api/reviews/jobs/{id}` for the new id reports
///   `"running","stage":"fetch"` until the daemon restarts.
/// * cancelling without checking is the same lie one sweep later.
///
/// `abort()` takes effect at the task's next poll, so a body parked at
/// an `.await` (a wedged channel, a pending lock) is cancelled
/// immediately and its entry goes on the next sweep. A body stuck
/// inside `spawn_blocking` — which no `abort()` can interrupt, and which
/// only a git deadline it ignores can produce — keeps its entry (the
/// in-flight count [`spawn_blocking_tracked`] maintains per job is what
/// `sweep` checks; the task being finished proves nothing about it), and
/// keeps 409ing its `(kind, repo, pr)` key: which is TRUE, because the
/// work really is still in flight. Its entry goes on the first sweep
/// after that body returns.
///
/// A cancellation-pending entry is deliberately still ATTACHABLE and
/// still 409s a different request: the alternative — minting a fresh
/// job for the same key while the corpse may still hold `repo_guard` —
/// is the exact queue-behind-a-dead-lock this pairing exists to stop.
/// The attached poller gets the 404 one or two sweeps later and
/// re-submits, which is what `STUCK_JOB_HORIZON_SECS`'s own doc tells
/// it to do.
fn sweep(jobs: &ReviewJobs) {
    let ttl = Duration::from_secs(JOB_TTL_SECS);
    let horizon = Duration::from_secs(STUCK_JOB_HORIZON_SECS);
    let mut table = jobs.lock();
    let expired: Vec<String> = table
        .jobs
        .iter()
        .filter(|(_, j)| match j.settled {
            Some(at) => at.elapsed() >= ttl,
            None => j.created.elapsed() >= horizon,
        })
        .map(|(id, _)| id.clone())
        .collect();
    for id in expired {
        // A live task behind this entry: cancel it, and keep the entry
        // until the cancellation has actually landed.
        let live = table.tasks.get(&id).is_some_and(|task| {
            if task.is_finished() {
                false
            } else {
                task.abort();
                true
            }
        });
        if live {
            table.aborted.insert(id);
            continue;
        }
        // The task is gone, but a `spawn_blocking` closure it started is
        // not: `abort()` cannot reach it. The entry stays (and keeps
        // 409ing its key) until the last such closure has returned.
        if table
            .blocking
            .get(&id)
            .is_some_and(|c| c.load(Ordering::SeqCst) > 0)
        {
            continue;
        }
        table.tasks.remove(&id);
        table.blocking.remove(&id);
        table.aborted.remove(&id);
        table.jobs.remove(&id);
    }
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
///
/// There is deliberately NO wait-budget parameter: a caller's `--wait`
/// cannot reach here, which is why [`STUCK_JOB_HORIZON_SECS`] is an
/// absolute ceiling rather than something the caller negotiates.
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
        let table = state.review_jobs.lock();
        table
            .jobs
            .values()
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
        let mut table = state.review_jobs.lock();
        table.jobs.insert(
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

    let counter = Arc::new(AtomicUsize::new(0));
    state
        .review_jobs
        .lock()
        .blocking
        .insert(job_id.clone(), counter.clone());
    let state2 = state.clone();
    let id2 = job_id.clone();
    let body = async move {
        let handle: JobHandle = (state2.review_jobs.clone(), id2.clone());
        let outcome = run(state2.clone(), handle).await;
        // One lock, dropped before this task ends — never across an await.
        let mut table = state2.review_jobs.lock();
        if let Some(j) = table.jobs.get_mut(&id2) {
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
    };
    let join = tokio::spawn(CURRENT_BLOCKING.scope(
        counter,
        CURRENT_REPO_GUARD.scope(parking_lot::Mutex::new(None), body),
    ));
    // The handle the horizon needs. Registering it AFTER the spawn is
    // the only ordering that is sound: a task that finished (or was
    // swept) before this line cannot have been aborted anyway, and one
    // that has not yet been polled has not yet taken `repo_guard`.
    state
        .review_jobs
        .lock()
        .tasks
        .insert(job_id.clone(), join.abort_handle());
    // The watcher: `settled` is written only by the body's last lines, so
    // a body that PANICS (or is cancelled by something other than the
    // horizon) would otherwise leave the entry `running` for the whole
    // horizon. Awaiting the JoinHandle is what makes that visible.
    let watch_state = state.clone();
    let watch_id = job_id.clone();
    tokio::spawn(async move {
        let Err(e) = join.await else { return };
        let mut table = watch_state.review_jobs.lock();
        if table.aborted.contains(&watch_id) {
            // The horizon cancelled it; the entry is being swept.
            return;
        }
        if let Some(j) = table.jobs.get_mut(&watch_id) {
            if j.settled.is_none() {
                j.settled = Some(Instant::now());
                j.status = "failed";
                j.error_status = Some(500);
                j.error = Some(if e.is_panic() {
                    format!("the {} job panicked; no result was recorded", j.kind)
                } else {
                    format!("the {} job was cancelled before it finished", j.kind)
                });
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
    let snap = { state.review_jobs.lock().jobs.get(&id).cloned() };
    let Some(j) = snap else {
        // Two rules can reach here, and they are different events: the
        // TTL drops a job that SETTLED [`JOB_TTL_SECS`] ago, and the
        // stuck-job horizon CANCELS one that was still running at
        // [`STUCK_JOB_HORIZON_SECS`]. A poller that reaches the second
        // one must re-submit — the work was cancelled, not finished.
        return Err(ApiError::not_found(format!(
            "no such review job {id:?} (unknown, or swept: the {}s TTL once it settled, the {}s stuck-job horizon — which CANCELLED it — while it ran)",
            JOB_TTL_SECS, STUCK_JOB_HORIZON_SECS
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
    // Edition 2021: no prelude `Future`.
    use std::future::Future;

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
        let jobs: ReviewJobs = parking_lot::Mutex::new(JobTable::default());
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
            jobs.lock().jobs.insert(j.id.clone(), j);
        }
        sweep(&jobs);
        let table = jobs.lock();
        assert!(table.jobs.contains_key("job_fresh"));
        assert!(table.jobs.contains_key("job_long"));
        assert!(table.jobs.contains_key("job_just_done"));
        assert!(!table.jobs.contains_key("job_stale"));
        assert!(!table.jobs.contains_key("job_wedged"));
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

    /// A job body that reports its own CANCELLATION when the runtime
    /// drops the future. A wedged body is `Pending` forever, so the
    /// only observable that the horizon really cancelled the work —
    /// rather than merely forgetting it — is this flag: with the entry
    /// gone and the body still alive, the next admission would block on
    /// the `repo_guard` the corpse holds, and the job would report
    /// `"running","stage":"fetch"` forever.
    struct DropFlag(std::sync::Arc<std::sync::atomic::AtomicBool>);

    impl Drop for DropFlag {
        fn drop(&mut self) {
            self.0.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }

    impl Future for DropFlag {
        type Output = Result<(StatusCode, serde_json::Value), ApiError>;
        fn poll(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Self::Output> {
            std::task::Poll::Pending
        }
    }

    /// The horizon is the ONLY thing that can drop a running entry, and
    /// it must take the ENTRY and the TASK with it. The defect this
    /// pins: `sweep` removed the entry and left the spawned body
    /// running, so the corpse kept `repo_guard` for the life of the
    /// process while the map claimed the job was gone — the next
    /// `POST …?async=1` minted a fresh job that blocked on that mutex
    /// forever, reporting `"running","stage":"fetch"` until the daemon
    /// restarted. Cancelling without dropping is the same lie a sweep
    /// later, so the order is pinned too: the entry outlives the work
    /// and goes only once the work is provably finished.
    ///
    /// No sleeping: the entry is aged by rewriting `created` (the field
    /// the horizon reads), and the cancellation is observed through the
    /// future's own `Drop`, which the runtime runs as soon as the
    /// aborted task is polled.
    #[tokio::test]
    async fn a_running_entry_past_the_horizon_takes_its_task_with_it() {
        let (_tmp, state) = booted_state().await;
        let dropped = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = dropped.clone();
        let resp = start_job(
            state.clone(),
            "sync",
            "widget".into(),
            7,
            "key-a".into(),
            move |_state, _handle| DropFlag(flag),
        )
        .await
        .expect("start_job admits");
        let body: serde_json::Value = serde_json::from_slice(
            &axum::body::to_bytes(resp.into_body(), 1 << 20)
                .await
                .unwrap(),
        )
        .unwrap();
        let job_id = body["job_id"].as_str().expect("job_id").to_string();
        // Let the task actually start (and park) before the sweep.
        tokio::task::yield_now().await;

        // Inside the horizon: kept, and its task is NOT cancelled.
        sweep(&state.review_jobs);
        assert!(state.review_jobs.lock().jobs.contains_key(&job_id));
        assert!(!dropped.load(std::sync::atomic::Ordering::SeqCst));

        // Age it past the horizon the way six hours of wall clock would.
        state
            .review_jobs
            .lock()
            .jobs
            .get_mut(&job_id)
            .unwrap()
            .created = Instant::now() - Duration::from_secs(STUCK_JOB_HORIZON_SECS + 1);
        sweep(&state.review_jobs);
        for _ in 0..64 {
            if dropped.load(std::sync::atomic::Ordering::SeqCst) {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert!(
            dropped.load(std::sync::atomic::Ordering::SeqCst),
            "the horizon dropped the entry without cancelling the task — the corpse keeps \
             repo_guard and the next admission blocks on it forever"
        );

        // The entry goes as soon as the cancellation has landed, and not
        // before: `sweep` is what a later admission or read calls, so a
        // poller's next 404 needs no reaper of its own.
        for _ in 0..64 {
            sweep(&state.review_jobs);
            if !state.review_jobs.lock().jobs.contains_key(&job_id) {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert!(
            !state.review_jobs.lock().jobs.contains_key(&job_id),
            "a cancelled job's entry is never dropped"
        );
        // The task handle went with it, so nothing keeps a corpse around
        // after the map has forgotten it.
        assert!(!state.review_jobs.lock().tasks.contains_key(&job_id));
    }

    async fn job_id_of(resp: Response) -> String {
        let body: serde_json::Value = serde_json::from_slice(
            &axum::body::to_bytes(resp.into_body(), 1 << 20)
                .await
                .unwrap(),
        )
        .unwrap();
        body["job_id"].as_str().expect("job_id").to_string()
    }

    /// Poll `cond` for up to ~10 s (real time: these observe other threads).
    async fn eventually(what: &str, mut cond: impl FnMut() -> bool) {
        for _ in 0..1000 {
            if cond() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("timed out waiting for: {what}");
    }

    /// N1 / A6.f2: `settled` is written only by the body's last lines, so a
    /// PANICKING body left the entry `running` until the 6 h horizon (and
    /// every poller read "running" for hours). The watcher awaits the
    /// JoinHandle and settles it `failed`.
    #[tokio::test]
    async fn a_panicking_job_body_settles_failed_instead_of_running_forever() {
        let (_tmp, state) = booted_state().await;
        let resp = start_job(
            state.clone(),
            "sync",
            "widget".into(),
            7,
            "key-a".into(),
            move |_state, _handle| async move {
                if std::hint::black_box(0u8) == 0 {
                    panic!("boom");
                }
                Ok((StatusCode::OK, serde_json::json!({})))
            },
        )
        .await
        .expect("start_job admits");
        let job_id = job_id_of(resp).await;
        eventually("the panicked job to settle", || {
            state.review_jobs.lock().jobs[&job_id].settled.is_some()
        })
        .await;
        let job = state.review_jobs.lock().jobs[&job_id].clone();
        assert_eq!(job.status, "failed", "{job:?}");
        assert_eq!(job.error_status, Some(500));
        assert!(
            job.error.as_deref().is_some_and(|e| e.contains("panicked")),
            "{job:?}"
        );
        // A failed job no longer blocks a fresh admission for the same key.
        let again = start_job(
            state.clone(),
            "sync",
            "widget".into(),
            7,
            "key-a".into(),
            never_settling,
        )
        .await
        .expect("start_job admits");
        assert_eq!(again.status(), StatusCode::ACCEPTED);
    }

    /// M5 / A5.f3: `abort()` cancels the ASYNC task but can never interrupt
    /// a closure already on the blocking pool, so "the task finished" was
    /// not "the work stopped" — the sweep dropped the entry (and the
    /// `repo_guard` the work conceptually held) while git was still
    /// running. This runs a REAL `spawn_blocking` body through the horizon:
    /// the entry must outlive the cancellation until the closure returns.
    #[tokio::test]
    async fn the_horizon_keeps_an_entry_while_its_blocking_work_is_still_running() {
        use std::sync::atomic::AtomicBool;
        let (_tmp, state) = booted_state().await;
        let started = Arc::new(AtomicBool::new(false));
        let release = Arc::new(AtomicBool::new(false));
        let (s2, r2) = (started.clone(), release.clone());
        let resp = start_job(
            state.clone(),
            "start-pr",
            "widget".into(),
            7,
            String::new(),
            move |_state, _handle| async move {
                let _ = spawn_blocking_tracked(move || {
                    s2.store(true, Ordering::SeqCst);
                    while !r2.load(Ordering::SeqCst) {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                })
                .await;
                Ok((StatusCode::OK, serde_json::json!({})))
            },
        )
        .await
        .expect("start_job admits");
        let job_id = job_id_of(resp).await;
        eventually("the blocking body to start", || {
            started.load(Ordering::SeqCst)
        })
        .await;
        assert_eq!(
            state.review_jobs.lock().blocking[&job_id].load(Ordering::SeqCst),
            1
        );

        // Past the horizon: the sweep cancels the async task ...
        state
            .review_jobs
            .lock()
            .jobs
            .get_mut(&job_id)
            .unwrap()
            .created = Instant::now() - Duration::from_secs(STUCK_JOB_HORIZON_SECS + 1);
        sweep(&state.review_jobs);
        eventually("the cancellation to land", || {
            state.review_jobs.lock().tasks[&job_id].is_finished()
        })
        .await;

        // ... but the blocking closure is still running, so the entry stays
        // (and keeps 409ing its key) no matter how often the sweep runs.
        for _ in 0..5 {
            sweep(&state.review_jobs);
        }
        assert!(
            state.review_jobs.lock().jobs.contains_key(&job_id),
            "the sweep dropped an entry whose spawn_blocking work was still running"
        );

        // Once the closure returns, the next sweep drops it, tables and all.
        release.store(true, Ordering::SeqCst);
        eventually("the blocking closure to return", || {
            state.review_jobs.lock().blocking[&job_id].load(Ordering::SeqCst) == 0
        })
        .await;
        sweep(&state.review_jobs);
        let table = state.review_jobs.lock();
        assert!(!table.jobs.contains_key(&job_id));
        assert!(!table.tasks.contains_key(&job_id));
        assert!(!table.blocking.contains_key(&job_id));
        assert!(!table.aborted.contains(&job_id));
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
        let job = state.review_jobs.lock().jobs[&job_id].clone();
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
        let jobs: ReviewJobs = parking_lot::Mutex::new(JobTable::default());
        jobs.lock().jobs.insert(
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
        assert_eq!(handle.0.lock().jobs["job_x"].stage, "patchset");
        // An unknown id never panics (a swept job mid-run is unobservable).
        set_stage(&Some((handle.0.clone(), "job_gone".to_string())), "base");
    }

    /// K2 carry — aborting a job body must NOT release `repo_guard` while a
    /// `spawn_blocking` closure it started is still running: the next
    /// admission would otherwise run git against the same repo concurrently
    /// with the orphan. The lock is freed only when the closure returns.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_aborted_job_keeps_the_repo_guard_until_its_blocking_work_finishes() {
        let lock = Arc::new(tokio::sync::Mutex::new(()));
        let (started_tx, started_rx) = std::sync::mpsc::channel::<()>();
        let (go_tx, go_rx) = std::sync::mpsc::channel::<()>();
        let l2 = lock.clone();
        let counter = Arc::new(AtomicUsize::new(0));
        let task = tokio::spawn(CURRENT_BLOCKING.scope(
            counter,
            CURRENT_REPO_GUARD.scope(parking_lot::Mutex::new(None), async move {
                let guard: SharedRepoGuard = Arc::new(l2.lock_owned().await);
                register_repo_guard(&guard);
                let _serial = guard;
                let _ = spawn_blocking_tracked(move || {
                    started_tx.send(()).unwrap();
                    go_rx.recv().unwrap();
                })
                .await;
            }),
        ));
        tokio::task::spawn_blocking(move || started_rx.recv().unwrap())
            .await
            .unwrap();
        task.abort();
        let _ = task.await;
        assert!(
            lock.try_lock().is_err(),
            "the orphaned blocking closure still holds the repo guard"
        );
        go_tx.send(()).unwrap();
        let mut freed = false;
        for _ in 0..300 {
            if lock.try_lock().is_ok() {
                freed = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            freed,
            "the guard is released once the blocking work returns"
        );
    }

    /// A job that takes `repo_guard` once per item (retrack-bulk's apply
    /// pass) must find the lock FREE between items: the task-local slot
    /// may not keep the first guard alive.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_job_can_retake_the_repo_guard_for_every_row() {
        let lock = Arc::new(tokio::sync::Mutex::new(()));
        let counter = Arc::new(AtomicUsize::new(0));
        let l2 = lock.clone();
        let task = tokio::spawn(CURRENT_BLOCKING.scope(
            counter,
            CURRENT_REPO_GUARD.scope(parking_lot::Mutex::new(None), async move {
                for _ in 0..3 {
                    let guard: SharedRepoGuard =
                        tokio::time::timeout(Duration::from_secs(5), async {
                            Arc::new(l2.clone().lock_owned().await)
                        })
                        .await
                        .expect("the previous row's guard must be released");
                    register_repo_guard(&guard);
                    let _ = spawn_blocking_tracked(|| ()).await;
                    drop(guard);
                    assert!(l2.try_lock().is_ok(), "lock is free between rows");
                }
            }),
        ));
        task.await.expect("the job body finished without deadlock");
    }
}
