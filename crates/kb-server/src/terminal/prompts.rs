//! The terminal prompt queue (kbt2 PQSTORE): a daemon-owned, per-project
//! ledger of saved to-do prompts (`kb-term-prompts/1`).
//!
//! The daemon only STORES the queue. Sending an item to a terminal is the
//! SPA's job through the existing REST paste, and this module never talks to
//! a terminal, never sends and never marks anything done on its own.
//!
//! Routes (`/api/term-prompts/{project}[/{item}]`, deliberately outside
//! `/api/terms/...` so no `{id}` route can shadow them) are terminal-tier
//! routes: every handler runs [`gate`] (mutations) or [`gate_read`] (GET)
//! first, so token callers are refused and loopback does not bypass. The
//! ledger is mutated ONLY here, under ONE daemon-wide async lock held across
//! the load-modify-write (no other `.await` happens while it is held), and
//! written tmp file -> fsync -> rename so a crash leaves the old or the new
//! file, never a torn one.
//!
//! The document is `{"schema":"kb-term-prompts/1","project","rev","items":[…]}`.
//! The `Vec` order IS the order (the wire's `order` is the index, so no stored
//! field can drift); `rev` bumps by 1 per durable mutation and is the ETag. An
//! unknown schema or corrupt JSON is refused with a typed 500 and the file is
//! left untouched, never silently reset.
//!
//! Prompt text is operator-authored content: it is never logged, never
//! indexed, never put in an event and never echoed in a problem `detail`; the
//! `Debug` impls of every type that carries it elide it.

use std::fmt;
use std::io::Write;
use std::path::Path;
use std::sync::Arc;

use axum::body::{Body, Bytes};
use axum::extract::{Path as AxPath, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Extension;
use kb_core::paths::KbPaths;
use kb_core::slate::SlateSlug;
use serde::{Deserialize, Serialize};

use crate::middleware::Identity;
use crate::state::KbHandles;
use crate::terminal::gate::{
    gate, gate_read, Problem, ERR_TERMINAL_BAD_REQUEST, ERR_TERMINAL_PAYLOAD_TOO_LARGE,
};

/// The on-disk schema string.
pub const SCHEMA: &str = "kb-term-prompts/1";
/// Longest prompt, in bytes after trimming.
pub const MAX_TEXT_BYTES: usize = 8192;
/// Most items per project, done items included.
pub const MAX_ITEMS: usize = 100;
/// Most projects with a ledger file.
pub const MAX_PROJECTS: usize = 64;
/// Largest request body any route here parses.
pub const MAX_BODY_BYTES: usize = 16 * 1024;

/// `urn:kb:errors:bad-text` — the prompt text is empty, too long or has a NUL.
pub(crate) const ERR_BAD_TEXT: &str = "urn:kb:errors:bad-text";
/// `urn:kb:errors:queue-full` — the project already holds `MAX_ITEMS` items.
pub(crate) const ERR_QUEUE_FULL: &str = "urn:kb:errors:queue-full";
/// `urn:kb:errors:too-many-projects` — `MAX_PROJECTS` ledgers already exist.
pub(crate) const ERR_TOO_MANY_PROJECTS: &str = "urn:kb:errors:too-many-projects";
/// `urn:kb:errors:prompt-not-found` — no such item in the project's queue.
pub(crate) const ERR_PROMPT_NOT_FOUND: &str = "urn:kb:errors:prompt-not-found";
/// `urn:kb:errors:prompts-stale` — `If-Match` named an old revision (412).
pub(crate) const ERR_PROMPTS_STALE: &str = "urn:kb:errors:prompts-stale";
/// `urn:kb:errors:precondition-required` — a PATCH without `If-Match` (428).
pub(crate) const ERR_PRECONDITION_REQUIRED: &str = "urn:kb:errors:precondition-required";
/// `urn:kb:errors:prompts-corrupt` — the ledger file is unreadable or has an
/// unknown schema; it is left untouched (500).
pub(crate) const ERR_PROMPTS_CORRUPT: &str = "urn:kb:errors:prompts-corrupt";
/// `urn:kb:errors:prompts-io` — the ledger could not be written (500).
pub(crate) const ERR_PROMPTS_IO: &str = "urn:kb:errors:prompts-io";

// ---------------------------------------------------------------------------
// Wire types
// ---------------------------------------------------------------------------

/// One queued prompt on the wire. `order` is the index in the queue.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Clone, Serialize)]
pub struct PromptItem {
    pub id: String,
    pub text: String,
    pub created_at: i64,
    pub done_at: Option<i64>,
    pub order: u32,
}

impl fmt::Debug for PromptItem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PromptItem")
            .field("id", &self.id)
            .field("text", &format_args!("<{} bytes elided>", self.text.len()))
            .field("created_at", &self.created_at)
            .field("done_at", &self.done_at)
            .field("order", &self.order)
            .finish()
    }
}

/// One project's whole queue.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Serialize)]
pub struct PromptList {
    pub project: String,
    #[cfg_attr(feature = "ts-export", ts(type = "number"))]
    pub rev: u64,
    pub items: Vec<PromptItem>,
}

/// `POST` body.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PromptAdd {
    pub text: String,
}

