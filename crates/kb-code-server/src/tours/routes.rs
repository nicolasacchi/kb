//! `kbc-tour/1`'s HTTP surface (V74-L3b, D12 + D10).
//!
//! # The routes, and their gates
//!
//! | route | gate |
//! |---|---|
//! | `GET /api/tours?repo=[&status=]` | `auth_bearer` |
//! | `GET /api/tours/{slug}?repo=[&ctx=1]` | `auth_bearer` |
//! | `GET /api/tours/{slug}/pack?repo=[&budget=]` | `auth_bearer` |
//! | `GET /api/tours/{slug}/export?repo=&format=` | `auth_bearer` |
//! | `POST /api/tours/apply` | **loopback-only** |
//! | `DELETE /api/tours/{slug}?repo=` | **loopback-only** |
//!
//! Same gates, same sub-routers and the same reasoning as
//! `boards::routes` — a tour IS a board (see this module's parent doc), so
//! nothing about its admission posture is new.
//!
//! # `apply` is `/api/tours/apply`, not `POST /api/tours`
//!
//! Mirroring `POST /api/boards/apply` rather than the brief's `POST
//! /api/tours`, so the two families read identically in `router.rs`, in the
//! CLI and in the docs, and so `apply` is a RESERVED SLUG on both. A
//! recorded deviation, taken for symmetry.

use super::*;
use crate::boards::resolve::{self as bresolve, NodeOut};
use crate::routes::{find_repo, ApiError};
use crate::state::SharedState;
use crate::store::{
    CanvasNodeRow, NewCanvasBoard, NewCanvasEdge, NewCanvasNode, NewCanvasStep, StoreBlocking,
};
use axum::extract::{Path as AxumPath, Query, State};
use axum::http::{header, StatusCode};
use axum::response::IntoResponse;
use axum::Json;

/// Slugs shadowed by a literal route segment under `/api/tours`.
pub const RESERVED_SLUGS: [&str; 1] = ["apply"];

fn now_unix() -> i64 {
    chrono::Utc::now().timestamp()
}

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

// --- list -------------------------------------------------------------------

#[derive(Debug, serde::Deserialize)]
pub struct ListParams {
    pub repo: String,
    #[serde(default)]
    pub status: Option<String>,
}

pub fn list_params_accept_without(omit: &str) -> bool {
    accepts_without::<ListParams>(&[("repo", "r".into()), ("status", "pending".into())], omit)
}

#[derive(Debug, serde::Serialize)]
pub struct TourSummaryOut {
    pub slug: String,
    pub title: String,
    pub status: String,
    pub revision: i64,
    pub updated_unix: i64,
    pub steps: i64,
}

#[derive(Debug, serde::Serialize)]
pub struct ListOut {
    pub schema: &'static str,
    pub repo: String,
    pub statuses_available: Vec<&'static str>,
    pub tours: Vec<TourSummaryOut>,
}

/// `GET /api/tours?repo=[&status=]`.
pub async fn list_tours(
    State(state): State<SharedState>,
    Query(params): Query<ListParams>,
) -> Result<impl IntoResponse, ApiError> {
    let (repo, repo_id) = find_repo(&state, &params.repo)?;
    let repo_name = repo.name.clone();
    let status = match &params.status {
        Some(s) if !boards::is_valid_status(s) => {
            return Err(ApiError::bad_request(format!(
                "unknown status {s:?} — expected one of {}",
                boards::STATUSES.join(", ")
            )))
        }
        other => other.clone(),
    };
    let out = state
        .store
        .run_blocking(move |store| {
            // The `steps` count and the `nodes` count are the same number
            // for a tour BY CONSTRUCTION (its nodes ARE its steps), so only
            // one is reported — two fields that must agree is exactly the
            // drift this codebase keeps designing out.
            let rows = store.list_canvas_boards(repo_id, BOARD_KIND_TOUR, status.as_deref())?;
            Ok::<_, ApiError>(ListOut {
                schema: SCHEMA,
                repo: repo_name,
                statuses_available: boards::STATUSES.to_vec(),
                tours: rows
                    .into_iter()
                    .map(|b| TourSummaryOut {
                        slug: b.slug,
                        title: b.title,
                        status: b.status,
                        revision: b.revision,
                        updated_unix: b.updated_unix,
                        steps: b.steps,
                    })
                    .collect(),
            })
        })
        .await?;
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(out)))
}

