//! RS-U7 — `review retrack` (README §3, §10 step 4, §12; D17/D20):
//! reconciling a review's stored base against what a CORRECT one would be
//! right now, without ever touching findings or a published verdict.
//!
//! * `POST /api/reviews/{id}/retrack {base?, dry_run?}` — one review.
//!   `base` is the SAME `--base` grammar every other creation route takes
//!   ([`crate::review_base::classify_base`], via [`StoreCtx::classify`]);
//!   omitted (or `"auto"`) re-runs the FULL resolution chain
//!   ([`StoreCtx::resolve_chain`]) against the review's CURRENT head,
//!   exactly like a brand-new review would resolve today. Every result
//!   (explicit or chain-resolved) is persisted `set_by=user` — UNLESS the
//!   caller passed the literal string `"auto"`, which keeps `set_by=auto`
//!   (README §12: "except that `--base auto` sets `set_by=auto`") so the
//!   review opts back INTO auto-follow rather than freezing a manual
//!   decision. The capture this mints (when the pair actually changed)
//!   always carries `kind=base-corrected` — never `push`/`rebase`/
//!   `base-moved`, so a patchset strip can tell "the operator fixed this"
//!   apart from an ordinary push.
//! * `POST /api/reviews/retrack-bulk {repo?, pinned?, legacy?, dry_run}` —
//!   every review in scope whose EFFECTIVE base is a `pin` (D17): classes
//!   `equivalent` (nothing would change) / `stale-pin` (a frozen sha that
//!   is an ancestor of the fresh target — the review-65 shape) / `custom`
//!   (a frozen sha OUTSIDE the target's history — never touched by
//!   `--yes`) / `unknown` (resolution failed, or the row isn't a pin at
//!   all). `dry_run=false` (`--yes`) applies ONLY `stale-pin` rows,
//!   sequentially, one review's failure recorded on its own row rather
//!   than aborting the batch (the CLI turns a non-empty `row_errors` into
//!   exit 7 — README §13's `partial`).
//!
//! Findings and a published verdict stay on the OLD patchset (D20) simply
//! because nothing here ever moves `review_findings`/`review_annotations`
//! anchors or `reviews.verdict_ps` — [`reviews::verdict_scope_changed`] is
//! the one ADDITIVE signal this unit adds so a caller can tell "the diff's
//! CONTENT is unchanged, only its base moved" apart from an ordinary
//! staleness.

