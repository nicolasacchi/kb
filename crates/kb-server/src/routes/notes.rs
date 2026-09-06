//! `notes` — free-standing notes / todo-lists (N-track) attached to a kb
//! or a folder within a kb. A note is an ordinary Markdown artifact carrying
//! `kb-category: note`, so it's searchable, commentable, and versioned for
//! free; these routes add the one thing artifacts can't otherwise do — edit
//! a note's body (toggle a checkbox, rewrite text).
//!
//! Routes:
//!
//! ```text
//! GET    /api/notes[?folder=&status=]            → cross-kb list
//! GET    /api/kb/{kb}/notes[?folder=&status=]    → per-kb list
//! GET    /api/kb/{kb}/notes/{id}                 → one note (raw body_md)
//! POST   /api/kb/{kb}/notes                      → create {folder?,title?,body_md?,tags?,status?,notepad?}
//! PATCH  /api/kb/{kb}/notes/{id}                 → update {title?,body_md?,status?,tags?}
//! POST   /api/kb/{kb}/notes/{id}/toggle          → {index,on} flip Nth task
//! POST   /api/kb/{kb}/notes/{id}/tasks           → {text} append a task
//! DELETE /api/kb/{kb}/notes/{id}                 → remove file + drop row
//! ```
//!
//! SSE: `note.created` / `note.updated` / `note.deleted` carry
//! `{kb, id, path?, folder?, is_notepad?}`. Writes follow the memory-ingest
//! pattern (write the file atomically, let the watcher re-index — a rewrite's
//! new content hash dodges the dedup gate); the route emits optimistically and
//! `artifact.indexed` is the authoritative confirm. Mutate endpoints return
//! the updated note so the SPA reconciles without waiting on the re-index.

use crate::middleware::error_to_problem_json;
use crate::routes::links::{load_backlinks, resolve_outgoing, NoteLinks, ResolvedLink};
use crate::state::KbHandles;
use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::{Response, StatusCode},
    response::IntoResponse,
    Json,
};
use kb_core::docs_query::{folder_matches, DocRow};
use kb_core::ids::ArtifactId;
use kb_core::notes::{self, note_summary_from_row, NoteFields, NoteSummary};
use kb_core::paths::{doc_folder, doc_rel_path};
use kb_core::storage::lance::DocSummary;
use kb_core::types::KbName;
use serde::{Deserialize, Serialize};
use std::path::Path as FsPath;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Default, Deserialize)]
pub struct ListQuery {
    /// Descendant-inclusive folder filter (same semantics as the gallery).
    pub folder: Option<String>,
    /// Exact-match `kb-status` filter (`active`/`done`/`archived`/…).
    pub status: Option<String>,
}

#[cfg_attr(
    feature = "ts-export",
    derive(ts_rs::TS),
    ts(export, rename = "NotesListResponse")
)]
#[derive(Debug, Serialize)]
pub struct ListResponse {
    pub notes: Vec<NoteSummary>,
}

#[cfg_attr(
    feature = "ts-export",
    derive(ts_rs::TS),
    ts(export, rename = "CreateNoteBody")
)]
#[derive(Debug, Deserialize)]
pub struct CreateBody {
    /// Scope folder ("" / absent = kb root). The note file lands here.
    #[serde(default)]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub folder: Option<String>,
    #[serde(default)]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub title: Option<String>,
    #[serde(default)]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub body_md: Option<String>,
    #[serde(default)]
    #[cfg_attr(feature = "ts-export", ts(as = "Option<Vec<String>>", optional))]
    pub tags: Vec<String>,
    #[serde(default)]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub status: Option<String>,
    /// When true, write/return the scope's canonical `_notepad.md` instead of
    /// an ad-hoc `note-<slug>-<unix>.md`. Idempotent: an existing notepad is
    /// returned as-is (never overwritten).
    #[serde(default)]
    #[cfg_attr(feature = "ts-export", ts(as = "Option<bool>", optional))]
    pub notepad: bool,
}

