//! V74-L1 — `kbc-canvas/1`'s HTTP surface.
//!
//! # The routes, and their gates
//!
//! | route | gate |
//! |---|---|
//! | `GET /api/boards?repo=[&status=]` | `auth_bearer` |
//! | `GET /api/boards/{slug}?repo=[&ctx=1][&live=1]` | `auth_bearer` |
//! | `GET /api/boards/{slug}/export?repo=&format=[&base=]` | `auth_bearer` |
//! | `GET /api/boards/sweep?repo=[&slug=]` | `auth_bearer` |
//! | `POST /api/boards/apply` | **loopback-only** |
//! | `POST /api/boards/{slug}/accept?repo=` | `[review] remote_mutations` |
//! | `POST /api/boards/{slug}/archive?repo=` | `[review] remote_mutations` |
//! | `DELETE /api/boards/{slug}?repo=` | `[review] remote_mutations` |
//!
//! Apply stays on `transcripts_api` (loopback-only HARD). Accept / archive
//! / DELETE ride the SAME `review_mutations_gate` as the five review
//! families (V76-R4a, D10) — no second gate, no new config key.
//! `security::audit_mutations` (invariant 1) records every attempt with
//! its actual outcome for free.
//!
//! # Why `/api/boards` and not `/api/canvas`
//!
//! `/api/canvas` and `/api/canvas/{id}` are the v3.4-C1 fragment-canvas
//! routes (`crate::canvas`), whose `{id}` is an `i64` row id. `GET
//! /api/canvas/{slug}` is the SAME axum route pattern as `GET
//! /api/canvas/{id}` — registering both would panic at boot, and changing
//! the existing one's meaning would break the SPA that calls it today. So
//! the board family gets its own prefix, `canvas_sets` is FROZEN beside it
//! (the `/api/usages` → `/api/usages/2` treatment), and `board` is already
//! kbc-seq/1's own name for this projection. The CLI verb stays
//! `kb-code canvas …` (D10's own name), which is why the module is
//! `kbc-canvas/1` rather than `kbc-board/1`.
//!
//! # Two reserved slugs
//!
//! `apply` and `sweep` are literal path segments under `/api/boards`, so a
//! board slugged either would be unreachable. They are refused at apply
//! time by name ([`RESERVED_SLUGS`]) rather than shadowed silently.

use super::*;
use crate::routes::{find_repo, ApiError};
use crate::state::SharedState;
use crate::store::{
    CanvasNodeRow, NewCanvasBoard, NewCanvasEdge, NewCanvasNode, NewCanvasStep, Store,
    StoreBlocking,
};
use axum::extract::{Path as AxumPath, Query, State};
use axum::http::{header, StatusCode};
use axum::response::IntoResponse;
use axum::Json;

/// Slugs that would be shadowed by a literal route segment.
pub const RESERVED_SLUGS: [&str; 2] = ["apply", "sweep"];

/// How many hits a live query card asks for. A card is a COUNT plus a
/// delta, not a result list, so this is the smallest number that still
/// answers "did it grow"; the response says the basis is a page.
const LIVE_QUERY_LIMIT: usize = 200;

fn now_unix() -> i64 {
    chrono::Utc::now().timestamp()
}

// --- list ------------------------------------------------------------------

#[derive(Debug, serde::Deserialize)]
pub struct ListParams {
    pub repo: String,
    /// One of [`STATUSES`]; absent = every status.
    #[serde(default)]
    pub status: Option<String>,
}

#[derive(Debug, serde::Serialize)]
pub struct BoardSummaryOut {
    pub slug: String,
    pub title: String,
    pub status: String,
    pub revision: i64,
    pub updated_unix: i64,
    pub nodes: i64,
    pub edges: i64,
    pub steps: i64,
}

#[derive(Debug, serde::Serialize)]
pub struct ListOut {
    pub schema: &'static str,
    pub repo: String,
    /// The whole status vocabulary, so a caller never has to infer it from
    /// the rows it happens to see (`seq::SeqListOut::projections_available`'s
    /// precedent).
    pub statuses_available: Vec<&'static str>,
    pub boards: Vec<BoardSummaryOut>,
}

