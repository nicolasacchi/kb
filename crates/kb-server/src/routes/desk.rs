//! `POST /api/kb/{kb}/desk` — ephemeral LLM↔human handoff.
//! `GET /api/desk` — federated aggregate of indexed `handoff/` drafts.
//!
//! Thin slice on top of `kb_core::capture`: one file (or a `text` field)
//! written under the kb's `handoff/` folder with a **stable** filename
//! (`<slug>.<ext>`, no unix-secs suffix). Re-POSTing the same `name`
//! overwrites atomically so comments / versions / reading-progress keep
//! the path-derived id (invariant #27). Mounted INSIDE the `/api` nest
//! so it inherits `auth_bearer` (invariant #4 — do not repeat the
//! share-target `/capture` trap).
//!
//! Provenance: `kb-category: handoff`, tag `draft`, `from:` as capture
//! stamps it. `kb-session` / `kb-expires-at` are display-only stamps
//! (no sweeper). Indexing is the watcher's job (invariant #17).
//!
//! `GET /api/desk` fans out over `state.kbs` through `buffered_join`
//! (invariant #28). `?kb=` restricts to one corpus (404 on unknown).

use crate::routes::capture::{
    capture_settings, from_default, map_capture_error, non_empty, parse_bool_field, problem,
    reject_oversized_request, split_tags,
};
use crate::state::KbHandles;
use axum::{
    body::Body,
    extract::{Extension, Multipart, Path, Query, State},
    http::{HeaderMap, Response, StatusCode},
    response::IntoResponse,
    Json,
};
use kb_core::capture::{self, CaptureInput, DESK_DIR};
use kb_core::extmap::Pipeline;
use kb_core::paths::doc_rel_path;
use serde::{Deserialize, Serialize};
use std::path::Path as FsPath;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

/// Reject `ttl_secs` of 0 or greater than this (10 × 365 days).
const TEN_YEARS_SECS: u64 = 10 * 365 * 24 * 60 * 60;

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize)]
pub struct DeskItem {
    pub kb: String,
    pub id: String,
    pub source_relative: String,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub url: Option<String>,
    /// `false` when this POST overwrote an existing `handoff/<slug>.<ext>`.
    pub created: bool,
}

/// Frozen `GET /api/desk` item. Field names are a SPA contract.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Serialize)]
pub struct DeskListItem {
    pub kb: String,
    pub id: String,
    pub source_relative: String,
    pub title: String,
    pub updated_unix: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub expires_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub session_id: Option<String>,
    pub comments_open: u64,
    pub comments_total: u64,
    pub read_state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub last_opened_unix: Option<i64>,
    pub changed_since_read: bool,
}

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize)]
pub struct DeskListResponse {
    pub items: Vec<DeskListItem>,
    pub attention: u64,
}

#[derive(Debug, Deserialize, Default)]
pub struct DeskListParams {
    /// Restrict to a single corpus. Absent → every configured kb.
    pub kb: Option<String>,
}

#[derive(Default)]
struct DeskForm {
    name: Option<String>,
    title: Option<String>,
    tags: Vec<String>,
    session: Option<String>,
    ttl_secs: Option<String>,
    sanitize: Option<bool>,
    text: Option<String>,
    file: Option<(Option<String>, Vec<u8>)>,
}

#[allow(clippy::result_large_err)]
async fn drain_desk_form(
    multipart: &mut Multipart,
    max_bytes: u64,
) -> Result<DeskForm, Response<Body>> {
    let mut form = DeskForm::default();
    loop {
        let mut field = match multipart.next_field().await {
            Ok(Some(f)) => f,
            Ok(None) => break,
            Err(e) => {
                return Err(problem(
                    StatusCode::BAD_REQUEST,
                    format!("malformed multipart: {e}"),
                ))
            }
        };
        let name = field.name().unwrap_or_default().to_string();
        if name == "files" || name == "file" {
            if form.file.is_some() {
                return Err(problem(
                    StatusCode::BAD_REQUEST,
                    "desk accepts exactly one file",
                ));
            }
            let fname = field.file_name().map(|s| s.to_string());
            let mut buf: Vec<u8> = Vec::new();
            loop {
                match field.chunk().await {
                    Ok(Some(chunk)) => {
                        if buf.len() as u64 + chunk.len() as u64 > max_bytes {
                            return Err(problem(
                                StatusCode::PAYLOAD_TOO_LARGE,
                                format!("desk file exceeds the {max_bytes}-byte limit"),
                            ));
                        }
                        buf.extend_from_slice(chunk.as_ref());
                    }
                    Ok(None) => break,
                    Err(e) => {
                        return Err(problem(
                            StatusCode::BAD_REQUEST,
                            format!("upload read failed: {e}"),
                        ))
                    }
                }
            }
            if !buf.is_empty() {
                form.file = Some((fname, buf));
            }
            continue;
        }
        let text = match field.text().await {
            Ok(t) => t,
            Err(e) => {
                return Err(problem(
                    StatusCode::BAD_REQUEST,
                    format!("malformed multipart field `{name}`: {e}"),
                ))
            }
        };
        match name.as_str() {
            "name" => form.name = non_empty(text),
            "title" => form.title = non_empty(text),
            "tags" => form.tags = split_tags(&text),
            "session" => form.session = non_empty(text),
            "ttl_secs" => form.ttl_secs = non_empty(text),
            "sanitize" => form.sanitize = Some(parse_bool_field(&text)),
            "text" => form.text = non_empty(text),
            _ => {}
        }
    }
    Ok(form)
}

