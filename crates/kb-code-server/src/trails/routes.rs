//! `kbc-trail/1`'s HTTP surface (V74-L3b, design D12 + D17).
//!
//! # The routes, and their gates
//!
//! | route | gate |
//! |---|---|
//! | `GET /api/trails/state` | `auth_bearer` |
//! | `GET /api/trails?repo=[&limit=]` | **loopback-only** |
//! | `GET /api/trails/{id}?repo=[&from=][&notes=1]` | **loopback-only** |
//! | `GET /api/trails/aggregate?repo=[&since=][&until=][&limit=]` | `auth_bearer` |
//! | `POST /api/trails/state` | **loopback-only** |
//! | `POST /api/trails/steps` | **loopback-only** |
//! | `POST /api/trails` (authored) | **loopback-only** |
//! | `POST /api/trails/{id}/fork` | **loopback-only** |
//! | `POST /api/trails/purge` | **loopback-only** |
//!
//! # Why the two HUMAN reads are loopback-only and the AGGREGATE is not
//!
//! `GET /api/trails` and `GET /api/trails/{id}` return the operator's own
//! movement record — an ordered list of where a person went and how long
//! they stayed. The design's Security §Privacy line is "read-tracking never
//! leaves the operator's box" and D21 repeats it, so those two ride the
//! same `transcripts_api` loopback-only sub-router the raw-transcript reads
//! do. That is a STRICTER gate than any other read in this crate, and it is
//! the same reasoning invariant 23(b) applies to a transcript-text join.
//!
//! `GET /api/trails/aggregate` is the one surface D17 puts in front of an
//! agent, and it is capped at counts per file/symbol with no ordering below
//! the day — so it rides `auth_bearer` like every other read. The cap is
//! structural: `store::TrailAggregateRow` has no timestamp field, and the
//! query never selects `entered_at`.
//!
//! `GET /api/trails/state` is `auth_bearer` because the SPA's INDICATOR is
//! mandatory (D17: "an explicit opt-in that is off on first boot with a
//! visible indicator") and an indicator that cannot render is not an
//! indicator. It reports `mutable: false` for a caller that could not
//! change the mode anyway, so the SPA hides the control instead of offering
//! a button that 403s.
//!
//! # Two gates, not one
//!
//! Every WRITE checks BOTH `[trails] enabled` (the operator's master
//! switch, default `false`) and the persisted mode
//! ([`super::MODE_RECORDING`]). A refusal names WHICH gate it hit —
//! `trails-disabled` vs `trails-paused` vs `trails-off` — because "nothing
//! is being recorded" has three different fixes and a generic 403 tells the
//! operator none of them.

use super::*;
use crate::routes::{find_repo, ApiError};
use crate::state::SharedState;
use crate::store::{NewTrail, NewTrailStep, StoreBlocking};
use axum::extract::{Path as AxumPath, Query, State};
use axum::http::{header, StatusCode};
use axum::response::IntoResponse;
use axum::Json;

/// RFC 7807 `type` URNs — one per refusal cause, so a client can tell the
/// three "nothing is being recorded" states apart without parsing prose.
pub const ERR_DISABLED: &str = "urn:kb:errors:trails-disabled";
pub const ERR_OFF: &str = "urn:kb:errors:trails-off";
pub const ERR_PAUSED: &str = "urn:kb:errors:trails-paused";
pub const ERR_SUB_STEP: &str = "urn:kb:errors:trails-sub-step";
pub const ERR_FULL: &str = "urn:kb:errors:trail-full";

fn now_unix() -> i64 {
    chrono::Utc::now().timestamp()
}

/// The two-gate check every write runs. `Ok(())` only when the operator
/// enabled the feature AND the persisted mode is `recording`.
fn admit_write(enabled: bool, mode: &str) -> Result<(), ApiError> {
    if !enabled {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "trails are disabled on this daemon — set `[trails] enabled = true` in \
             kb-code.toml and restart. kbc-trail/1 is OFF on first boot by design \
             (D17): a record of where a person looked is opt-in, never a default.",
        )
        .with_problem_type(ERR_DISABLED));
    }
    match mode {
        MODE_RECORDING => Ok(()),
        MODE_PAUSED => Err(ApiError::new(
            StatusCode::CONFLICT,
            "trail recording is PAUSED — nothing is being recorded. Resume with \
             `kb-code trail state --mode recording` (or the TopBar indicator).",
        )
        .with_problem_type(ERR_PAUSED)),
        _ => Err(ApiError::new(
            StatusCode::CONFLICT,
            "trail recording has not been turned on — the feature is permitted but no \
             one has opted in on this volume. Start with `kb-code trail state --mode \
             recording` (or the TopBar indicator).",
        )
        .with_problem_type(ERR_OFF)),
    }
}

// --- state -----------------------------------------------------------------

#[derive(Debug, serde::Deserialize)]
pub struct StateParams {}

pub fn state_params_accept_without(_omit: &str) -> bool {
    serde_json::from_value::<StateParams>(serde_json::Value::Object(serde_json::Map::new())).is_ok()
}