use axum::extract::{Path as AxumPath, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::Arc;

use crate::config::RepoEntry;
use crate::review_base::capture::{
    pr_of_head, read_mapped_remotes, BaseFetchMemo, Forge, Recapture, StoreCtx,
};
use crate::review_base::{
    base_out, classify_retrack, decide_kind, effective_base, BaseError, BaseMode, BaseStatus,
    BaseWarningOut, EffectiveBase, PatchsetKind, RetrackClass, ReviewBaseOut, SetBy,
    URN_CAPTURE_FAILED,
};
use crate::reviews::{
    self, admit_store, require_review, store_member, verdict_scope_changed, with_store_ctx,
    ReviewGitError,
};
use crate::routes::{find_repo, ApiError};
use crate::state::SharedState;
use crate::store::{ReviewRow, Store, StoreBlocking};

pub const RETRACK_SCHEMA: &str = "kbc-review-retrack/1";
pub const RETRACK_ALL_SCHEMA: &str = "kbc-review-retrack-all/1";

fn git_err(e: ReviewGitError) -> BaseError {
    BaseError::new(500, URN_CAPTURE_FAILED, e.to_string())
}

fn store_required(repo: &str) -> ApiError {
    ApiError::new(
        StatusCode::CONFLICT,
        format!(
            "{repo} has no ready review store yet — run `kb-code store sync --repo {repo}` first"
        ),
    )
    .with_problem_type("urn:kb:errors:store-required")
}

/// The review's CURRENT effective base, reduced to what [`classify_retrack`]
/// needs: its mode, its pin sha (only set for `Pin`), and who set it. Pure
/// DB read — mirrors [`StoreCtx::recapture`]'s own first step (RS-U6) but
/// returns the reduced triple rather than the full [`EffectiveBase`],
/// since retrack needs it BEFORE it decides what to resolve TO.
fn current_base_state(
    store: &Store,
    review: &ReviewRow,
    mapped: &[String],
) -> (Option<BaseMode>, Option<String>, SetBy) {
    let base_row = store.get_review_base(review.id).ok().flatten();
    let binding = store
        .get_review_pr_binding(review.id)
        .ok()
        .flatten()
        .unwrap_or_default();
    let meta_base: Option<String> = binding
        .pr_meta_json
        .as_deref()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
        .and_then(|v| {
            v.get("base_ref")
                .and_then(|b| b.as_str())
                .map(str::to_string)
        });
    let set_by_col = base_row
        .as_ref()
        .map(|b| b.base_set_by.clone())
        .unwrap_or_else(|| "legacy".into());
    let class = effective_base(
        &review.base_ref,
        base_row.as_ref().and_then(|b| b.base_mode.as_deref()),
        base_row.as_ref().and_then(|b| b.base_branch.as_deref()),
        base_row.as_ref().and_then(|b| b.base_member),
        &set_by_col,
        None,
        pr_of_head(&review.head_ref).is_some(),
        meta_base.as_deref(),
        mapped,
    );
    match class.effective {
        EffectiveBase::Policy(p) => (Some(p.mode), p.pin.clone(), p.set_by),
        EffectiveBase::Verbatim(_) => (None, None, SetBy::Legacy),
    }
}

/// The PR target recorded in the review's own PR snapshot
/// (`pr_meta_json.base_ref`). A failed read is `None`, which only ever makes
/// the caller MORE conservative (an unknown target refuses an apply).
fn stored_pr_target(store: &Store, review: &ReviewRow) -> Option<String> {
    store
        .get_review_pr_binding(review.id)
        .ok()
        .flatten()
        .and_then(|b| b.pr_meta_json)
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| {
            v.get("base_ref")
                .and_then(|b| b.as_str())
                .map(str::to_string)
        })
}

/// One retrack outcome — the shared shape both the single and the bulk
/// route render into JSON.
pub(crate) struct RetrackOutcome {
    pub(crate) id: i64,
    pub(crate) repo: String,
    pub(crate) minted: bool,
    pub(crate) ps_number: Option<i64>,
    pub(crate) kind: Option<String>,
    pub(crate) class: RetrackClass,
    pub(crate) base: ReviewBaseOut,
    pub(crate) warnings: Vec<BaseWarningOut>,
    pub(crate) verdict_scope_changed: bool,
}

fn outcome_json(o: &RetrackOutcome, dry_run: bool) -> serde_json::Value {
    serde_json::json!({
        "schema": RETRACK_SCHEMA,
        "id": o.id,
        "repo": o.repo,
        "dry_run": dry_run,
        "minted": o.minted,
        "ps_number": o.ps_number,
        "kind": o.kind,
        "class": o.class.as_str(),
        "base": o.base,
        "warnings": o.warnings,
        "verdict_scope_changed": o.verdict_scope_changed,
    })
}

/// The classify-and-maybe-apply pass for ONE review, run synchronously
/// inside [`with_store_ctx`] (README §4.2 ops lock — every write this
/// function makes goes through [`StoreCtx::recapture`], which already
/// takes it). `dry_run=false` fetches AND captures via `recapture`;
/// `dry_run=true` fetches (README §5.3: retrack always fetches, so a dry
/// run classifies against FRESH data) but calls no capture/persist path
/// at all.
#[allow(clippy::too_many_arguments)]
pub(crate) fn retrack_sync(
    ctx: &StoreCtx<'_>,
    review: &ReviewRow,
    base_input: Option<&str>,
    is_pr: bool,
    forge_base_ref: Option<&str>,
    api_warnings: Vec<BaseWarningOut>,
    dry_run: bool,
) -> Result<RetrackOutcome, BaseError> {
    retrack_sync_with(
        ctx,
        review,
        base_input,
        is_pr,
        forge_base_ref,
        api_warnings,
        dry_run,
        None,
    )
}

