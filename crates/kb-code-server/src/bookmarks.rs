//! Phase N ("kb-code v3 — Navigate") — durable, per-repo bookmarks: places
//! (path + line), not conversations (annotations already cover those).
//! Optional single-char mnemonic (`[0-9a-z]`) with vim-style delete-then-set
//! move semantics when reassigned within a repo.
//!
//! # Wire routes
//!
//! `GET /api/bookmarks?repo=` ([`list_bookmarks`]) · `POST /api/bookmarks`
//! ([`create_bookmark`]) · `PATCH /api/bookmarks/{id}` ([`patch_bookmark`])
//! · `DELETE /api/bookmarks/{id}` ([`delete_bookmark`]). Every mutation
//! emits `bookmark.changed {repo}` on `state.bus` — same pattern as
//! `set.changed` (`reading_sets::create_set`).

use crate::routes::{find_repo, safe_rel_path, ApiError};
use crate::state::SharedState;
use crate::store;
use crate::store::StoreBlocking;
use axum::extract::{Path as AxumPath, Query, State};
use axum::http::{header, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};

pub const SCHEMA: &str = "bookmarks/1";

// --- wire shapes --------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct ListBookmarksParams {
    pub repo: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct BookmarkOut {
    pub id: i64,
    pub repo: String,
    pub path: String,
    pub line: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mnemonic: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

impl From<store::BookmarkRow> for BookmarkOut {
    fn from(r: store::BookmarkRow) -> Self {
        BookmarkOut {
            id: r.id,
            repo: r.repo,
            path: r.path,
            line: r.line,
            mnemonic: r.mnemonic,
            note: r.note,
            created_at: r.created_at,
            updated_at: r.updated_at,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct BookmarksListOut {
    pub schema: &'static str,
    pub bookmarks: Vec<BookmarkOut>,
}

/// `GET /api/bookmarks?repo=` — every bookmark in `repo`, mnemonic-first
/// then `created_at` (`store::Store::list_bookmarks`'s own order).
pub async fn list_bookmarks(
    State(state): State<SharedState>,
    Query(params): Query<ListBookmarksParams>,
) -> Result<impl IntoResponse, ApiError> {
    let (_repo, _repo_id) = find_repo(&state, &params.repo)?;
    let repo_name = params.repo.clone();
    // 2026-08-31 incident (store.rs module doc): single store call, still
    // wrapped so it can never park this async worker on the mutex wait.
    let rows = state
        .store
        .run_blocking(move |store| store.list_bookmarks(&repo_name))
        .await?;
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(BookmarksListOut {
            schema: SCHEMA,
            bookmarks: rows.into_iter().map(BookmarkOut::from).collect(),
        }),
    ))
}

// --- POST /api/bookmarks ------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct CreateBookmarkBody {
    pub repo: String,
    pub path: String,
    pub line: u32,
    #[serde(default)]
    pub mnemonic: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
}

/// Validate a mnemonic is exactly one `[0-9a-z]` character. `None` is fine
/// (anonymous bookmark). Returns the validated single-char string, or a
/// 422 with the same `{ "error": ... }` shape annotations use
/// (`ApiError` → `IntoResponse`).
pub fn validate_mnemonic(m: Option<&str>) -> Result<Option<String>, ApiError> {
    let Some(raw) = m else {
        return Ok(None);
    };
    let mut chars = raw.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) if c.is_ascii_digit() || c.is_ascii_lowercase() => Ok(Some(c.to_string())),
        _ => Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("invalid mnemonic {raw:?}: must be a single [0-9a-z] character"),
        )),
    }
}

/// `POST /api/bookmarks` — create. If `mnemonic` is set and already taken
/// in the repo, the mnemonic MOVES to the new bookmark (delete-then-set).
/// `404` unknown repo; `422` invalid mnemonic; `400` bad path/line.
pub async fn create_bookmark(
    State(state): State<SharedState>,
    Json(body): Json<CreateBookmarkBody>,
) -> Result<impl IntoResponse, ApiError> {
    let (repo, _repo_id) = find_repo(&state, &body.repo)?;
    safe_rel_path(&body.path)?;
    if body.line < 1 {
        return Err(ApiError::bad_request("line must be >= 1 (1-based)"));
    }
    let mnemonic = validate_mnemonic(body.mnemonic.as_deref())?;
    let now = chrono::Utc::now().timestamp();
    let repo_name = repo.name.clone();
    let path = body.path.clone();
    let line = i64::from(body.line);
    let note = body.note.clone();
    // 2026-08-31 incident (store.rs module doc): create + read-back run as
    // one closure; `bus.emit` doesn't touch the store, so it stays
    // outside, after.
    let repo_name_bg = repo_name.clone();
    let row = state
        .store
        .run_blocking(move |store| {
            let id = store.create_bookmark(
                &repo_name_bg,
                &path,
                line,
                mnemonic.as_deref(),
                note.as_deref(),
                now,
            )?;
            store.get_bookmark(id)?.ok_or_else(|| {
                ApiError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("bookmark {id} vanished immediately after create"),
                )
            })
        })
        .await?;
    state
        .bus
        .emit("bookmark.changed", serde_json::json!({ "repo": repo_name }));
    Ok((
        StatusCode::CREATED,
        [(header::CACHE_CONTROL, "no-store")],
        Json(BookmarkOut::from(row)),
    ))
}

