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

use crate::config::RepoEntry;
use crate::review_base::capture::{pr_of_head, read_mapped_remotes, Forge, Recapture, StoreCtx};
use crate::review_base::{
    base_out, classify_retrack, decide_kind, effective_base, BaseError, BaseMode, BaseStatus,
    BaseWarningOut, EffectiveBase, PatchsetKind, RetrackClass, ReviewBaseOut, SetBy,
    URN_CAPTURE_FAILED,
};
use crate::reviews::{
    self, admit_store, forge_pr_base_ref, require_review, store_member, verdict_scope_changed,
    with_store_ctx, ReviewGitError,
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

/// One retrack outcome — the shared shape both the single and the bulk
/// route render into JSON.
struct RetrackOutcome {
    id: i64,
    repo: String,
    minted: bool,
    ps_number: Option<i64>,
    kind: Option<String>,
    class: RetrackClass,
    base: ReviewBaseOut,
    warnings: Vec<BaseWarningOut>,
    verdict_scope_changed: bool,
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
fn retrack_sync(
    ctx: &StoreCtx<'_>,
    review: &ReviewRow,
    base_input: Option<&str>,
    is_pr: bool,
    forge_base_ref: Option<&str>,
    api_warnings: Vec<BaseWarningOut>,
    dry_run: bool,
) -> Result<RetrackOutcome, BaseError> {
    let mapped = ctx.mapped_remotes();
    let (old_mode, old_pin, _old_set_by) = current_base_state(ctx.store, review, &mapped);
    let auto_requested = base_input == Some("auto");

    let classified = ctx.classify(base_input, is_pr)?;
    let (mut policy, mut warnings) = match classified.policy.clone() {
        Some(p) => (p, classified.warnings.clone()),
        None => ctx.resolve_chain(is_pr, &review.head_ref, None, forge_base_ref, classified)?,
    };
    // README §12: every retrack result is a deliberate USER decision
    // UNLESS the caller literally asked for `auto` — in which case the
    // chain's own `SetBy::Auto` policies are left alone so the review
    // keeps following retargets.
    if !auto_requested {
        policy.set_by = SetBy::User;
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
            let fetch = ctx.fetch_forge(
                access.as_ref().map_err(String::as_str),
                &branches,
                pr_number,
            );
            warnings.extend(fetch.warnings());
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
        let target_tip = ctx.base_tip(&eff)?;
        let head_tip = ctx.head_tip(&review.head_ref)?;
        let merge_base =
            reviews::merge_base_sha(&ctx.root(), &target_tip, &head_tip).map_err(git_err)?;
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
        let class = classify_retrack(old_mode, would_mint, is_ancestor);
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
    // `policy_override` above means `recapture` ALWAYS persisted the new
    // policy (`set_review_base`/`set_review_base_ref`), whether or not a
    // patchset minted — so this is a real change worth a `review.changed`
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

/// `POST /api/reviews/{id}/retrack {base?, dry_run?}` — LOOPBACK-ONLY.
pub async fn retrack_route(
    State(state): State<SharedState>,
    AxumPath(id): AxumPath<i64>,
    raw: axum::body::Bytes,
) -> Result<impl IntoResponse, ApiError> {
    let body: RetrackBody = if raw.iter().all(u8::is_ascii_whitespace) {
        RetrackBody::default()
    } else {
        serde_json::from_slice(&raw)
            .map_err(|e| ApiError::bad_request(format!("invalid retrack body: {e}")))?
    };
    let (review, _repo, _) = require_review(&state, id).await?;
    let outcome = retrack_one(&state, review, body.base, body.dry_run).await?;
    Ok((
        [(axum::http::header::CACHE_CONTROL, "no-store")],
        Json(outcome_json(&outcome, body.dry_run)),
    ))
}

async fn retrack_one(
    state: &SharedState,
    review: ReviewRow,
    base_input: Option<String>,
    dry_run: bool,
) -> Result<RetrackOutcome, ApiError> {
    let handle = admit_store(state, &review.repo)
        .await?
        .ok_or_else(|| store_required(&review.repo))?;
    let member = store_member(state, &review.repo)?;
    let is_pr = pr_of_head(&review.head_ref).is_some();
    let (forge_base_ref, api_warnings) = match pr_of_head(&review.head_ref) {
        Some(n) => {
            forge_pr_base_ref(
                state,
                &handle,
                &review.repo,
                n,
                state.github.with_cli_token(None),
                crate::review_store::GhCli::from_process_env(),
            )
            .await
        }
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

#[derive(Debug, Deserialize, Default)]
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

/// `POST /api/reviews/retrack-bulk {repo?, pinned?, legacy?, dry_run}` —
/// LOOPBACK-ONLY. README D17: `dry_run=false` applies ONLY `stale-pin`
/// rows; `custom` rows are NEVER auto-applied. One repo's store-admission
/// failure (seeding, locked) is recorded on `repo_errors` and skipped —
/// never aborts the whole scan.
pub async fn retrack_all_route(
    State(state): State<SharedState>,
    Json(body): Json<RetrackAllBody>,
) -> Result<impl IntoResponse, ApiError> {
    let repo_names: Vec<String> = match &body.repo {
        Some(r) => {
            find_repo(&state, r)?;
            vec![r.clone()]
        }
        None => state.repos.iter().map(|r| r.name.clone()).collect(),
    };
    let mut rows = Vec::new();
    let mut repo_errors = Vec::new();
    for name in &repo_names {
        let Ok((repo, _)) = find_repo(&state, name) else {
            continue;
        };
        let repo = repo.clone();
        match retrack_all_for_repo(&state, &repo, body.pinned, body.legacy, !body.dry_run).await {
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
    Ok((
        [(axum::http::header::CACHE_CONTROL, "no-store")],
        Json(serde_json::json!({
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
        })),
    ))
}

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
            let reviews = store.list_reviews(&repo_name, None)?;
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
    // BEFORE the one sequential sync pass below (spawn_blocking cannot
    // await).
    let mut forge_refs: HashMap<i64, (Option<String>, Vec<BaseWarningOut>)> = HashMap::new();
    for review in &candidates {
        if let Some(n) = pr_of_head(&review.head_ref) {
            let r = forge_pr_base_ref(
                state,
                &handle,
                &repo.name,
                n,
                state.github.with_cli_token(None),
                crate::review_store::GhCli::from_process_env(),
            )
            .await;
            forge_refs.insert(review.id, r);
        }
    }

    let rows = with_store_ctx(state, handle, member, move |ctx| {
        candidates
            .into_iter()
            .map(|review| {
                let is_pr = pr_of_head(&review.head_ref).is_some();
                let (forge_base_ref, api_warnings) =
                    forge_refs.get(&review.id).cloned().unwrap_or_default();
                let dry = match retrack_sync(
                    ctx,
                    &review,
                    None,
                    is_pr,
                    forge_base_ref.as_deref(),
                    api_warnings.clone(),
                    true,
                ) {
                    Ok(o) => o,
                    Err(e) => {
                        return serde_json::json!({
                            "id": review.id,
                            "repo": review.repo,
                            "row_error": e.message,
                        })
                    }
                };
                if apply && matches!(dry.class, RetrackClass::StalePin) {
                    match retrack_sync(
                        ctx,
                        &review,
                        None,
                        is_pr,
                        forge_base_ref.as_deref(),
                        api_warnings,
                        false,
                    ) {
                        Ok(o) => outcome_json(&o, false),
                        Err(e) => serde_json::json!({
                            "id": review.id,
                            "repo": review.repo,
                            "class": "stale-pin",
                            "row_error": e.message,
                        }),
                    }
                } else {
                    outcome_json(&dry, true)
                }
            })
            .collect::<Vec<_>>()
    })
    .await?;
    Ok(rows)
}
