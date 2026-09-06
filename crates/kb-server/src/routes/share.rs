//! `kb share` HTTP surface — the daemon-side entry to the kb-core share
//! engine, used by both the CLI and the SPA "Share…" button.
//!
//! - `POST /api/kb/{kb}/share` — publish (deploy + gate) → URL.
//! - `GET  /api/kb/{kb}/shares` — list recorded shares.
//! - `DELETE /api/kb/{kb}/shares/{name}` — revoke (teardown + drop row).
//!
//! The engine runs server-side; the Cloudflare/GitHub API tokens come from
//! the *daemon's* environment (resolved when the backend is built from
//! `[share.*]`), not from the request. Validation (host/gate combo) 400s
//! before any backend is constructed.

use crate::middleware::error_to_problem_json;
use crate::state::KbHandles;
use axum::{
    extract::{Path, State},
    http::{header, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use kb_core::review::Anchor;
use kb_core::share::{
    self, LinksMode, ShareBackend, ShareCtx, ShareFileSetOpts, ShareHostKind, ShareIndexEntry,
    ShareIndexPage, ShareOpts, StagedShare,
};
use kb_core::storage::sqlite::ShareRow;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::io::{Cursor, Write};
use std::sync::Arc;
use zip::write::{SimpleFileOptions, ZipWriter};

/// Same bounds as `routes::download` — the export zip is built entirely in RAM
/// before streaming, so cap peak memory + entry count.
const EXPORT_MAX_BYTES: usize = 256 * 1024 * 1024;
const EXPORT_MAX_FILES: usize = 10_000;

#[derive(Debug, Deserialize)]
pub struct ShareRequest {
    /// Source-relative target — a file (single artifact) or a folder.
    pub target: String,
    /// `cloudflare-pages` (default) or `github-pages`.
    #[serde(default)]
    pub host: Option<String>,
    /// `--gate` rules (repeatable). Cloudflare only.
    #[serde(default)]
    pub gate: Vec<String>,
    #[serde(default)]
    pub public: bool,
    /// `warn` (default) or `absolute`.
    #[serde(default)]
    pub links: Option<String>,
    #[serde(default)]
    pub update: bool,
    #[serde(default)]
    pub no_scrub: bool,
    /// Y-track — also publish each artifact's comment thread + attachments
    /// into the static site. PUBLISHES otherwise-private review state.
    #[serde(default)]
    pub include_comments: bool,
}

#[derive(Debug, Serialize)]
pub struct ShareResponse {
    pub name: String,
    pub url: String,
    pub host: String,
    pub gate: Option<String>,
    pub danglers: Vec<String>,
    pub files: usize,
    pub updated: bool,
}

#[derive(Debug, Serialize)]
pub struct ShareListItem {
    pub name: String,
    pub target: String,
    pub host: String,
    pub url: String,
    pub gate: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

impl From<&ShareRow> for ShareListItem {
    fn from(r: &ShareRow) -> Self {
        Self {
            name: r.name.clone(),
            target: r.target.clone(),
            host: r.host.clone(),
            url: r.deployed_url.clone(),
            gate: r.gate.clone(),
            created_at: r.created_at_unix,
            updated_at: r.updated_at_unix,
        }
    }
}

/// `POST /api/kb/{kb}/share`.
pub async fn create(
    State(state): State<Arc<KbHandles>>,
    Path(kb): Path<String>,
    Json(req): Json<ShareRequest>,
) -> Response {
    let (kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };

    let host = match ShareHostKind::parse(req.host.as_deref().unwrap_or("cloudflare-pages")) {
        Some(h) => h,
        None => {
            return error_to_problem_json(&kb_core::Error::BadRequest(format!(
                "unknown host {:?}: cloudflare-pages | github-pages",
                req.host.unwrap_or_default()
            )))
        }
    };
    let links = match req.links.as_deref().unwrap_or("warn") {
        "warn" => LinksMode::Warn,
        "absolute" => LinksMode::Absolute,
        other => {
            return error_to_problem_json(&kb_core::Error::BadRequest(format!(
                "unknown links mode {other:?}: warn | absolute"
            )))
        }
    };

    // Comment publishing reads the kb's daemon-state dirs (review files +
    // attachment blobs). Only wired when opted in.
    let (review_dir, attachments_root) = if req.include_comments {
        (
            Some(state.paths.kb_review_dir(&kb_name)),
            Some(state.paths.kb_state(&kb_name).join(".attachments")),
        )
    } else {
        (None, None)
    };

    let opts = ShareOpts {
        target: req.target,
        host,
        gate: req.gate,
        public: req.public,
        links,
        update: req.update,
        no_scrub: req.no_scrub,
        include_comments: req.include_comments,
        review_dir,
        attachments_root,
    };

    // Cheap 400 on a bad host/gate combo before building any backend.
    if let Err(e) = share::validate(&opts) {
        return error_to_problem_json(&e);
    }
    let backend = match ShareBackend::from_config(host, &state.share) {
        Ok(b) => b,
        Err(e) => return error_to_problem_json(&e),
    };

    let sctx = ShareCtx {
        handle: &ctx.storage,
        source_path: &ctx.source_path,
        kb_name: &kb,
        suffix: &state.origin.artifact_host_suffix,
        live_origin: state.share.live_origin.as_deref(),
        outbound: ctx.outbound.as_deref(),
    };

    match share::run_share(&sctx, &backend, &opts).await {
        Ok(o) => Json(ShareResponse {
            name: o.name,
            url: o.url,
            host: o.host,
            gate: o.gate,
            danglers: o.danglers,
            files: o.files,
            updated: o.updated,
        })
        .into_response(),
        Err(e) => error_to_problem_json(&e),
    }
}

/// `POST /api/kb/{kb}/share/export` — stage the target (scrub + relativize
/// in-share links + render markdown) and return it as a self-contained `.zip`
/// for OFFLINE use. No host backend, no registry row, no tokens — the CLI
/// (`kb share --local`) writes/extracts the bytes. `host`/`gate`/`public`/
/// `update` on the request are ignored; `target`/`links`/`no_scrub`/
/// `include_comments` drive the same engine `stage_files` the publish path uses.
pub async fn export(
    State(state): State<Arc<KbHandles>>,
    Path(kb): Path<String>,
    Json(req): Json<ShareRequest>,
) -> Response {
    let (kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let links = match req.links.as_deref().unwrap_or("warn") {
        "warn" => LinksMode::Warn,
        "absolute" => LinksMode::Absolute,
        other => {
            return error_to_problem_json(&kb_core::Error::BadRequest(format!(
                "unknown links mode {other:?}: warn | absolute"
            )))
        }
    };
    let (review_dir, attachments_root) = if req.include_comments {
        (
            Some(state.paths.kb_review_dir(&kb_name)),
            Some(state.paths.kb_state(&kb_name).join(".attachments")),
        )
    } else {
        (None, None)
    };
    // A local export never pushes to a host; `host`/`gate`/`public`/`update`
    // are unused by `stage_files`, so a placeholder host is fine.
    let opts = ShareOpts {
        target: req.target,
        host: ShareHostKind::CloudflarePages,
        gate: Vec::new(),
        public: false,
        links,
        update: false,
        no_scrub: req.no_scrub,
        include_comments: req.include_comments,
        review_dir,
        attachments_root,
    };
    let sctx = ShareCtx {
        handle: &ctx.storage,
        source_path: &ctx.source_path,
        kb_name: &kb,
        suffix: &state.origin.artifact_host_suffix,
        live_origin: state.share.live_origin.as_deref(),
        outbound: ctx.outbound.as_deref(),
    };

    let staged = match share::stage_files(&sctx, &opts).await {
        Ok(s) => s,
        Err(e) => return error_to_problem_json(&e),
    };
    let slug = opts.target.trim_matches('/').replace('/', "-");
    let filename = if slug.is_empty() {
        format!("{kb_name}-all.zip")
    } else {
        format!("{kb_name}-{slug}.zip")
    };
    zip_staged_response(staged, filename, None)
}

/// Request body for `POST …/lists/{id}/share/export` — same scrub/links knobs
/// as path export; the file set comes from the list, not a `target`.
#[derive(Debug, Deserialize)]
pub struct ListShareExportRequest {
    #[serde(default)]
    pub links: Option<String>,
    #[serde(default)]
    pub no_scrub: bool,
    #[serde(default)]
    pub include_comments: bool,
}

/// `POST /api/kb/{kb}/lists/{id}/share/export` — stage a reading list as a
/// self-contained offline `.zip`: ordered entries become the export set, a
/// generated `index.html` is the TOC/entry page. Tombstoned or unresolvable
/// entries are skipped and reported via `x-kb-share-skipped` (comma-joined
/// entry ids). Empty resolvable set → 400 (never an empty zip). Same auth
/// posture as `/share/export` (auth_bearer; loopback bypass).
pub async fn export_list(
    State(state): State<Arc<KbHandles>>,
    Path((kb, id)): Path<(String, String)>,
    Json(req): Json<ListShareExportRequest>,
) -> Response {
    let (kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let links = match req.links.as_deref().unwrap_or("warn") {
        "warn" => LinksMode::Warn,
        "absolute" => LinksMode::Absolute,
        other => {
            return error_to_problem_json(&kb_core::Error::BadRequest(format!(
                "unknown links mode {other:?}: warn | absolute"
            )))
        }
    };

    let list_row = match ctx.storage.list_get(id.clone()).await {
        Ok(Some(r)) => r,
        Ok(None) => {
            return error_to_problem_json(&kb_core::Error::NotFound(format!(
                "list {id} in kb {kb_name}"
            )))
        }
        Err(e) => return error_to_problem_json(&e),
    };
    let entry_rows = match ctx.storage.list_entries_for_list(id.clone()).await {
        Ok(rows) => rows,
        Err(e) => return error_to_problem_json(&e),
    };

    // Enrich from lance (same signals lists::assemble_entry uses) — we only
    // need source_relative / title / tombstone + the stored note/anchor.
    let artifact_ids: Vec<String> = entry_rows
        .iter()
        .map(|r| r.artifact_id.clone())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    let mut doc_by_id: std::collections::HashMap<String, kb_core::storage::lance::DocSummary> =
        std::collections::HashMap::new();
    if !artifact_ids.is_empty() {
        for d in ctx
            .storage
            .get_by_ids(artifact_ids)
            .await
            .unwrap_or_default()
        {
            doc_by_id.insert(d.id.clone(), d);
        }
    }

    let mut paths: Vec<String> = Vec::new();
    let mut index_entries: Vec<ShareIndexEntry> = Vec::new();
    let mut skipped: Vec<String> = Vec::new();
    for row in &entry_rows {
        let Some(doc) = doc_by_id.get(&row.artifact_id) else {
            skipped.push(row.id.clone());
            continue;
        };
        let src_rel = kb_core::paths::doc_rel_path(&doc.path, &ctx.source_path);
        if src_rel.is_empty() {
            skipped.push(row.id.clone());
            continue;
        }
        // Verify the source file still exists on disk (lance row alone is not
        // enough — a tombstoned path can linger briefly).
        if !ctx.source_path.join(&src_rel).is_file() {
            skipped.push(row.id.clone());
            continue;
        }
        let section_id = row.anchor_json.as_deref().and_then(|j| {
            match serde_json::from_str::<Anchor>(j) {
                // Only Section has a stable offline fragment; Chapter/Selection
                // link to the artifact page without a fragment.
                Ok(Anchor::Section { id, .. }) if !id.is_empty() => Some(id),
                _ => None,
            }
        });
        let title = if doc.title.trim().is_empty() {
            src_rel.clone()
        } else {
            doc.title.clone()
        };
        paths.push(src_rel.clone());
        index_entries.push(ShareIndexEntry {
            title,
            source_relative: src_rel,
            note: row.note.clone(),
            section_id,
        });
    }

    if paths.is_empty() {
        return error_to_problem_json(&kb_core::Error::BadRequest(
            "reading list has no resolvable artifacts to share (all entries \
             missing, tombstoned, or unreadable)"
                .into(),
        ));
    }

    let (review_dir, attachments_root) = if req.include_comments {
        (
            Some(state.paths.kb_review_dir(&kb_name)),
            Some(state.paths.kb_state(&kb_name).join(".attachments")),
        )
    } else {
        (None, None)
    };
    let opts = ShareFileSetOpts {
        links,
        no_scrub: req.no_scrub,
        include_comments: req.include_comments,
        review_dir,
        attachments_root,
    };
    let index = ShareIndexPage {
        title: list_row.title.clone(),
        description: list_row.description.clone(),
        entries: index_entries,
    };
    let sctx = ShareCtx {
        handle: &ctx.storage,
        source_path: &ctx.source_path,
        kb_name: &kb,
        suffix: &state.origin.artifact_host_suffix,
        live_origin: state.share.live_origin.as_deref(),
        outbound: ctx.outbound.as_deref(),
    };
    let staged = match share::stage_file_set(&sctx, &opts, &paths, &index).await {
        Ok(s) => s,
        Err(e) => return error_to_problem_json(&e),
    };

    let slug = slugify_list_name(&list_row.title);
    let filename = format!("{kb_name}-list-{slug}.zip");
    let skipped = if skipped.is_empty() {
        None
    } else {
        Some(skipped)
    };
    zip_staged_response(staged, filename, skipped.as_deref())
}

fn slugify_list_name(title: &str) -> String {
    let mut out = String::with_capacity(title.len());
    let mut prev_dash = false;
    for ch in title.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    let trimmed: String = out.trim_matches('-').chars().take(40).collect();
    if trimmed.is_empty() {
        "share".into()
    } else {
        trimmed
    }
}

/// Zip a staged share into an HTTP response with the standard
/// `x-kb-share-*` headers. `skipped` (list export only) becomes
/// `x-kb-share-skipped` as a comma-joined list of entry ids.
fn zip_staged_response(
    staged: StagedShare,
    filename: String,
    skipped: Option<&[String]>,
) -> Response {
    if staged.files.len() > EXPORT_MAX_FILES {
        return too_large(format!(
            "{} files exceeds the {EXPORT_MAX_FILES}-file export cap",
            staged.files.len()
        ));
    }

    // Build the zip in memory (bounded; mirrors routes::download).
    let mut zip = ZipWriter::new(Cursor::new(Vec::<u8>::new()));
    let zopts = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    let mut total = 0usize;
    for (name, bytes) in &staged.files {
        total = total.saturating_add(bytes.len());
        if total > EXPORT_MAX_BYTES {
            return too_large(format!("export exceeds the {EXPORT_MAX_BYTES}-byte cap"));
        }
        if zip.start_file(name, zopts).is_err() || zip.write_all(bytes).is_err() {
            return error_to_problem_json(&kb_core::Error::Internal(anyhow::anyhow!(
                "zip write failed for {name}"
            )));
        }
    }
    let buf = match zip.finish() {
        Ok(c) => c.into_inner(),
        Err(e) => {
            return error_to_problem_json(&kb_core::Error::Internal(anyhow::anyhow!(
                "zip finalise failed: {e}"
            )))
        }
    };

    let mut resp = (StatusCode::OK, buf).into_response();
    let h = resp.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/zip"),
    );
    if let Ok(v) = HeaderValue::from_str(&super::docs::attachment_disposition(&filename)) {
        h.insert(header::CONTENT_DISPOSITION, v);
    }
    // Surface the entry page, file count, and out-of-share danglers to the
    // CLI / SPA out-of-band (the body is the binary zip).
    if let Ok(v) = HeaderValue::from_str(&staged.entry_path) {
        h.insert("x-kb-share-entry", v);
    }
    if let Ok(v) = HeaderValue::from_str(&staged.files.len().to_string()) {
        h.insert("x-kb-share-files", v);
    }
    if let Ok(v) = HeaderValue::from_str(&staged.danglers.join(",")) {
        h.insert("x-kb-share-danglers", v);
    }
    if let Some(ids) = skipped {
        if !ids.is_empty() {
            if let Ok(v) = HeaderValue::from_str(&ids.join(",")) {
                h.insert("x-kb-share-skipped", v);
            }
        }
    }
    resp
}

/// `POST /api/kb/{kb}/share/export/page` — stage ONE artifact for an
/// UNCOMPRESSED, native-format download. HTML artifacts come back as a
/// scrubbed standalone `.html`; Markdown artifacts as their scrubbed RAW
/// `.md` source (never rendered — the "native format" decision). No zip, no
/// host backend, no registry row. The same export scrub as the bundle
/// (`scrub_export` always strips the kb-prompt; outbound redactions apply),
/// so a single page is just as safe to hand off. `target` must name a single
/// file; `host`/`gate`/`public`/`links`/`update`/`include_comments` are
/// ignored. Used by `kb share --page` and the SPA "Download page".
pub async fn export_page(
    State(state): State<Arc<KbHandles>>,
    Path(kb): Path<String>,
    Json(req): Json<ShareRequest>,
) -> Response {
    let (_kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    // Only `target` + `no_scrub` matter; the rest are publish/bundle knobs.
    let opts = ShareOpts {
        target: req.target,
        host: ShareHostKind::CloudflarePages, // unused — never pushes to a host
        gate: Vec::new(),
        public: false,
        links: LinksMode::Warn,
        update: false,
        no_scrub: req.no_scrub,
        include_comments: false, // a single-page download injects no comments
        review_dir: None,
        attachments_root: None,
    };
    let sctx = ShareCtx {
        handle: &ctx.storage,
        source_path: &ctx.source_path,
        kb_name: &kb,
        suffix: &state.origin.artifact_host_suffix,
        live_origin: state.share.live_origin.as_deref(),
        outbound: ctx.outbound.as_deref(),
    };
    let page = match share::stage_single_page(&sctx, &opts) {
        Ok(p) => p,
        Err(e) => return error_to_problem_json(&e),
    };

    let mut resp = (StatusCode::OK, page.bytes).into_response();
    let h = resp.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(page.content_type),
    );
    // The body is fetched + saved (never navigated to / rendered in the app
    // origin), but stay defensive: forbid sniffing the declared type into
    // active content (mirrors the artifact-bytes serve path).
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    if let Ok(v) = HeaderValue::from_str(&super::docs::attachment_disposition(&page.filename)) {
        h.insert(header::CONTENT_DISPOSITION, v);
    }
    // Out-of-band: cross-artifact links that will be dead in the lone file.
    if let Ok(v) = HeaderValue::from_str(&page.danglers.join(",")) {
        h.insert("x-kb-share-danglers", v);
    }
    resp
}

/// 413 problem+json — `kb_core::Error` has no payload-too-large variant, so the
/// response is built here (same shape as `routes::download::too_large`).
fn too_large(detail: String) -> Response {
    let body = json!({
        "type": "urn:kb:errors:payload-too-large",
        "title": "Payload Too Large",
        "status": 413,
        "detail": detail,
    });
    let mut resp = (StatusCode::PAYLOAD_TOO_LARGE, Json(body)).into_response();
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/problem+json"),
    );
    resp
}

/// `GET /api/kb/{kb}/shares`.
pub async fn list(State(state): State<Arc<KbHandles>>, Path(kb): Path<String>) -> Response {
    let (_kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    match share::list_shares(&ctx.storage).await {
        Ok(rows) => {
            let items: Vec<ShareListItem> = rows.iter().map(ShareListItem::from).collect();
            Json(items).into_response()
        }
        Err(e) => error_to_problem_json(&e),
    }
}

/// `DELETE /api/kb/{kb}/shares/{name}`.
pub async fn delete(
    State(state): State<Arc<KbHandles>>,
    Path((kb, name)): Path<(String, String)>,
) -> Response {
    let (_kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };

    // Look up first: a 404 for an unknown share needs no backend (and so
    // works even when the host's credentials aren't configured).
    let row = match ctx.storage.shares_get(name.clone()).await {
        Ok(Some(r)) => r,
        Ok(None) => {
            return error_to_problem_json(&kb_core::Error::NotFound(format!("share {name:?}")))
        }
        Err(e) => return error_to_problem_json(&e),
    };
    let Some(host) = ShareHostKind::parse(&row.host) else {
        return error_to_problem_json(&kb_core::Error::Internal(anyhow::anyhow!(
            "share {name:?} has unknown host {:?}",
            row.host
        )));
    };
    let backend = match ShareBackend::from_config(host, &state.share) {
        Ok(b) => b,
        Err(e) => return error_to_problem_json(&e),
    };
    match share::revoke(&ctx.storage, &backend, &name).await {
        Ok(revoked) => Json(json!({ "revoked": revoked, "name": name })).into_response(),
        Err(e) => error_to_problem_json(&e),
    }
}
