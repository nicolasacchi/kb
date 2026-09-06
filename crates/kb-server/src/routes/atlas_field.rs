//! `atlas_field` — the DUAL-FIELD ATLAS's operator half (W3 F-b): a JSON
//! Canvas `.canvas` sidecar the operator hand-positions, plus a route that
//! joins it against the machine layout to score where the two disagree.
//!
//! Overlays two layouts of the same corpus: the **machine field** (today's
//! atlas — `routes::atlas::points`, PCA/UMAP + k-means) and the **operator
//! field** — islands and artifacts a human placed by hand. Where the two
//! disagree is the interesting part: it's the visible gap between your
//! mental map of the corpus and the model's. The pure codec + disagreement
//! math live in `kb_core::atlas_field`; this module is the thin HTTP
//! wrapper, built by cloning `routes::boards` (the shipped precedent for
//! "a JSON Canvas sidecar in the corpus, stored verbatim").
//!
//! # Storage ruling (decided; do not revisit)
//!
//! The operator field is **ONE JSON Canvas sidecar per kb**, at the FIXED
//! path `<source_root>/atlas/operator.canvas` — no id in the path (unlike
//! `boards`, which is per-list), so there is no `is_safe_id` check and no
//! existence check before reading/writing it: the path is built entirely
//! from the corpus root, never from caller input.
//!
//! It is **not a reading list**: `migrations/V0015__lists.sql:8-13` states
//! outright that lists are USER-CURATED state the indexer's unlink pass and
//! `purge_kb_data` deliberately never touch, and this field carries NO
//! membership at all — a node exists only because a hand placed it, and a
//! node whose artifact has since vanished from the index is silently
//! DROPPED AT READ TIME by [`kb_core::atlas_field::disagreement`] (never
//! persisted, never pruned from the file). Modelling that as a
//! machine-synced list would violate the very rule that forbids one. It is
//! also not a board: a board is geometry for ONE list, keyed by `list_id`;
//! this is geometry for a whole kb, keyed by nothing, with no backing
//! collection whatsoever. The one-container-primitive rule is satisfied by
//! recording that here, in the route module the SPA/CLI actually calls,
//! rather than only in `kb_core::atlas_field`'s doc comment.
//!
//! Like boards' `.canvas`, this sidecar is **index-INERT by design**:
//! `.canvas` has no entry in any kb's `ExtensionMap` (exactly two parse
//! pipelines, html/md), so it is invisible to the indexer — no lance row,
//! no watcher reindex, no `index_generation` bump (invariant #15). Do NOT
//! add it to the map; that inertness is the reason this shape was chosen.
//!
//! Routes:
//!
//! ```text
//! GET  /api/kb/{kb}/atlas/field                → raw JSON Canvas doc
//!                                                 (absent file → the empty
//!                                                 default)
//! PUT  /api/kb/{kb}/atlas/field                → replace it wholesale
//! GET  /api/kb/{kb}/atlas/field/disagreement   → machine-vs-operator
//!                                                 displacement, sorted
//! ```
//!
//! `GET`/`PUT /field` are "store what parses" exactly like boards: shallow
//! JSON-shape validation (an object whose `nodes`/`edges`, if present, are
//! arrays), the same 256 KiB cap, `kb_core::fsx::write_atomic`, and VERBATIM
//! passthrough of caller bytes (no serde round-trip through the daemon, so
//! unknown/future JSON Canvas fields survive). `PUT` emits `atlas.field.
//! updated {kb}` on the bus (registered in `routes::schema`); a read-only
//! corpus (prod's `:ro` bind-mount) surfaces a write failure as 409, the
//! same treatment `routes::boards`/`routes::capture` give it.
//!
//! `GET /field/disagreement` reads the sidecar the same tolerant way (a
//! missing file is the empty field, never a 404), reads the machine layout
//! from the same memoised full-corpus scan `routes::atlas::points` uses
//! (`atlas_docs_cached`, keyed on the storage actor's index generation —
//! invariant #15), joins the two by source-relative path, and returns
//! `kb_core::atlas_field::disagreement(...)` verbatim: Procrustes-aligned,
//! per-doc displacement, largest disagreement first, ids present on only
//! one side dropped rather than invented.