fn state_out(
    cfg: &crate::config::TrailsSection,
    stored: Option<(String, i64)>,
    mutable: bool,
) -> StateOut {
    let (mode, changed) = match stored {
        Some((m, c)) => (mode_static(&m), Some(c)),
        None => (MODE_OFF, None),
    };
    // A mode a disabled daemon happens to still have stored is reported as
    // `off`: the master switch wins, and an indicator that said "recording"
    // while nothing was being recorded would be the one lie this whole
    // surface exists to prevent.
    let mode = if cfg.enabled { mode } else { MODE_OFF };
    let mut notes = vec![
        "the agent-facing read is GET /api/trails/aggregate — counts per file/symbol, \
         never per-line spans and never an ordering below the day"
            .to_string(),
        "no trail number is a ranking term, a filter default or a gate".to_string(),
    ];
    if !cfg.enabled {
        notes.push(
            "`[trails] enabled` is false on this daemon — nothing is recorded and the mode \
             cannot be changed"
                .to_string(),
        );
    }
    if cfg.retention_days == 0 {
        notes.push(
            "retention_days = 0 — the background sweep is not running and nothing ever \
             expires; purge is the only removal"
                .to_string(),
        );
    }
    StateOut {
        schema: SCHEMA,
        enabled: cfg.enabled,
        mode,
        modes_available: MODES.to_vec(),
        retention_days: cfg.retention_days,
        step_granularity_secs: cfg.step_granularity_secs.max(1),
        changed_unix: changed,
        mutable,
        notes,
    }
}

/// `GET /api/trails/state` — the indicator's source of truth.
///
/// Reads `ConnectInfo` the same way `routes::repos` and
/// `actions::actions_route` do, for the same reason and with the same
/// caveat: this handler has to answer "can *I*, this caller, change the
/// mode", and only the SERVER can decide that (`mutable`). It is a
/// report, never a gate — the mode-changing route has its own
/// loopback-only middleware and does not consult this value.
pub async fn get_state(
    State(state): State<SharedState>,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    headers: axum::http::HeaderMap,
    Query(_params): Query<StateParams>,
) -> Result<impl IntoResponse, ApiError> {
    let loopback = kb_server::middleware::is_loopback_origin(
        Some(peer.ip()),
        &headers,
        &state.auth.trusted_proxies,
    );
    let cfg = state.trails.clone();
    let stored = state
        .store
        .run_blocking(move |store| Ok::<_, ApiError>(store.trails_state()?))
        .await?;
    let out = state_out(&cfg, stored, cfg.enabled && loopback);
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(out)))
}

#[derive(Debug, serde::Deserialize)]
pub struct SetStateIn {
    /// One of [`MODES`].
    pub mode: String,
}

/// `POST /api/trails/state` — loopback-only and audited (`audit_mutations`
/// records every attempt with its outcome, invariant 1). The ONLY writer of
/// the opt-in row.
pub async fn set_state(
    State(state): State<SharedState>,
    Json(body): Json<SetStateIn>,
) -> Result<impl IntoResponse, ApiError> {
    if !is_valid_mode(&body.mode) {
        return Err(ApiError::bad_request(format!(
            "unknown mode {:?} — expected one of {}",
            body.mode,
            MODES.join(", ")
        )));
    }
    let cfg = state.trails.clone();
    if !cfg.enabled {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "trails are disabled on this daemon — the mode cannot be changed while \
             `[trails] enabled` is false",
        )
        .with_problem_type(ERR_DISABLED));
    }
    let mode = body.mode.clone();
    let now = now_unix();
    let stored = state
        .store
        .run_blocking(move |store| {
            store.set_trails_state(&mode, now)?;
            Ok::<_, ApiError>(store.trails_state()?)
        })
        .await?;
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(state_out(&cfg, stored, true)),
    ))
}

// --- ingest ----------------------------------------------------------------

#[derive(Debug, serde::Serialize)]
pub struct StepsOut {
    pub schema: &'static str,
    pub trail_id: String,
    pub appended: usize,
    pub total: i64,
    /// The granularity every dwell in this batch was floored to — echoed so
    /// a client never has to guess why its 400 ms hop recorded 0.
    pub step_granularity_secs: i64,
    pub notes: Vec<String>,
}

