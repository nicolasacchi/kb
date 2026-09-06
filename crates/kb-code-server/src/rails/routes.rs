//! `GET /api/rails/*` — the `rails/1` read surface: one passport, eight
//! noun lists, one orphan report.
//!
//! Ordinary `auth_bearer` browsing reads on the `api` sub-router, the same
//! tier as `/framework/edges` and `/entity`: everything here is derived
//! from data those routes already serve, nothing mutates, and no raw
//! transcript text is involved. Every handler makes exactly ONE hop to the
//! blocking pool (the 2026-08-31 starvation convention) with the whole join
//! and every controller source read inside it.
//!
//! Each response reports its own read state ([`super::Honesty`]) and, on a
//! list, the TRUE total beside what the page returned — a cap is named,
//! never silent.

use super::{
    build_index, orphans, Honesty, IndexInputs, LensFreshness, RailsIndex, RailsRow, ZeitwerkNote,
    DEFAULT_LIMIT, MAX_LIMIT, NOUNS, SCHEMA,
};
use crate::entities::RouteContract;
use crate::routes::{find_repo, ApiError};
use crate::state::SharedState;
use crate::store::StoreBlocking;
use axum::{extract::Query, extract::State, http::header, response::IntoResponse, Json};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

// --- params ------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct RailsRepoParams {
    pub repo: String,
}

#[derive(Debug, Deserialize)]
pub struct RailsListParams {
    pub repo: String,
    /// Case-insensitive substring over a row's name and path. Applied
    /// BEFORE `total` is counted, so `total` is the true post-filter size.
    pub q: Option<String>,
    pub limit: Option<usize>,
    pub offset: Option<usize>,
}

// --- wire --------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct RailsHomeOut {
    pub schema: &'static str,
    pub repo: String,
    pub detected: bool,
    /// The resolved Rails version, or `null` — honestly unknown, never a
    /// guessed default.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rails_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version_source: Option<&'static str>,
    /// TRUE totals per noun — the whole index, not a page of it.
    pub counts: BTreeMap<&'static str, usize>,
    pub nouns: Vec<&'static str>,
    pub lens: LensFreshness,
    pub zeitwerk: ZeitwerkNote,
    pub honesty: Honesty,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RailsListOut {
    pub schema: &'static str,
    pub repo: String,
    pub noun: &'static str,
    pub rows: Vec<RailsRow>,
    /// Rows matching the query across the whole index.
    pub total: usize,
    pub returned: usize,
    pub offset: usize,
    pub limit: usize,
    /// `true` when `total` exceeds what this page returned.
    pub truncated: bool,
    pub honesty: Honesty,
    pub notes: Vec<String>,
}

// --- contracts ---------------------------------------------------------

fn repo_params_accept_without(omit: &str) -> bool {
    let mut map = serde_json::Map::new();
    if omit != "repo" {
        map.insert("repo".to_string(), serde_json::Value::String("r".into()));
    }
    serde_json::from_value::<RailsRepoParams>(serde_json::Value::Object(map)).is_ok()
}

fn list_params_accept_without(omit: &str) -> bool {
    let mut map = serde_json::Map::new();
    if omit != "repo" {
        map.insert("repo".to_string(), serde_json::Value::String("r".into()));
    }
    serde_json::from_value::<RailsListParams>(serde_json::Value::Object(map)).is_ok()
}

macro_rules! list_contract {
    ($konst:ident, $path:literal, $handler:literal) => {
        pub const $konst: RouteContract = RouteContract {
            path: $path,
            handler: $handler,
            required_params: &["repo"],
            params_accept_without: list_params_accept_without,
        };
    };
}

pub const RAILS_HOME_ROUTE: RouteContract = RouteContract {
    path: "/api/rails/home",
    handler: "rails::routes::home_route",
    required_params: &["repo"],
    params_accept_without: repo_params_accept_without,
};

pub const RAILS_ORPHANS_ROUTE: RouteContract = RouteContract {
    path: "/api/rails/orphans",
    handler: "rails::routes::orphans_route",
    required_params: &["repo"],
    params_accept_without: repo_params_accept_without,
};

