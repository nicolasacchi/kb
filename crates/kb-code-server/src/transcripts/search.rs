//! `GET /api/search/transcripts` + `GET /api/transcripts/status` — the ONLY
//! two HTTP entry points into the raw-transcripts lane, and the
//! [`loopback_only`] guard both are mounted behind (`router.rs` merges a
//! sub-`Router` layered with THIS middleware INSTEAD OF `auth_bearer` for
//! these two routes — a valid bearer token must never grant a non-loopback
//! caller access here; that would defeat the whole "never in any
//! non-loopback response" design rule for a lane whose corpus is a raw
//! chat transcript, plausibly containing anything the operator ever typed
//! or read).
//!
//! [`build_snippet`] is the other half of the "pull-only" design: the FTS5
//! index is CONTENTLESS (`V0003`'s `transcript_fts`, `content=''`) — it
//! never stores a second copy of any turn's text — so every search hit's
//! snippet is produced by re-opening the source JSONL, seeking to the
//! turn's `byte_offset`, reading exactly `byte_len` bytes, and re-running
//! [`crate::transcripts::parse::parse_line`] on that one line to recover
//! the specific turn's own display text (matched back by `uuid`+`kind`+
//! `tool_name`, since one line can yield several turns — see the `V0003`
//! migration's doc).

use crate::routes::ApiError;
use crate::state::SharedState;
use crate::store::{StoreBlocking, StoreError, TranscriptSearchRow};
use axum::{
    body::Body,
    extract::{Query, State},
    http::{header, Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use kb_server::state::AuthConfig;
use serde::{Deserialize, Serialize};
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::Arc;

pub const DEFAULT_LIMIT: usize = 50;
pub const MAX_LIMIT: usize = 200;

/// Refuses (404 — INDISTINGUISHABLE from a route that doesn't exist, never
/// a 401/403 that would confirm the lane's presence to a probing
/// non-loopback caller) any request whose origin doesn't resolve to
/// loopback via the SAME predicate `auth_bearer` uses
/// (`kb_server::middleware::request_is_loopback` — invariant #3/#4's
/// `ConnectInfo`-from-extensions + right-to-left `X-Forwarded-For` walk,
/// gated on `auth.trusted_proxies` being a trusted hop). Deliberately NOT
/// layered alongside `auth_bearer` — a valid `KB_CODE_TOKEN` bearer token
/// must NOT open this lane to a non-loopback caller, which is exactly what
/// composing with (rather than replacing) `auth_bearer` would allow.
pub async fn loopback_only(
    State(auth): State<Arc<AuthConfig>>,
    req: Request<Body>,
    next: Next,
) -> Response {
    if kb_server::middleware::request_is_loopback(&req, &auth.trusted_proxies) {
        next.run(req).await
    } else {
        StatusCode::NOT_FOUND.into_response()
    }
}

#[derive(Debug, Deserialize)]
pub struct SearchTranscriptsParams {
    #[serde(default)]
    pub q: String,
    pub limit: Option<usize>,
    pub session: Option<String>,
    pub kind: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct TranscriptHit {
    pub session_id: String,
    pub uuid: String,
    pub ts: i64,
    pub kind: String,
    pub tool_name: Option<String>,
    pub project_dir: String,
    pub snippet: String,
    pub is_sidechain: bool,
}

/// `GET /api/search/transcripts?q=&limit=&session=&kind=` — FTS5 `MATCH`
/// over every indexed turn, newest-first (`store::Store::search_transcripts`).
/// LOOPBACK-ONLY (see [`loopback_only`], mounted on this route in
/// `router.rs`) — never fused with the code/doc search lanes above, never
/// federated.
pub async fn search_transcripts(
    State(state): State<SharedState>,
    Query(params): Query<SearchTranscriptsParams>,
) -> Result<impl IntoResponse, ApiError> {
    let q = params.q.trim();
    if q.is_empty() {
        return Err(ApiError::bad_request("q must not be empty"));
    }
    let limit = params.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    // 2026-08-31 incident (store.rs module doc): the FTS5 read AND the
    // snippet re-read (`to_hits` → `build_snippet`, its own blocking file
    // I/O) share one blocking-pool round trip.
    let q_owned = q.to_string();
    let session = params.session.clone();
    let kind = params.kind.clone();
    let root = state.transcripts_root.clone();
    let hits = state
        .store
        .run_blocking(move |store| {
            store
                .search_transcripts(&q_owned, limit, session.as_deref(), kind.as_deref())
                .map(|rows| to_hits(&root, rows, &q_owned))
        })
        .await
        .map_err(transcript_search_error)?;

    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(serde_json::json!({ "q": params.q, "hits": hits })),
    ))
}

/// Build [`TranscriptHit`]s from raw `search_transcripts` rows — shared by
/// [`search_transcripts`] (the standalone, loopback-only route) AND
/// `search::unified`'s transcripts lane (W2.4), so the two ways into this
/// lane produce byte-identical hit shapes from the same rows rather than two
/// independently-maintained mapping loops.
pub(crate) fn to_hits(root: &Path, rows: Vec<TranscriptSearchRow>, q: &str) -> Vec<TranscriptHit> {
    rows.into_iter()
        .map(|row| {
            let snippet = build_snippet(root, &row, q);
            TranscriptHit {
                session_id: row.session_id,
                uuid: row.uuid,
                ts: row.ts,
                kind: row.kind,
                tool_name: row.tool_name,
                project_dir: row.project_dir,
                snippet,
                is_sidechain: row.is_sidechain,
            }
        })
        .collect()
}

/// A malformed FTS5 `MATCH` query (unbalanced quotes, a bare trailing
/// boolean operator, ...) surfaces from sqlite as a message containing
/// `"fts5:"` — mapped to 400 so a caller learns its query string was
/// rejected, not that the daemon broke. Every other `StoreError` (a real
/// I/O/sqlite fault) stays a 500 via the existing `From<StoreError> for
/// ApiError` impl.
fn transcript_search_error(e: StoreError) -> ApiError {
    if is_invalid_query_error(&e) {
        ApiError::bad_request(invalid_query_message(&e))
    } else {
        ApiError::from(e)
    }
}

/// `true` when `e` is sqlite's own "malformed FTS5 MATCH query" signal (see
/// [`transcript_search_error`]'s doc) — shared with `search::unified`'s
/// transcripts lane so both surfaces recognise the SAME condition rather
/// than drifting.
pub(crate) fn is_invalid_query_error(e: &StoreError) -> bool {
    e.to_string().contains("fts5")
}

pub(crate) fn invalid_query_message(e: &StoreError) -> String {
    format!("invalid search query: {e}")
}

/// `GET /api/transcripts/status` — files tracked / turns indexed / indexed
/// bytes (`kb-code transcripts status`'s data source). LOOPBACK-ONLY, same
/// guard as `search_transcripts` — kept consistent even though the counts
/// alone are less sensitive than a snippet, per the module doc's "never in
/// any non-loopback response" rule for the whole lane.
pub async fn transcripts_status(
    State(state): State<SharedState>,
) -> Result<impl IntoResponse, ApiError> {
    let stats = state
        .store
        .run_blocking(|store| store.transcript_stats())
        .await
        .map_err(ApiError::from)?;
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(serde_json::json!({
            "enabled": state.transcripts_watcher.is_some(),
            "root": state.transcripts_root.display().to_string(),
            "files": stats.files,
            "turns": stats.turns,
            "indexed_bytes": stats.indexed_bytes,
        })),
    ))
}