/// `POST /api/trails/steps` — loopback-only, batched.
pub async fn ingest_steps(
    State(state): State<SharedState>,
    raw: String,
) -> Result<impl IntoResponse, ApiError> {
    if raw.len() > MAX_INGEST_BYTES {
        return Err(ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            format!(
                "trail batch is {} bytes; the cap is {MAX_INGEST_BYTES} — refused, not \
                 truncated",
                raw.len()
            ),
        ));
    }
    let value: serde_json::Value = serde_json::from_str(&raw)
        .map_err(|e| ApiError::bad_request(format!("trail batch is not JSON: {e}")))?;
    // The SUB-STEP refusal runs on the raw JSON, BEFORE the typed parse, so
    // the message names the RULE rather than serde's "unknown field" (the
    // `boards::lint::precheck_raw` coordinate precedent, same technique for
    // the same teaching reason).
    reject_sub_step_keys(&value)
        .map_err(|m| ApiError::bad_request(m).with_problem_type(ERR_SUB_STEP))?;
    let body: StepsIn = serde_json::from_value(value)
        .map_err(|e| ApiError::bad_request(format!("trail batch does not parse: {e}")))?;

    let cfg = state.trails.clone();
    let (_repo, repo_id) = find_repo(&state, &body.repo)?;
    if body.steps.is_empty() {
        return Err(ApiError::bad_request("a trail batch carries no steps"));
    }
    if body.steps.len() > MAX_STEPS_PER_BATCH {
        return Err(ApiError::bad_request(format!(
            "a trail batch carries {} steps; the cap is {MAX_STEPS_PER_BATCH} — refused, \
             not truncated",
            body.steps.len()
        )));
    }
    for (i, s) in body.steps.iter().enumerate() {
        validate_step(i, s)?;
    }

    let granularity = cfg.step_granularity_secs.max(1);
    let now = now_unix();
    let session_hint = clamp_label(body.session_hint.as_deref());
    let steps: Vec<NewTrailStep> = body
        .steps
        .iter()
        .map(|s| NewTrailStep {
            via: s.via.clone(),
            path: s.path.clone(),
            line_start: s.line_start,
            line_end: s.line_end,
            symbol: clamp_label(s.symbol.as_deref()),
            blob_sha: s.blob_sha.clone(),
            entered_at: s.entered_at,
            dwell_secs: quantise_dwell(s.entered_at, s.left_at, granularity),
            day: day_of(s.entered_at),
            note: clamp_label(s.note.as_deref()),
        })
        .collect();
    let day = day_of(now);

    let out = state
        .store
        .run_blocking(move |store| {
            let stored = store.trails_state()?;
            let mode = stored
                .as_ref()
                .map(|(m, _)| mode_static(m))
                .unwrap_or(MODE_OFF);
            admit_write(cfg.enabled, mode)?;
            let trail_id =
                store.current_trail_for_day(repo_id, &day, session_hint.as_deref(), now)?;
            let existing = store.trail_step_count(&trail_id)?;
            if existing as usize + steps.len() > MAX_STEPS_PER_TRAIL {
                return Err(ApiError::new(
                    StatusCode::CONFLICT,
                    format!(
                        "trail {trail_id} holds {existing} steps and this batch would take \
                         it past the {MAX_STEPS_PER_TRAIL} cap — refused, not truncated. \
                         Fork the trail, or purge."
                    ),
                )
                .with_problem_type(ERR_FULL));
            }
            let (appended, total) = store.append_trail_steps(&trail_id, &steps, now)?;
            Ok::<_, ApiError>(StepsOut {
                schema: SCHEMA,
                trail_id,
                appended,
                total,
                step_granularity_secs: granularity,
                notes: vec![format!(
                    "dwell is derived from entered_at/left_at and FLOORED to \
                     {granularity}s — this daemon records nothing finer than a step"
                )],
            })
        })
        .await?;
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(out)))
}

fn clamp_label(s: Option<&str>) -> Option<String> {
    s.map(|v| v.chars().take(MAX_LABEL_LEN).collect::<String>())
        .filter(|v| !v.is_empty())
}

fn validate_step(i: usize, s: &StepIn) -> Result<(), ApiError> {
    if !is_valid_via(&s.via) {
        return Err(ApiError::bad_request(format!(
            "step {i}: unknown via {:?} — expected one of {}",
            s.via,
            VIA_KINDS.join(", ")
        )));
    }
    if let Some(p) = &s.path {
        crate::routes::safe_rel_path(p)
            .map_err(|e| ApiError::bad_request(format!("step {i}: path {p:?}: {}", e.message())))?;
    }
    if let (Some(a), Some(b)) = (s.line_start, s.line_end) {
        if a > b {
            return Err(ApiError::bad_request(format!(
                "step {i}: line_start {a} is after line_end {b}"
            )));
        }
    }
    if s.entered_at <= 0 {
        return Err(ApiError::bad_request(format!(
            "step {i}: entered_at must be a positive unix second"
        )));
    }
    Ok(())
}

// --- authored trails + fork -------------------------------------------------

#[derive(Debug, serde::Serialize)]
pub struct TrailCreatedOut {
    pub schema: &'static str,
    pub id: String,
    pub origin: &'static str,
    pub steps: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_ordinal: Option<i64>,
    pub notes: Vec<String>,
}

