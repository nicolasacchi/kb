//! `POST /api/kb/{kb}/artifacts` — synchronous memory ingest backing
//! `kb remember`. Renders the memory HTML via the shared
//! `kb_core::memory::render_artifact`, picks a collision-resistant
//! filename, and writes it into the corpus dir. The live watcher then
//! indexes it (write-only MVP — `Created` events index exactly once, so
//! there's zero double-embed and no new actor plumbing; "searchable
//! immediately" becomes "within one debounce").
//!
//! ## U3 — highlight → save-as-memory (provenance)
//!
//! The SPA's selection chooser (`web/src/components/SelectionActions.tsx`)
//! saves a highlight as a memory through THIS route — the same body, the
//! same renderer, the same file write, the same `memory.ingested` event.
//! It adds four optional, `#[serde(default)]` fields (`author`,
//! `source_kb`, `source_artifact`, `source_anchor`) that ride into the
//! rendered artifact as `<meta>` tags; every existing client omits them
//! and gets byte-identical output. There is no provenance store and no
//! second write path — see `kb_core::memory::MemoryProvenance` for the
//! three rules the record obeys (rides the existing path · `author` is a
//! ROLE not an identity · surfaced, never a score term).
//!
//! **CLI-parity exemption (recorded).** U3 introduces no new server NOUN —
//! it widens the body of a route `kb remember` already drives — so no new
//! `kb` verb ships with it, and `kb remember` deliberately gains no
//! `--source-artifact`/`--anchor` flags: a selection anchor is produced by
//! dragging over rendered HTML in the reader, and there is nothing in a
//! terminal to drag. An agent that wants provenance already has the
//! honest primitive (`--session-id`). If a headless caller ever needs to
//! stamp an anchor, the fields are on the wire and a flag is a one-liner.

use crate::middleware::error_to_problem_json;
use crate::state::KbHandles;
use axum::{
    body::Body,
    extract::{Multipart, Path, Query, State},
    http::{HeaderMap, Response, StatusCode},
    response::IntoResponse,
    Json,
};
use kb_core::ids::ArtifactId;
use kb_core::types::KbName;
use serde::{Deserialize, Serialize};
use std::path::Path as FsPath;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Deserialize)]
pub struct IngestBody {
    pub title: String,
    /// Plain-text body — escaped + wrapped into paragraphs. Ignored when
    /// `body_html` is present.
    #[serde(default)]
    pub body: Option<String>,
    /// Pre-rendered inner-body HTML (takes precedence over `body`).
    #[serde(default)]
    pub body_html: Option<String>,
    #[serde(default = "default_category")]
    pub category: String,
    #[serde(default)]
    pub tags: Vec<String>,
    pub salience: Option<f32>,
    pub decay: Option<String>,
    pub supersedes: Option<String>,
    /// RA4 — one-line summary distinct from the title, persisted as
    /// `<meta name="kb-summary">` and surfaced on recall hits.
    pub summary: Option<String>,
    /// v0.14 S1 — Claude Code session id the memory originated from.
    /// `kb remember` reads it from `~/.cache/kb/current-session` (set
    /// by the SessionStart hook). Persisted as `<meta name="kb-session">`
    /// in the rendered artifact and surfaced via the `kb_session` lance
    /// projection.
    pub session_id: Option<String>,
    /// L5 — memory visibility. `true` (the default) writes
    /// `<meta name="kb-global" content="true">` so the indexer's
    /// first-time seed registers the `*` sentinel in V0010
    /// `memory_links` — making the memory recallable from every kb.
    /// Set to `false` when the caller wants a scoped memory; the
    /// `linked_kbs` field then names the kbs that should see it.
    pub global: Option<bool>,
    /// L5 — explicit kb name list. Slugified the same way as kb-tags
    /// at parse time. When non-empty, each name must correspond to an
    /// existing kb in this daemon — unknown names cause the ingest to
    /// 400 (we won't quietly write a meta that the recall self-heal
    /// will then ignore).
    #[serde(default)]
    pub linked_kbs: Vec<String>,
    /// U3 — the human↔agent ROLE the memory was written under (`"you"` /
    /// `"claude"`). Not an identity: kb is one daemon, one operator
    /// (README → Non-goals), so there is no user record behind this — it
    /// is the same two-valued split a `kb-comments/1` comment carries.
    /// Absent ⇒ unattributed (every pre-U3 memory), NOT "claude by
    /// default": we don't retro-attribute what we didn't record.
    #[serde(default)]
    pub author: Option<kb_core::review::Author>,
    /// U3 — kb name of the artifact a highlighted memory was cut from.
    /// Recorded VERBATIM (not validated against `state.kbs`, unlike
    /// `linked_kbs`): provenance is a historical fact about where the
    /// text came from, and a kb that has since been renamed or unmounted
    /// must not make the record un-writable. Same treatment as
    /// `session_id`.
    #[serde(default)]
    pub source_kb: Option<String>,
    /// U3 — artifact id within `source_kb`.
    #[serde(default)]
    pub source_artifact: Option<String>,
    /// U3 — the selection the memory text was lifted from, as a
    /// `review::Anchor` (invariant #25: ONE anchor shape — the same one
    /// highlights, comments and reading-list entries already use). A
    /// malformed anchor fails serde deserialization ⇒ 422, rather than
    /// silently writing a meta nothing can resolve.
    #[serde(default)]
    pub source_anchor: Option<kb_core::review::Anchor>,
    /// MI-W3.4 — write-time trust tag: `fetched-web` | `user-dictated` |
    /// `agent-inference`. Free-text on the wire, validated against the
    /// closed set here (400 on an unknown value — a typo'd trust tag must
    /// never silently persist as an ordinary string meta). SURFACED, NEVER
    /// SCORED (see `kb_core::memory::MemoryProvenance::source`'s doc).
    #[serde(default)]
    pub source: Option<String>,
    /// MI-W3.3a — optional CoALA-minimal type: `episodic` | `semantic` |
    /// `procedural`. Same closed-set validation as `source`. Absent (the
    /// default) leaves the memory untyped — never inferred, never
    /// backfilled.
    #[serde(default)]
    pub memory_type: Option<String>,
    /// CT-C3 — negative-memory outcome: `failed` (`kb remember --failed` —
    /// "this approach was tried and did NOT work"). Same closed-set
    /// validation as `source`/`memory_type` (400 on anything else). When
    /// failed, the route writes `<meta name="kb-outcome" content="failed">`
    /// AND ensures the paired `outcome:failed` kb-tag (the indexed carrier
    /// recall's `warns` + census's `failed` derive from) — see
    /// `kb_core::memory::MemoryOutcome`. Absent (the default) is the
    /// ordinary ok state; there is no `"ok"` value.
    #[serde(default)]
    pub outcome: Option<String>,
}