/// `GET /api/boards?repo=[&status=]`.
pub async fn list_boards(
    State(state): State<SharedState>,
    Query(params): Query<ListParams>,
) -> Result<impl IntoResponse, ApiError> {
    let (repo, repo_id) = find_repo(&state, &params.repo)?;
    let repo_name = repo.name.clone();
    let status = match &params.status {
        Some(s) if !is_valid_status(s) => {
            return Err(ApiError::bad_request(format!(
                "unknown status {s:?} — expected one of {}",
                STATUSES.join(", ")
            )));
        }
        other => other.clone(),
    };
    let out = state
        .store
        .run_blocking(move |store| {
            let rows = store.list_canvas_boards(
                repo_id,
                crate::tours::BOARD_KIND_BOARD,
                status.as_deref(),
            )?;
            Ok::<_, ApiError>(ListOut {
                schema: SCHEMA,
                repo: repo_name,
                statuses_available: STATUSES.to_vec(),
                boards: rows
                    .into_iter()
                    .map(|r| BoardSummaryOut {
                        slug: r.slug,
                        title: r.title,
                        status: r.status,
                        revision: r.revision,
                        updated_unix: r.updated_unix,
                        nodes: r.nodes,
                        edges: r.edges,
                        steps: r.steps,
                    })
                    .collect(),
            })
        })
        .await?;
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(out)))
}

// --- get -------------------------------------------------------------------

#[derive(Debug, serde::Deserialize)]
pub struct GetParams {
    pub repo: String,
    /// `1` to include each `code` node's CONTEXT range text beside its
    /// primary range.
    #[serde(default)]
    pub ctx: Option<String>,
    /// `1` to execute every `query` node and report the delta. Off by
    /// default — see `resolve`'s module doc.
    #[serde(default)]
    pub live: Option<String>,
}

fn flag(v: &Option<String>) -> bool {
    matches!(v.as_deref(), Some("1") | Some("true") | Some("yes"))
}

/// `GET /api/boards/{slug}?repo=[&ctx=1][&live=1]` — the board, with every
/// reference re-resolved NOW.
pub async fn get_board(
    State(state): State<SharedState>,
    AxumPath(slug): AxumPath<String>,
    Query(params): Query<GetParams>,
) -> Result<impl IntoResponse, ApiError> {
    let want_context = flag(&params.ctx);
    let live = flag(&params.live);
    let (repo, repo_id) = find_repo(&state, &params.repo)?;
    let mut board = load_and_resolve(&state, repo.clone(), repo_id, &slug, want_context).await?;
    if live {
        run_live_queries(&state, &params.repo, &mut board).await;
    }
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(board)))
}

/// The shared read: one blocking closure that loads the four tables and
/// resolves every node against the live tree.
async fn load_and_resolve(
    state: &SharedState,
    repo: crate::config::RepoEntry,
    repo_id: i64,
    slug: &str,
    want_context: bool,
) -> Result<resolve::BoardOut, ApiError> {
    let slug = slug.to_string();
    state
        .store
        .run_blocking(move |store| resolve_board(store, &repo, repo_id, &slug, want_context))
        .await
}

/// Load + resolve, synchronously. Separated from the route so `sweep` can
/// fold many boards inside ONE blocking closure rather than one hop per
/// board (the 2026-08-31 starvation incident's own rule, applied to a
/// fan-out).
pub(crate) fn resolve_board(
    store: &Store,
    repo: &crate::config::RepoEntry,
    repo_id: i64,
    slug: &str,
    want_context: bool,
) -> Result<resolve::BoardOut, ApiError> {
    let row = store
        .get_canvas_board(repo_id, crate::tours::BOARD_KIND_BOARD, slug)?
        .ok_or_else(|| ApiError::not_found(format!("board {slug:?} in {}", repo.name)))?;
    let nodes = store.canvas_board_nodes(row.id)?;
    let edges = store.canvas_board_edges(row.id)?;
    let steps = store.canvas_board_steps(row.id)?;
    let ctx = resolve::Ctx {
        repo,
        store,
        repo_id,
        want_context,
    };
    let resolved: Vec<resolve::NodeOut> = nodes
        .iter()
        .map(|n| resolve::resolve_node(&ctx, n))
        .collect();
    let pins: std::collections::BTreeMap<String, Pin> = nodes
        .iter()
        .filter_map(|n| match (n.pin_x, n.pin_y) {
            (Some(x), Some(y)) => Some((n.node_id.clone(), Pin { x, y })),
            _ => None,
        })
        .collect();
    let mut notes = Vec::new();
    let orphans = resolved
        .iter()
        .filter(|n| n.state == resolve::STATE_ORPHAN)
        .count();
    if orphans > 0 {
        notes.push(format!(
            "{orphans} node(s) no longer resolve; they are shown with their last-known \
             address and text rather than dropped"
        ));
    }
    let honesty = resolve::honesty(&resolved, edges.len(), steps.len(), false, notes);
    Ok(resolve::BoardOut {
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
        nodes: resolved,
        edges: edges
            .into_iter()
            .map(|e| resolve::EdgeOut {
                from: e.from_node,
                to: e.to_node,
                kind: e.kind,
                label: e.label,
                provenance: e.provenance,
                trust: e.trust,
            })
            .collect(),
        steps: steps
            .into_iter()
            .map(|s| resolve::StepOut {
                node: s.node_id,
                caption: s.caption,
            })
            .collect(),
        pins,
        honesty,
    })
}