#[cfg_attr(
    feature = "ts-export",
    derive(ts_rs::TS),
    ts(export, rename = "CreateNoteResponse")
)]
#[derive(Debug, Serialize)]
pub struct CreateResponse {
    pub id: String,
    pub path: String,
    pub is_notepad: bool,
}

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize)]
pub struct NoteDetail {
    pub id: String,
    pub kb: String,
    pub title: String,
    /// Raw Markdown body (frontmatter stripped) — what the inline editor edits.
    pub body_md: String,
    pub folder: String,
    pub source_relative: String,
    pub is_notepad: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub status: Option<String>,
    pub tags: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub updated_at: Option<i64>,
    pub task_done: u32,
    pub task_total: u32,
    pub comment_count: usize,
    /// Outgoing `[[wikilinks]]` resolved against the corpus, in document order
    /// (deduped by target). The SPA renders the body's `[[…]]` from this map;
    /// empty when the note has no wikilinks.
    pub links: Vec<ResolvedLink>,
}

#[cfg_attr(
    feature = "ts-export",
    derive(ts_rs::TS),
    ts(export, rename = "PatchNoteBody", optional_fields)
)]
#[derive(Debug, Deserialize)]
pub struct UpdateBody {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub body_md: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub tags: Option<Vec<String>>,
}

#[cfg_attr(
    feature = "ts-export",
    derive(ts_rs::TS),
    ts(export, rename = "ToggleNoteTaskBody")
)]
#[derive(Debug, Deserialize)]
pub struct ToggleBody {
    pub index: usize,
    pub on: bool,
}

#[cfg_attr(
    feature = "ts-export",
    derive(ts_rs::TS),
    ts(export, rename = "AppendNoteTaskBody")
)]
#[derive(Debug, Deserialize)]
pub struct AppendTaskBody {
    pub text: String,
}