impl fmt::Debug for PromptAdd {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PromptAdd")
            .field("text", &format_args!("<{} bytes elided>", self.text.len()))
            .finish()
    }
}

/// `PATCH` body: at least one field. `position` is the new index in the
/// queue, clamped to the last slot.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PromptPatch {
    #[serde(default)]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub text: Option<String>,
    #[serde(default)]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub done: Option<bool>,
    #[serde(default)]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub position: Option<u32>,
}

impl fmt::Debug for PromptPatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PromptPatch")
            .field("text", &self.text.as_ref().map(|t| t.len()))
            .field("done", &self.done)
            .field("position", &self.position)
            .finish()
    }
}

/// The `term.prompts` SSE payload: the project and its new revision, never
/// any text. A client refetches the list.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Serialize)]
pub struct TermPromptsEvent {
    pub project: String,
    #[cfg_attr(feature = "ts-export", ts(type = "number"))]
    pub rev: u64,
}

// ---------------------------------------------------------------------------
// The store (sync; the handlers hold the daemon-wide lock around it)
// ---------------------------------------------------------------------------

#[derive(Clone, Serialize, Deserialize)]
struct StoredItem {
    id: String,
    text: String,
    created_at: i64,
    #[serde(default)]
    done_at: Option<i64>,
}

impl fmt::Debug for StoredItem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StoredItem")
            .field("id", &self.id)
            .field("text", &format_args!("<{} bytes elided>", self.text.len()))
            .finish()
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct Doc {
    schema: String,
    project: String,
    rev: u64,
    items: Vec<StoredItem>,
}

impl Doc {
    fn empty(slug: &SlateSlug) -> Self {
        Doc {
            schema: SCHEMA.to_string(),
            project: slug.as_str().to_string(),
            rev: 0,
            items: Vec::new(),
        }
    }

    fn to_list(&self) -> PromptList {
        PromptList {
            project: self.project.clone(),
            rev: self.rev,
            items: self
                .items
                .iter()
                .enumerate()
                .map(|(i, it)| PromptItem {
                    id: it.id.clone(),
                    text: it.text.clone(),
                    created_at: it.created_at,
                    done_at: it.done_at,
                    order: u32::try_from(i).unwrap_or(u32::MAX),
                })
                .collect(),
        }
    }
}

/// Why a store operation refused. Carries no prompt text.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum StoreError {
    BadText(&'static str),
    QueueFull,
    TooManyProjects,
    NotFound,
    Stale { current: u64 },
    Corrupt,
    Io,
}

/// One mutation.
pub(crate) enum Op {
    Add(String),
    Patch {
        id: String,
        text: Option<String>,
        done: Option<bool>,
        position: Option<u32>,
    },
    Delete {
        id: String,
    },
}

/// What [`apply`] did. `changed` is false for a no-op PATCH (no rev bump,
/// no file write, no event).
#[derive(Debug)]
pub(crate) struct Applied {
    pub list: PromptList,
    pub changed: bool,
}

/// Trim and bound a prompt: `1..=MAX_TEXT_BYTES` bytes after trim, no NUL.
/// (UTF-8 is guaranteed by the `String` the JSON decoder produced; invalid
/// UTF-8 never reaches here, the body is refused as malformed.)
pub(crate) fn validate_text(raw: &str) -> Result<String, StoreError> {
    if raw.contains('\0') {
        return Err(StoreError::BadText("prompt text must not contain NUL"));
    }
    let t = raw.trim();
    if t.is_empty() {
        return Err(StoreError::BadText("prompt text must not be empty"));
    }
    if t.len() > MAX_TEXT_BYTES {
        return Err(StoreError::BadText("prompt text is over 8192 bytes"));
    }
    Ok(t.to_string())
}

/// `q_` plus 12 lowercase hex (48 random bits).
fn mint_id() -> Result<String, StoreError> {
    let mut b = [0u8; 6];
    getrandom::fill(&mut b).map_err(|_| StoreError::Io)?;
    let mut s = String::with_capacity(14);
    s.push_str("q_");
    for byte in b {
        s.push_str(&format!("{byte:02x}"));
    }
    Ok(s)
}

/// True iff `s` has the minted id shape `q_<12 lowercase hex>`.
pub(crate) fn is_valid_item_id(s: &str) -> bool {
    s.len() == 14
        && s.starts_with("q_")
        && s[2..]
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn load_doc(paths: &KbPaths, slug: &SlateSlug) -> Result<Doc, StoreError> {
    let path = paths.term_prompts_file(slug);
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Doc::empty(slug)),
        Err(_) => {
            tracing::warn!(project = %slug, "term-prompts: ledger unreadable");
            return Err(StoreError::Io);
        }
    };
    // The serde error is dropped on purpose: it must not become a log line
    // or a response detail.
    match serde_json::from_slice::<Doc>(&bytes) {
        Ok(doc) if doc.schema == SCHEMA && doc.project == slug.as_str() => Ok(doc),
        _ => {
            tracing::warn!(
                project = %slug,
                "term-prompts: ledger refused (corrupt or unknown schema), left untouched"
            );
            Err(StoreError::Corrupt)
        }
    }
}

