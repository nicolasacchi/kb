//! V76-R3c — `GET /api/compare/file?repo=&path=&a=&b=`.
//!
//! Two blobs (ODB reads, same shape as `GET /api/file`'s content pair)
//! plus a hunk list parsed from [`crate::diff::diff_file`]'s unified
//! text. The spawn stays in `diff.rs` (already on the git-argv allowlist);
//! this module never constructs `Command::new("git")`.

use crate::diff::diff_file;
use crate::entities::RouteContract;
use crate::frames::{self, FrameClaim};
use crate::git::Revspec;
use crate::ingest;
use crate::routes::{find_repo, parse_revspec, read_repo_file, safe_rel_path, ApiError};
use crate::state::SharedState;
use axum::extract::{Query, State};
use axum::http::header;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};

pub const SCHEMA: &str = "kbc-compare-file/1";

#[derive(Debug, Deserialize)]
pub struct CompareFileParams {
    pub repo: String,
    pub path: String,
    pub a: String,
    pub b: String,
}

fn compare_file_params_accept_without(omit: &str) -> bool {
    let mut q = serde_json::json!({
        "repo": "r",
        "path": "a.rs",
        "a": "HEAD~1",
        "b": "HEAD",
    });
    q.as_object_mut().expect("object").remove(omit);
    serde_json::from_value::<CompareFileParams>(q).is_ok()
}

pub const COMPARE_FILE_ROUTE: RouteContract = RouteContract {
    path: "/api/compare/file",
    handler: "compare_file::compare_file_route",
    required_params: &["repo", "path", "a", "b"],
    params_accept_without: compare_file_params_accept_without,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Hunk {
    pub old_start: u32,
    pub old_count: u32,
    pub new_start: u32,
    pub new_count: u32,
    pub header: String,
}

#[derive(Debug, Serialize)]
pub struct BlobSide {
    #[serde(rename = "ref")]
    pub rev: String,
    pub size: u64,
    pub blob_hash: String,
    pub encoding: &'static str,
    pub content: String,
    pub frame: FrameClaim,
}

#[derive(Debug, Serialize)]
pub struct CompareFileResponse {
    pub schema: &'static str,
    pub repo: String,
    pub path: String,
    pub a: BlobSide,
    pub b: BlobSide,
    pub diff: String,
    pub hunks: Vec<Hunk>,
}

/// Parse `@@ -old[,count] +new[,count] @@` headers out of a unified diff.
/// Lines that are not hunk headers are ignored. Counts default to 1 when
/// omitted (git's own rule).
pub fn parse_unified_hunks(diff: &str) -> Vec<Hunk> {
    let mut out = Vec::new();
    for line in diff.lines() {
        let Some(rest) = line.strip_prefix("@@ ") else {
            continue;
        };
        let Some((spec, _)) = rest.split_once(" @@") else {
            continue;
        };
        let mut parts = spec.split_whitespace();
        let Some(old) = parts.next() else { continue };
        let Some(new) = parts.next() else { continue };
        let Some((old_start, old_count)) = parse_hunk_span(old, '-') else {
            continue;
        };
        let Some((new_start, new_count)) = parse_hunk_span(new, '+') else {
            continue;
        };
        out.push(Hunk {
            old_start,
            old_count,
            new_start,
            new_count,
            header: line.to_string(),
        });
    }
    out
}

fn parse_hunk_span(tok: &str, sign: char) -> Option<(u32, u32)> {
    let rest = tok.strip_prefix(sign)?;
    match rest.split_once(',') {
        Some((s, c)) => Some((s.parse().ok()?, c.parse().ok()?)),
        None => Some((rest.parse().ok()?, 1)),
    }
}

fn encode_bytes(bytes: &[u8]) -> (&'static str, String) {
    match String::from_utf8(bytes.to_vec()) {
        Ok(s) => ("utf8", s),
        Err(_) => (
            "base64",
            base64::engine::Engine::encode(&base64::engine::general_purpose::STANDARD, bytes),
        ),
    }
}

fn side_from_read(rev: String, bytes: &[u8]) -> BlobSide {
    let (encoding, content) = encode_bytes(bytes);
    BlobSide {
        rev,
        size: bytes.len() as u64,
        blob_hash: ingest::git_blob_hash(bytes),
        encoding,
        content,
        frame: frames::claim("file_at_ref", false),
    }
}

pub async fn compare_file_route(
    State(state): State<SharedState>,
    Query(params): Query<CompareFileParams>,
) -> Result<impl IntoResponse, ApiError> {
    let (repo, _repo_id) = find_repo(&state, &params.repo)?;
    let path = safe_rel_path(&params.path)?.to_string();
    let a: Revspec = parse_revspec(&params.a)?;
    let b: Revspec = parse_revspec(&params.b)?;

    let read_a = read_repo_file(repo, &path, Some(a.as_str()))?;
    let read_b = read_repo_file(repo, &path, Some(b.as_str()))?;

    let repo_root = repo.path.clone();
    let path_for_task = path.clone();
    let a_for_task = a.clone();
    let b_for_task = b.clone();
    let diff_text = tokio::task::spawn_blocking(move || {
        diff_file(&repo_root, &a_for_task, Some(&b_for_task), &path_for_task)
    })
    .await
    .map_err(|e| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("compare-file task panicked: {e}"),
        )
    })??;

    let hunks = parse_unified_hunks(&diff_text);
    let body = CompareFileResponse {
        schema: SCHEMA,
        repo: params.repo,
        path,
        a: side_from_read(params.a, &read_a.bytes),
        b: side_from_read(params.b, &read_b.bytes),
        diff: diff_text,
        hunks,
    };
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(body)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_standard_and_countless_headers() {
        let diff = "\
diff --git a/f b/f
--- a/f
+++ b/f
@@ -1,3 +1,4 @@
 a
-b
+B
 c
+d
@@ -10 +12 @@ lone
";
        let hunks = parse_unified_hunks(diff);
        assert_eq!(hunks.len(), 2);
        assert_eq!(
            hunks[0],
            Hunk {
                old_start: 1,
                old_count: 3,
                new_start: 1,
                new_count: 4,
                header: "@@ -1,3 +1,4 @@".into(),
            }
        );
        assert_eq!(hunks[1].old_start, 10);
        assert_eq!(hunks[1].old_count, 1);
        assert_eq!(hunks[1].new_start, 12);
        assert_eq!(hunks[1].new_count, 1);
    }

    #[test]
    fn ignores_noise_and_is_empty_on_no_diff() {
        assert!(parse_unified_hunks("").is_empty());
        assert!(parse_unified_hunks("not a diff\n").is_empty());
    }
}