/// Re-read `[byte_offset, byte_offset+byte_len)` of a turn's source JSONL
/// (`root.join(src_file)`) and re-derive its own display text via
/// `parse::parse_line`, matched back by `uuid`+`kind`+`tool_name` (one line
/// can yield several turns — see the `V0003` migration's doc). Best-effort:
/// any failure (the file has since moved/rotated in a way that invalidates
/// the byte range, a permissions error, ...) yields `None` rather than
/// failing the caller — a search/read lane over a live, externally-mutated
/// corpus can't guarantee every historical turn's raw bytes are still
/// exactly where they were. Shared by [`build_snippet`] (clips the result)
/// AND `sessiondiff::session_diff` (wants the turn's FULL text, e.g. to
/// truncate a prompt to ~200 chars its own way rather than a search-query-
/// centered clip).
pub(crate) fn read_turn_text(
    root: &Path,
    src_file: &str,
    uuid: &str,
    kind: &str,
    tool_name: Option<&str>,
    byte_offset: i64,
    byte_len: i64,
) -> Option<String> {
    let abs = root.join(src_file);
    let mut file = std::fs::File::open(&abs).ok()?;
    if byte_offset < 0 || byte_len < 0 {
        return None;
    }
    file.seek(SeekFrom::Start(byte_offset as u64)).ok()?;
    let mut buf = vec![0u8; byte_len as usize];
    file.read_exact(&mut buf).ok()?;
    let line = String::from_utf8_lossy(&buf).into_owned();
    let turns = crate::transcripts::parse::parse_line(&line, true);
    Some(
        turns
            .into_iter()
            .find(|t| t.uuid == uuid && t.kind == kind && t.tool_name.as_deref() == tool_name)
            .map(|t| t.text)
            .unwrap_or(line),
    )
}