/// tmp file -> fsync -> rename (-> best-effort directory fsync).
fn save_doc(paths: &KbPaths, slug: &SlateSlug, doc: &Doc) -> Result<(), StoreError> {
    let dir = paths.term_prompts_dir();
    std::fs::create_dir_all(&dir).map_err(|_| StoreError::Io)?;
    let final_path = paths.term_prompts_file(slug);
    let mut tmp_name = final_path.as_os_str().to_owned();
    tmp_name.push(".tmp");
    let tmp_path = std::path::PathBuf::from(tmp_name);
    let bytes = serde_json::to_vec(doc).map_err(|_| StoreError::Io)?;
    let write = || -> std::io::Result<()> {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp_path)?;
        f.write_all(&bytes)?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp_path, &final_path)?;
        if let Ok(d) = std::fs::File::open(&dir) {
            let _ = d.sync_all();
        }
        Ok(())
    };
    if write().is_err() {
        let _ = std::fs::remove_file(&tmp_path);
        tracing::warn!(project = %slug, "term-prompts: ledger write failed");
        return Err(StoreError::Io);
    }
    Ok(())
}

/// Ledger files in the directory (a `.tmp` leftover is not a project).
fn count_projects(dir: &Path) -> usize {
    std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .filter(|e| e.file_name().to_string_lossy().ends_with(".json"))
                .count()
        })
        .unwrap_or(0)
}

/// Read one project's queue (rev 0 and no items when it has no file).
pub(crate) fn read_list(paths: &KbPaths, slug: &SlateSlug) -> Result<PromptList, StoreError> {
    Ok(load_doc(paths, slug)?.to_list())
}

/// Apply one mutation to a project's ledger. The CALLER holds the
/// daemon-wide lock. `if_match` is the revision the client saw (`None` =
/// unconditional: only an add and an unconditional delete pass `None`).
pub(crate) fn apply(
    paths: &KbPaths,
    slug: &SlateSlug,
    if_match: Option<u64>,
    op: Op,
    now: i64,
) -> Result<Applied, StoreError> {
    let mut doc = load_doc(paths, slug)?;
    if let Some(seen) = if_match {
        if seen != doc.rev {
            return Err(StoreError::Stale { current: doc.rev });
        }
    }
    let mut changed = false;
    match op {
        Op::Add(raw) => {
            let text = validate_text(&raw)?;
            if doc.items.len() >= MAX_ITEMS {
                return Err(StoreError::QueueFull);
            }
            // A saved ledger always has rev >= 1, so rev 0 = "no file yet".
            if doc.rev == 0 && count_projects(&paths.term_prompts_dir()) >= MAX_PROJECTS {
                return Err(StoreError::TooManyProjects);
            }
            doc.items.push(StoredItem {
                id: mint_id()?,
                text,
                created_at: now,
                done_at: None,
            });
            changed = true;
        }
        Op::Patch {
            id,
            text,
            done,
            position,
        } => {
            let text = text.as_deref().map(validate_text).transpose()?;
            let idx = doc
                .items
                .iter()
                .position(|i| i.id == id)
                .ok_or(StoreError::NotFound)?;
            if let Some(t) = text {
                if doc.items[idx].text != t {
                    doc.items[idx].text = t;
                    changed = true;
                }
            }
            match done {
                Some(true) if doc.items[idx].done_at.is_none() => {
                    doc.items[idx].done_at = Some(now);
                    changed = true;
                }
                Some(false) if doc.items[idx].done_at.is_some() => {
                    doc.items[idx].done_at = None;
                    changed = true;
                }
                _ => {}
            }
            if let Some(p) = position {
                let target = (p as usize).min(doc.items.len() - 1);
                if target != idx {
                    let item = doc.items.remove(idx);
                    doc.items.insert(target, item);
                    changed = true;
                }
            }
        }
        Op::Delete { id } => {
            let idx = doc
                .items
                .iter()
                .position(|i| i.id == id)
                .ok_or(StoreError::NotFound)?;
            doc.items.remove(idx);
            changed = true;
        }
    }
    if changed {
        doc.rev += 1;
        save_doc(paths, slug, &doc)?;
        tracing::debug!(project = %slug, rev = doc.rev, "term-prompts: ledger mutated");
    }
    Ok(Applied {
        list: doc.to_list(),
        changed,
    })
}

// ---------------------------------------------------------------------------
// HTTP
// ---------------------------------------------------------------------------

fn no_store(mut resp: Response<Body>) -> Response<Body> {
    resp.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    resp
}

fn with_etag(mut resp: Response<Body>, rev: u64) -> Response<Body> {
    if let Ok(v) = HeaderValue::from_str(&format!("\"{rev}\"")) {
        resp.headers_mut().insert(header::ETAG, v);
    }
    resp
}

fn problem(
    urn: &'static str,
    status: StatusCode,
    title: &'static str,
    detail: &str,
) -> Response<Body> {
    no_store(Problem::response(urn, status, title, detail.to_string()))
}

fn bad_request(detail: &str) -> Response<Body> {
    problem(
        ERR_TERMINAL_BAD_REQUEST,
        StatusCode::BAD_REQUEST,
        "Bad Request",
        detail,
    )
}