/// [`retrack_sync`] with a bulk run's base-fetch memo (X1/K3): every distinct
/// base branch is fetched once per run, by the dry classification or the
/// apply, whichever needs it first.
#[allow(clippy::too_many_arguments)]
pub(crate) fn retrack_sync_with(
    ctx: &StoreCtx<'_>,
    review: &ReviewRow,
    base_input: Option<&str>,
    is_pr: bool,
    forge_base_ref: Option<&str>,
    api_warnings: Vec<BaseWarningOut>,
    dry_run: bool,
    memo: Option<&Arc<BaseFetchMemo>>,
) -> Result<RetrackOutcome, BaseError> {
    let mapped = ctx.mapped_remotes();
    let (old_mode, old_pin, _old_set_by) = current_base_state(ctx.store, review, &mapped);
    let auto_requested = base_input == Some("auto");

    let classified = ctx.classify(base_input, is_pr)?;
    // A6-4 — the PR's target comes from the fresh forge read, else from the
    // target the review's own PR snapshot recorded (`pr_meta_json.base_ref`,
    // refreshed by every sweep/sync), BEFORE the chain falls to a
    // default-branch guess. A degraded forge API must not turn a
    // develop-targeting PR into a main-targeting one.
    let stored_target = if is_pr {
        stored_pr_target(ctx.store, review)
    } else {
        None
    };
    let chain_forge = forge_base_ref.or(stored_target.as_deref());
    let explicit = classified.policy.is_some();
    let (mut policy, mut warnings) = match classified.policy.clone() {
        Some(p) => (p, classified.warnings.clone()),
        None => ctx.resolve_chain(is_pr, &review.head_ref, None, chain_forge, classified)?,
    };
    // A PR target that NO rung could name although the forge API WAS
    // consulted and could not answer (`api_warnings` non-empty: a failed
    // read, or a D12 gh-login warning) AND no stored snapshot names it
    // landed on the assumed default branch. That is a guess, not a decision:
    // it is never persisted as `set_by=user`, never classed `stale-pin`, and
    // an apply is refused. (A store with no forge API at all — a local or
    // non-GitHub forge — has no warnings and keeps the documented
    // default-branch assumption, README §6.)
    let target_guessed = is_pr
        && !explicit
        && !api_warnings.is_empty()
        && policy.source == crate::review_base::BaseSource::DefaultAssumed;
    // README §12: every retrack result is a deliberate USER decision
    // UNLESS the caller literally asked for `auto` — in which case the
    // chain's own `SetBy::Auto` policies are left alone so the review
    // keeps following retargets.
    // A target that fell all the way to the assumed default branch is a
    // GUESS whether the forge failed (`target_guessed`, refused on apply) or
    // has no API at all (nothing to ask): either way it is never recorded as
    // a person's decision — it stays `auto` / `default-assumed`, so the
    // review keeps following a retarget once the target is knowable.
    let assumed_default =
        is_pr && !explicit && policy.source == crate::review_base::BaseSource::DefaultAssumed;
    if !auto_requested && !assumed_default {
        policy.set_by = SetBy::User;
    }
    if target_guessed && !auto_requested && !dry_run {
        return Err(BaseError::undetermined(format!(
            "the PR's target branch could not be read from the forge or from the review's stored PR snapshot (assumed {:?}); retrack refuses to record that guess — pass --base <branch> or retry when the forge API answers",
            policy.branch.as_deref().unwrap_or("?")
        )));
    }

    let pr_number = pr_of_head(&review.head_ref);

    if dry_run {
        // The apply branch below carries these into `Recapture::
        // api_warnings` instead, so `recapture`'s own merge doesn't
        // duplicate them into `r.warnings`.
        warnings.extend(api_warnings.iter().cloned());
        let has_forge = !matches!(ctx.forge(), Forge::None);
        if has_forge {
            let branches: Vec<String> = if policy.mode == BaseMode::Track {
                policy.branch.iter().cloned().collect()
            } else {
                Vec::new()
            };
            let access = ctx.access();
            let fetch = ctx.fetch_forge_memo(
                memo.map(|m| &**m),
                access.as_ref().map_err(String::as_str),
                &branches,
                pr_number,
            );
            warnings.extend(fetch.warnings());
            // A6-7 — the apply path (`recapture`) answers 409 `base-vanished`
            // for a tracked branch the forge no longer has; the dry run must
            // say the same instead of classifying against the stale
            // `refs/remotes/base/<branch>` the failed fetch left behind.
            if let Some(b) = branches.first().filter(|b| fetch.vanished.contains(*b)) {
                return Err(BaseError::new(
                    409,
                    crate::review_base::URN_BASE_VANISHED,
                    format!(
                        "the base branch {b:?} no longer exists on the forge — retrack the review with --base <branch>"
                    ),
                ));
            }
        }
        // Mirrors `StoreCtx::capture_with`'s own two import calls (the
        // ONLY other path that resolves `base_tip`/`head_tip`), since a
        // dry run never reaches `capture`/`capture_with` itself: the
        // review's own head (non-PR only — a PR's head comes from the
        // fetch above) and whatever the TARGET policy needs (a `local`
        // branch, or a pin/legacy rev not yet in the store).
        if pr_number.is_none() {
            ctx.import_head(&review.head_ref)?;
        }
        let eff = EffectiveBase::Policy(policy.clone());
        ctx.import_base(&eff)?;
        let mut target_tip = ctx.base_tip(&eff)?;
        let head_tip = ctx.head_tip(&review.head_ref)?;
        let mut merge_base =
            reviews::merge_base_sha(&ctx.root(), &target_tip, &head_tip).map_err(git_err)?;
        // A6-1 twin: the apply pins a head the target already contains to
        // its merge-time base (or refuses when no merge names one); the dry
        // run predicts exactly that.
        if is_pr {
            if let Some(pin) = crate::review_base::capture::merged_pin(
                &ctx.root(),
                &head_tip,
                &merge_base,
                &target_tip,
            )? {
                merge_base =
                    reviews::merge_base_sha(&ctx.root(), &pin, &head_tip).map_err(git_err)?;
                target_tip = pin;
            }
        }
        let latest = ctx.store.latest_patchset(review.id).ok().flatten();
        let would_mint = decide_kind(
            latest
                .as_ref()
                .map(|l| (l.tip_sha.as_str(), l.base_sha.as_str())),
            &head_tip,
            &merge_base,
            None,
            false,
        )
        .is_some();
        let is_ancestor = match (old_mode, &old_pin) {
            (Some(BaseMode::Pin), Some(pin)) => {
                Some(reviews::is_ancestor(&ctx.root(), pin, &target_tip).map_err(git_err)?)
            }
            _ => None,
        };
        let class = if target_guessed {
            // Never `stale-pin`: the target is a guess.
            RetrackClass::Unknown
        } else {
            classify_retrack(old_mode, would_mint, is_ancestor)
        };
        let set_by_str = policy.set_by.as_str().to_string();
        let status = BaseStatus {
            state: Some(if would_mint {
                "would-change".to_string()
            } else {
                "ok".to_string()
            }),
            ..BaseStatus::default()
        };
        let base = base_out(&eff, &set_by_str, &status, Some(&merge_base));
        return Ok(RetrackOutcome {
            id: review.id,
            repo: review.repo.clone(),
            minted: false,
            ps_number: latest.map(|l| l.ps_number),
            kind: None,
            class,
            base,
            warnings,
            verdict_scope_changed: false,
        });
    }

    // --- apply --------------------------------------------------------
    let rc = Recapture {
        network: true,
        force: false,
        forge_base_ref: forge_base_ref.map(str::to_string),
        kind_hint: Some(PatchsetKind::BaseCorrected),
        // RS-U6 fix heads-up: pass the policy explicitly so `recapture`
        // ALWAYS persists it (never relies on the `class.upgraded` path,
        // which is for legacy auto-upgrades, not a deliberate retrack).
        policy_override: Some(policy.clone()),
        api_warnings,
        base_memo: memo.cloned(),
        #[cfg(test)]
        after_fetch: tests_seam::AFTER_FETCH.with(|h| h.borrow().clone()),
    };
    let r = ctx.recapture(review, &rc)?;
    warnings.extend(r.warnings.clone());

    let eff = EffectiveBase::Policy(policy.clone());
    // Ref-only (no network): resolves against what `recapture` just fetched.
    let target_tip = ctx
        .base_tip(&eff)
        .unwrap_or_else(|_| r.outcome.ps.base_sha.clone());
    let is_ancestor = match (old_mode, &old_pin) {
        (Some(BaseMode::Pin), Some(pin)) => {
            Some(reviews::is_ancestor(&ctx.root(), pin, &target_tip).unwrap_or(false))
        }
        _ => None,
    };
    let class = classify_retrack(old_mode, r.outcome.minted, is_ancestor);

    let verdict_tip: Option<String> = review.verdict_ps.and_then(|vp| {
        ctx.store
            .get_patchset(review.id, vp)
            .ok()
            .flatten()
            .map(|p| p.tip_sha)
    });
    let vsc = verdict_scope_changed(
        review,
        Some(r.outcome.ps.ps_number),
        verdict_tip.as_deref(),
        Some(r.outcome.ps.tip_sha.as_str()),
    );

    let set_by_str = r
        .effective
        .policy()
        .map(|p| p.set_by.as_str())
        .unwrap_or("legacy")
        .to_string();
    let base = base_out(
        &r.effective,
        &set_by_str,
        &r.status,
        Some(&r.outcome.ps.base_sha),
    );
    // `policy_override` above means `recapture` persisted the new policy
    // (`set_review_base`/`set_review_base_ref`), whether or not a patchset
    // minted (a concurrent policy change instead fails the whole call with
    // 409 `base-changed` before anything is minted or written) — so this is a real change worth a `review.changed`
    // every time the apply branch runs (capture's own `patchset` emit,
    // inside `capture_at`, only fires when something minted).
    reviews::emit_review_changed(ctx.bus, review.id, &review.repo, "meta", false);
    Ok(RetrackOutcome {
        id: review.id,
        repo: review.repo.clone(),
        minted: r.outcome.minted,
        ps_number: Some(r.outcome.ps.ps_number),
        kind: r.outcome.kind.clone(),
        class,
        base,
        warnings,
        verdict_scope_changed: vsc,
    })
}