/// Lightweight response from `toggle` / `tasks` — enough for the SPA to
/// reconcile the rendered checklist + progress bar without a re-fetch.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize)]
pub struct NoteMutate {
    pub id: String,
    pub body_md: String,
    pub task_done: u32,
    pub task_total: u32,
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Slugify + dedup tags exactly as the indexer / meta-patch route does.
fn slug_tags(tags: &[String]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    tags.iter()
        .map(|t| kb_core::parser::slugify_tag(t))
        .filter(|s| !s.is_empty())
        .filter(|s| seen.insert(s.clone()))
        .collect()
}

/// Reject a folder that would escape the corpus root.
fn folder_ok(folder: &str) -> bool {
    !folder.split('/').any(|seg| seg == ".." || seg == ".")
}

/// Build a [`NoteSummary`] from a lance row + source root, stamping the kb.
fn summary(kb: &KbName, doc: DocSummary, source_root: &FsPath) -> NoteSummary {
    let folder = doc_folder(&doc.path, source_root);
    let row = DocRow { doc, folder };
    let mut s = note_summary_from_row(&row, source_root);
    s.kb = kb.as_str().to_string();
    s
}

/// `GET /api/kb/{kb}/notes` — notes in one kb, newest-first.
pub async fn list(
    State(state): State<Arc<KbHandles>>,
    Path(kb): Path<String>,
    Query(q): Query<ListQuery>,
) -> Response<Body> {
    let (kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let rows = match ctx.storage.list_notes(u32::MAX).await {
        Ok(r) => r,
        Err(e) => return error_to_problem_json(&e),
    };
    let mut out: Vec<NoteSummary> = rows
        .into_iter()
        .map(|doc| summary(&kb_name, doc, &ctx.source_path))
        .filter(|n| {
            q.folder
                .as_deref()
                .is_none_or(|f| folder_matches(&n.folder, f))
        })
        .filter(|n| {
            q.status
                .as_deref()
                .is_none_or(|s| n.status.as_deref() == Some(s))
        })
        .collect();
    out.sort_by(|a, b| {
        b.updated_at
            .unwrap_or(i64::MIN)
            .cmp(&a.updated_at.unwrap_or(i64::MIN))
    });
    Json(ListResponse { notes: out }).into_response()
}

/// `GET /api/notes` — cross-kb list (BTreeMap fan-out, newest-first).
pub async fn list_all(
    State(state): State<Arc<KbHandles>>,
    Query(q): Query<ListQuery>,
) -> Response<Body> {
    // FF-D — fan out each kb's note listing concurrently (bounded), flattening
    // in BTreeMap order before the newest-first sort. Pure reads.
    let q = &q;
    let mut futs: Vec<super::CorpusFut<'_, Vec<NoteSummary>>> = Vec::new();
    for (kb_name, ctx) in state.kbs.iter() {
        futs.push(Box::pin(async move {
            let rows = match ctx.storage.list_notes(u32::MAX).await {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!(kb = %kb_name, error = %e, "list_notes failed");
                    return Vec::new();
                }
            };
            rows.into_iter()
                .filter_map(|doc| {
                    let s = summary(kb_name, doc, &ctx.source_path);
                    if let Some(f) = q.folder.as_deref() {
                        if !folder_matches(&s.folder, f) {
                            return None;
                        }
                    }
                    if let Some(st) = q.status.as_deref() {
                        if s.status.as_deref() != Some(st) {
                            return None;
                        }
                    }
                    Some(s)
                })
                .collect()
        }));
    }
    // PF-R1 — the operator-configurable `[server] fanout_cap` (default 8,
    // byte-identical to the old hardcoded `super::FANOUT_CAP`).
    let mut out: Vec<NoteSummary> = super::buffered_join(futs, state.fanout_cap)
        .await
        .into_iter()
        .flatten()
        .collect();
    out.sort_by(|a, b| {
        b.updated_at
            .unwrap_or(i64::MIN)
            .cmp(&a.updated_at.unwrap_or(i64::MIN))
    });
    Json(ListResponse { notes: out }).into_response()
}

/// Fetch a note's lance row, asserting it's a note. Returns the problem+json
/// response on a miss / non-note.
async fn note_row(
    ctx: &crate::state::KbContext,
    kb_name: &KbName,
    id: &str,
) -> Result<DocSummary, Response<Body>> {
    match ctx.storage.get_by_id(id.to_string()).await {
        Ok(Some(d)) if notes::is_note(&d.path, d.kb_category.as_deref()) => Ok(d),
        Ok(_) => Err(error_to_problem_json(&kb_core::Error::NotFound(format!(
            "note {id} in kb {kb_name}"
        )))),
        Err(e) => Err(error_to_problem_json(&e)),
    }
}

/// Resolve a note body's outgoing `[[wikilinks]]` against the corpus. Gated
/// on a literal `[[` so the lookup only runs for notes that actually link —
/// the common note (a todo list) costs nothing. Wave-2 — reads through the
/// per-(kb, index-generation) wikilink memo (`links_index`, invariant #15)
/// instead of running its own `list_docs(u32::MAX)` corpus scan per view.
async fn outgoing_links(
    ctx: &crate::state::KbContext,
    kb_name: &KbName,
    body: &str,
) -> Vec<ResolvedLink> {
    if !body.contains("[[") {
        return Vec::new();
    }
    match crate::routes::links::links_index(ctx).await {
        Ok(index) => resolve_outgoing(kb_name, body, &index),
        Err(e) => {
            tracing::warn!(kb = %kb_name, error = %e, "links index for note links failed");
            Vec::new()
        }
    }
}

fn build_detail(
    kb: &KbName,
    doc: &DocSummary,
    source_root: &FsPath,
    src: &str,
    comment_count: usize,
    links: Vec<ResolvedLink>,
) -> NoteDetail {
    let (fm, body) = notes::split_note_source(src);
    let source_relative = {
        let r = doc_rel_path(&doc.path, source_root);
        if r.is_empty() {
            doc.path.clone()
        } else {
            r
        }
    };
    let (task_done, task_total) = notes::checklist_counts(body);
    let title = fm
        .title
        .clone()
        .or_else(|| kb_core::markdown::first_h1(body))
        .unwrap_or_else(|| "Untitled".to_string());
    NoteDetail {
        id: doc.id.clone(),
        kb: kb.as_str().to_string(),
        title,
        body_md: body.to_string(),
        folder: doc_folder(&doc.path, source_root),
        is_notepad: notes::is_notepad(&source_relative),
        source_relative,
        status: fm.kb_status.clone(),
        tags: fm.kb_tags.clone(),
        updated_at: doc.mtime_unix,
        task_done,
        task_total,
        comment_count,
        links,
    }
}

/// `GET /api/kb/{kb}/notes/{id}` — one note, raw body + frontmatter + counts.
pub async fn get_one(
    State(state): State<Arc<KbHandles>>,
    Path((kb, id)): Path<(String, String)>,
) -> Response<Body> {
    let (kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let doc = match note_row(ctx, &kb_name, &id).await {
        Ok(d) => d,
        Err(resp) => return resp,
    };
    let src = match std::fs::read_to_string(&doc.path) {
        Ok(s) => s,
        Err(e) => {
            return error_to_problem_json(&kb_core::Error::Storage(format!(
                "read {}: {e}",
                doc.path
            )))
        }
    };
    let comment_count = kb_core::review::load(&state.paths.kb_review_file(&kb_name, &id))
        .ok()
        .flatten()
        .map(|r| r.open_count())
        .unwrap_or(0);
    let (_, body) = notes::split_note_source(&src);
    let links = outgoing_links(ctx, &kb_name, body).await;
    Json(build_detail(
        &kb_name,
        &doc,
        &ctx.source_path,
        &src,
        comment_count,
        links,
    ))
    .into_response()
}

/// `POST /api/kb/{kb}/notes` — create a note (or return the scope notepad).
pub async fn create(
    State(state): State<Arc<KbHandles>>,
    Path(kb): Path<String>,
    Json(body): Json<CreateBody>,
) -> Response<Body> {
    let (kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let folder = body
        .folder
        .as_deref()
        .unwrap_or("")
        .trim_matches('/')
        .to_string();
    if !folder_ok(&folder) {
        return error_to_problem_json(&kb_core::Error::BadRequest(
            "folder must not contain '..' or '.' segments".into(),
        ));
    }

    let body_md = body.body_md.unwrap_or_default();
    let tags = slug_tags(&body.tags);
    let nf = NoteFields {
        title: body.title.as_deref(),
        status: body.status.as_deref(),
        tags: &tags,
        body: &body_md,
    };
    let src = notes::compose_note_source(&nf);

    // Pick the path: deterministic notepad, or a collision-safe ad-hoc name.
    let rel_path = if body.notepad {
        notes::notepad_path(&folder)
    } else {
        let slug = notes::slugify(body.title.as_deref().unwrap_or(""));
        let ts = unix_now();
        let mut n = 0;
        loop {
            let p = notes::adhoc_path(&folder, &slug, ts, n);
            if !ctx.source_path.join(&p).exists() {
                break p;
            }
            n += 1;
        }
    };
    let abs = ctx.source_path.join(&rel_path);

    // A notepad that already exists is returned untouched (the "Start
    // notepad" affordance is idempotent — never clobber existing content).
    let already = body.notepad && abs.exists();
    if !already {
        if let Some(parent) = abs.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                return error_to_problem_json(&kb_core::Error::Storage(format!(
                    "create dir {}: {e}",
                    parent.display()
                )));
            }
        }
        if let Err(e) = kb_core::fsx::write_atomic(&abs, src.as_bytes()) {
            return error_to_problem_json(&e);
        }
    }

    // Compute the id the same way the indexer will (canonical rel path).
    let rel = {
        let r = doc_rel_path(&abs.to_string_lossy(), &ctx.source_path);
        if r.is_empty() {
            rel_path.clone()
        } else {
            r
        }
    };
    let id = ArtifactId::from_path(&rel).to_string();

    if !already {
        ctx.bus.emit(
            "note.created",
            serde_json::json!({
                "kb": kb_name.as_str(),
                "id": id,
                "path": rel,
                "folder": folder,
                "is_notepad": body.notepad,
            }),
        );
        nudge_indexer(ctx, &abs).await;
    }
    let code = if already {
        StatusCode::OK
    } else {
        StatusCode::CREATED
    };
    (
        code,
        Json(CreateResponse {
            id,
            path: rel,
            is_notepad: body.notepad,
        }),
    )
        .into_response()
}

/// `PATCH /api/kb/{kb}/notes/{id}` — edit title / body / status / tags.
/// Whole-file rewrite (safe — notes carry no kb-prompt template); the
/// watcher re-indexes. Returns the updated note.
pub async fn update(
    State(state): State<Arc<KbHandles>>,
    Path((kb, id)): Path<(String, String)>,
    Json(body): Json<UpdateBody>,
) -> Response<Body> {
    let (kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let doc = match note_row(ctx, &kb_name, &id).await {
        Ok(d) => d,
        Err(resp) => return resp,
    };
    let src = match std::fs::read_to_string(&doc.path) {
        Ok(s) => s,
        Err(e) => {
            return error_to_problem_json(&kb_core::Error::Storage(format!(
                "read {}: {e}",
                doc.path
            )))
        }
    };

    let mut new_src = src.clone();
    if let Some(b) = &body.body_md {
        new_src = notes::replace_body(&new_src, b);
    }
    if let Some(t) = &body.title {
        let t = t.trim();
        new_src = kb_core::markdown::set_frontmatter_field(
            &new_src,
            "title",
            (!t.is_empty()).then_some(t),
        );
    }
    if let Some(s) = &body.status {
        let s = s.trim();
        new_src = kb_core::markdown::set_frontmatter_field(
            &new_src,
            "kb-status",
            (!s.is_empty()).then_some(s),
        );
    }
    if let Some(tags) = &body.tags {
        let slugged = slug_tags(tags);
        let joined = slugged.join(", ");
        new_src = kb_core::markdown::set_frontmatter_field(
            &new_src,
            "kb-tags",
            (!slugged.is_empty()).then_some(joined.as_str()),
        );
    }

    if new_src != src {
        if let Err(e) = kb_core::fsx::write_atomic(FsPath::new(&doc.path), new_src.as_bytes()) {
            return error_to_problem_json(&e);
        }
        emit_updated(ctx, &kb_name, &id, &doc).await;
    }
    let comment_count = kb_core::review::load(&state.paths.kb_review_file(&kb_name, &id))
        .ok()
        .flatten()
        .map(|r| r.open_count())
        .unwrap_or(0);
    let (_, body) = notes::split_note_source(&new_src);
    let links = outgoing_links(ctx, &kb_name, body).await;
    Json(build_detail(
        &kb_name,
        &doc,
        &ctx.source_path,
        &new_src,
        comment_count,
        links,
    ))
    .into_response()
}

/// `POST /api/kb/{kb}/notes/{id}/toggle` — flip the Nth GFM task line.
pub async fn toggle(
    State(state): State<Arc<KbHandles>>,
    Path((kb, id)): Path<(String, String)>,
    Json(body): Json<ToggleBody>,
) -> Response<Body> {
    let (kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let doc = match note_row(ctx, &kb_name, &id).await {
        Ok(d) => d,
        Err(resp) => return resp,
    };
    let src = match std::fs::read_to_string(&doc.path) {
        Ok(s) => s,
        Err(e) => {
            return error_to_problem_json(&kb_core::Error::Storage(format!(
                "read {}: {e}",
                doc.path
            )))
        }
    };
    let (_, old_body) = notes::split_note_source(&src);
    let new_body = match notes::toggle_task(old_body, body.index, body.on) {
        Some(b) => b,
        None => {
            return error_to_problem_json(&kb_core::Error::NotFound(format!(
                "task index {} out of range",
                body.index
            )))
        }
    };
    finish_body_mutation(&state, ctx, &kb_name, &id, &doc, &src, new_body).await
}

/// `POST /api/kb/{kb}/notes/{id}/tasks` — append an unchecked task.
pub async fn append_task(
    State(state): State<Arc<KbHandles>>,
    Path((kb, id)): Path<(String, String)>,
    Json(body): Json<AppendTaskBody>,
) -> Response<Body> {
    let (kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let text = body.text.trim();
    if text.is_empty() {
        return error_to_problem_json(&kb_core::Error::BadRequest(
            "task text must not be empty".into(),
        ));
    }
    let doc = match note_row(ctx, &kb_name, &id).await {
        Ok(d) => d,
        Err(resp) => return resp,
    };
    let src = match std::fs::read_to_string(&doc.path) {
        Ok(s) => s,
        Err(e) => {
            return error_to_problem_json(&kb_core::Error::Storage(format!(
                "read {}: {e}",
                doc.path
            )))
        }
    };
    let (_, old_body) = notes::split_note_source(&src);
    let new_body = notes::append_task(old_body, text);
    finish_body_mutation(&state, ctx, &kb_name, &id, &doc, &src, new_body).await
}

/// Shared tail for toggle / append: recompose, write, emit, return counts.
async fn finish_body_mutation(
    _state: &Arc<KbHandles>,
    ctx: &crate::state::KbContext,
    kb_name: &KbName,
    id: &str,
    doc: &DocSummary,
    src: &str,
    new_body: String,
) -> Response<Body> {
    let new_src = notes::replace_body(src, &new_body);
    if new_src != src {
        if let Err(e) = kb_core::fsx::write_atomic(FsPath::new(&doc.path), new_src.as_bytes()) {
            return error_to_problem_json(&e);
        }
        emit_updated(ctx, kb_name, id, doc).await;
    }
    let (task_done, task_total) = notes::checklist_counts(&new_body);
    Json(NoteMutate {
        id: id.to_string(),
        body_md: new_body,
        task_done,
        task_total,
    })
    .into_response()
}

/// `DELETE /api/kb/{kb}/notes/{id}` — remove the file + drop the row.
pub async fn delete(
    State(state): State<Arc<KbHandles>>,
    Path((kb, id)): Path<(String, String)>,
) -> Response<Body> {
    let (kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let doc = match note_row(ctx, &kb_name, &id).await {
        Ok(d) => d,
        Err(resp) => return resp,
    };
    match std::fs::remove_file(&doc.path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            return error_to_problem_json(&kb_core::Error::Storage(format!(
                "remove {}: {e}",
                doc.path
            )))
        }
    }
    let _ = ctx
        .storage
        .delete_by_path(std::path::PathBuf::from(&doc.path))
        .await;
    ctx.bus.emit(
        "note.deleted",
        serde_json::json!({ "kb": kb_name.as_str(), "id": id }),
    );
    StatusCode::NO_CONTENT.into_response()
}

/// `GET /api/kb/{kb}/notes/{id}/links` — a note's outgoing wikilinks
/// (resolved) + its backlinks in one call. Drives `kb notes links`. The note
/// detail already carries `links` (outgoing) for inline rendering, so the SPA
/// reads backlinks from the generic `/backlinks/{id}` endpoint instead.
pub async fn note_links(
    State(state): State<Arc<KbHandles>>,
    Path((kb, id)): Path<(String, String)>,
) -> Response<Body> {
    let (kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let doc = match note_row(ctx, &kb_name, &id).await {
        Ok(d) => d,
        Err(resp) => return resp,
    };
    let src = match std::fs::read_to_string(&doc.path) {
        Ok(s) => s,
        Err(e) => {
            return error_to_problem_json(&kb_core::Error::Storage(format!(
                "read {}: {e}",
                doc.path
            )))
        }
    };
    let (_, body) = notes::split_note_source(&src);
    let outgoing = outgoing_links(ctx, &kb_name, body).await;
    let backlinks = load_backlinks(ctx, &kb_name, &id).await;
    Json(NoteLinks {
        outgoing,
        backlinks,
    })
    .into_response()
}

/// `pub(crate)` for CT-F3's `links::apply_unlinked`, which edits a note's
/// body through the same read → splice → `write_atomic` shape the toggle /
/// append verbs use and must emit the same event + indexer nudge.
pub(crate) async fn emit_updated(
    ctx: &crate::state::KbContext,
    kb_name: &KbName,
    id: &str,
    doc: &DocSummary,
) {
    ctx.bus.emit(
        "note.updated",
        serde_json::json!({
            "kb": kb_name.as_str(),
            "id": id,
            "folder": doc_folder(&doc.path, &ctx.source_path),
        }),
    );
    nudge_indexer(ctx, FsPath::new(&doc.path)).await;
}

/// Emit a `watch.modify` envelope so the indexer re-reads the file
/// immediately — independent of the inotify watcher. Notes can be written
/// into a *new* subfolder whose recursive watch wasn't registered yet (a
/// race the corpus-root-only memory ingest never hits); the explicit nudge
/// makes "create → searchable" deterministic. The content-hash dedup gate
/// makes a duplicate watcher event for the same bytes a no-op, so this is
/// safe even when inotify also catches the write.
pub(crate) async fn nudge_indexer(ctx: &crate::state::KbContext, abs: &FsPath) {
    // G7 — push through the ingest sink (the indexer no longer reads `watch.*`
    // off the bus). The sink mirrors to the bus too, so observers still see the
    // `watch.modify`. force=false: a note write changes the bytes, so the
    // content-hash gate correctly lets it through on its own.
    ctx.ingest
        .send(
            kb_core::indexer::WatchKind::Modified,
            abs.to_path_buf(),
            false,
        )
        .await;
}