use crate::middleware::error_to_problem_json;
use crate::routes::atlas::atlas_docs_cached;
use crate::state::KbHandles;
use axum::{
    body::Body,
    extract::{Path, State},
    http::{header, HeaderValue, Response, StatusCode},
    response::IntoResponse,
    Json,
};
use kb_core::atlas_field::{self, MachinePoint};
use kb_core::paths::doc_rel_path;
use serde::Serialize;
use serde_json::Value;
use std::path::{Path as FsPath, PathBuf};
use std::sync::Arc;

/// The empty JSON Canvas document — served whenever no sidecar has been
/// written yet. Byte-identical every call (matches `routes::boards`'s
/// `DEFAULT_CANVAS`, and MUST — `kb_core::atlas_field::parse_field` treats
/// them identically, both being the empty-field fixed point).
const DEFAULT_CANVAS: &str = r#"{"nodes":[],"edges":[]}"#;

/// `PUT` body cap — the operator field is a hand-curated overlay (a few
/// dozen island/placement nodes), not a bulk-import target. Mirrors
/// `routes::boards::MAX_CANVAS_BYTES`.
const MAX_CANVAS_BYTES: usize = 256 * 1024;

/// Confirm `bytes` parses as JSON shaped like a JSON Canvas document.
/// Identical rule to `routes::boards::validate_canvas_bytes` — duplicated
/// rather than shared, matching that module's own stance (`kb-cli`'s board
/// vs. list resolvers): two independent sidecars, each self-contained.
fn validate_canvas_bytes(bytes: &[u8]) -> Result<(), kb_core::Error> {
    let v: Value = serde_json::from_slice(bytes)
        .map_err(|e| kb_core::Error::BadRequest(format!("canvas is not valid JSON: {e}")))?;
    let obj = v
        .as_object()
        .ok_or_else(|| kb_core::Error::BadRequest("canvas must be a JSON object".into()))?;
    for key in ["nodes", "edges"] {
        if let Some(val) = obj.get(key) {
            if !val.is_array() {
                return Err(kb_core::Error::BadRequest(format!(
                    "canvas {key:?} must be an array"
                )));
            }
        }
    }
    Ok(())
}

/// The FIXED sidecar path for this kb's operator field. No caller input
/// feeds into this — `source_root` is the corpus root the daemon already
/// resolved for `kb`, so there is nothing here to path-confine against.
fn field_path(source_root: &FsPath) -> PathBuf {
    source_root.join("atlas").join("operator.canvas")
}

/// Wrap `bytes` as a `application/json` response, verbatim. Identical to
/// `routes::boards::json_response`.
fn json_response(status: StatusCode, bytes: Vec<u8>) -> Response<Body> {
    let mut resp = (status, bytes).into_response();
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    resp
}

/// A read-only corpus surfaces as `Error::Io(PermissionDenied)`; map it to
/// 409, same treatment as `routes::boards::map_write_error`.
fn map_write_error(e: kb_core::Error) -> Response<Body> {
    if let kb_core::Error::Io(io_err) = &e {
        if io_err.kind() == std::io::ErrorKind::PermissionDenied {
            return error_to_problem_json(&kb_core::Error::Conflict(
                "atlas field destination is read-only".to_string(),
            ));
        }
    }
    error_to_problem_json(&e)
}

/// Read the sidecar's raw bytes, treating a missing file as
/// [`DEFAULT_CANVAS`] rather than an error. Shared by `get_field` and
/// `disagreement` — both need the "honest empty state, never a 404" read.
fn read_field_bytes(path: &FsPath) -> Result<Vec<u8>, kb_core::Error> {
    match std::fs::read(path) {
        Ok(b) => Ok(b),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Ok(DEFAULT_CANVAS.as_bytes().to_vec())
        }
        Err(e) => Err(kb_core::Error::Storage(format!(
            "read {}: {e}",
            path.display()
        ))),
    }
}