#[derive(Debug, Deserialize, Default)]
pub struct RetrackBody {
    #[serde(default)]
    pub base: Option<String>,
    #[serde(default)]
    pub dry_run: bool,
}

/// `?async=1` on the retrack routes: run as a daemon job (X1/K3, the
/// start-pr / sync pattern) — the request answers 202 + `job_id` at once and
/// the caller polls `GET /api/reviews/jobs/{id}` instead of holding one HTTP
/// request open for the whole network fetch.
#[derive(Debug, Deserialize, Default)]
pub struct AsyncParams {
    #[serde(default, rename = "async")]
    pub async_: Option<String>,
}

impl AsyncParams {
    pub fn wants_async(&self) -> bool {
        matches!(
            self.async_.as_deref(),
            Some("1") | Some("true") | Some("yes")
        )
    }
}

/// `POST /api/reviews/{id}/retrack {base?, dry_run?}[?async=1]` —
/// LOOPBACK-ONLY.
pub async fn retrack_route(
    State(state): State<SharedState>,
    AxumPath(id): AxumPath<i64>,
    axum::extract::Query(params): axum::extract::Query<AsyncParams>,
    raw: axum::body::Bytes,
) -> Result<axum::response::Response, ApiError> {
    let body: RetrackBody = if raw.iter().all(u8::is_ascii_whitespace) {
        RetrackBody::default()
    } else {
        serde_json::from_slice(&raw)
            .map_err(|e| ApiError::bad_request(format!("invalid retrack body: {e}")))?
    };
    let (review, _repo, _) = require_review(&state, id).await?;
    if params.wants_async() {
        // The job's number slot carries the REVIEW id (this job is about one
        // review, not one PR); the key makes a different request a 409.
        let key = format!("{}|{}", body.base.as_deref().unwrap_or(""), body.dry_run);
        let repo = review.repo.clone();
        let dry_run = body.dry_run;
        let base = body.base.clone();
        return crate::review_jobs::start_job(
            state,
            "retrack",
            repo,
            u32::try_from(id).unwrap_or(0),
            key,
            move |st, _handle| async move {
                let outcome = retrack_one(&st, review, base, dry_run).await?;
                Ok((StatusCode::OK, outcome_json(&outcome, dry_run)))
            },
        )
        .await;
    }
    let outcome = retrack_one(&state, review, body.base, body.dry_run).await?;
    Ok((
        [(axum::http::header::CACHE_CONTROL, "no-store")],
        Json(outcome_json(&outcome, body.dry_run)),
    )
        .into_response())
}