/// Execute every `query` node's kbcq/1 query and fill in the delta.
///
/// `is_loopback` is hardcoded `false`: a board is a bearer-readable object,
/// so a query card must never surface a lane the caller's own
/// `GET /api/search` would refuse it. The count is a PAGE count and the
/// card says so.
async fn run_live_queries(state: &SharedState, repo: &str, board: &mut resolve::BoardOut) {
    for node in board.nodes.iter_mut() {
        let Some(card) = node.query.as_mut() else {
            continue;
        };
        let resp = crate::search::unified::run(
            state,
            false,
            &card.query,
            Some(repo),
            Some(LIVE_QUERY_LIMIT),
        )
        .await;
        let mut total = 0usize;
        let mut truncated = false;
        for s in &resp.sections {
            if let Some(arr) = s.results.as_array() {
                total += arr.len();
            }
            truncated |= s.truncated;
        }
        card.current_count = Some(total as u32);
        card.basis = Some("page");
        card.truncated = Some(truncated);
        card.delta = card.authored_count.map(|a| total as i64 - a as i64);
        node.note = Some(format!(
            "count is over the {LIVE_QUERY_LIMIT}-hit page each lane returned, not a \
             corpus total"
        ));
    }
    board.honesty.live_queries = true;
}

// --- apply -----------------------------------------------------------------

#[derive(Debug, serde::Serialize)]
pub struct ApplyOut {
    pub schema: &'static str,
    pub repo: String,
    pub slug: String,
    pub created: bool,
    /// `true` when the document's content hash already matched — nothing
    /// was written and `revision` did not move.
    pub unchanged: bool,
    pub revision: i64,
    pub status: String,
    /// `true` when a CHANGED apply moved an accepted/archived board back to
    /// the status the document asked for (D21).
    pub status_reset: bool,
    pub dry_run: bool,
    pub lint: lint::Report,
    /// Warnings from RESOLVING the board that was (or would be) written —
    /// the semantic half the pure lint cannot see.
    pub resolution_warnings: Vec<String>,
    pub honesty: resolve::Honesty,
}