fn default_category() -> String {
    "memory-user".to_string()
}

/// Trim, then treat an all-whitespace value as absent — a blank
/// `source_kb`/`source_artifact` must not become an empty meta.
fn nonempty(s: Option<&str>) -> Option<String> {
    s.map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

#[derive(Debug, Serialize)]
pub struct IngestResponse {
    /// The path-derived 12-hex artifact id the indexer will assign.
    pub id: String,
    /// Source-root-relative path of the written file.
    pub path: String,
}

pub async fn create(
    State(state): State<Arc<KbHandles>>,
    Path(kb): Path<String>,
    Json(body): Json<IngestBody>,
) -> Response<Body> {
    let (kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    match ingest(&state, &kb_name, ctx, body) {
        Ok(resp) => (StatusCode::CREATED, Json(resp)).into_response(),
        Err(resp) => resp,
    }
}

/// Shared memory-write core: validate → render → write the artifact file →
/// emit `memory.ingested`/`memory.stale`. This is the WHOLE body of `create`
/// above pre-W2.15b, lifted out unchanged so `routes::proposals::approve`
/// can call the EXACT same path — approving a queued proposal must be
/// indistinguishable from a `kb remember`-authored memory (provenance like
/// `session_id` rides through unchanged). No route wraps this directly, so
/// it stays `pub(crate)`, out of the public HTTP surface. Synchronous (no
/// `.await`): the write-only ingest never touches storage, only the source
/// filesystem + the event bus.
#[allow(clippy::result_large_err)]
pub(crate) fn ingest(
    state: &KbHandles,
    kb_name: &KbName,
    ctx: &crate::state::KbContext,
    body: IngestBody,
) -> Result<IngestResponse, Response<Body>> {
    let title = body.title.trim();
    if title.is_empty() {
        return Err(error_to_problem_json(&kb_core::Error::BadRequest(
            "artifact title must not be empty".into(),
        )));
    }

    // body_html wins; else render plain text into paragraphs.
    let body_html = match (&body.body_html, &body.body) {
        (Some(h), _) => h.clone(),
        (None, Some(t)) => kb_core::memory::text_to_body_html(t),
        (None, None) => String::new(),
    };
    let salience = body.salience.map(|s| s.clamp(0.0, 1.0));

    // L5 — visibility resolution. Default (`global` absent) = global,
    // matching the CLI's pre-MI-W3.4 default. An explicit `linked_kbs`
    // list flips the default to scoped: each named kb must exist or we
    // 400.
    //
    // MI-W3.R — scope note (deliberate, not an oversight): `kb remember`'s
    // untrusted-source narrowing (`commands::memory::resolve_visibility` —
    // `--source fetched-web` with nothing else specified defaults to
    // non-global instead of global) is a CLI-DEFAULT decision, not a
    // trust boundary enforced here. This route takes `body.global`/
    // `body.linked_kbs` verbatim from the request — it has no notion of
    // "the caller didn't say", so there is nothing to narrow: a direct API
    // caller (curl, a non-`kb`-CLI agent, `routes::proposals::approve`'s
    // internal call) either supplies visibility explicitly or accepts
    // THIS route's own always-global-by-default rule above, unaffected by
    // the CLI's gate. See the mirror comment at `resolve_visibility`.
    let global = body.global.unwrap_or(body.linked_kbs.is_empty());
    for k in &body.linked_kbs {
        let target = match KbName::new(k) {
            Ok(k) => k,
            Err(e) => return Err(error_to_problem_json(&e)),
        };
        if !state.kbs.contains_key(&target) {
            return Err(error_to_problem_json(&kb_core::Error::BadRequest(format!(
                "linked_kbs: unknown kb '{k}'"
            ))));
        }
    }

    // RA3 — stamp the write time as `kb-created` so the memory's decay basis
    // is stable across reindex (the meta is in the source). Reused below as
    // the collision-resistant filename timestamp, so both agree.
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    // MI-W3.4 — validate the trust tag against the closed set BEFORE it can
    // reach the source as a free-text meta; an unrecognised value 400s
    // rather than silently persisting a typo forever (kb never corrects a
    // written meta after the fact).
    let trust_source = match body
        .source
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(s) => match kb_core::memory::TrustSource::parse(s) {
            Some(t) => Some(t),
            None => {
                return Err(error_to_problem_json(&kb_core::Error::BadRequest(format!(
                    "source must be one of: fetched-web, user-dictated, agent-inference; got {s:?}"
                ))))
            }
        },
        None => None,
    };
    // MI-W3.3a — same closed-set validation for the optional memory type.
    let memory_type = match body
        .memory_type
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(s) => match kb_core::memory::MemoryType::parse(s) {
            Some(t) => Some(t),
            None => {
                return Err(error_to_problem_json(&kb_core::Error::BadRequest(format!(
                    "memory_type must be one of: episodic, semantic, procedural; got {s:?}"
                ))))
            }
        },
        None => None,
    };
    // CT-C3 — same closed-set validation for the optional outcome ("failed"
    // is the only value; a typo'd outcome must never persist as silence).
    let outcome = match body
        .outcome
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(s) => match kb_core::memory::MemoryOutcome::parse(s) {
            Some(o) => Some(o),
            None => {
                return Err(error_to_problem_json(&kb_core::Error::BadRequest(format!(
                    "outcome must be \"failed\" (or absent); got {s:?}"
                ))))
            }
        },
        None => None,
    };
    // CT-C3 — the write-time pairing: a failed outcome ALWAYS carries the
    // `outcome:failed` kb-tag (the indexed carrier — the meta below is the
    // durable declaration, the tag is what the existing tags pipeline
    // surfaces through recall `warns` / census `failed` / `?tags=`).
    // Idempotent, so a CLI that already appended the tag client-side
    // doesn't double-tag.
    let mut tags = body.tags.clone();
    if outcome == Some(kb_core::memory::MemoryOutcome::Failed) {
        kb_core::memory::ensure_failed_outcome_tag(&mut tags);
    }

    // U3 — provenance for a memory the operator kept from a highlight.
    // Additive: absent on every `kb remember` / proposal-approval call,
    // in which case `provenance` is empty and the rendered HTML is
    // byte-identical to pre-U3. Recorded in the artifact source only —
    // it is never a recall score term (invariant #10; the ruling mirrors
    // #11's R3 treatment of recollect staleness).
    let provenance = kb_core::memory::MemoryProvenance {
        author: body.author,
        source_kb: nonempty(body.source_kb.as_deref()),
        source_artifact: nonempty(body.source_artifact.as_deref()),
        source_anchor: body.source_anchor.clone(),
        source: trust_source,
    };

    let html = kb_core::memory::render_artifact(
        title,
        &body_html,
        &body.category,
        &tags,
        salience,
        body.decay.as_deref(),
        body.supersedes.as_deref(),
        body.session_id.as_deref(),
        global,
        &body.linked_kbs,
        Some(ts as i64),
        body.summary.as_deref(),
        (!provenance.is_empty()).then_some(&provenance),
        memory_type,
        outcome,
    );

    // Collision-resistant filename: `<slug>-<unix_secs>[-n].html`. Two
    // same-title remembers land on distinct paths → distinct ids → two
    // artifacts (no silent merge_insert overwrite).
    let base = kb_core::memory::memory_slug(title);
    let mut filename = format!("{base}-{ts}.html");
    let mut n = 1;
    while ctx.source_path.join(&filename).exists() {
        filename = format!("{base}-{ts}-{n}.html");
        n += 1;
    }

    let abs = ctx.source_path.join(&filename);
    if let Err(e) = std::fs::write(&abs, &html) {
        return Err(error_to_problem_json(&kb_core::Error::Storage(format!(
            "write memory artifact {}: {e}",
            abs.display()
        ))));
    }

    // The id is the SHA-256 prefix of the source-relative path — the same
    // value the indexer will assign when the watcher picks the file up.
    let rel = kb_core::paths::doc_rel_path(&abs.to_string_lossy(), &ctx.source_path);
    let id = ArtifactId::from_path(&rel).to_string();

    // M6 — signal the SPA /memory view. memory.stale fires when this
    // memory shadows an older one (supersede is a 2-memory relationship,
    // not a function of one file's bytes — so it's emitted here, not from
    // the per-file indexer hook). The recall-time drop stays in rerank.
    ctx.bus.emit(
        "memory.ingested",
        serde_json::json!({ "kb": kb_name.as_str(), "id": id.as_str(), "path": rel.as_str() }),
    );
    if let Some(sup) = body.supersedes.as_deref().filter(|s| !s.is_empty()) {
        ctx.bus.emit(
            "memory.stale",
            serde_json::json!({ "kb": kb_name.as_str(), "id": sup, "superseded_by": id.as_str() }),
        );
    }

    Ok(IngestResponse { id, path: rel })
}