fn list_response(status: StatusCode, list: &PromptList) -> Response<Body> {
    match serde_json::to_vec(list) {
        Ok(bytes) => {
            let mut resp =
                (status, [(header::CONTENT_TYPE, "application/json")], bytes).into_response();
            resp = no_store(resp);
            with_etag(resp, list.rev)
        }
        Err(_) => problem(
            ERR_PROMPTS_IO,
            StatusCode::INTERNAL_SERVER_ERROR,
            "Internal Server Error",
            "could not encode the prompt queue",
        ),
    }
}

fn store_error_response(e: StoreError) -> Response<Body> {
    match e {
        StoreError::BadText(why) => {
            problem(ERR_BAD_TEXT, StatusCode::BAD_REQUEST, "Bad Request", why)
        }
        StoreError::QueueFull => problem(
            ERR_QUEUE_FULL,
            StatusCode::CONFLICT,
            "Conflict",
            "this project's prompt queue is full (100 items, done items included)",
        ),
        StoreError::TooManyProjects => problem(
            ERR_TOO_MANY_PROJECTS,
            StatusCode::CONFLICT,
            "Conflict",
            "too many projects have a prompt queue (64)",
        ),
        StoreError::NotFound => problem(
            ERR_PROMPT_NOT_FOUND,
            StatusCode::NOT_FOUND,
            "Not Found",
            "no such prompt in this project's queue",
        ),
        StoreError::Stale { current } => with_etag(
            problem(
                ERR_PROMPTS_STALE,
                StatusCode::PRECONDITION_FAILED,
                "Precondition Failed",
                &format!("the queue changed; the current revision is {current}"),
            ),
            current,
        ),
        StoreError::Corrupt => problem(
            ERR_PROMPTS_CORRUPT,
            StatusCode::INTERNAL_SERVER_ERROR,
            "Internal Server Error",
            "the prompt ledger is unreadable or has an unknown schema; it was left untouched",
        ),
        StoreError::Io => problem(
            ERR_PROMPTS_IO,
            StatusCode::INTERNAL_SERVER_ERROR,
            "Internal Server Error",
            "the prompt ledger could not be read or written",
        ),
    }
}

/// A bad project slug is a 400 here, before any path join.
fn parse_slug(project: &str) -> Result<SlateSlug, Response<Body>> {
    SlateSlug::new(project).map_err(|_| {
        bad_request("invalid project slug (expected 1-64 characters of a-z, 0-9, `_`, `-`)")
    })
}

fn check_item_id(item: &str) -> Result<(), Response<Body>> {
    if is_valid_item_id(item) {
        Ok(())
    } else {
        Err(bad_request("invalid prompt id"))
    }
}

enum IfMatch {
    Absent,
    Rev(u64),
    Malformed,
}

/// `If-Match: "<rev>"` (a bare number and a `W/` prefix are tolerated).
fn parse_if_match(headers: &HeaderMap) -> IfMatch {
    let Some(raw) = headers.get(header::IF_MATCH) else {
        return IfMatch::Absent;
    };
    let Ok(s) = raw.to_str() else {
        return IfMatch::Malformed;
    };
    let s = s.trim();
    let s = s.strip_prefix("W/").unwrap_or(s);
    let s = s.trim_matches('"');
    match s.parse::<u64>() {
        Ok(rev) => IfMatch::Rev(rev),
        Err(_) => IfMatch::Malformed,
    }
}

fn parse_body<T: serde::de::DeserializeOwned>(body: &Bytes) -> Result<T, Response<Body>> {
    if body.len() > MAX_BODY_BYTES {
        return Err(problem(
            ERR_TERMINAL_PAYLOAD_TOO_LARGE,
            StatusCode::PAYLOAD_TOO_LARGE,
            "Payload Too Large",
            "request body over 16 KiB",
        ));
    }
    serde_json::from_slice(body).map_err(|_| bad_request("malformed request body"))
}

fn now_unix() -> i64 {
    chrono::Utc::now().timestamp()
}

/// Emit `term.prompts` `{project, rev}` for one durable mutation.
fn emit_changed(state: &KbHandles, list: &PromptList) {
    let ev = TermPromptsEvent {
        project: list.project.clone(),
        rev: list.rev,
    };
    state.bus.emit(
        "term.prompts",
        serde_json::to_value(&ev).unwrap_or(serde_json::Value::Null),
    );
}

/// Run one mutation under the daemon-wide lock and finish the response.
/// The lock is held across the (synchronous) load-modify-write and the
/// (synchronous) emit and across nothing else.
async fn mutate(
    state: &KbHandles,
    slug: &SlateSlug,
    if_match: Option<u64>,
    op: Op,
    ok_status: StatusCode,
) -> Response<Body> {
    let guard = state.terminals.prompts_lock.lock().await;
    let out = apply(&state.paths, slug, if_match, op, now_unix());
    if let Ok(applied) = &out {
        if applied.changed {
            emit_changed(state, &applied.list);
        }
    }
    drop(guard);
    match out {
        Ok(applied) => list_response(ok_status, &applied.list),
        Err(e) => store_error_response(e),
    }
}