/// `POST /api/boards/apply` — LOOPBACK-ONLY. The whole document, upserted
/// by slug.
///
/// Body is taken as `raw: String` (not `Json<T>`) so the RAW byte cap runs
/// before any parse — `canvas::reject_oversize_raw`'s posture, and the only
/// way to bound the parse cost by this unit's own cap rather than axum's
/// default body limit.
pub async fn apply_board(
    State(state): State<SharedState>,
    Query(params): Query<ApplyParams>,
    raw: String,
) -> Result<impl IntoResponse, ApiError> {
    if raw.len() > MAX_APPLY_BYTES {
        return Err(ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            format!(
                "board document is {} bytes; the cap is {MAX_APPLY_BYTES} — refused, not \
                 truncated",
                raw.len()
            ),
        ));
    }
    // The COORDINATE refusal runs on the raw JSON, BEFORE the typed parse:
    // `BoardDoc` is `deny_unknown_fields`, so serde would otherwise refuse
    // an `"x": 10` with a generic "unknown field" instead of the message
    // that teaches an author what a board is (see `lint::COORDINATE_KEYS`).
    let value: serde_json::Value = serde_json::from_str(&raw)
        .map_err(|e| ApiError::bad_request(format!("board document is not JSON: {e}")))?;
    let pre = lint::precheck_raw(&value);
    if !pre.is_empty() {
        let report = lint::Report {
            findings: pre,
            components: Vec::new(),
        };
        return Err(refusal(&report));
    }
    let doc: BoardDoc = serde_json::from_value(value)
        .map_err(|e| ApiError::bad_request(format!("board document is not kbc-canvas/1: {e}")))?;

    let opts = lint::Opts {
        allow_disconnected: flag(&params.allow_disconnected),
    };
    let mut report = lint::check(&doc, opts);
    if RESERVED_SLUGS.contains(&doc.slug.as_str()) {
        report.findings.push(reserved_slug_finding(&doc.slug));
    }
    if report.refused() {
        return Err(refusal(&report));
    }

    let (repo, repo_id) = find_repo(&state, &doc.repo)?;
    let repo = repo.clone();
    let repo_name = repo.name.clone();
    let dry_run = flag(&params.dry_run);
    let slug = doc.slug.clone();
    let now = now_unix();

    let (outcome, board) = state
        .store
        .run_blocking(move |store| {
            let content_hash = content_hash(&doc);
            let status = doc
                .status
                .clone()
                .unwrap_or_else(|| STATUS_PENDING.to_string());
            let nodes = build_nodes(&repo, &doc);
            let edges: Vec<NewCanvasEdge> = doc
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
            let steps: Vec<NewCanvasStep> = doc
                .steps
                .iter()
                .map(|s| NewCanvasStep {
                    node_id: s.node.clone(),
                    caption: s.caption.clone(),
                    // V74-L3b — a per-step CAMERA is a tour's, never a
                    // board's: a board's walkthrough camera is the SPA's
                    // own viewport state, not authored content.
                    camera_json: None,
                })
                .collect();
            let new_board = NewCanvasBoard {
                kind: crate::tours::BOARD_KIND_BOARD.to_string(),
                slug: doc.slug.clone(),
                title: doc.title.trim().to_string(),
                description_md: doc.description_md.clone(),
                status,
                authored_ref: doc.authored_ref.clone(),
                content_hash,
            };
            if dry_run {
                // Resolve the rows that WOULD be written, without writing
                // them: a dry run must report the same orphans a real apply
                // would, or it is not a rehearsal.
                let outcome = crate::store::CanvasApplyOutcome {
                    board_id: 0,
                    created: store
                        .get_canvas_board(repo_id, crate::tours::BOARD_KIND_BOARD, &doc.slug)?
                        .is_none(),
                    unchanged: false,
                    revision: 0,
                    status: new_board.status.clone(),
                    status_reset: false,
                };
                let board = resolve_pending(store, &repo, repo_id, &new_board, &nodes, &doc);
                return Ok::<_, ApiError>((outcome, board));
            }
            let outcome =
                store.apply_canvas_board(repo_id, &new_board, &nodes, &edges, &steps, now)?;
            let board = resolve_board(store, &repo, repo_id, &doc.slug, false)?;
            Ok((outcome, board))
        })
        .await?;

    let resolution_warnings: Vec<String> = board
        .nodes
        .iter()
        .filter(|n| n.state == resolve::STATE_ORPHAN)
        .map(|n| {
            format!(
                "node {:?} does not resolve ({}): it will be shown as an orphan with its \
                 address and last-known text",
                n.id, n.reason
            )
        })
        .collect();

    Ok((
        if outcome.created {
            StatusCode::CREATED
        } else {
            StatusCode::OK
        },
        [(header::CACHE_CONTROL, "no-store")],
        Json(ApplyOut {
            schema: SCHEMA,
            repo: repo_name,
            slug,
            created: outcome.created,
            unchanged: outcome.unchanged,
            revision: outcome.revision,
            status: outcome.status,
            status_reset: outcome.status_reset,
            dry_run,
            lint: report,
            resolution_warnings,
            honesty: board.honesty,
        }),
    ))
}

#[derive(Debug, serde::Deserialize)]
pub struct ApplyParams {
    #[serde(default)]
    pub dry_run: Option<String>,
    #[serde(default)]
    pub allow_disconnected: Option<String>,
}

fn reserved_slug_finding(slug: &str) -> lint::Finding {
    lint::Finding {
        rule: "slug",
        severity: lint::Severity::Refuse,
        at: Some(slug.to_string()),
        message: format!(
            "{slug:?} is a reserved slug — it is a literal segment under /api/boards \
             ({}), so a board named it would be unreachable",
            RESERVED_SLUGS.join(", ")
        ),
    }
}

fn refusal(report: &lint::Report) -> ApiError {
    ApiError::bad_request(report.refusal_summary())
}

