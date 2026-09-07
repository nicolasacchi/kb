//! V71-G0 — `kbc-seq/1`: the sequence PROJECTION layer.
//!
//! Design §P6: "Reading sets, workspaces, tours, trails, boards and the
//! question stack are six projections of one shape: an ordered list of
//! anchored references … In v7.1 kbc-seq/1 lands as a projection layer:
//! one wire schema with a required projection discriminator … and a
//! resolver over the three existing tables." This module is that resolver's
//! READ half. It is deliberately a LAYER: no table is created, moved or
//! merged here, and every row it returns is still owned (and still
//! mutated) by the family that already owns it — `reading_sets` for
//! sets/workspaces/tours/trails, `canvas_sets` for boards. The physical
//! unification is its own later unit, with the full migration treatment.
//!
//! ## The discriminator, and the aliases
//!
//! [`PROJECTIONS`] is the closed vocabulary; [`resolve_projection`] also
//! accepts the familiar plural/legacy names as DOCUMENTED aliases (design
//! §P6: "the familiar names … kept as documented aliases"), so `?projection=
//! boards` and `?projection=canvas` both mean `board`. An unknown value is
//! a 400 naming the whole vocabulary — never a silent empty list, which is
//! the shape a typo takes when a filter is applied without validation.
//!
//! Four of the five projections are a `reading_sets.kind`
//! (`reading_sets::SET_KINDS`, widened from two to four by this unit) —
//! which is why a tour and a trail need no table of their own: invariant 9's
//! "a `reading_sets` row with `kind = 'workspace'` IS a workspace, not a
//! new entity", applied to the two remaining names.
//!
//! ## What this unit does NOT ship (cut, with the reason)
//!
//! - The MUTATION family (`kb-code seq apply -f - --projection board`).
//!   Every projection already has its own create/patch route with its own
//!   validation; a second write door would be a second copy of that
//!   validation before the physical merge has decided which one survives.
//!   The read layer is what unblocks G0's own consumers (a workspace's
//!   projections, the CLI's `seq list`).
//! - `canvas_sets.workspace_id`. Boards ARE listed as projections, but
//!   nothing in this unit would write a workspace binding for one, and a
//!   column with no writer is the v7.0 dead-surface defect. A `?workspace=`
//!   filter therefore excludes boards outright and SAYS SO in `notes` —
//!   the honest degrade, not a silent absence.
//!
//! ## V74-L1 amendment
//!
//! The `board` projection now resolves out of TWO tables — `canvas_sets`
//! (v3.4-C1's opaque fragment canvases) and `canvas_boards` (kbc-canvas/1's
//! reference boards). Nothing about this module's posture changes: it still
//! creates, moves and merges nothing, and `source` still names the table
//! each row physically lives in. The workspace note above is unchanged —
//! neither board table carries a `workspace_id`.

use crate::routes::{find_repo, ApiError};
use crate::state::SharedState;
use crate::store::StoreBlocking;
use axum::extract::{Query, State};
use axum::http::header;
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};

pub const SEQ_SCHEMA: &str = "kbc-seq/1";

pub const PROJECTION_SET: &str = "set";
pub const PROJECTION_WORKSPACE: &str = "workspace";
pub const PROJECTION_TOUR: &str = "tour";
pub const PROJECTION_TRAIL: &str = "trail";
pub const PROJECTION_BOARD: &str = "board";

/// The closed discriminator vocabulary, in the order `GET /api/seq` lists
/// them.
pub const PROJECTIONS: [&str; 5] = [
    PROJECTION_SET,
    PROJECTION_WORKSPACE,
    PROJECTION_TOUR,
    PROJECTION_TRAIL,
    PROJECTION_BOARD,
];

/// Documented aliases → canonical projection. Plurals (what a human types)
/// plus `canvas` (what the board's own table and routes are called).
pub const PROJECTION_ALIASES: [(&str, &str); 6] = [
    ("sets", PROJECTION_SET),
    ("workspaces", PROJECTION_WORKSPACE),
    ("tours", PROJECTION_TOUR),
    ("trails", PROJECTION_TRAIL),
    ("boards", PROJECTION_BOARD),
    ("canvas", PROJECTION_BOARD),
];

/// Canonicalise a caller-supplied projection name. Pure; the ONLY place
/// the alias table is consulted.
pub fn resolve_projection(raw: &str) -> Option<&'static str> {
    let s = raw.trim();
    if let Some(p) = PROJECTIONS.iter().find(|p| **p == s) {
        return Some(p);
    }
    PROJECTION_ALIASES
        .iter()
        .find(|(alias, _)| *alias == s)
        .map(|(_, canonical)| *canonical)
}