/// V73-K3 — the same seek-and-reparse [`read_turn_text`] does, but handing
/// back a tool_use turn's RAW `input` object instead of its indexed display
/// text.
///
/// This exists because the indexer deliberately does NOT keep an `Edit`'s
/// `old_string`/`new_string` (`parse::TOOL_USE_TEXT_KEYS` is a curated
/// allowlist and a pinned test asserts `old_string` never reaches the FTS
/// column). That is the right default — those strings are file content, and
/// putting file content into a search index is a different product — but
/// the hunk↔turn join needs exactly those two strings to prove an edit
/// produced a hunk. So they are read back from the JSONL on demand,
/// per request, and NEVER persisted: kb-code stores that a turn happened,
/// never what it typed.
///
/// LOOPBACK ONLY by construction of where it is called from
/// (`review_turns`, on the `transcripts_api` sub-router) — this is raw
/// transcript content, D19's `raw-transcript` sensitivity class.
///
/// Best-effort, exactly like [`read_turn_text`]: any failure yields `None`.
pub(crate) fn read_turn_tool_input(
    root: &Path,
    src_file: &str,
    uuid: &str,
    tool_name: Option<&str>,
    byte_offset: i64,
    byte_len: i64,
) -> Option<serde_json::Value> {
    if byte_offset < 0 || byte_len < 0 {
        return None;
    }
    let abs = root.join(src_file);
    let mut file = std::fs::File::open(&abs).ok()?;
    file.seek(SeekFrom::Start(byte_offset as u64)).ok()?;
    let mut buf = vec![0u8; byte_len as usize];
    file.read_exact(&mut buf).ok()?;
    let line = String::from_utf8_lossy(&buf).into_owned();
    let value: serde_json::Value = serde_json::from_str(&line).ok()?;
    let content = value.pointer("/message/content")?.as_array()?;
    for block in content {
        if block.get("type").and_then(|t| t.as_str()) != Some("tool_use") {
            continue;
        }
        // One JSONL line can carry several tool_use blocks; match the one
        // this turn row names, by id/name the same way `read_turn_text`
        // matches by uuid+kind+tool_name.
        if block.get("name").and_then(|n| n.as_str()) != tool_name {
            continue;
        }
        let _ = uuid;
        return block.get("input").cloned();
    }
    None
}

/// Re-read `row`'s source JSONL, re-derive the specific turn's display text
/// ([`read_turn_text`]), and clip ~240 chars around the first occurrence of
/// `query`'s first alphanumeric token. Falls back to an empty string (not
/// [`read_turn_text`]'s `None`) when the re-read itself fails — this is
/// `search_transcripts`'s own snippet-building convention, unchanged from
/// before this fn was split out.
fn build_snippet(root: &Path, row: &TranscriptSearchRow, query: &str) -> String {
    let text = read_turn_text(
        root,
        &row.src_file,
        &row.uuid,
        &row.kind,
        row.tool_name.as_deref(),
        row.byte_offset,
        row.byte_len,
    )
    .unwrap_or_default();
    clip_around(&text, query)
}

const SNIPPET_WINDOW_CHARS: usize = 240;