/// Turn the document's nodes into storable rows, capturing each `code`
/// node's ANCHOR SNIPPET from the working tree.
///
/// The capture rule is `lane_facts`' (invariant 21) verbatim: a snippet is
/// taken ONLY when the file is readable AND the node's claimed blob is what
/// is on disk right now (or it claimed none, in which case the daemon
/// records the current blob as the authoring one). A snippet captured
/// against some other bytes would manufacture a match later instead of
/// admitting an orphan.
/// `pub(crate)` — V74-L3b's `tours::routes::apply_tour` lowers a tour to a
/// `BoardDoc` and calls THIS, rather than a second copy of the capture
/// rule (D10: one step model, and therefore one place that decides when a
/// snippet may honestly be taken).
pub(crate) fn build_nodes(repo: &crate::config::RepoEntry, doc: &BoardDoc) -> Vec<NewCanvasNode> {
    doc.nodes
        .iter()
        .map(|n| {
            let mut reference = n.reference.clone();
            let mut anchor_snippet = None;
            if n.kind == KIND_CODE {
                if let (Some(path), Some(range)) = (reference.path.clone(), reference.range) {
                    if let Ok(read) = crate::routes::read_repo_file(repo, &path, None) {
                        let claimed_matches = reference
                            .blob_sha
                            .as_deref()
                            .is_none_or(|c| c == read.blob_hash);
                        if claimed_matches {
                            if let Ok(text) = String::from_utf8(read.bytes) {
                                let slice = line_slice(&text, range);
                                if !slice.is_empty() {
                                    anchor_snippet =
                                        slice.lines().next().map(|l| l.trim().to_string());
                                    if reference.guard_hash.is_none() {
                                        reference.guard_hash = Some(guard_hash(slice.as_bytes()));
                                    }
                                }
                            }
                            if reference.blob_sha.is_none() {
                                reference.blob_sha = Some(read.blob_hash);
                            }
                        }
                    }
                }
            }
            let pin = doc.pins.get(&n.id);
            NewCanvasNode {
                node_id: n.id.clone(),
                kind: n.kind.clone(),
                title: n.title.clone(),
                body_md: n.body_md.clone(),
                ref_json: serde_json::to_string(&reference).unwrap_or_else(|_| "{}".to_string()),
                group_id: n.group.clone(),
                thread_id: n.thread_id.clone(),
                anchor_snippet,
                pin_x: pin.map(|p| p.x),
                pin_y: pin.map(|p| p.y),
            }
        })
        .collect()
}

/// Resolve the rows a dry run WOULD write, in memory.
fn resolve_pending(
    store: &Store,
    repo: &crate::config::RepoEntry,
    repo_id: i64,
    board: &NewCanvasBoard,
    nodes: &[NewCanvasNode],
    doc: &BoardDoc,
) -> resolve::BoardOut {
    let ctx = resolve::Ctx {
        repo,
        store,
        repo_id,
        want_context: false,
    };
    let rows: Vec<CanvasNodeRow> = nodes
        .iter()
        .enumerate()
        .map(|(i, n)| CanvasNodeRow {
            node_id: n.node_id.clone(),
            ordinal: i as i64,
            kind: n.kind.clone(),
            title: n.title.clone(),
            body_md: n.body_md.clone(),
            ref_json: n.ref_json.clone(),
            group_id: n.group_id.clone(),
            thread_id: n.thread_id.clone(),
            anchor_snippet: n.anchor_snippet.clone(),
            pin_x: n.pin_x,
            pin_y: n.pin_y,
        })
        .collect();
    let resolved: Vec<resolve::NodeOut> = rows
        .iter()
        .map(|n| resolve::resolve_node(&ctx, n))
        .collect();
    let honesty = resolve::honesty(
        &resolved,
        doc.edges.len(),
        doc.steps.len(),
        false,
        vec!["dry run — nothing was written".to_string()],
    );
    resolve::BoardOut {
        schema: SCHEMA,
        repo: repo.name.clone(),
        slug: board.slug.clone(),
        title: board.title.clone(),
        description_md: board.description_md.clone(),
        status: board.status.clone(),
        authored_ref: board.authored_ref.clone(),
        revision: 0,
        content_hash: board.content_hash.clone(),
        created_unix: 0,
        updated_unix: 0,
        nodes: resolved,
        edges: Vec::new(),
        steps: Vec::new(),
        pins: doc.pins.clone(),
        honesty,
    }
}

// --- status transitions + delete -------------------------------------------

#[derive(Debug, serde::Deserialize)]
pub struct RepoParam {
    pub repo: String,
}

#[derive(Debug, serde::Serialize)]
pub struct StatusOut {
    pub schema: &'static str,
    pub repo: String,
    pub slug: String,
    pub status: String,
    pub revision: i64,
}