// --- PATCH /api/bookmarks/{id} ------------------------------------------

/// Serde helper: distinguish absent (outer `None`) from JSON `null`
/// (inner `None`). Pair with `#[serde(default)]`. Same shape as
/// `kb_server::routes::lists::double_option`.
fn double_option<'de, T, D>(de: D) -> Result<Option<Option<T>>, D::Error>
where
    T: serde::Deserialize<'de>,
    D: serde::Deserializer<'de>,
{
    serde::Deserialize::deserialize(de).map(Some)
}

#[derive(Debug, Deserialize)]
pub struct PatchBookmarkBody {
    #[serde(default)]
    pub line: Option<u32>,
    #[serde(default, deserialize_with = "double_option")]
    pub note: Option<Option<String>>,
    /// Absent = leave alone; `null` = clear; string = set (move semantics).
    #[serde(default, deserialize_with = "double_option")]
    pub mnemonic: Option<Option<String>>,
}

/// `PATCH /api/bookmarks/{id}` — update line/note/mnemonic. Explicit null
/// on `mnemonic` clears it; a new mnemonic MOVES (deletes any other owner
/// in the same repo). `404` unknown id; `422` invalid mnemonic.
pub async fn patch_bookmark(
    State(state): State<SharedState>,
    AxumPath(id): AxumPath<i64>,
    Json(body): Json<PatchBookmarkBody>,
) -> Result<impl IntoResponse, ApiError> {
    if let Some(line) = body.line {
        if line < 1 {
            return Err(ApiError::bad_request("line must be >= 1 (1-based)"));
        }
    }
    let mnemonic = match &body.mnemonic {
        None => None,
        Some(None) => Some(None),
        Some(Some(m)) => Some(validate_mnemonic(Some(m))?),
    };
    let note = body.note.clone();
    let now = chrono::Utc::now().timestamp();
    // 2026-08-31 incident (store.rs module doc): existence check, update,
    // and read-back are four sequential store calls with no async work
    // between them — one closure. `bus.emit` doesn't touch the store, so
    // it stays outside, after.
    let (row, repo_name) = state
        .store
        .run_blocking(move |store| {
            let existing = store
                .get_bookmark(id)?
                .ok_or_else(|| ApiError::not_found(format!("bookmark {id}")))?;
            if !store.update_bookmark(
                id,
                body.line.map(i64::from),
                note.as_ref().map(|opt| opt.as_deref()),
                mnemonic.as_ref().map(|opt| opt.as_deref()),
                now,
            )? {
                return Err(ApiError::not_found(format!("bookmark {id}")));
            }
            let row = store.get_bookmark(id)?.ok_or_else(|| {
                ApiError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("bookmark {id} vanished immediately after update"),
                )
            })?;
            Ok::<_, ApiError>((row, existing.repo))
        })
        .await?;
    state
        .bus
        .emit("bookmark.changed", serde_json::json!({ "repo": repo_name }));
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(BookmarkOut::from(row)),
    ))
}

// --- DELETE /api/bookmarks/{id} -----------------------------------------

/// `DELETE /api/bookmarks/{id}` — `404` unknown id. Emits
/// `bookmark.changed {repo}` after the delete.
pub async fn delete_bookmark(
    State(state): State<SharedState>,
    AxumPath(id): AxumPath<i64>,
) -> Result<impl IntoResponse, ApiError> {
    // 2026-08-31 incident (store.rs module doc): existence check + delete
    // run as one closure; `bus.emit` doesn't touch the store, so it stays
    // outside, after.
    let repo = state
        .store
        .run_blocking(move |store| {
            let row = store
                .get_bookmark(id)?
                .ok_or_else(|| ApiError::not_found(format!("bookmark {id}")))?;
            store.delete_bookmark(id)?;
            Ok::<_, ApiError>(row.repo)
        })
        .await?;
    state
        .bus
        .emit("bookmark.changed", serde_json::json!({ "repo": repo }));
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_mnemonic_accepts_single_alnum_rejects_rest() {
        assert_eq!(validate_mnemonic(None).unwrap(), None);
        assert_eq!(validate_mnemonic(Some("a")).unwrap().as_deref(), Some("a"));
        assert_eq!(validate_mnemonic(Some("0")).unwrap().as_deref(), Some("0"));
        assert_eq!(validate_mnemonic(Some("z")).unwrap().as_deref(), Some("z"));
        assert!(validate_mnemonic(Some("A")).is_err()); // uppercase rejected
        assert!(validate_mnemonic(Some("ab")).is_err());
        assert!(validate_mnemonic(Some("")).is_err());
        assert!(validate_mnemonic(Some("_")).is_err());
        let err = validate_mnemonic(Some("A")).unwrap_err();
        assert!(
            err.message().contains("invalid mnemonic"),
            "422-class error message: {}",
            err.message()
        );
    }
}