/// `GET /api/term-prompts/{project}` — the queue, with its `ETag`.
pub async fn list(
    State(state): State<Arc<KbHandles>>,
    Extension(identity): Extension<Identity>,
    headers: HeaderMap,
    AxPath(project): AxPath<String>,
) -> Response<Body> {
    if let Err(e) = gate_read(&state, &headers, &identity).await {
        return e;
    }
    let slug = match parse_slug(&project) {
        Ok(s) => s,
        Err(e) => return e,
    };
    // A rename is atomic, so a reader never sees a torn file; no lock.
    match read_list(&state.paths, &slug) {
        Ok(list) => list_response(StatusCode::OK, &list),
        Err(e) => store_error_response(e),
    }
}

/// `POST /api/term-prompts/{project}` — append `{text}` (201). No
/// `If-Match`: adds commute.
pub async fn add(
    State(state): State<Arc<KbHandles>>,
    Extension(identity): Extension<Identity>,
    headers: HeaderMap,
    AxPath(project): AxPath<String>,
    body: Bytes,
) -> Response<Body> {
    if let Err(e) = gate(&state, &headers, &identity).await {
        return e;
    }
    let slug = match parse_slug(&project) {
        Ok(s) => s,
        Err(e) => return e,
    };
    let parsed: PromptAdd = match parse_body(&body) {
        Ok(p) => p,
        Err(e) => return e,
    };
    mutate(
        &state,
        &slug,
        None,
        Op::Add(parsed.text),
        StatusCode::CREATED,
    )
    .await
}

/// `PATCH /api/term-prompts/{project}/{item}` — edit text, mark done or
/// reorder. `If-Match` REQUIRED (428 without, 412 when stale).
pub async fn patch(
    State(state): State<Arc<KbHandles>>,
    Extension(identity): Extension<Identity>,
    headers: HeaderMap,
    AxPath((project, item)): AxPath<(String, String)>,
    body: Bytes,
) -> Response<Body> {
    if let Err(e) = gate(&state, &headers, &identity).await {
        return e;
    }
    let slug = match parse_slug(&project) {
        Ok(s) => s,
        Err(e) => return e,
    };
    if let Err(e) = check_item_id(&item) {
        return e;
    }
    let rev = match parse_if_match(&headers) {
        IfMatch::Rev(r) => r,
        IfMatch::Absent => {
            return problem(
                ERR_PRECONDITION_REQUIRED,
                StatusCode::PRECONDITION_REQUIRED,
                "Precondition Required",
                "PATCH requires an `If-Match` header carrying the queue's ETag",
            )
        }
        IfMatch::Malformed => return bad_request("malformed If-Match header"),
    };
    let parsed: PromptPatch = match parse_body(&body) {
        Ok(p) => p,
        Err(e) => return e,
    };
    if parsed.text.is_none() && parsed.done.is_none() && parsed.position.is_none() {
        return bad_request("PATCH needs at least one of `text`, `done`, `position`");
    }
    mutate(
        &state,
        &slug,
        Some(rev),
        Op::Patch {
            id: item,
            text: parsed.text,
            done: parsed.done,
            position: parsed.position,
        },
        StatusCode::OK,
    )
    .await
}