/// `POST /api/boards/{slug}/accept?repo=` — `[review] remote_mutations`
/// (loopback always; non-loopback bearer when the flag is on).
///
/// D21: an agent-proposed board is PENDING until a human accepts it. This
/// route is the only way `accepted` is ever written; `apply` refuses to
/// author it (`lint`'s `status` rule), which is what makes "impossible by
/// the lint" true by construction rather than by convention — a
/// bearer-authored document naming `accepted` is refused before any gate
/// matters. Apply itself stays loopback-only.
pub async fn accept_board(
    State(state): State<SharedState>,
    AxumPath(slug): AxumPath<String>,
    Query(params): Query<RepoParam>,
) -> Result<impl IntoResponse, ApiError> {
    set_status(state, slug, params.repo, STATUS_ACCEPTED).await
}

/// `POST /api/boards/{slug}/archive?repo=` — `[review] remote_mutations`.
pub async fn archive_board(
    State(state): State<SharedState>,
    AxumPath(slug): AxumPath<String>,
    Query(params): Query<RepoParam>,
) -> Result<impl IntoResponse, ApiError> {
    set_status(state, slug, params.repo, STATUS_ARCHIVED).await
}

async fn set_status(
    state: SharedState,
    slug: String,
    repo: String,
    status: &'static str,
) -> Result<impl IntoResponse, ApiError> {
    let (repo_entry, repo_id) = find_repo(&state, &repo)?;
    let repo_name = repo_entry.name.clone();
    let now = now_unix();
    let slug_bg = slug.clone();
    let row = state
        .store
        .run_blocking(move |store| {
            store.set_canvas_board_status(
                repo_id,
                crate::tours::BOARD_KIND_BOARD,
                &slug_bg,
                status,
                now,
            )
        })
        .await?
        .ok_or_else(|| ApiError::not_found(format!("board {slug:?} in {repo_name}")))?;
    let repo_name = repo_entry.name.clone();
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(StatusOut {
            schema: SCHEMA,
            repo: repo_name,
            slug: row.slug,
            status: row.status,
            revision: row.revision,
        }),
    ))
}

/// `DELETE /api/boards/{slug}?repo=` — `[review] remote_mutations`, audited.
pub async fn delete_board(
    State(state): State<SharedState>,
    AxumPath(slug): AxumPath<String>,
    Query(params): Query<RepoParam>,
) -> Result<impl IntoResponse, ApiError> {
    let (repo, repo_id) = find_repo(&state, &params.repo)?;
    let repo_name = repo.name.clone();
    let slug_bg = slug.clone();
    let ok = state
        .store
        .run_blocking(move |store| {
            store.delete_canvas_board(repo_id, crate::tours::BOARD_KIND_BOARD, &slug_bg)
        })
        .await?;
    if !ok {
        return Err(ApiError::not_found(format!(
            "board {slug:?} in {repo_name}"
        )));
    }
    Ok((
        StatusCode::NO_CONTENT,
        [(header::CACHE_CONTROL, "no-store")],
    ))
}

// --- export ----------------------------------------------------------------

#[derive(Debug, serde::Deserialize)]
pub struct ExportParams {
    pub repo: String,
    /// One of [`export::FORMATS`].
    pub format: String,
    /// An absolute `http(s)` prefix for the reader links a `kb-html` export
    /// emits. Absent = root-relative links.
    #[serde(default)]
    pub base: Option<String>,
}

/// `GET /api/boards/{slug}/export?repo=&format=[&base=]`.
pub async fn export_board(
    State(state): State<SharedState>,
    AxumPath(slug): AxumPath<String>,
    Query(params): Query<ExportParams>,
) -> Result<impl IntoResponse, ApiError> {
    if !export::is_valid_format(&params.format) {
        return Err(ApiError::bad_request(format!(
            "unknown format {:?} — expected one of {}",
            params.format,
            export::FORMATS.join(", ")
        )));
    }
    let base = export::validate_base(params.base.as_deref().unwrap_or(""))
        .map_err(ApiError::bad_request)?;
    let (repo, repo_id) = find_repo(&state, &params.repo)?;
    let board = load_and_resolve(&state, repo.clone(), repo_id, &slug, false).await?;
    let body = match params.format.as_str() {
        export::FORMAT_JSONCANVAS => serde_json::to_string_pretty(&export::to_json_canvas(&board))
            .unwrap_or_else(|_| "{}".to_string()),
        export::FORMAT_KB_HTML => export::to_kb_html(&board, &base),
        _ => export::to_markdown(&board),
    };
    let filename = export::filename(&board.slug, &params.format);
    Ok((
        [
            (header::CACHE_CONTROL, "no-store".to_string()),
            (
                header::CONTENT_TYPE,
                export::content_type(&params.format).to_string(),
            ),
            (
                header::CONTENT_DISPOSITION,
                format!("inline; filename=\"{filename}\""),
            ),
        ],
        body,
    ))
}