/// `POST /api/trails` — an AUTHORED trail (D12: "an agent lays a path, the
/// human walks `]`/`[` and dissents inline").
///
/// An authored trail is NOT gated on the recording mode: it records nothing
/// about the operator, it is content an agent wrote for them. It IS gated
/// on `[trails] enabled`, because a daemon whose operator turned the whole
/// family off should not grow trail rows by another door.
pub async fn create_authored(
    State(state): State<SharedState>,
    raw: String,
) -> Result<impl IntoResponse, ApiError> {
    if raw.len() > MAX_INGEST_BYTES {
        return Err(ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            format!(
                "trail document is {} bytes; the cap is {MAX_INGEST_BYTES}",
                raw.len()
            ),
        ));
    }
    let value: serde_json::Value = serde_json::from_str(&raw)
        .map_err(|e| ApiError::bad_request(format!("trail document is not JSON: {e}")))?;
    reject_sub_step_keys(&value)
        .map_err(|m| ApiError::bad_request(m).with_problem_type(ERR_SUB_STEP))?;
    let body: TrailIn = serde_json::from_value(value)
        .map_err(|e| ApiError::bad_request(format!("trail document does not parse: {e}")))?;
    if body.schema != SCHEMA {
        return Err(ApiError::bad_request(format!(
            "schema must be {SCHEMA:?}, not {:?}",
            body.schema
        )));
    }
    if !body.authored {
        return Err(ApiError::bad_request(
            "POST /api/trails creates an AUTHORED trail and the document must say so \
             (`\"authored\": true`). A RECORDED trail is minted by the daemon from \
             POST /api/trails/steps — it is never authored.",
        ));
    }
    if body.steps.is_empty() {
        return Err(ApiError::bad_request("an authored trail carries no steps"));
    }
    if body.steps.len() > MAX_AUTHORED_STEPS {
        return Err(ApiError::bad_request(format!(
            "an authored trail carries {} steps; the cap is {MAX_AUTHORED_STEPS} — \
             refused, not truncated",
            body.steps.len()
        )));
    }
    for (i, s) in body.steps.iter().enumerate() {
        validate_step(i, s)?;
    }
    let cfg = state.trails.clone();
    if !cfg.enabled {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "trails are disabled on this daemon — set `[trails] enabled = true` to author one",
        )
        .with_problem_type(ERR_DISABLED));
    }
    let (_repo, repo_id) = find_repo(&state, &body.repo)?;
    let granularity = cfg.step_granularity_secs.max(1);
    let now = now_unix();
    let title = clamp_label(body.title.as_deref());
    let steps: Vec<NewTrailStep> = body
        .steps
        .iter()
        .map(|s| NewTrailStep {
            via: s.via.clone(),
            path: s.path.clone(),
            line_start: s.line_start,
            line_end: s.line_end,
            symbol: clamp_label(s.symbol.as_deref()),
            blob_sha: s.blob_sha.clone(),
            entered_at: s.entered_at,
            dwell_secs: quantise_dwell(s.entered_at, s.left_at, granularity),
            day: day_of(s.entered_at),
            note: clamp_label(s.note.as_deref()),
        })
        .collect();
    let count = steps.len();
    let id = state
        .store
        .run_blocking(move |store| {
            let id = store.create_trail(
                repo_id,
                &NewTrail {
                    origin: ORIGIN_AUTHORED.to_string(),
                    title,
                    parent_id: None,
                    parent_ordinal: None,
                    session_hint: None,
                },
                now,
            )?;
            store.append_trail_steps(&id, &steps, now)?;
            Ok::<_, ApiError>(id)
        })
        .await?;
    Ok((
        StatusCode::CREATED,
        [(header::CACHE_CONTROL, "no-store")],
        Json(TrailCreatedOut {
            schema: SCHEMA,
            id,
            origin: ORIGIN_AUTHORED,
            steps: count,
            parent_id: None,
            parent_ordinal: None,
            notes: vec![
                "the human's dissent on this trail lands as ordinary annotations carrying \
                 its id; `kb-code trail notes <id>` drains them"
                    .to_string(),
            ],
        }),
    ))
}

#[derive(Debug, serde::Deserialize)]
pub struct ForkIn {
    pub repo: String,
    /// The step to branch AT. Every step from here on is copied into the
    /// new trail, so the fork reads as "I was here, and then I went another
    /// way" rather than as an empty trail with a pointer.
    #[serde(default)]
    pub from_ordinal: i64,
    #[serde(default)]
    pub title: Option<String>,
}

/// `POST /api/trails/{id}/fork` — the fork chips' route.
pub async fn fork_trail(
    State(state): State<SharedState>,
    AxumPath(id): AxumPath<String>,
    Json(body): Json<ForkIn>,
) -> Result<impl IntoResponse, ApiError> {
    let cfg = state.trails.clone();
    if !cfg.enabled {
        return Err(
            ApiError::new(StatusCode::FORBIDDEN, "trails are disabled on this daemon")
                .with_problem_type(ERR_DISABLED),
        );
    }
    let (_repo, repo_id) = find_repo(&state, &body.repo)?;
    let from = body.from_ordinal.max(0);
    let title = clamp_label(body.title.as_deref());
    let now = now_unix();
    let parent = id.clone();
    let out = state
        .store
        .run_blocking(move |store| {
            let Some(src) = store.get_trail(repo_id, &parent)? else {
                return Err(ApiError::not_found(format!(
                    "no trail {parent} in this repo"
                )));
            };
            let carried = store.trail_steps(&parent, from, MAX_STEPS_PER_TRAIL)?;
            if carried.is_empty() {
                return Err(ApiError::bad_request(format!(
                    "trail {parent} has no step at or after ordinal {from} (it holds {} \
                     steps) — there is nothing to fork from",
                    src.steps
                )));
            }
            let new_id = store.create_trail(
                repo_id,
                &NewTrail {
                    origin: src.origin.clone(),
                    title,
                    parent_id: Some(parent.clone()),
                    parent_ordinal: Some(from),
                    session_hint: None,
                },
                now,
            )?;
            let steps: Vec<NewTrailStep> = carried
                .iter()
                .map(|s| NewTrailStep {
                    via: s.via.clone(),
                    path: s.path.clone(),
                    line_start: s.line_start,
                    line_end: s.line_end,
                    symbol: s.symbol.clone(),
                    blob_sha: s.blob_sha.clone(),
                    entered_at: s.entered_at,
                    dwell_secs: s.dwell_secs,
                    day: s.day.clone(),
                    note: s.note.clone(),
                })
                .collect();
            let n = steps.len();
            store.append_trail_steps(&new_id, &steps, now)?;
            Ok::<_, ApiError>(TrailCreatedOut {
                schema: SCHEMA,
                id: new_id,
                origin: if src.origin == ORIGIN_AUTHORED {
                    ORIGIN_AUTHORED
                } else {
                    ORIGIN_RECORDED
                },
                steps: n,
                parent_id: Some(parent),
                parent_ordinal: Some(from),
                notes: vec![
                    "a fork carries a copy of the steps from its branch point — the parent \
                     is unchanged, and either may be purged without the other"
                        .to_string(),
                ],
            })
        })
        .await?;
    Ok((
        StatusCode::CREATED,
        [(header::CACHE_CONTROL, "no-store")],
        Json(out),
    ))
}