list_contract!(
    RAILS_MODELS_ROUTE,
    "/api/rails/models",
    "rails::routes::models_route"
);
list_contract!(
    RAILS_CONTROLLERS_ROUTE,
    "/api/rails/controllers",
    "rails::routes::controllers_route"
);
list_contract!(
    RAILS_ACTIONS_ROUTE,
    "/api/rails/actions",
    "rails::routes::actions_route"
);
list_contract!(
    RAILS_ROUTES_ROUTE,
    "/api/rails/routes",
    "rails::routes::routes_route"
);
list_contract!(
    RAILS_JOBS_ROUTE,
    "/api/rails/jobs",
    "rails::routes::jobs_route"
);
list_contract!(
    RAILS_MAILERS_ROUTE,
    "/api/rails/mailers",
    "rails::routes::mailers_route"
);
list_contract!(
    RAILS_VIEWS_ROUTE,
    "/api/rails/views",
    "rails::routes::views_route"
);
list_contract!(
    RAILS_CONCERNS_ROUTE,
    "/api/rails/concerns",
    "rails::routes::concerns_route"
);

/// Every route V72-I1 adds. Both dead-surface walks (server-side in
/// `entities`, CLI-side in kb-code-cli) read THIS list — a route added
/// without a row here is the one gap neither test can see, which is why it
/// lives beside the routes.
pub const V72_I1_ROUTES: &[RouteContract] = &[
    RAILS_HOME_ROUTE,
    RAILS_MODELS_ROUTE,
    RAILS_CONTROLLERS_ROUTE,
    RAILS_ACTIONS_ROUTE,
    RAILS_ROUTES_ROUTE,
    RAILS_JOBS_ROUTE,
    RAILS_MAILERS_ROUTE,
    RAILS_VIEWS_ROUTE,
    RAILS_CONCERNS_ROUTE,
    RAILS_ORPHANS_ROUTE,
];

// --- handlers ----------------------------------------------------------

async fn index_for(state: &SharedState, repo: &str) -> Result<(String, RailsIndex), ApiError> {
    let (entry, repo_id) = find_repo(state, repo)?;
    let repo_root = entry.path.clone();
    let repo_name = entry.name.clone();
    let idx = state
        .store
        .run_blocking(move |store| {
            build_index(
                store,
                IndexInputs {
                    repo_root: &repo_root,
                    repo_id,
                },
            )
        })
        .await?;
    Ok((repo_name, idx))
}

/// `GET /api/rails/home?repo=` — the `rails/1` passport.
pub async fn home_route(
    State(state): State<SharedState>,
    Query(params): Query<RailsRepoParams>,
) -> Result<impl IntoResponse, ApiError> {
    let (repo, idx) = index_for(&state, &params.repo).await?;
    let counts = idx.counts();
    let total: usize = counts.values().sum();
    let out = RailsHomeOut {
        schema: SCHEMA,
        repo,
        detected: idx.detected,
        rails_version: idx.rails_version.clone(),
        version_source: idx.version_source,
        counts,
        nouns: NOUNS.to_vec(),
        lens: idx.lens.clone(),
        zeitwerk: idx.zeitwerk.clone(),
        honesty: idx.honesty(total),
        notes: idx.notes.clone(),
    };
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(out)))
}