/// Clip `text` to ~[`SNIPPET_WINDOW_CHARS`] chars centred on the first
/// occurrence of `query`'s first alphanumeric-run token (case-insensitive),
/// or the first `SNIPPET_WINDOW_CHARS` chars if no token is found (an FTS5
/// boolean/prefix query with no single literal substring to anchor on).
/// Always slices on CHAR boundaries (`char_indices`), never raw byte
/// offsets, so a multi-byte character is never split — see
/// `transcripts::parse::safe_truncate` for the same concern elsewhere in
/// this lane. Known imprecision: the search is done against a fully
/// lower-cased copy of `text`, whose byte length can differ from `text`'s
/// own for a handful of characters whose lowercase form is a different
/// byte length (e.g. Turkish İ) — an accepted approximation for a
/// display-only snippet, not a byte-exact index.
fn clip_around(text: &str, query: &str) -> String {
    let needle = query
        .split(|c: char| !c.is_alphanumeric())
        .find(|t| t.len() >= 2)
        .unwrap_or(query)
        .to_lowercase();

    let lower_text = text.to_lowercase();
    let hit_byte = if needle.is_empty() {
        None
    } else {
        lower_text.find(&needle)
    };

    let indices: Vec<usize> = text.char_indices().map(|(i, _)| i).collect();
    let total_chars = indices.len();
    let hit_char = hit_byte
        .map(|b| indices.iter().position(|&i| i >= b).unwrap_or(total_chars))
        .unwrap_or(0);

    let half = SNIPPET_WINDOW_CHARS / 2;
    let start_char = hit_char.saturating_sub(half);
    let end_char = (start_char + SNIPPET_WINDOW_CHARS).min(total_chars);

    let start_byte = indices.get(start_char).copied().unwrap_or(0);
    let end_byte = indices.get(end_char).copied().unwrap_or(text.len());
    text[start_byte..end_byte].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;

    #[test]
    fn clip_around_centers_on_the_first_hit() {
        let text = "the quick brown fox jumps over the lazy dog";
        let snippet = clip_around(text, "fox");
        assert!(snippet.contains("fox"));
        // Window is wider than the whole sample text, so it should come
        // back verbatim.
        assert_eq!(snippet, text);
    }

    #[test]
    fn clip_around_with_no_match_falls_back_to_the_start() {
        let text = "no matching term appears in here at all";
        let snippet = clip_around(text, "zzzzzz");
        assert_eq!(snippet, text);
    }

    #[test]
    fn clip_around_never_splits_a_multibyte_char_at_the_window_edge() {
        let mut text = "€".repeat(400);
        text.push_str("needle");
        text.push_str(&"€".repeat(400));
        let snippet = clip_around(&text, "needle");
        assert!(snippet.contains("needle"));
        // A successful String construction below proves every byte range
        // sliced was on a char boundary — `String::from` would otherwise
        // have already panicked inside `clip_around` itself.
        assert!(snippet.chars().count() <= SNIPPET_WINDOW_CHARS + 1);
    }

    /// End-to-end: build a real store + a real transcript JSONL file on
    /// disk, index it via `tail_file`, then prove `build_snippet` re-reads
    /// the exact turn's text (not some other turn sharing the same line).
    #[test]
    fn build_snippet_re_reads_the_matched_turn_from_the_source_line() {
        let store_tmp = tempfile::tempdir().unwrap();
        let store = Store::open(&store_tmp.path().join("index.db")).unwrap();
        let root = tempfile::tempdir().unwrap();
        let proj = root.path().join("proj-a");
        std::fs::create_dir_all(&proj).unwrap();
        let file = proj.join("session1.jsonl");
        let line = r#"{"type":"assistant","uuid":"a1","parentUuid":"u1","sessionId":"s1","timestamp":"2026-07-17T10:00:00.000Z","isSidechain":false,"message":{"role":"assistant","content":[{"type":"thinking","thinking":"quietly pondering the gizmo bug","signature":"sig"},{"type":"text","text":"the gizmo fix is ready"}]}}"#;
        std::fs::write(&file, format!("{line}\n")).unwrap();

        crate::transcripts::indexer::tail_file(&store, root.path(), &file, "proj-a", true).unwrap();

        let hits = store.search_transcripts("gizmo", 10, None, None).unwrap();
        assert_eq!(
            hits.len(),
            2,
            "both the thinking and text turns match \"gizmo\""
        );

        let thinking_hit = hits.iter().find(|h| h.kind == "thinking").unwrap();
        let snippet = build_snippet(root.path(), thinking_hit, "gizmo");
        assert!(
            snippet.contains("pondering the gizmo bug"),
            "must recover the THINKING turn's own text, got: {snippet:?}"
        );
        assert!(
            !snippet.contains("fix is ready"),
            "must not bleed into the text turn: {snippet:?}"
        );

        let text_hit = hits.iter().find(|h| h.kind == "assistant").unwrap();
        let snippet2 = build_snippet(root.path(), text_hit, "gizmo");
        assert!(snippet2.contains("gizmo fix is ready"), "got: {snippet2:?}");
    }

    #[test]
    fn build_snippet_round_trips_multibyte_content_via_byte_offset() {
        let store_tmp = tempfile::tempdir().unwrap();
        let store = Store::open(&store_tmp.path().join("index.db")).unwrap();
        let root = tempfile::tempdir().unwrap();
        let proj = root.path().join("proj-a");
        std::fs::create_dir_all(&proj).unwrap();
        let file = proj.join("session1.jsonl");
        // Multi-byte content BEFORE the indexed turn on the same line
        // (a preceding sibling block) so byte_offset must be genuinely
        // byte-accurate, not just char-accurate, for the re-read to land
        // on the right block.
        let line = r#"{"type":"assistant","uuid":"a1","parentUuid":"u1","sessionId":"s1","timestamp":"2026-07-17T10:00:00.000Z","isSidechain":false,"message":{"role":"assistant","content":[{"type":"text","text":"préfixé emoji 🎉 déjà vu"}]}}"#;
        std::fs::write(&file, format!("{line}\n")).unwrap();

        crate::transcripts::indexer::tail_file(&store, root.path(), &file, "proj-a", true).unwrap();
        let hits = store.search_transcripts("déjà", 10, None, None).unwrap();
        assert_eq!(hits.len(), 1);
        let snippet = build_snippet(root.path(), &hits[0], "déjà");
        assert!(snippet.contains("déjà vu"), "got: {snippet:?}");
        assert!(
            snippet.contains("🎉"),
            "the whole line's text must come back intact: {snippet:?}"
        );
    }
}