// --- read -------------------------------------------------------------------

#[derive(Debug, serde::Deserialize)]
pub struct GetParams {
    pub repo: String,
    /// Include each code step's CONTEXT range beside its primary one.
    #[serde(default)]
    pub ctx: Option<String>,
}

pub fn get_params_accept_without(omit: &str) -> bool {
    accepts_without::<GetParams>(&[("repo", "r".into()), ("ctx", "1".into())], omit)
}

fn flag(v: Option<&String>) -> bool {
    v.is_some_and(|s| s == "1" || s == "true")
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct TourStepOut {
    /// 0-based position in the walk — what `?step=` addresses.
    pub ordinal: usize,
    /// The step's reference, resolved through the ONE Ladder. This is
    /// `boards::resolve::NodeOut` VERBATIM — same states, same reasons,
    /// same cards — because a tour step IS a board node (D10).
    pub node: NodeOut,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub camera: Option<Camera>,
    /// The K1 ref string this step's reference projects to, when it
    /// projects to one at all. A PROJECTION for a reader and for citation,
    /// never a second address.
    #[serde(rename = "ref", skip_serializing_if = "Option::is_none")]
    pub ref_str: Option<String>,
}

#[derive(Debug, serde::Serialize)]
pub struct TourOut {
    pub schema: &'static str,
    pub repo: String,
    pub slug: String,
    pub title: String,
    pub description_md: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub authored_ref: Option<String>,
    pub revision: i64,
    pub content_hash: String,
    pub created_unix: i64,
    pub updated_unix: i64,
    pub steps: Vec<TourStepOut>,
    /// `boards::resolve::Honesty`, unchanged — a tour counts its pinned,
    /// carried and ORPHAN steps exactly as a board counts its nodes.
    pub honesty: bresolve::Honesty,
}

/// Load and resolve one tour. Shared by the read, the pack and both
/// exports, so those three can never disagree about what a tour IS.
pub(crate) fn load_resolved(
    store: &crate::store::Store,
    repo: &crate::config::RepoEntry,
    repo_id: i64,
    slug: &str,
    want_context: bool,
) -> Result<TourOut, ApiError> {
    let Some(row) = store.get_canvas_board(repo_id, BOARD_KIND_TOUR, slug)? else {
        return Err(ApiError::not_found(format!(
            "no tour {slug:?} in this repo"
        )));
    };
    let nodes: Vec<CanvasNodeRow> = store.canvas_board_nodes(row.id)?;
    let steps = store.canvas_board_steps(row.id)?;
    let ctx = bresolve::Ctx {
        repo,
        store,
        repo_id,
        want_context,
    };
    let by_id: std::collections::HashMap<&str, &CanvasNodeRow> =
        nodes.iter().map(|n| (n.node_id.as_str(), n)).collect();

    let mut out_steps: Vec<TourStepOut> = Vec::with_capacity(steps.len());
    let mut resolved_nodes: Vec<NodeOut> = Vec::with_capacity(steps.len());
    let mut notes: Vec<String> = Vec::new();
    for (i, s) in steps.iter().enumerate() {
        let Some(row) = by_id.get(s.node_id.as_str()) else {
            // Structurally unreachable (both rows are written in one tx),
            // and reported rather than skipped if it ever happens: a tour
            // that silently loses a stop is worse than one that admits it.
            notes.push(format!(
                "step {i} names {:?}, which has no stored reference — the step is omitted",
                s.node_id
            ));
            continue;
        };
        let node = bresolve::resolve_node(&ctx, row);
        let camera = s
            .camera_json
            .as_deref()
            .and_then(|j| serde_json::from_str::<Camera>(j).ok());
        let ref_str = super::k1_ref_for(&node.kind, &node.reference);
        resolved_nodes.push(node.clone());
        out_steps.push(TourStepOut {
            ordinal: i,
            node,
            camera,
            ref_str,
        });
    }
    let honesty = bresolve::honesty(&resolved_nodes, 0, out_steps.len(), false, notes);
    Ok(TourOut {
        schema: SCHEMA,
        repo: repo.name.clone(),
        slug: row.slug,
        title: row.title,
        description_md: row.description_md,
        status: row.status,
        authored_ref: row.authored_ref,
        revision: row.revision,
        content_hash: row.content_hash,
        created_unix: row.created_unix,
        updated_unix: row.updated_unix,
        steps: out_steps,
        honesty,
    })
}

/// `GET /api/tours/{slug}?repo=[&ctx=1]`.
pub async fn get_tour(
    State(state): State<SharedState>,
    AxumPath(slug): AxumPath<String>,
    Query(params): Query<GetParams>,
) -> Result<impl IntoResponse, ApiError> {
    let (repo, repo_id) = find_repo(&state, &params.repo)?;
    let repo = repo.clone();
    let want_context = flag(params.ctx.as_ref());
    let out = state
        .store
        .run_blocking(move |store| load_resolved(store, &repo, repo_id, &slug, want_context))
        .await?;
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(out)))
}