/// `GET /api/kb/{kb}/atlas/field` — the raw JSON Canvas doc, or
/// [`DEFAULT_CANVAS`] when no sidecar has been written yet.
pub async fn get_field(
    State(state): State<Arc<KbHandles>>,
    Path(kb): Path<String>,
) -> Response<Body> {
    let (kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let path = field_path(&ctx.source_path);
    let bytes = match read_field_bytes(&path) {
        Ok(b) => b,
        Err(e) => return error_to_problem_json(&e),
    };
    if let Err(e) = validate_canvas_bytes(&bytes) {
        return error_to_problem_json(&kb_core::Error::Storage(format!(
            "operator atlas field for kb {kb_name} is corrupt: {e}"
        )));
    }
    json_response(StatusCode::OK, bytes)
}

/// `PUT /api/kb/{kb}/atlas/field` — replace the sidecar wholesale. Stored
/// VERBATIM once it passes [`validate_canvas_bytes`] and the size cap.
/// Emits `atlas.field.updated {kb}` on success.
pub async fn put_field(
    State(state): State<Arc<KbHandles>>,
    Path(kb): Path<String>,
    body: String,
) -> Response<Body> {
    let (kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    if body.len() > MAX_CANVAS_BYTES {
        return error_to_problem_json(&kb_core::Error::BadRequest(format!(
            "canvas body too large: {} bytes (cap {MAX_CANVAS_BYTES})",
            body.len()
        )));
    }
    if let Err(e) = validate_canvas_bytes(body.as_bytes()) {
        return error_to_problem_json(&e);
    }
    let path = field_path(&ctx.source_path);
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            return map_write_error(kb_core::Error::Io(e));
        }
    }
    if let Err(e) = kb_core::fsx::write_atomic(&path, body.as_bytes()) {
        return map_write_error(e);
    }
    ctx.bus.emit(
        "atlas.field.updated",
        serde_json::json!({ "kb": kb_name.as_str() }),
    );
    json_response(StatusCode::OK, body.into_bytes())
}

/// One artifact's machine-vs-operator displacement, on the wire. Flattens
/// [`kb_core::atlas_field::Disagreement`]'s `(f32, f32)` pairs into named
/// x/y fields — mirrors `routes::atlas::AtlasAlignmentOut`'s own flattening
/// of `procrustes::Transform` for the same reason (a plain tuple doesn't
/// carry which axis is which across the wire).
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize, PartialEq)]
pub struct AtlasFieldDisagreementOut {
    /// The join key — a source-relative path.
    pub id: String,
    pub machine_x: f32,
    pub machine_y: f32,
    /// The operator's position AFTER Procrustes alignment (directly
    /// comparable to `machine_x`/`machine_y`).
    pub operator_x: f32,
    pub operator_y: f32,
    /// The operator's position AS PLACED, before alignment.
    pub operator_raw_x: f32,
    pub operator_raw_y: f32,
    pub distance: f32,
}

impl From<atlas_field::Disagreement> for AtlasFieldDisagreementOut {
    fn from(d: atlas_field::Disagreement) -> Self {
        AtlasFieldDisagreementOut {
            id: d.id,
            machine_x: d.machine.0,
            machine_y: d.machine.1,
            operator_x: d.operator.0,
            operator_y: d.operator.1,
            operator_raw_x: d.operator_raw.0,
            operator_raw_y: d.operator_raw.1,
            distance: d.distance,
        }
    }
}