// --- reads -----------------------------------------------------------------

#[derive(Debug, serde::Deserialize)]
pub struct ListParams {
    pub repo: String,
    #[serde(default)]
    pub limit: Option<usize>,
}

pub fn list_params_accept_without(omit: &str) -> bool {
    accepts_without::<ListParams>(
        &[("repo", "r".into()), ("limit", serde_json::json!(10))],
        omit,
    )
}

#[derive(Debug, serde::Serialize)]
pub struct TrailSummaryOut {
    pub id: String,
    pub origin: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub day: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_ordinal: Option<i64>,
    /// NOT `steps`: [`TrailOut`] FLATTENS this struct beside its own
    /// `steps` ARRAY, and two keys of the same name would make the count
    /// unreachable on the wire (every JSON parser keeps the last
    /// duplicate — the exact defect `boards::resolve::NodeOut::query`
    /// records for `query_card`). One name, one meaning, both surfaces.
    pub step_count: i64,
    pub dwell_secs: i64,
    pub created_unix: i64,
    pub updated_unix: i64,
}

#[derive(Debug, serde::Serialize)]
pub struct ListOut {
    pub schema: &'static str,
    pub repo: String,
    /// The state, echoed on the list so a caller with an empty list can
    /// tell "nothing recorded" from "recording is off" without a second
    /// request. This is what makes the ledger-off and ledger-empty reads
    /// distinguishable to a HUMAN while staying identical in every other
    /// field.
    pub enabled: bool,
    pub mode: &'static str,
    pub origins_available: Vec<&'static str>,
    pub trails: Vec<TrailSummaryOut>,
    pub notes: Vec<String>,
}

/// `GET /api/trails?repo=[&limit=]` — the operator's own read.
/// LOOPBACK-ONLY (see the module doc).
pub async fn list_trails(
    State(state): State<SharedState>,
    Query(params): Query<ListParams>,
) -> Result<impl IntoResponse, ApiError> {
    let (repo, repo_id) = find_repo(&state, &params.repo)?;
    let repo_name = repo.name.clone();
    let cfg = state.trails.clone();
    let limit = params
        .limit
        .unwrap_or(DEFAULT_LIST_LIMIT)
        .clamp(1, MAX_LIST_LIMIT);
    let (rows, stored) = state
        .store
        .run_blocking(move |store| {
            Ok::<_, ApiError>((store.list_trails(repo_id, limit)?, store.trails_state()?))
        })
        .await?;
    let mode = if cfg.enabled {
        stored
            .as_ref()
            .map(|(m, _)| mode_static(m))
            .unwrap_or(MODE_OFF)
    } else {
        MODE_OFF
    };
    let mut notes = Vec::new();
    if rows.is_empty() && mode != MODE_RECORDING {
        notes.push(
            "this list is empty AND recording is not on — the two are different states, \
             and `mode` above says which one you are looking at"
                .to_string(),
        );
    }
    let out = ListOut {
        schema: SCHEMA,
        repo: repo_name,
        enabled: cfg.enabled,
        mode,
        origins_available: ORIGINS.to_vec(),
        trails: rows.into_iter().map(summary_out).collect(),
        notes,
    };
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(out)))
}

fn summary_out(r: crate::store::TrailSummaryRow) -> TrailSummaryOut {
    TrailSummaryOut {
        id: r.id,
        origin: r.origin,
        title: r.title,
        day: r.day,
        parent_id: r.parent_id,
        parent_ordinal: r.parent_ordinal,
        step_count: r.steps,
        dwell_secs: r.dwell_secs,
        created_unix: r.created_unix,
        updated_unix: r.updated_unix,
    }
}

#[derive(Debug, serde::Deserialize)]
pub struct GetParams {
    pub repo: String,
    /// Start at this ordinal — the fork chip's own read.
    #[serde(default)]
    pub from: Option<i64>,
    /// Include the trail's dissent notes.
    #[serde(default)]
    pub notes: Option<String>,
}

pub fn get_params_accept_without(omit: &str) -> bool {
    accepts_without::<GetParams>(
        &[
            ("repo", "r".into()),
            ("from", serde_json::json!(0)),
            ("notes", "1".into()),
        ],
        omit,
    )
}

#[derive(Debug, serde::Serialize)]
pub struct StepOut {
    pub ordinal: i64,
    pub via: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line_start: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line_end: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    /// The blob the operator was looking at — the Ladder's witness.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blob_sha: Option<String>,
    /// `pinned` | `carried` | `orphan` | `inert`, computed PER REQUEST by
    /// [`step_state`] and stored nowhere (invariant 24(a)).
    pub state: &'static str,
    pub dwell_secs: i64,
    /// The DAY, never the second — even on the operator's own read, because
    /// a per-step wall-clock is the one thing nothing downstream needs.
    pub day: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

pub const STEP_PINNED: &str = "pinned";
pub const STEP_CARRIED: &str = "carried";
pub const STEP_ORPHAN: &str = "orphan";
pub const STEP_INERT: &str = "inert";

/// The Ladder verdict for one trail step, computed per request.
///
/// This is deliberately the SHALLOW end of the ladder that
/// `boards::resolve` walks in full: a trail step is a place someone went,
/// not an anchored claim about specific bytes, so re-anchoring a line into
/// a changed blob would be inventing precision the record never had. The
/// three states are therefore: the blob is still the blob (`pinned`), the
/// file is still there under different bytes (`carried`), the path is gone
/// (`orphan`), or there was no path at all (`inert`).
pub fn step_state(
    path: Option<&str>,
    authored: Option<&str>,
    current: Option<&str>,
) -> &'static str {
    match (path, authored, current) {
        (None, _, _) => STEP_INERT,
        (Some(_), _, None) => STEP_ORPHAN,
        (Some(_), None, Some(_)) => STEP_CARRIED,
        (Some(_), Some(a), Some(c)) => {
            if c.starts_with(a) || a.starts_with(c) {
                STEP_PINNED
            } else {
                STEP_CARRIED
            }
        }
    }
}