// --- pack -------------------------------------------------------------------

#[derive(Debug, serde::Deserialize)]
pub struct PackParams {
    pub repo: String,
    /// Byte budget, `1..=`[`MAX_PACK_BUDGET`]; absent =
    /// [`DEFAULT_PACK_BUDGET`].
    #[serde(default)]
    pub budget: Option<usize>,
}

pub fn pack_params_accept_without(omit: &str) -> bool {
    accepts_without::<PackParams>(
        &[("repo", "r".into()), ("budget", serde_json::json!(4096))],
        omit,
    )
}

/// `GET /api/tours/{slug}/pack?repo=[&budget=]` — the budgeted context
/// pack behind `kb-code tour pack`.
pub async fn pack_tour(
    State(state): State<SharedState>,
    AxumPath(slug): AxumPath<String>,
    Query(params): Query<PackParams>,
) -> Result<impl IntoResponse, ApiError> {
    let (repo, repo_id) = find_repo(&state, &params.repo)?;
    let repo = repo.clone();
    let budget = params
        .budget
        .unwrap_or(DEFAULT_PACK_BUDGET)
        .clamp(1, MAX_PACK_BUDGET);
    let out = state
        .store
        .run_blocking(move |store| {
            let tour = load_resolved(store, &repo, repo_id, &slug, false)?;
            Ok::<_, ApiError>(pack::pack(&tour, budget))
        })
        .await?;
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(out)))
}

// --- export -----------------------------------------------------------------

#[derive(Debug, serde::Deserialize)]
pub struct ExportParams {
    pub repo: String,
    /// `md` | `codetour`.
    pub format: String,
}

pub fn export_params_accept_without(omit: &str) -> bool {
    accepts_without::<ExportParams>(&[("repo", "r".into()), ("format", "md".into())], omit)
}

/// `GET /api/tours/{slug}/export?repo=&format=`.
pub async fn export_tour(
    State(state): State<SharedState>,
    AxumPath(slug): AxumPath<String>,
    Query(params): Query<ExportParams>,
) -> Result<impl IntoResponse, ApiError> {
    let (repo, repo_id) = find_repo(&state, &params.repo)?;
    let repo = repo.clone();
    let format = params.format.clone();
    if !export::is_valid_format(&format) {
        return Err(ApiError::bad_request(format!(
            "unknown format {format:?} — expected one of {}",
            export::FORMATS.join(", ")
        )));
    }
    let tour = state
        .store
        .run_blocking(move |store| load_resolved(store, &repo, repo_id, &slug, false))
        .await?;
    let (content_type, body) = export::render(&tour, &format);
    Ok((
        [
            (header::CONTENT_TYPE, content_type),
            (header::CACHE_CONTROL, "no-store"),
        ],
        body,
    ))
}