/// MI-W2.3 — `?purge=true` on `DELETE …/artifacts/{id}` bypasses the
/// soft-forget tombstone below and hard-deletes (the pre-W2.3 behavior):
/// removes the source file + drops the index row immediately, with no
/// trace left anywhere in kb. This is the escape hatch the EPOCH HONESTY
/// caveat machinery (MI-W2.4c, `kb_core::memory::tombstone_era` /
/// `kb memory log` / `kb diff --between`) exists BECAUSE of — a purge is
/// exactly the kind of unrecoverable deletion a temporal query can't see
/// past.
#[derive(Debug, Deserialize, Default)]
pub struct DeleteParams {
    #[serde(default)]
    pub purge: bool,
}

#[derive(Debug, Serialize)]
pub struct DeleteResponse {
    pub id: String,
    /// `true` when this call hard-deleted (file + index row gone, no
    /// trace); `false` when it soft-forgot (tombstoned in place — still on
    /// disk, still listed by `census`, dropped from `recall`). Lets a
    /// caller (`kb forget`) print which actually happened rather than
    /// assuming.
    pub purged: bool,
}

/// `DELETE /api/kb/{kb}/artifacts/{id}[?purge=true]` — forget a memory.
///
/// **Default (no `?purge`): soft forget (MI-W2.3).** Splices `kb-status:
/// forgotten` + `kb-forgotten-at: <now>` into the artifact's own source
/// ([`kb_core::memory::mark_forgotten`]) and lets the watcher reindex —
/// the file is untouched otherwise, still readable, still `kb versions`/
/// `kb diff`-able. This is what lights up invariant #10's previously-dead
/// `status == "forgotten"` recall filter. Every OTHER consumer keeps
/// seeing it: `kb search` of the memory corpus still matches it (no
/// automatic `kb-status` exclusion — see the doc comment on
/// `docs_query::matches`'s status handling), and `GET /api/memory/census`
/// lists it with the tombstone flagged (the whole point of a tombstone:
/// visible, not vanished).
///
/// **`?purge=true`: hard delete (pre-W2.3 behavior).** Removes the source
/// file + drops the index row immediately, with no trace anywhere.
/// Irreversible.
///
/// 404 when the id is unknown in this kb, either way.
pub async fn delete(
    State(state): State<Arc<KbHandles>>,
    Path((kb, id)): Path<(String, String)>,
    Query(params): Query<DeleteParams>,
) -> Response<Body> {
    let (kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let doc = match ctx.storage.get_by_id(id.clone()).await {
        Ok(Some(d)) => d,
        Ok(None) => {
            return error_to_problem_json(&kb_core::Error::NotFound(format!(
                "artifact {id} in kb {kb_name}"
            )))
        }
        Err(e) => return error_to_problem_json(&e),
    };

    if params.purge {
        match std::fs::remove_file(&doc.path) {
            Ok(()) => {}
            // Already gone on disk — fall through to drop the row.
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
            "memory.forgotten",
            serde_json::json!({ "kb": kb_name.as_str(), "id": id.as_str(), "purged": true }),
        );
        return Json(DeleteResponse { id, purged: true }).into_response();
    }

    // MI-W2.3 — soft forget: tombstone the source in place.
    let src = match std::fs::read_to_string(&doc.path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return error_to_problem_json(&kb_core::Error::NotFound(format!(
                "artifact source {} missing on disk",
                doc.path
            )))
        }
        Err(e) => {
            return error_to_problem_json(&kb_core::Error::Storage(format!(
                "read {}: {e}",
                doc.path
            )))
        }
    };
    let is_md = ctx.ext_map.is_markdown(FsPath::new(&doc.path));
    let now_unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let new_src = kb_core::memory::mark_forgotten(&src, is_md, now_unix);
    if new_src != src {
        if let Err(e) = kb_core::fsx::write_atomic(FsPath::new(&doc.path), new_src.as_bytes()) {
            return error_to_problem_json(&e);
        }
    }
    ctx.bus.emit(
        "memory.forgotten",
        serde_json::json!({ "kb": kb_name.as_str(), "id": id.as_str(), "purged": false }),
    );
    Json(DeleteResponse { id, purged: false }).into_response()
}

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize)]
pub struct ContentReplaceResponse {
    pub id: String,
    pub source_relative: String,
    pub bytes: u64,
}