/// The shared body of all eight noun lists.
async fn list(
    state: SharedState,
    noun: &'static str,
    params: RailsListParams,
) -> Result<impl IntoResponse, ApiError> {
    let (repo, idx) = index_for(&state, &params.repo).await?;
    let limit = params.limit.unwrap_or(DEFAULT_LIMIT).min(MAX_LIMIT);
    let offset = params.offset.unwrap_or(0);
    let needle = params.q.as_deref().map(str::to_lowercase);

    let matched: Vec<&RailsRow> = idx
        .rows(noun)
        .iter()
        .filter(|r| match &needle {
            None => true,
            Some(n) => r.name.to_lowercase().contains(n) || r.path.to_lowercase().contains(n),
        })
        .collect();
    let total = matched.len();
    let rows: Vec<RailsRow> = matched
        .into_iter()
        .skip(offset)
        .take(limit)
        .cloned()
        .collect();
    let returned = rows.len();

    let mut notes = idx.notes.clone();
    if params.limit.is_some_and(|l| l > MAX_LIMIT) {
        notes.push(format!(
            "limit was clamped to {MAX_LIMIT}, the per-page ceiling"
        ));
    }
    let out = RailsListOut {
        schema: SCHEMA,
        repo,
        noun,
        rows,
        total,
        returned,
        offset,
        limit,
        truncated: offset + returned < total,
        honesty: idx.honesty(returned),
        notes,
    };
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(out)))
}

/// `GET /api/rails/models?repo=[&q=][&limit=][&offset=]`.
pub async fn models_route(
    State(state): State<SharedState>,
    Query(params): Query<RailsListParams>,
) -> Result<impl IntoResponse, ApiError> {
    list(state, "model", params).await
}

/// `GET /api/rails/controllers?repo=[&q=][&limit=][&offset=]`.
pub async fn controllers_route(
    State(state): State<SharedState>,
    Query(params): Query<RailsListParams>,
) -> Result<impl IntoResponse, ApiError> {
    list(state, "controller", params).await
}

/// `GET /api/rails/actions?repo=[&q=][&limit=][&offset=]`.
pub async fn actions_route(
    State(state): State<SharedState>,
    Query(params): Query<RailsListParams>,
) -> Result<impl IntoResponse, ApiError> {
    list(state, "action", params).await
}

/// `GET /api/rails/routes?repo=[&q=][&limit=][&offset=]`.
pub async fn routes_route(
    State(state): State<SharedState>,
    Query(params): Query<RailsListParams>,
) -> Result<impl IntoResponse, ApiError> {
    list(state, "route", params).await
}

/// `GET /api/rails/jobs?repo=[&q=][&limit=][&offset=]`.
pub async fn jobs_route(
    State(state): State<SharedState>,
    Query(params): Query<RailsListParams>,
) -> Result<impl IntoResponse, ApiError> {
    list(state, "job", params).await
}

/// `GET /api/rails/mailers?repo=[&q=][&limit=][&offset=]`.
pub async fn mailers_route(
    State(state): State<SharedState>,
    Query(params): Query<RailsListParams>,
) -> Result<impl IntoResponse, ApiError> {
    list(state, "mailer", params).await
}

/// `GET /api/rails/views?repo=[&q=][&limit=][&offset=]`.
pub async fn views_route(
    State(state): State<SharedState>,
    Query(params): Query<RailsListParams>,
) -> Result<impl IntoResponse, ApiError> {
    list(state, "view", params).await
}

/// `GET /api/rails/concerns?repo=[&q=][&limit=][&offset=]`.
pub async fn concerns_route(
    State(state): State<SharedState>,
    Query(params): Query<RailsListParams>,
) -> Result<impl IntoResponse, ApiError> {
    list(state, "concern", params).await
}

/// `GET /api/rails/orphans?repo=` — the triage queue.
pub async fn orphans_route(
    State(state): State<SharedState>,
    Query(params): Query<RailsRepoParams>,
) -> Result<impl IntoResponse, ApiError> {
    let (entry, repo_id) = find_repo(&state, &params.repo)?;
    let repo_root = entry.path.clone();
    let repo_name = entry.name.clone();
    // ONE hop: the join AND the locale-index read (which stats every
    // `config/locales` file behind the lens's own fingerprinted cache)
    // both belong on the blocking pool.
    let out = state
        .store
        .run_blocking(move |store| {
            let idx = build_index(
                store,
                IndexInputs {
                    repo_root: &repo_root,
                    repo_id,
                },
            )?;
            Ok::<_, ApiError>(orphans::build_report(&repo_name, &idx, &repo_root))
        })
        .await?;
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(out)))
}