// --- apply (loopback-only) --------------------------------------------------

#[derive(Debug, serde::Deserialize)]
pub struct ApplyParams {
    #[serde(default)]
    pub dry_run: Option<String>,
}

#[derive(Debug, serde::Serialize)]
pub struct ApplyOut {
    pub schema: &'static str,
    pub repo: String,
    pub slug: String,
    pub created: bool,
    pub unchanged: bool,
    pub dry_run: bool,
    pub status: String,
    pub status_reset: bool,
    pub revision: i64,
    pub steps: usize,
    pub lint: lint::Report,
}

/// `POST /api/tours/apply` — an idempotent upsert BY SLUG, LOOPBACK-ONLY.
pub async fn apply_tour(
    State(state): State<SharedState>,
    Query(params): Query<ApplyParams>,
    raw: String,
) -> Result<impl IntoResponse, ApiError> {
    if raw.len() > boards::MAX_APPLY_BYTES {
        return Err(ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            format!(
                "tour document is {} bytes; the cap is {} — refused, not truncated",
                raw.len(),
                boards::MAX_APPLY_BYTES
            ),
        ));
    }
    let value: serde_json::Value = serde_json::from_str(&raw)
        .map_err(|e| ApiError::bad_request(format!("tour document is not JSON: {e}")))?;
    // The COORDINATE refusal runs on the raw JSON, BEFORE the typed parse —
    // `boards::lint::precheck_raw`, unchanged. A tour is coordinate-free
    // for exactly the reason a board is (D10: layout is the SPA's, in
    // TypeScript, with one engine), so it runs the SAME pre-pass rather
    // than a copy of it.
    let pre = boards::lint::precheck_raw(&value);
    if !pre.is_empty() {
        let report = lint::Report {
            findings: pre,
            components: Vec::new(),
        };
        return Err(ApiError::bad_request(lint::refusal_summary(&report)));
    }
    let doc: TourDoc = serde_json::from_value(value)
        .map_err(|e| ApiError::bad_request(format!("tour document does not parse: {e}")))?;

    let report = lint::check(&doc);
    if report.refused() {
        return Err(ApiError::bad_request(lint::refusal_summary(&report)));
    }
    if RESERVED_SLUGS.contains(&doc.slug.as_str()) {
        return Err(ApiError::bad_request(format!(
            "the slug {:?} is reserved — it is a literal segment under /api/tours, so a \
             tour named it would be unreachable",
            doc.slug
        )));
    }
    let board = super::to_board_doc(&doc).map_err(|errs| {
        ApiError::bad_request(lint::refusal_summary(&lint::Report {
            findings: errs,
            components: Vec::new(),
        }))
    })?;
    let hash = super::content_hash(&doc, &board);
    let dry_run = flag(params.dry_run.as_ref());

    let (repo, repo_id) = find_repo(&state, &doc.repo)?;
    let repo_name = repo.name.clone();
    let repo = repo.clone();
    let status = doc
        .status
        .clone()
        .unwrap_or_else(|| boards::STATUS_PENDING.to_string());
    let step_count = doc.steps.len();
    let cameras: Vec<Option<String>> = doc
        .steps
        .iter()
        .map(|s| s.camera.and_then(|c| serde_json::to_string(&c).ok()))
        .collect();
    let now = now_unix();

    if dry_run {
        return Ok((
            StatusCode::OK,
            [(header::CACHE_CONTROL, "no-store")],
            Json(ApplyOut {
                schema: SCHEMA,
                repo: repo_name,
                slug: doc.slug.clone(),
                created: false,
                unchanged: false,
                dry_run: true,
                status,
                status_reset: false,
                revision: 0,
                steps: step_count,
                lint: report,
            }),
        ));
    }

    let out = state
        .store
        .run_blocking(move |store| {
            // The anchor snippet is captured under `lane_facts`' EXACT rule
            // (invariant 21/24(a)): only when the file on disk IS the blob
            // the step claims. `boards::routes` does the same for a node,
            // and this calls the same helper rather than a second copy.
            let nodes: Vec<NewCanvasNode> = crate::boards::routes::build_nodes(&repo, &board);
            let edges: Vec<NewCanvasEdge> = board
                .edges
                .iter()
                .map(|e| NewCanvasEdge {
                    from_node: e.from.clone(),
                    to_node: e.to.clone(),
                    kind: e.kind.clone(),
                    label: e.label.clone(),
                    provenance: e.provenance_or_default().to_string(),
                    trust: e.trust.clone(),
                })
                .collect();
            let steps: Vec<NewCanvasStep> = board
                .steps
                .iter()
                .zip(cameras.iter())
                .map(|(s, cam)| NewCanvasStep {
                    node_id: s.node.clone(),
                    caption: None,
                    camera_json: cam.clone(),
                })
                .collect();
            let outcome = store.apply_canvas_board(
                repo_id,
                &NewCanvasBoard {
                    kind: BOARD_KIND_TOUR.to_string(),
                    slug: board.slug.clone(),
                    title: board.title.clone(),
                    description_md: board.description_md.clone(),
                    status: status.clone(),
                    authored_ref: board.authored_ref.clone(),
                    content_hash: hash,
                },
                &nodes,
                &edges,
                &steps,
                now,
            )?;
            Ok::<_, ApiError>(ApplyOut {
                schema: SCHEMA,
                repo: repo_name,
                slug: board.slug.clone(),
                created: outcome.created,
                unchanged: outcome.unchanged,
                dry_run: false,
                status: outcome.status,
                status_reset: outcome.status_reset,
                revision: outcome.revision,
                steps: step_count,
                lint: report,
            })
        })
        .await?;
    // 201 on a create, 200 otherwise — `boards::routes::apply_board`'s own
    // contract, mirrored so the two families read identically on the wire.
    let status = if out.created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok((status, [(header::CACHE_CONTROL, "no-store")], Json(out)))
}