/// `true` when this projection's rows live in `reading_sets` (everything
/// but `board`) — the resolver's one dispatch decision, named so the route
/// and its tests share it.
pub fn is_reading_set_projection(projection: &str) -> bool {
    projection != PROJECTION_BOARD
}

#[derive(Debug, Deserialize)]
pub struct SeqParams {
    pub repo: String,
    /// One of [`PROJECTIONS`] or an alias; absent = every projection.
    #[serde(default)]
    pub projection: Option<String>,
    /// A workspace's `reading_sets.id` — lists only the projections bound
    /// to it.
    #[serde(default)]
    pub workspace: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SeqProjectionOut {
    /// The discriminator — always canonical on the wire, never an alias.
    pub projection: String,
    pub id: String,
    pub name: String,
    /// Number of ordered references the projection carries (spans, for a
    /// `reading_sets`-backed one). `null` for a `canvas_sets` board, whose
    /// geometry payload is opaque to this daemon (invariant:
    /// `canvas_sets.payload` is never parsed server-side) — an unknown
    /// count, never a 0 that would read as "empty". V74-L1's own
    /// `canvas_boards` rows DO carry a true node count, because their
    /// nodes are rows rather than an opaque blob.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "ref")]
    pub ref_label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    /// The table this projection is still physically stored in — the
    /// layer's honesty about being a layer.
    pub source: &'static str,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct SeqListOut {
    pub schema: &'static str,
    pub repo: String,
    /// Every projection name this daemon understands, so a caller never
    /// has to guess the vocabulary from the rows it happens to see.
    pub projections_available: Vec<&'static str>,
    pub projections: Vec<SeqProjectionOut>,
    pub notes: Vec<String>,
}

/// `GET /api/seq?repo=[&projection=][&workspace=]` — every sequence
/// projection in one repo, resolved over the tables that already own them.
pub async fn seq_route(
    State(state): State<SharedState>,
    Query(params): Query<SeqParams>,
) -> Result<impl IntoResponse, ApiError> {
    let (repo, repo_id) = find_repo(&state, &params.repo)?;
    let repo_name = repo.name.clone();
    let projection = match &params.projection {
        Some(raw) => Some(resolve_projection(raw).ok_or_else(|| {
            ApiError::bad_request(format!(
                "unknown projection {raw:?} — expected one of {}",
                PROJECTIONS.join(", ")
            ))
        })?),
        None => None,
    };
    let workspace = params
        .workspace
        .as_deref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());

    let want_reading_sets = projection.map(is_reading_set_projection).unwrap_or(true);
    // A board carries no workspace binding yet (see the module doc), so a
    // workspace filter can only ever exclude every board.
    let want_boards = projection
        .map(|p| !is_reading_set_projection(p))
        .unwrap_or(true)
        && workspace.is_none();
    // V74-L3b — a kbc-tour/1 tour is ALSO a `canvas_boards` row (V0039's
    // `kind` column), so the `tour` projection now resolves out of two
    // tables: `reading_sets` (kind='tour', V71-G0's own widening) and
    // `canvas_boards` (kind='tour'). Exactly the shape the `board`
    // projection already has after V74-L1, and the layer is unchanged:
    // nothing is created, moved or merged, and `source` still names the
    // table each row physically lives in. A canvas tour carries no
    // workspace binding either, so `?workspace=` excludes it for the same
    // reason it excludes a board.
    let want_canvas_tours =
        projection.map(|p| p == PROJECTION_TOUR).unwrap_or(true) && workspace.is_none();