/// `DELETE /api/term-prompts/{project}/{item}` — remove an item and answer
/// the queue. Honours `If-Match` when present (412 when stale).
pub async fn delete(
    State(state): State<Arc<KbHandles>>,
    Extension(identity): Extension<Identity>,
    headers: HeaderMap,
    AxPath((project, item)): AxPath<(String, String)>,
) -> Response<Body> {
    if let Err(e) = gate(&state, &headers, &identity).await {
        return e;
    }
    let slug = match parse_slug(&project) {
        Ok(s) => s,
        Err(e) => return e,
    };
    if let Err(e) = check_item_id(&item) {
        return e;
    }
    let rev = match parse_if_match(&headers) {
        IfMatch::Rev(r) => Some(r),
        IfMatch::Absent => None,
        IfMatch::Malformed => return bad_request("malformed If-Match header"),
    };
    mutate(&state, &slug, rev, Op::Delete { id: item }, StatusCode::OK).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use tracing_subscriber::fmt::MakeWriter;

    const SENTINEL: &str = "synthetic-prompt-sentinel-77ab";

    fn fixture() -> (tempfile::TempDir, KbPaths, SlateSlug) {
        let tmp = tempfile::tempdir().unwrap();
        let paths = KbPaths::rooted_at(tmp.path(), "t");
        (tmp, paths, SlateSlug::new("proj").unwrap())
    }

    fn add(paths: &KbPaths, slug: &SlateSlug, text: &str) -> Result<Applied, StoreError> {
        apply(paths, slug, None, Op::Add(text.to_string()), 1_000)
    }

    fn ids(list: &PromptList) -> Vec<String> {
        list.items.iter().map(|i| i.id.clone()).collect()
    }

    fn patch_op(id: &str, text: Option<&str>, done: Option<bool>, pos: Option<u32>) -> Op {
        Op::Patch {
            id: id.to_string(),
            text: text.map(String::from),
            done,
            position: pos,
        }
    }

    #[test]
    fn roundtrip_orders_by_index_and_leaves_no_tmp_file() {
        let (_t, paths, slug) = fixture();
        assert_eq!(read_list(&paths, &slug).unwrap().rev, 0);
        add(&paths, &slug, "  first  ").unwrap();
        let l = add(&paths, &slug, "second").unwrap().list;
        assert_eq!(l.rev, 2);
        assert_eq!(l.project, "proj");
        assert_eq!(l.items[0].text, "first", "text is stored trimmed");
        assert_eq!(l.items[0].order, 0);
        assert_eq!(l.items[1].order, 1);
        assert!(is_valid_item_id(&l.items[0].id), "{}", l.items[0].id);
        assert_ne!(l.items[0].id, l.items[1].id);
        // A fresh read sees the same thing from disk.
        let again = read_list(&paths, &slug).unwrap();
        assert_eq!(ids(&again), ids(&l));
        // The on-disk document is exactly the schema, with no stored order.
        let raw: serde_json::Value =
            serde_json::from_slice(&std::fs::read(paths.term_prompts_file(&slug)).unwrap())
                .unwrap();
        assert_eq!(raw["schema"], SCHEMA);
        assert_eq!(raw["rev"], 2);
        assert!(raw["items"][0].get("order").is_none());
        let names: Vec<String> = std::fs::read_dir(paths.term_prompts_dir())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["proj.json".to_string()], "no tmp left behind");
    }

    #[test]
    fn unknown_schema_and_corrupt_json_are_refused_and_left_untouched() {
        let (_t, paths, slug) = fixture();
        std::fs::create_dir_all(paths.term_prompts_dir()).unwrap();
        let file = paths.term_prompts_file(&slug);
        for body in [
            &b"{\"schema\":\"kb-term-prompts/2\",\"project\":\"proj\",\"rev\":3,\"items\":[]}"[..],
            &b"{not json"[..],
            &b"{\"schema\":\"kb-term-prompts/1\",\"project\":\"other\",\"rev\":3,\"items\":[]}"[..],
        ] {
            std::fs::write(&file, body).unwrap();
            assert_eq!(read_list(&paths, &slug).unwrap_err(), StoreError::Corrupt);
            assert_eq!(add(&paths, &slug, "x").unwrap_err(), StoreError::Corrupt);
            let r = apply(
                &paths,
                &slug,
                Some(3),
                Op::Delete {
                    id: "q_000000000000".into(),
                },
                1,
            );
            assert_eq!(r.unwrap_err(), StoreError::Corrupt);
            assert_eq!(
                std::fs::read(&file).unwrap(),
                body,
                "file must be untouched"
            );
        }
    }

    #[test]
    fn text_bounds_are_exact() {
        let (_t, paths, slug) = fixture();
        assert!(matches!(
            add(&paths, &slug, ""),
            Err(StoreError::BadText(_))
        ));
        assert!(matches!(
            add(&paths, &slug, " \n\t "),
            Err(StoreError::BadText(_))
        ));
        assert!(add(&paths, &slug, "a").is_ok(), "1 byte is the floor");
        assert!(add(&paths, &slug, &"a".repeat(8192)).is_ok(), "8192 passes");
        assert!(matches!(
            add(&paths, &slug, &"a".repeat(8193)),
            Err(StoreError::BadText(_))
        ));
        // The cap is on the trimmed text.
        let padded = format!("  {}  ", "a".repeat(8192));
        assert!(add(&paths, &slug, &padded).is_ok());
        assert!(matches!(
            add(&paths, &slug, "a\0b"),
            Err(StoreError::BadText(_))
        ));
        // Multi-byte text counts bytes, not chars: 4097 two-byte chars.
        assert!(matches!(
            add(&paths, &slug, &"é".repeat(4097)),
            Err(StoreError::BadText(_))
        ));
        assert!(add(&paths, &slug, &"é".repeat(4096)).is_ok());
        // A refused add never bumped the revision.
        assert_eq!(read_list(&paths, &slug).unwrap().rev, 4);
    }

    #[test]
    fn the_hundredth_item_is_accepted_and_the_next_is_not() {
        let (_t, paths, slug) = fixture();
        for i in 0..MAX_ITEMS {
            add(&paths, &slug, &format!("p{i}")).unwrap();
        }
        assert_eq!(
            add(&paths, &slug, "one more").unwrap_err(),
            StoreError::QueueFull
        );
        let l = read_list(&paths, &slug).unwrap();
        assert_eq!(l.items.len(), 100);
        assert_eq!(l.rev, 100);
        // Done items still count toward the cap.
        let first = l.items[0].id.clone();
        apply(
            &paths,
            &slug,
            None,
            patch_op(&first, None, Some(true), None),
            5,
        )
        .unwrap();
        assert_eq!(
            add(&paths, &slug, "still full").unwrap_err(),
            StoreError::QueueFull
        );
        // Deleting one makes room again.
        apply(&paths, &slug, None, Op::Delete { id: first }, 6).unwrap();
        assert!(add(&paths, &slug, "room").is_ok());
    }

    #[test]
    fn the_sixty_fourth_project_is_accepted_and_the_next_is_not() {
        let (_t, paths, _s) = fixture();
        for i in 0..MAX_PROJECTS {
            let slug = SlateSlug::new(format!("p{i}")).unwrap();
            add(&paths, &slug, "x").unwrap();
        }
        let extra = SlateSlug::new("one-too-many").unwrap();
        assert_eq!(
            add(&paths, &extra, "x").unwrap_err(),
            StoreError::TooManyProjects
        );
        assert!(!paths.term_prompts_file(&extra).exists());
        // An existing project keeps working at the cap.
        let first = SlateSlug::new("p0").unwrap();
        assert!(add(&paths, &first, "y").is_ok());
    }

    #[test]
    fn bad_slugs_never_parse() {
        let too_long = "a".repeat(65);
        let longest = "a".repeat(64);
        for bad in [
            "../x",
            "A",
            too_long.as_str(),
            "a/b",
            "",
            "a.b",
            "a b",
            "%2e%2e",
        ] {
            assert!(parse_slug(bad).is_err(), "{bad:?} must be refused");
        }
        for good in ["a", "my-proj_1", longest.as_str()] {
            assert!(parse_slug(good).is_ok(), "{good:?} must parse");
        }
    }

    #[test]
    fn item_id_shape() {
        assert!(is_valid_item_id("q_0123456789ab"));
        for bad in [
            "q_0123456789AB",
            "q_0123456789a",
            "x_0123456789ab",
            "q_0123456789abc",
            "../etc",
        ] {
            assert!(!is_valid_item_id(bad), "{bad}");
        }
    }

    #[test]
    fn position_moves_and_clamps() {
        let (_t, paths, slug) = fixture();
        for t in ["a", "b", "c", "d"] {
            add(&paths, &slug, t).unwrap();
        }
        let l = read_list(&paths, &slug).unwrap();
        let texts = |l: &PromptList| l.items.iter().map(|i| i.text.clone()).collect::<Vec<_>>();
        let a = l.items[0].id.clone();
        // Move the first to index 2.
        let l = apply(&paths, &slug, Some(4), patch_op(&a, None, None, Some(2)), 9)
            .unwrap()
            .list;
        assert_eq!(texts(&l), ["b", "c", "a", "d"]);
        assert_eq!(l.rev, 5);
        assert_eq!(l.items[2].order, 2);
        // A huge position clamps to the last slot.
        let l = apply(
            &paths,
            &slug,
            Some(5),
            patch_op(&a, None, None, Some(9999)),
            9,
        )
        .unwrap()
        .list;
        assert_eq!(texts(&l), ["b", "c", "d", "a"]);
        // Position 0 moves it to the front.
        let l = apply(&paths, &slug, Some(6), patch_op(&a, None, None, Some(0)), 9)
            .unwrap()
            .list;
        assert_eq!(texts(&l), ["a", "b", "c", "d"]);
        // Moving to where it already is is a no-op: no rev bump, no write.
        let r = apply(&paths, &slug, Some(7), patch_op(&a, None, None, Some(0)), 9).unwrap();
        assert!(!r.changed);
        assert_eq!(r.list.rev, 7);
    }

    #[test]
    fn done_sets_and_clears_done_at_and_a_repeat_is_a_noop() {
        let (_t, paths, slug) = fixture();
        let l = add(&paths, &slug, "a").unwrap().list;
        let id = l.items[0].id.clone();
        assert_eq!(l.items[0].done_at, None);
        let l = apply(
            &paths,
            &slug,
            Some(1),
            patch_op(&id, None, Some(true), None),
            4242,
        )
        .unwrap()
        .list;
        assert_eq!(l.items[0].done_at, Some(4242));
        // Done again: nothing changes, `done_at` keeps its first stamp.
        let r = apply(
            &paths,
            &slug,
            Some(2),
            patch_op(&id, None, Some(true), None),
            9999,
        )
        .unwrap();
        assert!(!r.changed);
        assert_eq!(r.list.items[0].done_at, Some(4242));
        assert_eq!(r.list.rev, 2);
        let l = apply(
            &paths,
            &slug,
            Some(2),
            patch_op(&id, None, Some(false), None),
            5,
        )
        .unwrap()
        .list;
        assert_eq!(l.items[0].done_at, None);
        assert_eq!(l.rev, 3);
    }

    #[test]
    fn text_edit_validates_and_an_unchanged_text_is_a_noop() {
        let (_t, paths, slug) = fixture();
        let id = add(&paths, &slug, "old").unwrap().list.items[0].id.clone();
        let l = apply(
            &paths,
            &slug,
            Some(1),
            patch_op(&id, Some("  new "), None, None),
            2,
        )
        .unwrap()
        .list;
        assert_eq!(l.items[0].text, "new");
        assert_eq!(l.rev, 2);
        let r = apply(
            &paths,
            &slug,
            Some(2),
            patch_op(&id, Some("new"), None, None),
            3,
        )
        .unwrap();
        assert!(!r.changed);
        assert!(matches!(
            apply(
                &paths,
                &slug,
                Some(2),
                patch_op(&id, Some(" "), None, None),
                3
            ),
            Err(StoreError::BadText(_))
        ));
        assert_eq!(
            apply(
                &paths,
                &slug,
                Some(2),
                patch_op("q_000000000000", Some("z"), None, None),
                3
            )
            .unwrap_err(),
            StoreError::NotFound
        );
    }

    #[test]
    fn stale_revision_is_refused_and_the_file_is_byte_identical() {
        let (_t, paths, slug) = fixture();
        let id = add(&paths, &slug, "a").unwrap().list.items[0].id.clone();
        let file = paths.term_prompts_file(&slug);
        let before = std::fs::read(&file).unwrap();
        let err = apply(
            &paths,
            &slug,
            Some(0),
            patch_op(&id, None, Some(true), None),
            5,
        )
        .unwrap_err();
        assert_eq!(err, StoreError::Stale { current: 1 });
        let err = apply(&paths, &slug, Some(0), Op::Delete { id }, 5).unwrap_err();
        assert_eq!(err, StoreError::Stale { current: 1 });
        assert_eq!(std::fs::read(&file).unwrap(), before);
    }

    #[test]
    fn debug_output_never_contains_the_text() {
        let item = PromptItem {
            id: "q_000000000000".into(),
            text: SENTINEL.into(),
            created_at: 1,
            done_at: None,
            order: 0,
        };
        let list = PromptList {
            project: "p".into(),
            rev: 1,
            items: vec![item.clone()],
        };
        let stored = StoredItem {
            id: "q_000000000000".into(),
            text: SENTINEL.into(),
            created_at: 1,
            done_at: None,
        };
        let doc = Doc {
            schema: SCHEMA.into(),
            project: "p".into(),
            rev: 1,
            items: vec![stored.clone()],
        };
        let add = PromptAdd {
            text: SENTINEL.into(),
        };
        let patch = PromptPatch {
            text: Some(SENTINEL.into()),
            done: None,
            position: None,
        };
        for dbg in [
            format!("{item:?}"),
            format!("{list:?}"),
            format!("{stored:?}"),
            format!("{doc:?}"),
            format!("{add:?}"),
            format!("{patch:?}"),
            format!(
                "{:#?}",
                Applied {
                    list,
                    changed: true
                }
            ),
        ] {
            assert!(!dbg.contains(SENTINEL), "Debug leaked the text: {dbg}");
        }
    }

    #[derive(Clone)]
    struct Capture(Arc<Mutex<Vec<u8>>>);

    impl Write for Capture {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> MakeWriter<'a> for Capture {
        type Writer = Capture;
        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    #[test]
    fn tracing_output_never_contains_the_text() {
        let cap = Capture(Arc::new(Mutex::new(Vec::new())));
        let sub = tracing_subscriber::fmt()
            .with_writer(cap.clone())
            .with_max_level(tracing::Level::TRACE)
            .with_ansi(false)
            .finish();
        tracing::subscriber::with_default(sub, || {
            let (_t, paths, slug) = fixture();
            tracing::info!("capture-marker");
            let l = add(&paths, &slug, SENTINEL).unwrap().list;
            let id = l.items[0].id.clone();
            apply(
                &paths,
                &slug,
                Some(1),
                patch_op(&id, Some(SENTINEL), Some(true), Some(0)),
                3,
            )
            .unwrap();
            let _ = apply(&paths, &slug, Some(99), Op::Delete { id: id.clone() }, 3);
            // A corrupt-ledger refusal logs too: it must not echo content.
            std::fs::write(
                paths.term_prompts_file(&slug),
                format!("{{\"schema\":\"{SENTINEL}\"}}"),
            )
            .unwrap();
            let _ = read_list(&paths, &slug);
            let _ = add(&paths, &slug, SENTINEL);
        });
        let logged = String::from_utf8_lossy(&cap.0.lock().unwrap()).into_owned();
        assert!(
            logged.contains("capture-marker"),
            "capture must work: {logged}"
        );
        assert!(
            logged.contains("term-prompts"),
            "the module logs, so the check is not vacuous: {logged}"
        );
        assert!(!logged.contains(SENTINEL), "text reached the log: {logged}");
    }

    #[test]
    fn if_match_parsing() {
        let h = |v: &str| {
            let mut m = HeaderMap::new();
            m.insert(header::IF_MATCH, HeaderValue::from_str(v).unwrap());
            m
        };
        assert!(matches!(parse_if_match(&HeaderMap::new()), IfMatch::Absent));
        assert!(matches!(parse_if_match(&h("\"7\"")), IfMatch::Rev(7)));
        assert!(matches!(parse_if_match(&h("7")), IfMatch::Rev(7)));
        assert!(matches!(parse_if_match(&h("W/\"7\"")), IfMatch::Rev(7)));
        assert!(matches!(parse_if_match(&h("*")), IfMatch::Malformed));
        assert!(matches!(parse_if_match(&h("\"x\"")), IfMatch::Malformed));
        assert!(matches!(parse_if_match(&h("\"-1\"")), IfMatch::Malformed));
    }

    #[test]
    fn event_payload_keys_match_the_declared_schema_keys() {
        let v = serde_json::to_value(TermPromptsEvent {
            project: "p".into(),
            rev: 3,
        })
        .unwrap();
        let mut got: Vec<String> = v.as_object().unwrap().keys().cloned().collect();
        got.sort();
        let mut want: Vec<String> = crate::routes::schema::TERM_PROMPTS_KEYS
            .iter()
            .map(|s| s.to_string())
            .collect();
        want.sort();
        assert_eq!(got, want);
    }
}