#[derive(Debug, serde::Deserialize)]
pub struct DeleteParams {
    pub repo: String,
}

/// `DELETE /api/tours/{slug}?repo=` — LOOPBACK-ONLY.
pub async fn delete_tour(
    State(state): State<SharedState>,
    AxumPath(slug): AxumPath<String>,
    Query(params): Query<DeleteParams>,
) -> Result<impl IntoResponse, ApiError> {
    let (_repo, repo_id) = find_repo(&state, &params.repo)?;
    let removed = state
        .store
        .run_blocking(move |store| {
            Ok::<_, ApiError>(store.delete_canvas_board(repo_id, BOARD_KIND_TOUR, &slug)?)
        })
        .await?;
    if !removed {
        return Err(ApiError::not_found("no such tour in this repo"));
    }
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_contract_requires_repo_and_the_export_requires_a_format() {
        assert!(list_params_accept_without(""));
        assert!(!list_params_accept_without("repo"));
        assert!(get_params_accept_without(""));
        assert!(!get_params_accept_without("repo"));
        assert!(pack_params_accept_without(""));
        assert!(!pack_params_accept_without("repo"));
        assert!(export_params_accept_without(""));
        assert!(!export_params_accept_without("repo"));
        assert!(!export_params_accept_without("format"));
    }

    #[test]
    fn the_reserved_slug_is_the_literal_route_segment() {
        assert!(RESERVED_SLUGS.contains(&"apply"));
        // …and it is a slug a lint would otherwise happily accept, which is
        // exactly why it has to be refused BY NAME.
        assert!(boards::is_valid_id("apply"));
    }
}