#[derive(Debug, serde::Serialize)]
pub struct NoteOut {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    pub author: String,
    pub intent: String,
    pub body: String,
    pub path: String,
    pub resolved: bool,
    pub created_at: i64,
}

#[derive(Debug, serde::Serialize)]
pub struct TrailOut {
    pub schema: &'static str,
    pub repo: String,
    #[serde(flatten)]
    pub trail: TrailSummaryOut,
    pub steps: Vec<StepOut>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes_list: Option<Vec<NoteOut>>,
    pub notes: Vec<String>,
}

/// `GET /api/trails/{id}?repo=` — the operator's own read, steps on the
/// Ladder. LOOPBACK-ONLY.
pub async fn get_trail(
    State(state): State<SharedState>,
    AxumPath(id): AxumPath<String>,
    Query(params): Query<GetParams>,
) -> Result<impl IntoResponse, ApiError> {
    let (repo, repo_id) = find_repo(&state, &params.repo)?;
    let repo_name = repo.name.clone();
    let repo_root = repo.path.clone();
    let from = params.from.unwrap_or(0).max(0);
    let want_notes = params
        .notes
        .as_deref()
        .is_some_and(|v| v == "1" || v == "true");
    let out = state
        .store
        .run_blocking(move |store| {
            let Some(row) = store.get_trail(repo_id, &id)? else {
                return Err(ApiError::not_found(format!("no trail {id} in this repo")));
            };
            let steps = store.trail_steps(&id, from, MAX_STEPS_PER_TRAIL)?;
            let notes_list = if want_notes {
                Some(
                    store
                        .trail_notes(&id, 500)?
                        .into_iter()
                        .map(|n| NoteOut {
                            id: n.id,
                            parent_id: n.parent_id,
                            author: n.author,
                            intent: n.intent,
                            body: n.body,
                            path: n.path,
                            resolved: n.resolved,
                            created_at: n.created_at,
                        })
                        .collect(),
                )
            } else {
                None
            };
            let mut blob_cache: std::collections::HashMap<String, Option<String>> =
                std::collections::HashMap::new();
            let steps: Vec<StepOut> = steps
                .into_iter()
                .map(|s| {
                    let current = s.path.as_deref().map(|p| {
                        blob_cache
                            .entry(p.to_string())
                            .or_insert_with(|| current_blob(&repo_root, p))
                            .clone()
                    });
                    let state = step_state(
                        s.path.as_deref(),
                        s.blob_sha.as_deref(),
                        current.flatten().as_deref(),
                    );
                    StepOut {
                        ordinal: s.ordinal,
                        via: s.via,
                        path: s.path,
                        line_start: s.line_start,
                        line_end: s.line_end,
                        symbol: s.symbol,
                        blob_sha: s.blob_sha,
                        state,
                        dwell_secs: s.dwell_secs,
                        day: s.day,
                        note: s.note,
                    }
                })
                .collect();
            let mut notes = vec![
                "a step's state is computed on THIS read and stored nowhere; a step whose \
                 file moved is `carried`, and its line number is not re-anchored — a trail \
                 records where someone went, not a claim about bytes"
                    .to_string(),
            ];
            if row.origin == ORIGIN_AUTHORED {
                notes.push(
                    "an AUTHORED trail is a path an agent laid down — walk it with ]/[ and \
                     dissent inline; the notes are ordinary annotations carrying this id"
                        .to_string(),
                );
            }
            Ok::<_, ApiError>(TrailOut {
                schema: SCHEMA,
                repo: repo_name,
                trail: summary_out(row),
                steps,
                notes_list,
                notes,
            })
        })
        .await?;
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(out)))
}

/// The blob a path has on disk right now — a plain content hash of the
/// working-tree bytes, through the SAME `ingest::git_blob_hash` the mirror
/// index uses, so "the blob moved" means the same thing here as everywhere
/// else in this crate.
fn current_blob(repo_root: &std::path::Path, rel: &str) -> Option<String> {
    let abs = crate::security::paths::contained_abs_path(repo_root, rel).ok()?;
    let bytes = std::fs::read(abs).ok()?;
    Some(crate::ingest::git_blob_hash(&bytes))
}

// --- aggregate (the ONLY agent-facing read) --------------------------------

#[derive(Debug, serde::Deserialize)]
pub struct AggregateParams {
    pub repo: String,
    /// An inclusive DAY bound, `YYYY-MM-DD`. A day, not a timestamp — the
    /// window this read accepts is the same resolution as the window it
    /// answers in.
    #[serde(default)]
    pub since: Option<String>,
    #[serde(default)]
    pub until: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
}