/// `PUT /api/kb/{kb}/artifacts/{id}/content` — replace-in-place.
///
/// Resolves `{id}` the same way [`delete`] does (`get_by_id` → source
/// path). ONE multipart file part. The uploaded extension must resolve
/// via `ctx.ext_map` AND map to the SAME pipeline family as the existing
/// file (`.md` cannot replace `.html` → 415). Capture byte caps apply.
/// Write is `fsx::write_atomic`; the watcher reindexes (invariant #17).
pub async fn put_content(
    State(state): State<Arc<KbHandles>>,
    Path((kb, id)): Path<(String, String)>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> Response<Body> {
    use crate::routes::capture::{capture_settings, problem, reject_oversized_request};

    let (kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let (max_bytes, max_request_bytes, _) = capture_settings(&state, &kb_name).await;
    if let Some(resp) = reject_oversized_request(&headers, max_request_bytes) {
        return resp;
    }

    let doc = match ctx.storage.get_by_id(id.clone()).await {
        Ok(Some(d)) => d,
        Ok(None) => {
            return error_to_problem_json(&kb_core::Error::NotFound(format!(
                "artifact {id} in kb {kb_name}"
            )))
        }
        Err(e) => return error_to_problem_json(&e),
    };

    let existing_pipeline = match ctx.ext_map.pipeline(FsPath::new(&doc.path)) {
        Some(p) => p,
        None => {
            return problem(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                format!(
                    "existing artifact `{}` has no indexable extension",
                    doc.path
                ),
            )
        }
    };

    let (fname, bytes) = match drain_one_file(&mut multipart, max_bytes).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let name_for_ext = fname.as_deref().unwrap_or("");
    let Some(new_pipeline) = ctx.ext_map.pipeline(FsPath::new(name_for_ext)) else {
        return problem(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            format!(
                "`{name_for_ext}` has no indexable extension for this kb (see \
                 [indexer.indexable_extensions] / [kb.*.indexable_extensions])"
            ),
        );
    };
    if new_pipeline != existing_pipeline {
        return problem(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            format!(
                "pipeline family mismatch: existing is {}, upload is {} \
                 (an .md body cannot replace an .html artifact)",
                existing_pipeline.as_str(),
                new_pipeline.as_str()
            ),
        );
    }

    if let Err(e) = kb_core::fsx::write_atomic(FsPath::new(&doc.path), &bytes) {
        if let kb_core::Error::Io(io_err) = &e {
            if io_err.kind() == std::io::ErrorKind::PermissionDenied {
                return error_to_problem_json(&kb_core::Error::Conflict(
                    "artifact destination is read-only".to_string(),
                ));
            }
        }
        return error_to_problem_json(&e);
    }

    let source_relative = kb_core::paths::doc_rel_path(&doc.path, &ctx.source_path);
    (
        StatusCode::OK,
        Json(ContentReplaceResponse {
            id,
            source_relative,
            bytes: bytes.len() as u64,
        }),
    )
        .into_response()
}

#[allow(clippy::result_large_err)]
async fn drain_one_file(
    multipart: &mut Multipart,
    max_bytes: u64,
) -> Result<(Option<String>, Vec<u8>), Response<Body>> {
    use crate::routes::capture::problem;
    let mut found: Option<(Option<String>, Vec<u8>)> = None;
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
        let fname = field.file_name().map(|s| s.to_string());
        // Skip non-file text fields (clients may send leftover form fields).
        if fname.is_none() && field.name() != Some("files") && field.name() != Some("file") {
            let _ = field.text().await;
            continue;
        }
        if found.is_some() {
            return Err(problem(
                StatusCode::BAD_REQUEST,
                "content replace accepts exactly one file",
            ));
        }
        let mut buf: Vec<u8> = Vec::new();
        loop {
            match field.chunk().await {
                Ok(Some(chunk)) => {
                    if buf.len() as u64 + chunk.len() as u64 > max_bytes {
                        return Err(problem(
                            StatusCode::PAYLOAD_TOO_LARGE,
                            format!("file exceeds the {max_bytes}-byte limit"),
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
        found = Some((fname, buf));
    }
    found.ok_or_else(|| {
        problem(
            StatusCode::BAD_REQUEST,
            "content replace requires one file part",
        )
    })
}

/// `PATCH /api/kb/{kb}/artifacts/{id}/meta` — edit an artifact's
/// `kb-tags` / `kb-category` in its own source file, then let the watcher
/// re-index it (the same "write-only, searchable within one debounce"
/// path as `create`). Scoped STRICTLY to those two facets — it never
/// touches the memory metas (`kb-salience`/`kb-decay`/`kb-global`/
/// `kb-linked-kbs`), which have their own owners.
///
/// HTML sources are edited via `kb_core::meta_edit` (a byte-preserving
/// `<meta>` splice that skips `<template>`/`<script>`/`<style>`/comment
/// regions); Markdown sources via `kb_core::markdown::set_frontmatter_field`.
/// Returns the EFFECTIVE values after the edit — for tags that means the
/// slugged set, or the path-derived fallback when cleared (there is no
/// "no tags" representation), so the response matches what lance will hold
/// after re-index.
#[derive(Debug, Deserialize)]
pub struct MetaPatchBody {
    /// When present, replaces the artifact's tags (slugified server-side
    /// with the same `parser::slugify_tag` the indexer applies). An empty
    /// array clears them. Absent ⇒ tags left untouched.
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    /// When present, sets `kb-category`; an empty/whitespace string clears
    /// it. Absent ⇒ category left untouched. Not slugified (the parser
    /// keeps category verbatim-trimmed).
    #[serde(default)]
    pub category: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct MetaPatchResponse {
    pub id: String,
    /// Effective tags after the edit (slugged, or path-derived if cleared).
    pub tags: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kb_category: Option<String>,
}

pub async fn patch_meta(
    State(state): State<Arc<KbHandles>>,
    Path((kb, id)): Path<(String, String)>,
    Json(body): Json<MetaPatchBody>,
) -> Response<Body> {
    let (kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    if body.tags.is_none() && body.category.is_none() {
        return error_to_problem_json(&kb_core::Error::BadRequest(
            "meta patch must set at least one of: tags, category".into(),
        ));
    }

    let doc = match ctx.storage.get_by_id(id.clone()).await {
        Ok(Some(d)) => d,
        Ok(None) => {
            return error_to_problem_json(&kb_core::Error::NotFound(format!(
                "artifact {id} in kb {kb_name}"
            )))
        }
        Err(e) => return error_to_problem_json(&e),
    };

    // Slugify the requested tags exactly as the indexer does, dropping
    // empties and order-preserving dups.
    let slugged: Option<Vec<String>> = body.tags.as_ref().map(|ts| {
        let mut seen = std::collections::HashSet::new();
        ts.iter()
            .map(|t| kb_core::parser::slugify_tag(t))
            .filter(|s| !s.is_empty())
            .filter(|s| seen.insert(s.clone()))
            .collect()
    });
    let category: Option<String> = body.category.as_ref().map(|c| c.trim().to_string());

    let src = match std::fs::read_to_string(&doc.path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return error_to_problem_json(&kb_core::Error::NotFound(format!(
                "artifact source {} missing on disk",
                doc.path
            )))
        }
        Err(e) => {
            return error_to_problem_json(&kb_core::Error::Storage(format!(
                "read {}: {e}",
                doc.path
            )))
        }
    };
    // SC5 — `ctx.ext_map`, not the hardcoded extension check: a mapped
    // extension (e.g. `txt = "markdown"`) writes frontmatter here too,
    // matching how the indexer actually parsed the file.
    let is_md = ctx.ext_map.is_markdown(FsPath::new(&doc.path));

    let mut new_src = src.clone();
    if let Some(tags) = &slugged {
        new_src = if is_md {
            let joined = tags.join(", ");
            let val = (!tags.is_empty()).then_some(joined.as_str());
            kb_core::markdown::set_frontmatter_field(&new_src, "kb-tags", val)
        } else {
            kb_core::meta_edit::set_tags(&new_src, tags)
        };
    }
    if let Some(cat) = &category {
        let val = (!cat.is_empty()).then_some(cat.as_str());
        new_src = if is_md {
            kb_core::markdown::set_frontmatter_field(&new_src, "kb-category", val)
        } else {
            kb_core::meta_edit::set_meta_content(&new_src, "kb-category", val)
        };
    }

    // Skip the write (and the re-index it would trigger) when nothing
    // actually changed — keeps mtime stable and avoids a no-op churn.
    if new_src != src {
        if let Err(e) = kb_core::fsx::write_atomic(FsPath::new(&doc.path), new_src.as_bytes()) {
            return error_to_problem_json(&e);
        }
    }

    // Effective values — what the next re-index will store. Tags cleared
    // to empty re-derive from the path; category cleared becomes None.
    let tags_out: Vec<String> = match &slugged {
        Some(t) if !t.is_empty() => t.clone(),
        Some(_) => kb_core::parser::path_derived_tags(&doc.path),
        None => doc.tags.clone(),
    };
    let category_out: Option<String> = match &category {
        Some(c) if !c.is_empty() => Some(c.clone()),
        Some(_) => None,
        None => doc.kb_category.clone(),
    };

    // No SSE emit here: rewriting the file makes the watcher re-index it,
    // which fires the authoritative `artifact.indexed` (within one
    // debounce) that the SPA already listens for. The PATCH response's
    // echoed values give the editing client instant optimistic feedback.
    (
        StatusCode::OK,
        Json(MetaPatchResponse {
            id,
            tags: tags_out,
            kb_category: category_out,
        }),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use kb_core::review::{Anchor, Author};

    /// The U3 provenance fields are ADDITIVE: an `IngestBody` from a
    /// pre-U3 client (`kb remember`, the proposal-approval path, any
    /// script) still deserializes, with every provenance field `None` —
    /// so the rendered memory HTML is byte-identical to what it was.
    #[test]
    fn memory_ingest_body_without_provenance_still_deserializes() {
        let body: IngestBody =
            serde_json::from_str(r#"{"title":"t","body":"b","tags":["x"]}"#).unwrap();
        assert!(body.author.is_none());
        assert!(body.source_kb.is_none());
        assert!(body.source_artifact.is_none());
        assert!(body.source_anchor.is_none());
        assert!(body.source.is_none());
        assert!(body.memory_type.is_none());
        assert!(body.outcome.is_none());
        let prov = kb_core::memory::MemoryProvenance {
            author: body.author,
            source_kb: nonempty(body.source_kb.as_deref()),
            source_artifact: nonempty(body.source_artifact.as_deref()),
            source_anchor: body.source_anchor.clone(),
            source: None,
        };
        assert!(prov.is_empty(), "no provenance ⇒ no provenance metas");
    }

    /// The SPA's highlight → save-as-memory body: the ROLE (`you`), the
    /// source artifact, and a `review::Anchor` selection — the SAME anchor
    /// shape a comment/list entry carries (invariant #25), not a second one.
    #[test]
    fn memory_ingest_body_parses_highlight_provenance() {
        let raw = r#"{
            "title":"Highlighted claim",
            "body":"the selection, verbatim",
            "author":"you",
            "source_kb":"kb-docs",
            "source_artifact":"a1b2c3d4e5f6",
            "source_anchor":{"kind":"selection","css_path":"main > p","offset":3,"snippet":"s"}
        }"#;
        let body: IngestBody = serde_json::from_str(raw).unwrap();
        assert_eq!(body.author, Some(Author::You));
        assert_eq!(body.source_kb.as_deref(), Some("kb-docs"));
        assert_eq!(body.source_artifact.as_deref(), Some("a1b2c3d4e5f6"));
        match body.source_anchor.as_ref().expect("anchor") {
            Anchor::Selection {
                css_path,
                offset,
                snippet,
            } => {
                assert_eq!(css_path, "main > p");
                assert_eq!(*offset, 3);
                assert_eq!(snippet, "s");
            }
            other => panic!("expected a selection anchor, got {other:?}"),
        }
        // The category default is untouched by provenance — a highlight
        // memory is an ordinary `memory-user` memory (ruling 5: no
        // "human memories matter more" salience multiplier either).
        assert_eq!(body.category, "memory-user");
        assert!(body.salience.is_none(), "default salience, no thumb on it");
    }

    /// CT-C3 — `outcome` is additive on the wire and validated through the
    /// ONE kb-core grammar; the write-time pairing appends the
    /// `outcome:failed` tag exactly once (idempotent even when the client
    /// already sent it, as `kb remember --failed` does).
    #[test]
    fn memory_ingest_body_parses_failed_outcome_and_the_pairing_is_idempotent() {
        let body: IngestBody =
            serde_json::from_str(r#"{"title":"t","body":"b","outcome":"failed"}"#).unwrap();
        assert_eq!(body.outcome.as_deref(), Some("failed"));
        assert_eq!(
            kb_core::memory::MemoryOutcome::parse(body.outcome.as_deref().unwrap()),
            Some(kb_core::memory::MemoryOutcome::Failed)
        );
        // The route's tag pairing (mirrored here on the same helper): a
        // bare tag list gains the tag; a client-tagged list is unchanged.
        let mut tags = body.tags.clone();
        kb_core::memory::ensure_failed_outcome_tag(&mut tags);
        assert_eq!(tags, vec!["outcome:failed".to_string()]);
        kb_core::memory::ensure_failed_outcome_tag(&mut tags);
        assert_eq!(tags.len(), 1);
        // An unknown outcome value is rejected by the closed set the route
        // 400s on — never silently persisted.
        assert_eq!(kb_core::memory::MemoryOutcome::parse("ok"), None);
    }

    /// Blank provenance strings are absent, not empty metas.
    #[test]
    fn memory_ingest_blank_provenance_strings_are_absent() {
        assert_eq!(nonempty(None), None);
        assert_eq!(nonempty(Some("   ")), None);
        assert_eq!(nonempty(Some("  kb-docs ")), Some("kb-docs".to_string()));
    }

    // ---- MI-W3.3a / MI-W3.4 — memory_type + source on the wire ---------

    #[test]
    fn memory_ingest_body_parses_source_and_memory_type() {
        let raw = r#"{
            "title":"t",
            "body":"b",
            "source":"fetched-web",
            "memory_type":"semantic"
        }"#;
        let body: IngestBody = serde_json::from_str(raw).unwrap();
        assert_eq!(body.source.as_deref(), Some("fetched-web"));
        assert_eq!(body.memory_type.as_deref(), Some("semantic"));
    }

    #[test]
    fn memory_ingest_body_source_and_memory_type_are_additive_and_absent_by_default() {
        let body: IngestBody = serde_json::from_str(r#"{"title":"t","body":"b"}"#).unwrap();
        assert!(body.source.is_none());
        assert!(body.memory_type.is_none());
    }
}