#[allow(clippy::result_large_err)] // same idiom as capture.rs form helpers
fn parse_ttl_secs(raw: &str) -> Result<u64, Response<Body>> {
    let n: u64 = raw.parse().map_err(|_| {
        problem(
            StatusCode::BAD_REQUEST,
            format!("ttl_secs must be a u64 (got {raw:?})"),
        )
    })?;
    if n == 0 || n > TEN_YEARS_SECS {
        return Err(problem(
            StatusCode::BAD_REQUEST,
            format!("ttl_secs must be in 1..={TEN_YEARS_SECS} (10 years); got {n}"),
        ));
    }
    Ok(n)
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// `POST /api/kb/{kb}/desk` — see the module doc.
pub async fn create(
    State(state): State<Arc<KbHandles>>,
    Path(kb): Path<String>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> Response<Body> {
    let (kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let (max_bytes, max_request_bytes, _) = capture_settings(&state, &kb_name).await;
    if let Some(resp) = reject_oversized_request(&headers, max_request_bytes) {
        return resp;
    }
    let mut form = match drain_desk_form(&mut multipart, max_bytes).await {
        Ok(f) => f,
        Err(resp) => return resp,
    };

    let Some(name) = form.name.take() else {
        return problem(
            StatusCode::BAD_REQUEST,
            "desk requires a `name` field (stable slug)",
        );
    };
    if form.file.is_some() && form.text.is_some() {
        return problem(
            StatusCode::BAD_REQUEST,
            "desk accepts one file or `text`, not both",
        );
    }

    let (pipeline, ext, fname, bytes) = if let Some((fname, bytes)) = form.file.take() {
        let name_for_ext = fname.as_deref().unwrap_or("");
        let Some(pipeline) = ctx.ext_map.pipeline(FsPath::new(name_for_ext)) else {
            return problem(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                format!(
                    "`{name_for_ext}` has no indexable extension for this kb (see \
                     [indexer.indexable_extensions] / [kb.*.indexable_extensions])"
                ),
            );
        };
        let ext = FsPath::new(name_for_ext)
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        (pipeline, ext, fname, bytes)
    } else if let Some(text) = form.text.take() {
        // stdin-shaped push: always Markdown, like `kb capture -`.
        let Some(pipeline) = ctx.ext_map.pipeline(FsPath::new("handoff.md")) else {
            return problem(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "this kb does not index `.md`",
            );
        };
        (
            pipeline,
            "md".to_string(),
            Some("handoff.md".to_string()),
            text.into_bytes(),
        )
    } else {
        return problem(
            StatusCode::BAD_REQUEST,
            "desk requires one file or a `text` field",
        );
    };

    finish_desk(
        ctx, &kb_name, &headers, name, form, pipeline, ext, fname, bytes,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn finish_desk(
    ctx: &crate::state::KbContext,
    kb_name: &kb_core::types::KbName,
    headers: &HeaderMap,
    name: String,
    mut form: DeskForm,
    pipeline: Pipeline,
    ext: String,
    fname: Option<String>,
    bytes: Vec<u8>,
) -> Response<Body> {
    let ttl = match form.ttl_secs.as_deref() {
        Some(raw) => match parse_ttl_secs(raw) {
            Ok(n) => Some(n),
            Err(resp) => return resp,
        },
        None => None,
    };
    let expires_at = ttl.map(|secs| now_unix().saturating_add(secs));

    if !form
        .tags
        .iter()
        .any(|t| kb_core::parser::slugify_tag(t) == "draft")
    {
        form.tags.push("draft".to_string());
    }

    let from = from_default(headers);
    let sanitize = form.sanitize.unwrap_or(false);
    let out = capture::capture(CaptureInput {
        source_root: &ctx.source_path,
        capture_dir: DESK_DIR,
        pipeline,
        ext: &ext,
        bytes: &bytes,
        original_filename: fname.as_deref(),
        title: form.title.as_deref(),
        tags: &form.tags,
        from: &from,
        sanitize,
        url: None,
        stable_name: Some(&name),
        session_id: form.session.as_deref(),
        expires_at,
        category: Some("handoff"),
    });
    let out = match out {
        Ok(o) => o,
        Err(e) => return map_capture_error(e),
    };

    // First offer mkdirs `handoff/` and writes the file in the same instant —
    // inotify can miss a file born inside a directory younger than the
    // recursive watch attach, leaving the artifact invisible until the next
    // reconcile. Deliver the event through the per-kb ingest sink ourselves
    // (invariant #17; same G7 path quarantine-restore uses). force=false: the
    // content-hash dedup gate still applies, so a byte-identical re-offer
    // stays a no-op.
    ctx.ingest
        .send(
            kb_core::indexer::WatchKind::Modified,
            ctx.source_path.join(&out.source_relative),
            false,
        )
        .await;

    let item = DeskItem {
        kb: kb_name.as_str().to_string(),
        id: out.id,
        source_relative: out.source_relative,
        title: out.title,
        url: None,
        created: out.created,
    };
    let status = if out.created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    (status, Json(item)).into_response()
}

/// `GET /api/desk` — federated handoff aggregate. `?kb=` restricts to one
/// corpus (404 on unknown). Per-corpus work through `buffered_join` (#28).
pub async fn list(
    State(state): State<Arc<KbHandles>>,
    Extension(identity): Extension<crate::middleware::Identity>,
    Query(params): Query<DeskListParams>,
) -> Response<Body> {
    let want_kb = params
        .kb
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    if let Some(name) = want_kb {
        if let Err(resp) = crate::routes::resolve_kb(&state, name) {
            return resp;
        }
    }

    let user = identity.user.clone();
    let mut futs: Vec<super::CorpusFut<'_, Vec<DeskListItem>>> = Vec::new();
    for (kb_name, ctx) in state.kbs.iter() {
        if let Some(w) = want_kb {
            if kb_name.as_str() != w {
                continue;
            }
        }
        let review_dir = state.paths.kb_review_dir(kb_name);
        let user = user.clone();
        futs.push(Box::pin(async move {
            collect_kb(kb_name.as_str(), ctx, &review_dir, &user).await
        }));
    }
    // PF-R1 — the operator-configurable `[server] fanout_cap` (default 8,
    // byte-identical to the old hardcoded `super::FANOUT_CAP`).
    let mut items: Vec<DeskListItem> = super::buffered_join(futs, state.fanout_cap)
        .await
        .into_iter()
        .flatten()
        .collect();
    items.sort_by(|a, b| {
        b.updated_unix
            .cmp(&a.updated_unix)
            .then_with(|| a.kb.cmp(&b.kb))
            .then_with(|| a.id.cmp(&b.id))
    });
    let attention = attention_count(&items);
    Json(DeskListResponse { items, attention }).into_response()
}

/// One corpus's `handoff/` docs. Errors fold to an empty Vec (#28).
async fn collect_kb(
    kb: &str,
    ctx: &crate::state::KbContext,
    review_dir: &FsPath,
    user: &str,
) -> Vec<DeskListItem> {
    // PF-R1 — reuse the per-(kb, generation) gallery row-set memo
    // (invariant #15) instead of an independent `list_docs(u32::MAX)` actor
    // round-trip; `GET /api/desk` fans this out over every kb concurrently
    // (invariant #28), so sharing the SAME warm scan the high-traffic
    // `/docs`/`/facets`/`/edges` routes keep hot matters more here, not
    // less. Only the (typically tiny) `handoff/`-prefixed subset is cloned
    // out of the memo, same cost as the old owned-Vec filter.
    let (rows, _edge_counts) = match crate::routes::docs::gallery_snapshot(ctx).await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(kb, error = %e, "desk aggregate gallery_snapshot failed");
            return Vec::new();
        }
    };
    let prefix = format!("{DESK_DIR}/");
    let handoff: Vec<_> = rows
        .iter()
        .filter(|r| doc_rel_path(&r.doc.path, &ctx.source_path).starts_with(&prefix))
        .map(|r| r.doc.clone())
        .collect();
    if handoff.is_empty() {
        return Vec::new();
    }
    let ids: Vec<String> = handoff.iter().map(|d| d.id.clone()).collect();
    let rollup = ctx
        .storage
        .reading_rollup_for_ids(ids, user.to_string())
        .await
        .unwrap_or_default();

    let mut items = Vec::with_capacity(handoff.len());
    for d in handoff {
        let source_relative = doc_rel_path(&d.path, &ctx.source_path);
        let updated_unix = d.mtime_unix.unwrap_or(0);
        let rr = rollup.get(&d.id);
        let read_state = desk_read_state(rr).to_string();
        let last_opened_unix = rr.and_then(|r| r.last_opened_unix);
        let changed = changed_since_read(updated_unix, last_opened_unix);
        let (comments_open, comments_total) = comment_counts_for(review_dir, &d.id);
        // `kb-session` is indexed (`DocSummary.kb_session`). `kb-expires-at`
        // is display-only and is not a lance column (schema change is out
        // of scope); omit rather than re-read the source file.
        let session_id = d
            .kb_session
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        items.push(DeskListItem {
            kb: kb.to_string(),
            id: d.id,
            source_relative,
            title: d.title,
            updated_unix,
            expires_at: None,
            session_id,
            comments_open,
            comments_total,
            read_state,
            last_opened_unix,
            changed_since_read: changed,
        });
    }
    items
}

fn comment_counts_for(review_dir: &FsPath, id: &str) -> (u64, u64) {
    match kb_core::review::load(&review_dir.join(format!("{id}.json"))) {
        Ok(Some(f)) => (f.open_count() as u64, f.comments.len() as u64),
        _ => (0, 0),
    }
}

/// Same rollup presence rule as `docs::list`'s `?read=` `never-opened`
/// token: absent from the map → never-opened; present → `ReadState`.
fn desk_read_state(rr: Option<&kb_core::reading::ReadRollup>) -> &'static str {
    match rr {
        None => "never-opened",
        Some(rr) => rr.state.as_str(),
    }
}

fn changed_since_read(updated_unix: i64, last_opened_unix: Option<i64>) -> bool {
    match last_opened_unix {
        Some(opened) => updated_unix > opened,
        None => false,
    }
}

fn needs_attention(read_state: &str, changed_since_read: bool) -> bool {
    read_state == "never-opened" || changed_since_read
}

fn attention_count(items: &[DeskListItem]) -> u64 {
    items
        .iter()
        .filter(|i| needs_attention(&i.read_state, i.changed_since_read))
        .count() as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use kb_core::lists::ReadState;
    use kb_core::reading::ReadRollup;

    #[test]
    fn changed_since_read_requires_opened_and_newer() {
        assert!(!changed_since_read(10, None));
        assert!(!changed_since_read(10, Some(10)));
        assert!(!changed_since_read(10, Some(11)));
        assert!(changed_since_read(11, Some(10)));
    }

    #[test]
    fn desk_read_state_absent_is_never_opened() {
        assert_eq!(desk_read_state(None), "never-opened");
        let unread = ReadRollup {
            last_opened_unix: None,
            completion_pct: 0,
            state: ReadState::Unread,
        };
        assert_eq!(desk_read_state(Some(&unread)), "unread");
        let read = ReadRollup {
            last_opened_unix: Some(1),
            completion_pct: 100,
            state: ReadState::Read,
        };
        assert_eq!(desk_read_state(Some(&read)), "read");
        let progress = ReadRollup {
            last_opened_unix: Some(1),
            completion_pct: 40,
            state: ReadState::InProgress,
        };
        assert_eq!(desk_read_state(Some(&progress)), "in_progress");
    }

    #[test]
    fn attention_counts_never_opened_or_changed() {
        assert!(needs_attention("never-opened", false));
        assert!(needs_attention("read", true));
        assert!(needs_attention("in_progress", true));
        assert!(!needs_attention("read", false));
        assert!(!needs_attention("in_progress", false));
        assert!(!needs_attention("unread", false));

        let items = [
            item("a", "never-opened", false),
            item("b", "read", true),
            item("c", "read", false),
            item("d", "in_progress", false),
        ];
        assert_eq!(attention_count(&items), 2);
    }

    fn item(id: &str, read_state: &str, changed: bool) -> DeskListItem {
        DeskListItem {
            kb: "smoke".into(),
            id: id.into(),
            source_relative: format!("handoff/{id}.md"),
            title: id.into(),
            updated_unix: 1,
            expires_at: None,
            session_id: None,
            comments_open: 0,
            comments_total: 0,
            read_state: read_state.into(),
            last_opened_unix: None,
            changed_since_read: changed,
        }
    }
}