// --- sweep -----------------------------------------------------------------

#[derive(Debug, serde::Deserialize)]
pub struct SweepParams {
    pub repo: String,
    /// One slug; absent = every board in the repo.
    #[serde(default)]
    pub slug: Option<String>,
}

#[derive(Debug, serde::Serialize)]
pub struct SweepNode {
    pub node: String,
    pub kind: String,
    pub state: &'static str,
    pub reason: &'static str,
    pub address: String,
    /// Present for a `carried` node: how far it moved.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shifted_by: Option<i64>,
    /// Present for a `query` node whose live count differs from the one it
    /// was authored with.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delta: Option<i64>,
    /// `true` when this node also carries a PIN — a fixed position held for
    /// a card that no longer points anywhere.
    pub stale_pin: bool,
}

#[derive(Debug, serde::Serialize)]
pub struct SweepBoard {
    pub slug: String,
    pub title: String,
    pub status: String,
    pub drifted: bool,
    pub orphans: usize,
    pub carried: usize,
    pub query_deltas: usize,
    pub stale_pins: usize,
    pub nodes: Vec<SweepNode>,
}

#[derive(Debug, serde::Serialize)]
pub struct SweepOut {
    pub schema: &'static str,
    pub repo: String,
    pub boards: Vec<SweepBoard>,
    /// `true` when ANY board drifted — the one bit `--check`'s exit code
    /// reads.
    pub drifted: bool,
    pub checked: usize,
}

/// `GET /api/boards/sweep?repo=[&slug=]` — re-resolve every node of every
/// (or one) board and report drift. NEVER mutates: this is the CI gate, and
/// a gate that repaired what it found could not fail.
///
/// Query cards ARE executed here (unlike an ordinary board read) — a sweep
/// is an explicit, occasional check, not a page load.
pub async fn sweep_boards(
    State(state): State<SharedState>,
    Query(params): Query<SweepParams>,
) -> Result<impl IntoResponse, ApiError> {
    let (repo, repo_id) = find_repo(&state, &params.repo)?;
    let repo = repo.clone();
    let repo_name = repo.name.clone();
    let only = params.slug.clone();

    // One blocking hop for EVERY board — a per-board hop would put N
    // round trips through the store mutex for a verb whose whole job is to
    // walk all of them.
    let repo_bg = repo.clone();
    let mut boards: Vec<resolve::BoardOut> = state
        .store
        .run_blocking(move |store| {
            let slugs: Vec<String> = match &only {
                Some(s) => vec![s.clone()],
                None => store
                    .list_canvas_boards(repo_id, crate::tours::BOARD_KIND_BOARD, None)?
                    .into_iter()
                    .map(|b| b.slug)
                    .collect(),
            };
            let mut out = Vec::new();
            for slug in slugs {
                out.push(resolve_board(store, &repo_bg, repo_id, &slug, false)?);
            }
            Ok::<_, ApiError>(out)
        })
        .await?;

    for b in boards.iter_mut() {
        run_live_queries(&state, &repo_name, b).await;
    }

    let mut out_boards = Vec::new();
    let mut any_drift = false;
    for b in &boards {
        let mut nodes = Vec::new();
        let (mut orphans, mut carried, mut deltas, mut stale_pins) = (0, 0, 0, 0);
        for n in &b.nodes {
            let delta = n.query.as_ref().and_then(|q| q.delta).filter(|d| *d != 0);
            let shifted = n.code.as_ref().map(|c| c.shifted_by).filter(|s| *s != 0);
            let is_orphan = n.state == resolve::STATE_ORPHAN;
            let stale_pin = is_orphan && n.pin.is_some();
            if !is_orphan && shifted.is_none() && delta.is_none() {
                continue;
            }
            if is_orphan {
                orphans += 1;
            }
            if shifted.is_some() {
                carried += 1;
            }
            if delta.is_some() {
                deltas += 1;
            }
            if stale_pin {
                stale_pins += 1;
            }
            nodes.push(SweepNode {
                node: n.id.clone(),
                kind: n.kind.clone(),
                state: n.state,
                reason: n.reason,
                address: n.address.clone(),
                shifted_by: shifted,
                delta,
                stale_pin,
            });
        }
        let drifted = !nodes.is_empty();
        any_drift |= drifted;
        out_boards.push(SweepBoard {
            slug: b.slug.clone(),
            title: b.title.clone(),
            status: b.status.clone(),
            drifted,
            orphans,
            carried,
            query_deltas: deltas,
            stale_pins,
            nodes,
        });
    }

    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(SweepOut {
            schema: SCHEMA,
            repo: repo_name,
            checked: out_boards.len(),
            drifted: any_drift,
            boards: out_boards,
        }),
    ))
}