pub fn aggregate_params_accept_without(omit: &str) -> bool {
    accepts_without::<AggregateParams>(
        &[
            ("repo", "r".into()),
            ("since", "2026-09-01".into()),
            ("until", "2026-09-30".into()),
            ("limit", serde_json::json!(10)),
        ],
        omit,
    )
}

#[derive(Debug, serde::Serialize)]
pub struct AggregateOut {
    pub schema: &'static str,
    pub repo: String,
    pub enabled: bool,
    pub rows: Vec<AggregateRow>,
    pub truncated: bool,
    pub notes: Vec<String>,
}

/// `GET /api/trails/aggregate` — D17's one agent-facing surface.
pub async fn aggregate_trails(
    State(state): State<SharedState>,
    Query(params): Query<AggregateParams>,
) -> Result<impl IntoResponse, ApiError> {
    let (repo, repo_id) = find_repo(&state, &params.repo)?;
    let repo_name = repo.name.clone();
    let cfg = state.trails.clone();
    for (name, v) in [("since", &params.since), ("until", &params.until)] {
        if let Some(day) = v {
            if !is_day(day) {
                return Err(ApiError::bad_request(format!(
                    "{name}={day:?} is not a day — this read's window is YYYY-MM-DD, \
                     because its answers carry no time finer than a day either"
                )));
            }
        }
    }
    let limit = params
        .limit
        .unwrap_or(MAX_AGGREGATE_ROWS)
        .clamp(1, MAX_AGGREGATE_ROWS);
    let since = params.since.clone();
    let until = params.until.clone();
    let rows = state
        .store
        .run_blocking(move |store| {
            Ok::<_, ApiError>(store.aggregate_trail_steps(
                repo_id,
                since.as_deref(),
                until.as_deref(),
                limit + 1,
            )?)
        })
        .await?;
    let truncated = rows.len() > limit;
    let out = AggregateOut {
        schema: SCHEMA,
        repo: repo_name,
        enabled: cfg.enabled,
        rows: rows
            .into_iter()
            .take(limit)
            .map(|r| AggregateRow {
                path: r.path,
                symbol: r.symbol,
                steps: r.steps,
                dwell_secs: r.dwell_secs,
                days: r.days,
                first_day: r.first_day,
                last_day: r.last_day,
            })
            .collect(),
        truncated,
        notes: vec![
            "counts per file/symbol over whole DAYS — kbc-trail/1 exposes no per-line \
             span, no step timestamp and no ordering below the day (design D17)"
                .to_string(),
            "attention is not comprehension, and no number here is a score, a gate or a \
             ranking term"
                .to_string(),
        ],
    };
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(out)))
}

/// `YYYY-MM-DD`, shape only — the aggregate's window grammar.
pub fn is_day(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && b.iter()
            .enumerate()
            .all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit())
}

// --- purge ------------------------------------------------------------------

#[derive(Debug, serde::Deserialize)]
pub struct PurgeIn {
    pub repo: String,
    /// Purge only trails created strictly BEFORE this unix second; absent =
    /// everything. The retention sweep's own cutoff, available by hand.
    #[serde(default)]
    pub before: Option<i64>,
    /// One trail instead of the repo's whole ledger.
    #[serde(default)]
    pub id: Option<String>,
}

#[derive(Debug, serde::Serialize)]
pub struct PurgeOut {
    pub schema: &'static str,
    pub repo: String,
    pub trails: usize,
    pub steps: usize,
    pub notes: Vec<String>,
}

/// `POST /api/trails/purge` — loopback-only and audited. WHOLESALE by
/// default: the point of a purge is that it leaves nothing behind, so the
/// unqualified form takes the repo's entire ledger.
///
/// Deliberately NOT gated on `[trails] enabled`: an operator who has just
/// turned the feature off must still be able to delete what it recorded
/// while it was on. A delete route that only worked while the thing it
/// deletes is enabled would be the wrong shape of safety.
pub async fn purge_trails(
    State(state): State<SharedState>,
    Json(body): Json<PurgeIn>,
) -> Result<impl IntoResponse, ApiError> {
    let (repo, repo_id) = find_repo(&state, &body.repo)?;
    let repo_name = repo.name.clone();
    let before = body.before;
    let one = body.id.clone();
    let out = state
        .store
        .run_blocking(move |store| {
            let (trails, steps) = match &one {
                Some(id) => {
                    let n = store.get_trail(repo_id, id)?.map(|t| t.steps).unwrap_or(0);
                    if store.delete_trail(repo_id, id)? {
                        (1, n as usize)
                    } else {
                        return Err(ApiError::not_found(format!("no trail {id} in this repo")));
                    }
                }
                None => store.purge_trails(repo_id, before)?,
            };
            Ok::<_, ApiError>(PurgeOut {
                schema: SCHEMA,
                repo: repo_name,
                trails,
                steps,
                notes: vec![
                    "a purge removes trails and their steps — the movement record. Dissent \
                     notes on an authored trail are ordinary annotations and SURVIVE: they \
                     are the human's own words, not derived data (invariant 23(a))."
                        .to_string(),
                ],
            })
        })
        .await?;
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(out)))
}

// --- shared -----------------------------------------------------------------