    let kind_filter = projection.filter(|p| is_reading_set_projection(p));
    let workspace_q = workspace.clone();
    let mut rows = state
        .store
        .run_blocking(move |store| {
            let mut rows: Vec<crate::store::SeqProjectionRow> = Vec::new();
            if want_reading_sets {
                rows.extend(store.seq_reading_sets(
                    repo_id,
                    kind_filter,
                    workspace_q.as_deref(),
                )?);
            }
            if want_boards {
                // V74-L1 — TWO sources for the ONE `board` projection: the
                // v3.4-C1 `canvas_sets` fragment canvases and kbc-canvas/1's
                // own `canvas_boards`. Still a LAYER (invariant 14):
                // nothing is created, moved or merged here, and each row
                // reports the table it physically lives in. A kbc-canvas/1
                // board reports a TRUE node count where a `canvas_sets` row
                // still honestly reports `null` (its payload is opaque to
                // this daemon and always was).
                rows.extend(store.seq_canvas_sets(repo_id)?);
                rows.extend(store.seq_canvas_boards(
                    repo_id,
                    crate::tours::BOARD_KIND_BOARD,
                    PROJECTION_BOARD,
                )?);
            }
            if want_canvas_tours {
                rows.extend(store.seq_canvas_boards(
                    repo_id,
                    crate::tours::BOARD_KIND_TOUR,
                    PROJECTION_TOUR,
                )?);
            }
            Ok::<_, ApiError>(rows)
        })
        .await?;
    // One stable order across both source tables: projection vocabulary
    // order, then name.
    rows.sort_by(|a, b| {
        projection_rank(&a.projection)
            .cmp(&projection_rank(&b.projection))
            .then_with(|| a.name.cmp(&b.name))
    });

    let mut notes: Vec<String> = Vec::new();
    if workspace.is_some() {
        notes.push(
            "boards and kbc-tour/1 tours carry no workspace binding yet — ?workspace= \
             excludes every one of them"
                .to_string(),
        );
    }
    if rows.is_empty() {
        notes.push(format!(
            "no sequence projections in {repo_name} match this query"
        ));
    }

    let out = SeqListOut {
        schema: SEQ_SCHEMA,
        repo: repo_name,
        projections_available: PROJECTIONS.to_vec(),
        projections: rows
            .into_iter()
            .map(|r| SeqProjectionOut {
                projection: r.projection,
                id: r.id,
                name: r.name,
                size: r.size,
                ref_label: r.ref_label,
                workspace_id: r.workspace_id,
                source: r.source,
                updated_at: r.updated_at,
            })
            .collect(),
        notes,
    };
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(out)))
}

fn projection_rank(p: &str) -> usize {
    PROJECTIONS
        .iter()
        .position(|x| *x == p)
        .unwrap_or(usize::MAX)
}

pub const SEQ_ROUTE: crate::entities::RouteContract = crate::entities::RouteContract {
    path: "/api/seq",
    handler: "seq::seq_route",
    required_params: &["repo"],
    params_accept_without: seq_params_accept_without,
};

fn seq_params_accept_without(omit: &str) -> bool {
    let mut map = serde_json::Map::new();
    for (k, v) in [
        ("repo", "r"),
        ("projection", "board"),
        ("workspace", "set_1"),
    ] {
        if k != omit {
            map.insert(k.to_string(), serde_json::Value::String(v.to_string()));
        }
    }
    serde_json::from_value::<SeqParams>(serde_json::Value::Object(map)).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_projection_and_alias_canonicalises_and_an_unknown_one_does_not() {
        for p in PROJECTIONS {
            assert_eq!(resolve_projection(p), Some(p));
        }
        for (alias, canonical) in PROJECTION_ALIASES {
            assert_eq!(
                resolve_projection(alias),
                Some(canonical),
                "alias {alias:?} must canonicalise"
            );
            assert!(
                PROJECTIONS.contains(&canonical),
                "alias {alias:?} points at {canonical:?}, which is not in the vocabulary"
            );
        }
        assert_eq!(resolve_projection("workspaceZ"), None);
        assert_eq!(resolve_projection(""), None);
        assert_eq!(resolve_projection(" board "), Some(PROJECTION_BOARD));
    }

    #[test]
    fn every_reading_set_projection_is_a_valid_set_kind_and_the_board_is_not() {
        for p in PROJECTIONS {
            if is_reading_set_projection(p) {
                assert!(
                    crate::reading_sets::is_valid_set_kind(p),
                    "{p:?} is resolved out of reading_sets but is not a valid set kind — the \
                     kind filter would silently match nothing"
                );
            } else {
                assert!(!crate::reading_sets::is_valid_set_kind(p));
            }
        }
        // …and the reverse: every set kind is addressable as a projection,
        // or a whole family of rows would be invisible to this layer.
        for k in crate::reading_sets::SET_KINDS {
            assert!(
                PROJECTIONS.contains(&k),
                "set kind {k:?} has no projection — its rows would never appear in kbc-seq/1"
            );
        }
    }

    #[test]
    fn the_projection_order_is_the_declared_vocabulary_order() {
        assert_eq!(projection_rank(PROJECTION_SET), 0);
        assert_eq!(projection_rank(PROJECTION_BOARD), PROJECTIONS.len() - 1);
        assert_eq!(projection_rank("nonsense"), usize::MAX);
    }
}