// --- RouteContract param probes (invariant 15) -----------------------------

fn accepts_without<T: serde::de::DeserializeOwned>(pairs: &[(&str, &str)], omit: &str) -> bool {
    let map: serde_json::Map<String, serde_json::Value> = pairs
        .iter()
        .filter(|(k, _)| *k != omit)
        .map(|(k, v)| (k.to_string(), serde_json::Value::String(v.to_string())))
        .collect();
    serde_json::from_value::<T>(serde_json::Value::Object(map)).is_ok()
}

pub fn list_params_accept_without(omit: &str) -> bool {
    accepts_without::<ListParams>(&[("repo", "r"), ("status", STATUS_DRAFT)], omit)
}

pub fn get_params_accept_without(omit: &str) -> bool {
    accepts_without::<GetParams>(&[("repo", "r"), ("ctx", "1"), ("live", "1")], omit)
}

pub fn export_params_accept_without(omit: &str) -> bool {
    accepts_without::<ExportParams>(
        &[("repo", "r"), ("format", export::FORMAT_MD), ("base", "")],
        omit,
    )
}

pub fn sweep_params_accept_without(omit: &str) -> bool {
    accepts_without::<SweepParams>(&[("repo", "r"), ("slug", "b")], omit)
}

#[cfg(test)]
mod tests {
    use super::*;

    // The "is every declared route registered" walk is NOT repeated here:
    // `entities::tests::every_declared_v71_g0_route_is_registered_and_
    // requires_its_params` chains `boards::V74_L1_ROUTES` into the ONE
    // crate-wide walk (invariant 15 — one loop over every unit's declared
    // contracts). What IS this module's own is the gate assertion below,
    // which that walk has no notion of.

    #[test]
    fn the_four_loopback_mutations_are_registered_on_the_loopback_sub_router() {
        const ROUTER_SRC: &str = include_str!("../router.rs");
        // The loopback sub-router is everything after the `transcripts_api`
        // binding; a mutation registered above that line would be on the
        // ordinary bearer surface, which is the whole defect this asserts
        // against.
        let (bearer, loopback) = ROUTER_SRC
            .split_once("let transcripts_api")
            .expect("router.rs declares transcripts_api");
        for handler in [
            "boards::routes::apply_board",
            "boards::routes::accept_board",
            "boards::routes::archive_board",
            "boards::routes::delete_board",
        ] {
            assert!(
                loopback.contains(handler),
                "{handler} must be registered on the loopback-only sub-router"
            );
            assert!(
                !bearer.contains(handler),
                "{handler} is registered on the ordinary bearer surface — board \
                 mutations are loopback-only"
            );
        }
    }

    #[test]
    fn a_reserved_slug_is_refused_by_name() {
        for s in RESERVED_SLUGS {
            let f = reserved_slug_finding(s);
            assert_eq!(f.rule, "slug");
            assert_eq!(f.severity, lint::Severity::Refuse);
            assert!(f.message.contains(s));
        }
        // …and the reserved names really ARE literal segments under
        // /api/boards, or reserving them would be superstition.
        const ROUTER_SRC: &str = include_str!("../router.rs");
        for s in RESERVED_SLUGS {
            assert!(
                ROUTER_SRC.contains(&format!("/boards/{s}")),
                "{s:?} is reserved but is not a literal route segment"
            );
        }
    }

    #[test]
    fn the_flag_parser_accepts_only_the_documented_truths() {
        for yes in ["1", "true", "yes"] {
            assert!(flag(&Some(yes.to_string())));
        }
        for no in ["0", "false", "", "TRUE", "on"] {
            assert!(!flag(&Some(no.to_string())), "{no:?}");
        }
        assert!(!flag(&None));
    }
}
