//! `GET /api/kb/{kb}/download?folder=<source-relative-folder>` — stream a
//! `.zip` of every indexed artifact whose folder is `<folder>` or a
//! descendant of it (the same descendant-inclusive semantics the gallery's
//! `folder=` filter uses on `docs::list`). Omit `folder` to archive the
//! whole kb.
//!
//! Each zip entry is keyed by the artifact's source-relative path, so the
//! archive reproduces the folder tree. The build is in-memory and bounded
//! by `DOWNLOAD_MAX_BYTES` / `DOWNLOAD_MAX_FILES` — over either cap the
//! route returns a 413 problem+json (mirroring `review::post_export`).
//! Individually unreadable files are skipped rather than failing the whole
//! archive (an artifact deleted from disk between index + download).

use crate::middleware::error_to_problem_json;
use crate::state::KbHandles;
use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::{header, HeaderValue, Response, StatusCode},
    response::IntoResponse,
    Json,
};
use kb_core::paths::{doc_folder, doc_rel_path};
use serde::Deserialize;
use std::io::{Cursor, Write};
use std::sync::Arc;
use zip::write::{SimpleFileOptions, ZipWriter};

/// Sum of uncompressed entry bytes we'll buffer before refusing the
/// archive. 256 MiB — 8× the review-export cap (`EXPORT_MAX_BYTES`);
/// generous for a folder of HTML artifacts while still bounding peak
/// memory (the zip is built entirely in RAM before streaming).
const DOWNLOAD_MAX_BYTES: usize = 256 * 1024 * 1024;

/// Hard cap on entry count — a runaway folder can't fan out into an
/// unbounded number of `fs::read` calls + zip entries.
const DOWNLOAD_MAX_FILES: usize = 10_000;

#[derive(Debug, Default, Deserialize)]
pub struct DownloadQuery {
    /// Source-relative folder to archive (descendant-inclusive). Absent
    /// or empty → the whole kb.
    #[serde(default)]
    pub folder: Option<String>,
}

pub async fn get(
    State(state): State<Arc<KbHandles>>,
    Path(kb): Path<String>,
    Query(q): Query<DownloadQuery>,
) -> Response<Body> {
    let (kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };

    let rows = match ctx.storage.list_docs(u32::MAX).await {
        Ok(r) => r,
        Err(e) => return error_to_problem_json(&e),
    };

    // Normalise the requested folder (trim trailing slash; treat empty as
    // "whole kb"). Descendant-inclusive: a row matches when its folder is
    // exactly `folder` or starts with `folder/`.
    let folder = q
        .folder
        .as_deref()
        .map(|f| f.trim_matches('/').to_string())
        .filter(|f| !f.is_empty());

    let mut entries: Vec<(String, String)> = Vec::new(); // (zip name, abs path)
    for row in &rows {
        let row_folder = doc_folder(&row.path, &ctx.source_path);
        let in_scope = match &folder {
            None => true,
            Some(f) => row_folder == *f || row_folder.starts_with(&format!("{f}/")),
        };
        if !in_scope {
            continue;
        }
        let rel = doc_rel_path(&row.path, &ctx.source_path);
        // A row whose path escapes the source root yields an empty rel;
        // fall back to the basename so it still lands in the archive.
        let name = if rel.is_empty() {
            std::path::Path::new(&row.path)
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or(&row.id)
                .to_string()
        } else {
            rel
        };
        entries.push((name, row.path.clone()));
    }

    if entries.is_empty() {
        let what = folder.as_deref().unwrap_or("(root)");
        return error_to_problem_json(&kb_core::Error::NotFound(format!(
            "no artifacts in {kb_name} folder {what:?}"
        )));
    }
    if entries.len() > DOWNLOAD_MAX_FILES {
        return too_large(format!(
            "{} artifacts exceeds the {DOWNLOAD_MAX_FILES}-file archive cap",
            entries.len()
        ));
    }

    // Walk + read + Deflate off the async worker (QFIX-4) — the archive is
    // bounded but still multi-MB of pure blocking IO.
    let kb_for_err = kb_name.as_str().to_string();
    let zip_result = tokio::task::spawn_blocking(move || -> Result<Vec<u8>, ZipBuildError> {
        let mut zip = ZipWriter::new(Cursor::new(Vec::<u8>::new()));
        let opts =
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        let mut total: usize = 0;
        let mut written = 0usize;
        for (name, abs) in &entries {
            let bytes = match std::fs::read(abs) {
                Ok(b) => b,
                // Skip a file that vanished between index and download rather
                // than failing the whole archive.
                Err(_) => continue,
            };
            total = total.saturating_add(bytes.len());
            if total > DOWNLOAD_MAX_BYTES {
                return Err(ZipBuildError::TooLarge);
            }
            if zip.start_file(name, opts).is_err() {
                continue;
            }
            if zip.write_all(&bytes).is_err() {
                continue;
            }
            written += 1;
        }
        if written == 0 {
            return Err(ZipBuildError::Empty(kb_for_err));
        }
        match zip.finish() {
            Ok(cursor) => Ok(cursor.into_inner()),
            Err(e) => Err(ZipBuildError::Finalise(e.to_string())),
        }
    })
    .await;

    let buf = match zip_result {
        Ok(Ok(buf)) => buf,
        Ok(Err(ZipBuildError::TooLarge)) => {
            return too_large(format!("archive exceeds the {DOWNLOAD_MAX_BYTES}-byte cap"));
        }
        Ok(Err(ZipBuildError::Empty(kb))) => {
            return error_to_problem_json(&kb_core::Error::NotFound(format!(
                "no readable artifacts in {kb}"
            )));
        }
        Ok(Err(ZipBuildError::Finalise(e))) => {
            return error_to_problem_json(&kb_core::Error::Internal(anyhow::anyhow!(
                "zip finalise failed: {e}"
            )));
        }
        Err(e) => {
            return error_to_problem_json(&kb_core::Error::Internal(anyhow::anyhow!(
                "zip build join failed: {e}"
            )));
        }
    };

    let filename = match &folder {
        Some(f) => format!("{kb_name}-{}.zip", f.replace('/', "-")),
        None => format!("{kb_name}-all.zip"),
    };
    let mut resp = (StatusCode::OK, buf).into_response();
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/zip"),
    );
    if let Ok(v) = HeaderValue::from_str(&super::docs::attachment_disposition(&filename)) {
        resp.headers_mut().insert(header::CONTENT_DISPOSITION, v);
    }
    resp
}

/// Error surface of the `spawn_blocking` zip builder — mapped back to the
/// same HTTP responses the pre-QFIX-4 inline path produced.
enum ZipBuildError {
    TooLarge,
    Empty(String),
    Finalise(String),
}

/// 413 problem+json — `kb_core::Error` has no payload-too-large variant,
/// so the response is built here (same shape as `review::export_too_large`).
fn too_large(detail: String) -> Response<Body> {
    let body = serde_json::json!({
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