/// The `params_accept_without` body every contract in this module shares:
/// deserialize the route's OWN params struct with one field removed.
/// The values are `serde_json::Value`s rather than strings because a
/// NUMERIC param (`limit`, `budget`, `from`) does not deserialize from a
/// JSON string — a helper that always sent strings would report every such
/// contract as rejecting its own COMPLETE query map, which is the shape
/// invariant 15's walk asserts against.
fn accepts_without<T: serde::de::DeserializeOwned>(
    fields: &[(&str, serde_json::Value)],
    omit: &str,
) -> bool {
    let mut map = serde_json::Map::new();
    for (k, v) in fields {
        if *k != omit {
            map.insert(k.to_string(), v.clone());
        }
    }
    serde_json::from_value::<T>(serde_json::Value::Object(map)).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(enabled: bool) -> crate::config::TrailsSection {
        crate::config::TrailsSection {
            enabled,
            retention_days: 30,
            step_granularity_secs: 1,
        }
    }

    #[test]
    fn the_write_gate_names_which_of_the_three_states_refused() {
        let disabled = admit_write(false, MODE_RECORDING).expect_err("disabled refuses");
        assert!(disabled.message().contains("disabled"), "{disabled:?}");
        let off = admit_write(true, MODE_OFF).expect_err("off refuses");
        assert!(off.message().contains("not been turned on"), "{off:?}");
        let paused = admit_write(true, MODE_PAUSED).expect_err("paused refuses");
        assert!(paused.message().contains("PAUSED"), "{paused:?}");
        // Three DISTINCT refusals — "nothing is being recorded" has three
        // different fixes and a generic 403 names none of them.
        assert_ne!(disabled.message(), off.message());
        assert_ne!(off.message(), paused.message());
        admit_write(true, MODE_RECORDING).expect("the one admitting combination");
    }

    /// D17's byte-identical pin, in the shape this surface can actually
    /// hold it: the state read with the ledger OFF and the state read with
    /// the ledger ON-but-never-opted-into differ in exactly the fields that
    /// EXIST to say which one you are looking at (`enabled`, `mutable` and
    /// the honest note), and in nothing else. A reader can therefore never
    /// mistake "off" for "empty", and no OTHER field of any response is a
    /// function of whether the ledger is running.
    #[test]
    fn the_state_read_differs_from_ledger_off_to_on_only_where_it_must() {
        let off = serde_json::to_value(state_out(&cfg(false), None, false)).expect("json");
        let on = serde_json::to_value(state_out(&cfg(true), None, true)).expect("json");
        let off_obj = off.as_object().expect("object").clone();
        let on_obj = on.as_object().expect("object").clone();
        assert_eq!(
            off_obj.keys().collect::<Vec<_>>(),
            on_obj.keys().collect::<Vec<_>>(),
            "the two reads must have identical SHAPE"
        );
        let differing: Vec<&String> = off_obj
            .keys()
            .filter(|k| off_obj[*k] != on_obj[*k])
            .collect();
        assert_eq!(
            differing,
            vec!["enabled", "mutable", "notes"],
            "only the three fields whose whole job is reporting the opt-in may differ"
        );
        assert_eq!(off["mode"], "off");
        assert_eq!(
            on["mode"], "off",
            "enabling the feature must NOT start recording — D17's opt-in is off on \
             first boot, and the absence of a decision is not a decision to record"
        );
    }

    #[test]
    fn a_stored_recording_mode_still_reads_off_while_the_master_switch_is_false() {
        let out = state_out(&cfg(false), Some((MODE_RECORDING.into(), 99)), false);
        assert_eq!(
            out.mode, MODE_OFF,
            "an indicator that said `recording` while nothing was recorded would be the \
             one lie this surface exists to prevent"
        );
        assert!(!out.mutable);
    }

    #[test]
    fn a_step_state_never_re_anchors_and_never_guesses() {
        assert_eq!(step_state(None, Some("abc"), Some("abc")), STEP_INERT);
        assert_eq!(step_state(Some("a.rb"), Some("abc"), None), STEP_ORPHAN);
        assert_eq!(
            step_state(Some("a.rb"), Some("abc"), Some("abc")),
            STEP_PINNED
        );
        assert_eq!(
            step_state(Some("a.rb"), Some("abc"), Some("def")),
            STEP_CARRIED
        );
        assert_eq!(
            step_state(Some("a.rb"), None, Some("def")),
            STEP_CARRIED,
            "no witness ⇒ no pin claim, ever"
        );
        // An abbreviated sha on either side still matches — the same
        // prefix rule `review_doc::cards` applies to a `@sha` a human typed.
        assert_eq!(
            step_state(Some("a.rb"), Some("abc"), Some("abcdef")),
            STEP_PINNED
        );
    }

    #[test]
    fn the_aggregate_window_is_a_day_grammar() {
        assert!(is_day("2026-09-07"));
        assert!(!is_day("2026-9-7"));
        assert!(!is_day("2026-09-07T10:00:00Z"));
        assert!(!is_day(""));
        assert!(!is_day("1757000000"));
    }

    #[test]
    fn every_contract_requires_repo_except_the_state_read() {
        assert!(list_params_accept_without(""));
        assert!(!list_params_accept_without("repo"));
        assert!(get_params_accept_without(""));
        assert!(!get_params_accept_without("repo"));
        assert!(aggregate_params_accept_without(""));
        assert!(!aggregate_params_accept_without("repo"));
        // The state read is daemon-wide: it has no repo to require.
        assert!(state_params_accept_without(""));
    }
}