/// `GET /api/kb/{kb}/atlas/field/disagreement` response.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize, PartialEq)]
pub struct AtlasFieldDisagreementResponse {
    /// Largest disagreement first (see `kb_core::atlas_field::disagreement`'s
    /// ordering + tie-break doc).
    pub disagreements: Vec<AtlasFieldDisagreementOut>,
    /// How many ids joined on BOTH sides and therefore contributed a row.
    pub matched: usize,
    /// How many `file` placements the operator field itself carries
    /// (before the join) — lets a caller distinguish "empty field" from
    /// "field is populated but shares no ids with the machine layout".
    pub operator_placements: usize,
}

/// `GET /api/kb/{kb}/atlas/field/disagreement` — machine-vs-operator
/// displacement (W3 F-b). Reads the sidecar the same tolerant way `get_field`
/// does (missing file → the empty field, never a 404), reads the machine
/// layout from the SAME memoised full-corpus scan `routes::atlas::points`
/// uses (so this and the map agree on the same coordinates + generation),
/// joins by source-relative path, and returns
/// `kb_core::atlas_field::disagreement` verbatim.
pub async fn disagreement(
    State(state): State<Arc<KbHandles>>,
    Path(kb): Path<String>,
) -> Response<Body> {
    let (_kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };

    let path = field_path(&ctx.source_path);
    let bytes = match read_field_bytes(&path) {
        Ok(b) => b,
        Err(e) => return error_to_problem_json(&e),
    };
    // `parse_field` NEVER fails (see its doc comment) — a corrupt sidecar
    // is treated as an empty field here, same tolerance `get_field`'s own
    // `validate_canvas_bytes` gate is stricter about (that route 500s on a
    // corrupt on-disk file; this one just has nothing to score).
    let field = atlas_field::parse_field(&String::from_utf8_lossy(&bytes));
    let operator_placements = field.placements().count();

    let generation = ctx.storage.index_generation();
    let docs = match atlas_docs_cached(ctx, generation).await {
        Ok(d) => d,
        Err(e) => return error_to_problem_json(&e),
    };
    let machine: Vec<MachinePoint> = docs
        .iter()
        .filter_map(|d| {
            let (Some(x), Some(y)) = (d.atlas_x, d.atlas_y) else {
                return None;
            };
            Some(MachinePoint {
                id: doc_rel_path(&d.path, &ctx.source_path),
                x,
                y,
            })
        })
        .collect();

    let rows = atlas_field::disagreement(&machine, &field);
    let matched = rows.len();
    Json(AtlasFieldDisagreementResponse {
        disagreements: rows
            .into_iter()
            .map(AtlasFieldDisagreementOut::from)
            .collect(),
        matched,
        operator_placements,
    })
    .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    // invariant:25 — an atlas-field write must never look like a list-row
    // or index mutation; these pin the pure helpers the routes build on.

    #[test]
    fn validate_canvas_bytes_accepts_the_default() {
        assert!(validate_canvas_bytes(DEFAULT_CANVAS.as_bytes()).is_ok());
    }

    #[test]
    fn validate_canvas_bytes_rejects_non_json() {
        assert!(validate_canvas_bytes(b"not json").is_err());
    }

    #[test]
    fn validate_canvas_bytes_rejects_non_array_nodes() {
        assert!(validate_canvas_bytes(br#"{"nodes":"oops"}"#).is_err());
    }

    #[test]
    fn field_path_is_fixed_under_an_atlas_dir() {
        let root = FsPath::new("/tmp/kb-corpus");
        let p = field_path(root);
        assert_eq!(p, root.join("atlas").join("operator.canvas"));
    }

    #[test]
    fn max_canvas_bytes_is_256_kib() {
        assert_eq!(MAX_CANVAS_BYTES, 256 * 1024);
    }

    #[test]
    fn read_field_bytes_treats_missing_as_default() {
        let missing =
            FsPath::new("/tmp/kb-corpus-does-not-exist-atlas-field/atlas/operator.canvas");
        let bytes = read_field_bytes(missing).unwrap();
        assert_eq!(bytes, DEFAULT_CANVAS.as_bytes());
    }
}