async fn retrack_one(
    state: &SharedState,
    review: ReviewRow,
    base_input: Option<String>,
    dry_run: bool,
) -> Result<RetrackOutcome, ApiError> {
    // A6-3 — an applying retrack serialises with start-pr/sync on the repo
    // (a dry run writes nothing and stays lock-free).
    let _serial = if dry_run {
        None
    } else {
        crate::review_sync::repo_guard(state, &review.repo).await
    };
    let handle = admit_store(state, &review.repo)
        .await?
        .ok_or_else(|| store_required(&review.repo))?;
    let member = store_member(state, &review.repo)?;
    let is_pr = pr_of_head(&review.head_ref).is_some();
    let (forge_base_ref, api_warnings) = match pr_of_head(&review.head_ref) {
        Some(n) => crate::reviews::forge_pr_base_ref_ambient(state, &handle, &review.repo, n).await,
        None => (None, vec![]),
    };
    let review2 = review.clone();
    let outcome = with_store_ctx(state, handle, member, move |ctx| {
        retrack_sync(
            ctx,
            &review2,
            base_input.as_deref(),
            is_pr,
            forge_base_ref.as_deref(),
            api_warnings,
            dry_run,
        )
    })
    .await??;
    Ok(outcome)
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct RetrackAllBody {
    #[serde(default)]
    pub repo: Option<String>,
    #[serde(default)]
    pub pinned: bool,
    #[serde(default)]
    pub legacy: bool,
    /// Default ON (a dry run) — matches every other bulk mutation in this
    /// crate (`store legacy-refs`, `review sweep --close`); an explicit
    /// `false` is required to apply.
    #[serde(default = "dry_run_default")]
    pub dry_run: bool,
}

fn dry_run_default() -> bool {
    true
}

/// `POST /api/reviews/retrack-bulk {repo?, pinned?, legacy?, dry_run}[?async=1]`
/// — LOOPBACK-ONLY. README D17: `dry_run=false` applies ONLY `stale-pin`
/// rows; `custom` rows are NEVER auto-applied. Only OPEN reviews are
/// scanned (a closed review's verdict and findings are final). One repo's
/// store-admission failure (seeding, locked) is recorded on `repo_errors`
/// and skipped — never aborts the whole scan. `?async=1` runs it as a
/// daemon job (X1/K3): 202 + `job_id`, polled on `GET /api/reviews/jobs/{id}`.
pub async fn retrack_all_route(
    State(state): State<SharedState>,
    axum::extract::Query(params): axum::extract::Query<AsyncParams>,
    Json(body): Json<RetrackAllBody>,
) -> Result<axum::response::Response, ApiError> {
    if let Some(r) = &body.repo {
        find_repo(&state, r)?;
    }
    if params.wants_async() {
        let key = format!(
            "{}|{}|{}|{}",
            body.repo.as_deref().unwrap_or("*"),
            body.pinned,
            body.legacy,
            body.dry_run
        );
        let repo = body.repo.clone().unwrap_or_else(|| "*".to_string());
        return crate::review_jobs::start_job(
            state,
            "retrack-bulk",
            repo,
            0,
            key,
            move |st, _handle| async move {
                Ok((StatusCode::OK, retrack_all_value(&st, &body).await?))
            },
        )
        .await;
    }
    let value = retrack_all_value(&state, &body).await?;
    Ok((
        [(axum::http::header::CACHE_CONTROL, "no-store")],
        Json(value),
    )
        .into_response())
}

/// The bulk scan's body, shared by the synchronous route and the job.
async fn retrack_all_value(
    state: &SharedState,
    body: &RetrackAllBody,
) -> Result<serde_json::Value, ApiError> {
    let repo_names: Vec<String> = match &body.repo {
        Some(r) => vec![r.clone()],
        None => state.repos.iter().map(|r| r.name.clone()).collect(),
    };
    let mut rows = Vec::new();
    let mut repo_errors = Vec::new();
    for name in &repo_names {
        let Ok((repo, _)) = find_repo(state, name) else {
            continue;
        };
        let repo = repo.clone();
        match retrack_all_for_repo(state, &repo, body.pinned, body.legacy, !body.dry_run).await {
            Ok(mut r) => rows.append(&mut r),
            Err(e) => repo_errors.push(serde_json::json!({
                "repo": name,
                "error": e.message(),
            })),
        }
    }
    let would_mint = rows
        .iter()
        .filter(|r| r["class"].as_str() == Some("stale-pin"))
        .count();
    let applied = rows
        .iter()
        .filter(|r| r["minted"].as_bool().unwrap_or(false))
        .count();
    let partial = !repo_errors.is_empty() || rows.iter().any(|r| r.get("row_error").is_some());
    Ok(serde_json::json!({
        "schema": RETRACK_ALL_SCHEMA,
        "dry_run": body.dry_run,
        "summary": {
            "scanned": rows.len(),
            "stale_pin": would_mint,
            "applied": applied,
        },
        "rows": rows,
        "repo_errors": repo_errors,
        "degraded": partial,
    }))
}

/// One repo's bulk scan (X1/K3).
///
/// * The classification pass (every network fetch) runs WITHOUT
///   [`crate::review_sync::repo_guard`]: it used to hold the per-repo lock
///   across every candidate's fetch, so `start-pr` / `sync` for that repo
///   blocked for the whole run. The guard is taken PER APPLIED ROW, only
///   around the one capture that writes.
/// * One [`BaseFetchMemo`] spans the repo's scan and its applies, so each
///   distinct base branch is fetched ONCE rather than once per candidate
///   (dry run) plus once more per applied row.
async fn retrack_all_for_repo(
    state: &SharedState,
    repo: &RepoEntry,
    pinned: bool,
    legacy: bool,
    apply: bool,
) -> Result<Vec<serde_json::Value>, ApiError> {
    let handle = match admit_store(state, &repo.name).await {
        Ok(Some(h)) => h,
        Ok(None) => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let member = store_member(state, &repo.name)?;

    let repo_name = repo.name.clone();
    let root = member.root.clone();
    let candidates: Vec<(ReviewRow, SetBy)> = state
        .store
        .run_blocking(move |store| -> Result<_, ApiError> {
            // Only OPEN reviews: a closed review's verdict and findings are
            // final, and re-basing one mints a `base-corrected` patchset that
            // orphans them against whatever the target is today.
            let reviews = store.list_reviews(&repo_name, Some("open"))?;
            let mapped = read_mapped_remotes(store, &repo_name, &root);
            Ok(reviews
                .into_iter()
                .filter_map(|r| {
                    let (mode, _pin, set_by) = current_base_state(store, &r, &mapped);
                    (mode == Some(BaseMode::Pin)).then_some((r, set_by))
                })
                .collect())
        })
        .await?;
    let candidates: Vec<ReviewRow> = candidates
        .into_iter()
        .filter(|(_, set_by)| {
            if legacy && !pinned {
                *set_by == SetBy::Legacy
            } else {
                true // `--pinned` (or neither flag given) — every pin row.
            }
        })
        .map(|(r, _)| r)
        .collect();

    // Forge `base.ref` per PR-bound candidate — network, so gathered
    // BEFORE the sequential sync pass below (spawn_blocking cannot await).
    let mut forge_refs: HashMap<i64, (Option<String>, Vec<BaseWarningOut>)> = HashMap::new();
    for review in &candidates {
        if let Some(n) = pr_of_head(&review.head_ref) {
            let r = crate::reviews::forge_pr_base_ref_ambient(state, &handle, &repo.name, n).await;
            forge_refs.insert(review.id, r);
        }
    }

    let memo = Arc::new(BaseFetchMemo::default());

    // Pass 1 — classify every candidate (dry run), lock-free.
    let dry_refs = forge_refs.clone();
    let dry_memo = memo.clone();
    let dry_rows: Vec<(ReviewRow, Result<RetrackOutcome, BaseError>)> =
        with_store_ctx(state, handle.clone(), member.clone(), move |ctx| {
            candidates
                .into_iter()
                .map(|review| {
                    let is_pr = pr_of_head(&review.head_ref).is_some();
                    let (forge_base_ref, api_warnings) =
                        dry_refs.get(&review.id).cloned().unwrap_or_default();
                    let dry = retrack_sync_with(
                        ctx,
                        &review,
                        None,
                        is_pr,
                        forge_base_ref.as_deref(),
                        api_warnings,
                        true,
                        Some(&dry_memo),
                    );
                    (review, dry)
                })
                .collect::<Vec<_>>()
        })
        .await?;

    // Pass 2 — render; apply stale-pin rows one at a time, each under the
    // repo guard for just its own capture.
    let mut rows = Vec::with_capacity(dry_rows.len());
    for (review, dry) in dry_rows {
        let dry = match dry {
            Ok(o) => o,
            Err(e) => {
                rows.push(serde_json::json!({
                    "id": review.id,
                    "repo": review.repo,
                    "row_error": e.message,
                }));
                continue;
            }
        };
        if !(apply && matches!(dry.class, RetrackClass::StalePin)) {
            rows.push(outcome_json(&dry, true));
            continue;
        }
        let _serial = crate::review_sync::repo_guard(state, &repo.name).await;
        let is_pr = pr_of_head(&review.head_ref).is_some();
        let (forge_base_ref, api_warnings) =
            forge_refs.get(&review.id).cloned().unwrap_or_default();
        let (id, repo_label) = (review.id, review.repo.clone());
        let apply_memo = memo.clone();
        let applied = with_store_ctx(state, handle.clone(), member.clone(), move |ctx| {
            retrack_sync_with(
                ctx,
                &review,
                None,
                is_pr,
                forge_base_ref.as_deref(),
                api_warnings,
                false,
                Some(&apply_memo),
            )
        })
        .await?;
        rows.push(match applied {
            Ok(o) => outcome_json(&o, false),
            Err(e) => serde_json::json!({
                "id": id,
                "repo": repo_label,
                "class": "stale-pin",
                "row_error": e.message,
            }),
        });
    }
    Ok(rows)
}

/// Test seam (K6 / A6.f9): lets a test land a concurrent snapshot's policy
/// write right after retrack's network fetch, where the race lives.
#[cfg(test)]
pub(crate) mod tests_seam {
    use crate::review_base::capture::TestHook;
    use std::cell::RefCell;

    thread_local! {
        pub(crate) static AFTER_FETCH: RefCell<Option<TestHook>> = const { RefCell::new(None) };
    }
}
